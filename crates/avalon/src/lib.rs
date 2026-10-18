//! Reading an Avalon over the cgminer API, and nothing else.
//!
//! An Avalon's MM speaks the cgminer socket protocol: TCP 4028, one JSON
//! request (`{"command":"summary"}`), one JSON reply terminated by a NUL
//! byte, then the server closes the connection. A browser cannot open
//! such a socket, so WarpAxe does it for its swarm page and hands the
//! answer on as an AxeOS-like `/api/system/info` document, which is what
//! the page's cards already read.
//!
//! What this crate sends is exactly [`COMMANDS`]: `summary`, `estats` and
//! `pools`, all reads. There is no code path for `ascset`, `setpool` or
//! any other write, and none should ever be added here: the firmware
//! holds an Avalon's address, not the right to change it.
//!
//! The replies are bounded ([`MAX_REPLY`]) and parsed defensively: the
//! `estats` of an Avalon Q is one long `Key[Value]` token string
//! (`MM ID0:Summary`) plus the per-chip arrays in `HBinfo`, both read
//! token by token so a field this firmware has not seen does no harm.
//! Field meanings were checked against Oliver's Avalon Q (MM 25052801,
//! cgminer 4.11.1, API 3.7) on 2026-10-18 and against WarpTune's own
//! Avalon client; the fixtures under `fixtures/` are those replies with
//! the wallet masked.

#![forbid(unsafe_code)]
#![recursion_limit = "256"]

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};

/// The cgminer API port.
pub const PORT: u16 = 4028;
/// The only commands this crate sends, in the order they are asked.
pub const COMMANDS: [&str; 3] = ["summary", "estats", "pools"];
/// Longest reply taken; the Avalon Q's `estats` is 3.6 KB, `pools` with
/// two pools 1.8 KB. Beyond this the reply is refused, not truncated.
pub const MAX_REPLY: usize = 16 * 1024;

/// Why a read failed; the text goes to the swarm card as "not reachable"
/// with the reason in the API reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No connection (refused, unreachable, timed out, no such host).
    Connect(String),
    /// The socket failed or timed out while the reply was being read.
    Read(String),
    /// More than [`MAX_REPLY`] bytes without the terminating NUL.
    TooLong,
    /// Not a JSON document (a truncated reply, or not a cgminer at all).
    NotJson(String),
    /// A JSON document without the section the command should carry.
    Shape(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(e) => write!(f, "connect: {e}"),
            Error::Read(e) => write!(f, "read: {e}"),
            Error::TooLong => write!(f, "reply longer than {MAX_REPLY} bytes"),
            Error::NotJson(e) => write!(f, "not a cgminer reply: {e}"),
            Error::Shape(what) => write!(f, "reply without {what}"),
        }
    }
}

/// Read one reply: everything up to the NUL byte that ends it, or to
/// EOF (the server closes after the reply), at most `max` bytes.
pub fn read_reply<R: Read>(r: &mut R, max: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(1024);
    let mut buf = [0u8; 512];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                // A timeout after part of the reply: hand back what came,
                // the parser says whether it is whole.
                if !out.is_empty()
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                {
                    break;
                }
                return Err(Error::Read(e.to_string()));
            }
        };
        if let Some(end) = buf[..n].iter().position(|&b| b == 0) {
            out.extend_from_slice(&buf[..end]);
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() > max {
            return Err(Error::TooLong);
        }
    }
    Ok(out)
}

/// The reply bytes as JSON. cgminer ends the document with a NUL and
/// some builds add a newline; both are dropped.
pub fn parse_reply(bytes: &[u8]) -> Result<Value, Error> {
    let end = bytes
        .iter()
        .rposition(|&b| b != 0 && !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    let text = std::str::from_utf8(&bytes[..end]).map_err(|e| Error::NotJson(e.to_string()))?;
    serde_json::from_str(text).map_err(|e| Error::NotJson(e.to_string()))
}

/// Send one of [`COMMANDS`] over an open stream and read the reply.
/// Other command names are refused here, so no caller can widen what
/// goes over the wire.
pub fn query(stream: &mut TcpStream, command: &str, timeout: Duration) -> Result<Value, Error> {
    if !COMMANDS.contains(&command) {
        return Err(Error::Shape("a read command"));
    }
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| Error::Read(e.to_string()))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| Error::Read(e.to_string()))?;
    let req = json!({ "command": command }).to_string();
    stream
        .write_all(req.as_bytes())
        .map_err(|e| Error::Read(e.to_string()))?;
    let bytes = read_reply(stream, MAX_REPLY)?;
    parse_reply(&bytes)
}

/// The three replies of one round.
#[derive(Debug, Clone)]
pub struct Raw {
    pub summary: Value,
    pub estats: Value,
    pub pools: Value,
}

/// Connect to `host:port` once per command (the server closes after
/// each reply) and read all three. `timeout` bounds the connect and
/// each read; a round on a LAN takes well under a second.
pub fn fetch(host: &str, port: u16, timeout: Duration) -> Result<Raw, Error> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| Error::Connect(e.to_string()))?
        .next()
        .ok_or_else(|| Error::Connect("no address".into()))?;
    let mut ask = |command: &str| -> Result<Value, Error> {
        let mut s = TcpStream::connect_timeout(&addr, timeout)
            .map_err(|e| Error::Connect(e.to_string()))?;
        let _ = s.set_nodelay(true);
        query(&mut s, command, timeout)
    };
    Ok(Raw {
        summary: ask("summary")?,
        estats: ask("estats")?,
        pools: ask("pools")?,
    })
}

/// `Key[Value]` tokens of an MM summary string. Values may hold spaces
/// (`PS[0 1208 2295 34 792 2296 840]`) and keys a space (`Nonce Mask`),
/// so a token is read up to its closing bracket and the key is what
/// stands between the previous bracket and this one.
pub fn mm_tokens(s: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let key = rest[..open]
            .rsplit(|c: char| c == ']' || c == '{' || c == ':')
            .next()
            .unwrap_or("")
            .trim();
        let value = &rest[open + 1..open + close];
        if !key.is_empty() {
            out.push((key, value));
        }
        rest = &rest[open + close + 1..];
    }
    out
}

/// What the translation reads out of `estats`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mm {
    /// `Ver[Q-25052801_14a19a2]`: the first letter group is the model.
    pub version: String,
    pub dna: String,
    pub state: Option<i64>,
    pub elapsed_s: Option<u64>,
    pub temp_max_c: Option<f64>,
    pub temp_avg_c: Option<f64>,
    pub temp_target_c: Option<f64>,
    pub temp_inlet_c: Option<f64>,
    pub fan_rpm: Vec<u32>,
    pub fan_pct: Option<f64>,
    /// `PS[type vin×100mV iin_mA ? vout×100mV iout_mA watts]`: the
    /// last field is the miner's draw as its own LCD shows it (840 W at
    /// 54 TH/s in Eco); the first two give a small auxiliary bus
    /// (12.08 V × 2.3 A), not the mining power.
    pub power_w: Option<f64>,
    pub ps_vin_v: Option<f64>,
    pub ghs_now: Option<f64>,
    pub ghs_avg: Option<f64>,
    pub freq_mhz: Option<f64>,
    pub work_mode: Option<u8>,
    pub work_level: Option<u8>,
    pub mode_power_cap_w: Option<f64>,
    /// `DH[3.501%]`: the device's hardware-error share of its work.
    pub error_percent: Option<f64>,
    pub chip_name: String,
    pub chip_count: Option<u32>,
    pub system_status: String,
    pub chip_temps: Vec<f64>,
}

fn num(v: &str) -> Option<f64> {
    v.trim().trim_end_matches('%').parse().ok()
}

/// Every string value of the `STATS` objects, token by token.
pub fn parse_estats(estats: &Value) -> Mm {
    let mut mm = Mm::default();
    let Some(stats) = estats.get("STATS").and_then(Value::as_array) else {
        return mm;
    };
    for obj in stats.iter().filter_map(Value::as_object) {
        for text in obj.values().filter_map(Value::as_str) {
            for (key, value) in mm_tokens(text) {
                match key {
                    "Ver" => mm.version = value.to_string(),
                    "DNA" => mm.dna = value.to_string(),
                    "STATE" => mm.state = value.trim().parse().ok(),
                    "Elapsed" => mm.elapsed_s = value.trim().parse().ok(),
                    "TMax" => mm.temp_max_c = num(value),
                    "TAvg" => mm.temp_avg_c = num(value),
                    "TarT" => mm.temp_target_c = num(value),
                    "ITemp" => mm.temp_inlet_c = num(value),
                    "FanR" => mm.fan_pct = num(value),
                    "Fan1" | "Fan2" | "Fan3" | "Fan4" => {
                        if let Some(rpm) = num(value) {
                            mm.fan_rpm.push(rpm as u32);
                        }
                    }
                    "PS" => {
                        let f: Vec<f64> = value.split_whitespace().filter_map(num).collect();
                        if let Some(w) = f.last().filter(|_| f.len() >= 7) {
                            mm.power_w = Some(*w);
                        }
                        if let Some(v) = f.get(1) {
                            mm.ps_vin_v = Some(v / 100.0);
                        }
                    }
                    "GHSspd" => mm.ghs_now = num(value),
                    "GHSavg" => mm.ghs_avg = num(value),
                    "Freq" => mm.freq_mhz = num(value),
                    "WORKMODE" => mm.work_mode = value.trim().parse().ok(),
                    "WORKLEVEL" => mm.work_level = value.trim().parse().ok(),
                    "MPO" => mm.mode_power_cap_w = num(value),
                    "DH" => mm.error_percent = num(value),
                    "Core" => mm.chip_name = value.trim().to_string(),
                    "TA" => mm.chip_count = value.trim().parse().ok(),
                    "SYSTEMSTATU" => mm.system_status = value.trim().to_string(),
                    "PVT_T0" => {
                        mm.chip_temps = value.split_whitespace().filter_map(num).collect();
                    }
                    _ => {}
                }
            }
        }
    }
    mm
}

/// The pool the Avalon mines on: the one with an active Stratum
/// session, else the first.
fn active_pool(pools: &Value) -> Option<&serde_json::Map<String, Value>> {
    let list = pools.get("POOLS")?.as_array()?;
    list.iter()
        .filter_map(Value::as_object)
        .find(|p| p.get("Stratum Active").and_then(Value::as_bool) == Some(true))
        .or_else(|| list.first().and_then(Value::as_object))
}

/// `stratum+tcp://192.168.178.3:3333` → ("192.168.178.3", 3333).
fn split_url(url: &str) -> (String, Option<u16>) {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split('/').next().unwrap_or(rest);
    match rest.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), p.parse().ok())
        }
        _ => (rest.to_string(), None),
    }
}

/// `Q-25052801_14a19a2` → ("Avalon Q", "25052801_14a19a2").
fn model_and_firmware(ver: &str) -> (String, String) {
    match ver.split_once('-') {
        Some((m, fw)) if !m.is_empty() => (format!("Avalon {m}"), fw.to_string()),
        _ => ("Avalon".into(), ver.to_string()),
    }
}

/// The work mode's name as the Avalon's own UI shows it.
pub fn work_mode_name(index: u8) -> &'static str {
    match index {
        0 => "Eco",
        1 => "Standard",
        2 => "Super",
        _ => "?",
    }
}

/// The three replies as one AxeOS-like document: the fields a swarm
/// card reads under AxeOS's names, the Avalon's own under their own,
/// and `cgminer` as the marker. A value the Avalon does not report is
/// `null`, never 0: it has no regulator temperature, no core voltage and
/// no shutdown temperature WarpAxe could know.
pub fn translate(raw: &Raw) -> Value {
    let s = raw
        .summary
        .get("SUMMARY")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null);
    let api = raw
        .summary
        .get("STATUS")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|st| st.get("Description"))
        .and_then(Value::as_str)
        .unwrap_or("cgminer")
        .to_string();
    let mm = parse_estats(&raw.estats);
    let pool = active_pool(&raw.pools);
    let pf = |k: &str| pool.and_then(|p| p.get(k)).cloned().unwrap_or(Value::Null);
    let sf = |k: &str| s.get(k).cloned().unwrap_or(Value::Null);
    let ghs = |k: &str| s.get(k).and_then(Value::as_f64).map(|mhs| mhs / 1000.0);
    let (model, firmware) = model_and_firmware(&mm.version);
    let url = pf("URL").as_str().unwrap_or("").to_string();
    let (host, port) = split_url(&url);
    let chip_count = if mm.chip_temps.is_empty() {
        mm.chip_count
    } else {
        Some(mm.chip_temps.len() as u32)
    };
    let hashing = mm.ghs_now.is_some_and(|g| g > 0.0);
    let alive = pf("Status").as_str() == Some("Alive");
    let best = sf("Best Share");
    json!({
        "cgminer": { "api": api, "commands": COMMANDS, "host": pf("URL") },
        "deviceModel": model, "boardVersion": model, "boardName": model, "hostname": model,
        "ASICModel": mm.chip_name, "asicCount": chip_count,
        "version": format!("MM {}", firmware.split('_').next().unwrap_or(&firmware)),
        "firmware": firmware, "dna": mm.dna, "systemStatus": mm.system_status,
        "chainPhase": if hashing { "READY" } else { "POWER" },
        "overheat_mode": 0, "shutdown": false,
        // The headline rate: the five-minute mean, near the ten minutes
        // of an AxeOS `hashRate`; the shorter and longer windows beside it.
        "hashRate": ghs("MHS 5m"), "hashRate_now": mm.ghs_now, "hashRate_1m": ghs("MHS 1m"),
        "hashRate_15m": ghs("MHS 15m"), "hashRate_av": ghs("MHS av"), "hashRate_avg_mm": mm.ghs_avg,
        "power": mm.power_w, "powerEstimated": false, "psVoltage": mm.ps_vin_v,
        "temp": mm.temp_avg_c, "tempMax": mm.temp_max_c, "tempTarget": mm.temp_target_c, "tempInlet": mm.temp_inlet_c,
        "chipTemps": mm.chip_temps,
        "overheat_temp": null, "vrTemp": null, "vrOverheatTemp": null, "hasVr": false, "hasFan": true,
        "fanrpm": mm.fan_rpm.first(), "fanRpms": mm.fan_rpm, "fanspeed": mm.fan_pct,
        "frequency": mm.freq_mhz.map(f64::round), "frequencyActual": mm.freq_mhz.map(f64::round),
        "coreVoltage": null, "coreVoltageActual": null,
        "workMode": mm.work_mode.map(work_mode_name), "workModeIndex": mm.work_mode,
        "workLevel": mm.work_level, "modePowerCapW": mm.mode_power_cap_w,
        "sharesAccepted": sf("Accepted"), "sharesRejected": sf("Rejected"), "sharesStale": sf("Stale"),
        "hardwareErrors": sf("Hardware Errors"), "errorPercentage": mm.error_percent,
        "deviceRejectedPercent": sf("Device Rejected%"),
        // One figure only: cgminer keeps its best share since it started
        // (weeks on an Avalon), nothing across restarts.
        "bestDiff": best, "bestSessionDiff": best,
        "uptimeSeconds": sf("Elapsed"), "blockFound": sf("Found Blocks"),
        "stratumURL": host, "stratumPort": port, "stratumUser": pf("User"),
        "activePool": url, "stratumConnected": alive,
        "isUsingFallbackStratum": u8::from(pf("Priority").as_u64().unwrap_or(0) > 0),
        "poolDifficulty": pf("Stratum Difficulty"), "blockHeight": pf("Current Block Height"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = format!(
            "{}/fixtures/avalonq-{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn raw() -> Raw {
        Raw {
            summary: fixture("summary"),
            estats: fixture("estats"),
            pools: fixture("pools"),
        }
    }

    #[test]
    fn tokens_keep_values_with_spaces_and_keys_with_spaces() {
        let t = mm_tokens("'STATS':{Ver[Q-1] PS[0 1208 2295 34 792 2296 840] Nonce Mask[25]}");
        assert_eq!(
            t,
            vec![
                ("Ver", "Q-1"),
                ("PS", "0 1208 2295 34 792 2296 840"),
                ("Nonce Mask", "25")
            ]
        );
        assert!(mm_tokens("no brackets").is_empty());
        assert_eq!(mm_tokens("A[1] broken[2"), vec![("A", "1")]);
    }

    #[test]
    fn the_operators_avalon_q_reads_as_documented() {
        let mm = parse_estats(&fixture("estats"));
        assert_eq!(mm.version, "Q-25052801_14a19a2");
        assert_eq!(mm.dna, "0201000054ea1a94");
        assert_eq!(mm.temp_max_c, Some(71.0));
        assert_eq!(mm.temp_avg_c, Some(65.0));
        assert_eq!(mm.temp_target_c, Some(65.0));
        assert_eq!(mm.fan_rpm, vec![1590, 1592, 1606, 1594]);
        assert_eq!(mm.fan_pct, Some(51.0));
        assert_eq!(mm.power_w, Some(840.0));
        assert_eq!(mm.ps_vin_v, Some(12.08));
        assert_eq!(mm.ghs_now, Some(54027.29));
        assert_eq!(mm.work_mode, Some(0));
        assert_eq!(mm.mode_power_cap_w, Some(800.0));
        assert_eq!(mm.error_percent, Some(3.501));
        assert_eq!(mm.chip_name, "A3197S");
        assert_eq!(mm.chip_count, Some(160));
        assert_eq!(mm.chip_temps.len(), 160);
        assert_eq!(mm.chip_temps[0], 55.0);
        assert_eq!(mm.chip_temps[159], 57.0);
    }

    #[test]
    fn translation_carries_axeos_names_and_nulls_for_what_an_avalon_lacks() {
        let v = translate(&raw());
        assert_eq!(v["deviceModel"], "Avalon Q");
        assert_eq!(v["hostname"], "Avalon Q");
        assert_eq!(v["version"], "MM 25052801");
        assert_eq!(v["ASICModel"], "A3197S");
        assert_eq!(v["asicCount"], 160);
        assert_eq!(v["hashRate"].as_f64().unwrap().round(), 52976.0);
        assert_eq!(v["hashRate_now"], 54027.29);
        assert_eq!(v["power"], 840.0);
        assert_eq!(v["temp"], 65.0);
        assert_eq!(v["tempMax"], 71.0);
        assert_eq!(v["workMode"], "Eco");
        assert_eq!(v["sharesAccepted"], 225944);
        assert_eq!(v["sharesRejected"], 130);
        assert_eq!(v["bestDiff"], 54_552_698_063u64);
        assert_eq!(v["uptimeSeconds"], 3_176_847);
        assert_eq!(v["stratumURL"], "192.168.178.3");
        assert_eq!(v["stratumPort"], 3333);
        assert_eq!(v["stratumConnected"], true);
        assert_eq!(v["isUsingFallbackStratum"], 0);
        assert_eq!(v["blockHeight"], 971_959);
        assert_eq!(v["poolDifficulty"].as_f64().unwrap().round(), 239_856.0);
        assert_eq!(v["errorPercentage"], 3.501);
        assert_eq!(v["fanrpm"], 1590);
        assert_eq!(v["fanspeed"], 51.0);
        assert_eq!(v["chainPhase"], "READY");
        assert!(v["vrTemp"].is_null());
        assert!(v["coreVoltage"].is_null());
        assert!(v["overheat_temp"].is_null());
        assert_eq!(v["hasVr"], false);
        assert_eq!(
            v["cgminer"]["commands"],
            json!(["summary", "estats", "pools"])
        );
        assert_eq!(
            v["stratumUser"],
            "bc1qexampleaddressxxxxxxxxxxxxxxxxxxxxxxxx.AvalonQ"
        );
    }

    #[test]
    fn empty_or_foreign_replies_translate_to_nulls_not_zeros() {
        let v = translate(&Raw {
            summary: json!({}),
            estats: json!({"STATUS": []}),
            pools: json!({"POOLS": []}),
        });
        assert!(v["hashRate"].is_null());
        assert!(v["power"].is_null());
        assert!(v["temp"].is_null());
        assert!(v["asicCount"].is_null());
        assert_eq!(v["deviceModel"], "Avalon");
        assert_eq!(v["chainPhase"], "POWER");
    }

    #[test]
    fn reader_stops_at_nul_bounds_the_length_and_rejects_fragments() {
        let mut ok: &[u8] = b"{\"a\":1}\0garbage after the nul";
        assert_eq!(read_reply(&mut ok, 64).unwrap(), b"{\"a\":1}");
        let mut eof: &[u8] = b"{\"a\":1}\n";
        assert_eq!(
            parse_reply(&read_reply(&mut eof, 64).unwrap()).unwrap(),
            json!({"a": 1})
        );
        let long = vec![b'x'; 2000];
        assert_eq!(read_reply(&mut long.as_slice(), 1024), Err(Error::TooLong));
        let text = std::fs::read_to_string(format!(
            "{}/fixtures/avalonq-estats.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let cut = &text.as_bytes()[..text.len() / 2];
        assert!(matches!(parse_reply(cut), Err(Error::NotJson(_))));
        assert!(matches!(
            parse_reply(b"STATUS=S,When=1|"),
            Err(Error::NotJson(_))
        ));
        assert!(matches!(parse_reply(b"\xff\xfe"), Err(Error::NotJson(_))));
        assert!(parse_reply(b"").is_err());
    }

    #[test]
    fn only_the_three_read_commands_go_over_the_wire() {
        let dev = std::sync::Arc::new(std::sync::Mutex::new(warpaxe_sim::AvalonQ::settled(
            1, 23.0, 1,
        )));
        let addr = warpaxe_cgminer::spawn("127.0.0.1:0", std::sync::Arc::clone(&dev)).unwrap();
        let mut s = TcpStream::connect(addr).unwrap();
        assert_eq!(
            query(&mut s, "ascset", Duration::from_secs(1)),
            Err(Error::Shape("a read command"))
        );
        let raw = fetch("127.0.0.1", addr.port(), Duration::from_secs(2)).unwrap();
        let v = translate(&raw);
        assert_eq!(v["workMode"], "Standard");
        assert_eq!(v["deviceModel"], "Avalon Q");
        assert!(v["hashRate"].as_f64().unwrap() > 0.0);
        assert!(v["power"].as_f64().unwrap() > 0.0);
        assert_eq!(
            dev.lock().unwrap().mode().index,
            1,
            "the device was not written to"
        );
        assert!(matches!(
            fetch("127.0.0.1", 1, Duration::from_millis(500)),
            Err(Error::Connect(_))
        ));
    }
}
