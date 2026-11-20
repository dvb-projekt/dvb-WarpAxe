//! Reading an Avalon over the cgminer API, and nothing else.
//!
//! An Avalon's MM speaks the cgminer socket protocol: TCP 4028, one JSON
//! request (`{"command":"summary"}`), one JSON reply terminated by a NUL
//! byte, then the server closes the connection. A browser cannot open
//! such a socket, so WarpAxe does it for its swarm page and hands the
//! answer on as an AxeOS-like `/api/system/info` document, which is what
//! the page's cards already read.
//!
//! What this crate sends is exactly [`COMMANDS`]: `summary`, `estats`,
//! `pools` and, to tell a cgminer from anything else on port 4028,
//! `version`, all reads. There is no code path for `ascset`, `setpool`
//! or any other write, and none should ever be added here: the firmware
//! holds an Avalon's address, not the right to change it.
//!
//! The [`scan`] walks the miner's own /24 for port 4028 with a few
//! connections at a time, a short timeout per host and a hard budget for
//! the whole run, and asks a host that accepts the connection for its
//! `version` only.
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
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};

/// The cgminer API port.
pub const PORT: u16 = 4028;
/// The only commands this crate sends: the three a card is built from
/// (asked in this order) and `version`, which the network search sends
/// once to a host that answers on port 4028.
pub const COMMANDS: [&str; 4] = ["summary", "estats", "pools", "version"];
/// Longest reply taken; the Avalon Q's `estats` is 3.6 KB, `pools` with
/// two pools 1.8 KB. Beyond this the reply is refused, not truncated.
pub const MAX_REPLY: usize = 16 * 1024;
/// Deepest nesting a reply may have. cgminer's documents are three or
/// four levels deep; serde_json would take 128, and each level costs
/// the parsing thread hundreds of bytes of stack (127 levels of `[`
/// measured at 45 KB, of `{"a":` at 74 KB), far more than the firmware's
/// reader thread has. A device behind a swarm address that answers with
/// something deeper is not a cgminer.
pub const MAX_DEPTH: usize = 8;
/// A whole reply must arrive within this: a device that trickles a byte
/// every second would otherwise hold the reader for as long as it likes.
pub const READ_BUDGET: Duration = Duration::from_secs(3);

/// Why a read failed; the text goes to the swarm card as "not reachable"
/// with the reason in the API reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No connection (refused, unreachable, timed out, no such host).
    Connect(String),
    /// The socket failed or timed out while the reply was being read.
    Read(String),
    /// No socket, buffer or memory free on this side (EMFILE, ENFILE,
    /// ENOBUFS, ENOMEM): worth a retry, not a verdict on the host.
    Busy(String),
    /// The caller's own memory check said no: this host was not tried.
    NoMemory,
    /// More than [`MAX_REPLY`] bytes without the terminating NUL.
    TooLong,
    /// Brackets nested deeper than [`MAX_DEPTH`].
    TooDeep,
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
            Error::Busy(e) => write!(f, "busy: {e}"),
            Error::NoMemory => write!(f, "internal memory short"),
            Error::TooLong => write!(f, "reply longer than {MAX_REPLY} bytes"),
            Error::TooDeep => write!(f, "reply nested deeper than {MAX_DEPTH} levels"),
            Error::NotJson(e) => write!(f, "not a cgminer reply: {e}"),
            Error::Shape(what) => write!(f, "reply without {what}"),
        }
    }
}

/// Read one reply: everything up to the NUL byte that ends it, or to
/// EOF (the server closes after the reply), at most `max` bytes and
/// within `budget` from the first call.
pub fn read_reply<R: Read>(r: &mut R, max: usize, budget: Duration) -> Result<Vec<u8>, Error> {
    // Small to begin with: on the firmware every allocation under 16 KB
    // comes from the internal heap, and six searchers read at once.
    let mut out = Vec::with_capacity(512);
    let mut buf = [0u8; 512];
    let t0 = std::time::Instant::now();
    loop {
        if t0.elapsed() > budget {
            return Err(Error::Read(format!(
                "reply not complete after {} ms",
                budget.as_millis()
            )));
        }
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

/// The deepest bracket nesting in `bytes`, strings skipped (a `[` inside
/// a quoted value does not count), stopping early once `limit` is passed.
pub fn nesting_depth(bytes: &[u8], limit: usize) -> usize {
    let (mut depth, mut deepest, mut in_str, mut esc) = (0usize, 0usize, false, false);
    for &b in bytes {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                deepest = deepest.max(depth);
                if deepest > limit {
                    return deepest;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// The reply bytes as JSON. cgminer ends the document with a NUL and
/// some builds add a newline; both are dropped. Nesting beyond
/// [`MAX_DEPTH`] is refused before the parser sees it.
pub fn parse_reply(bytes: &[u8]) -> Result<Value, Error> {
    parse_reply_depth(bytes, MAX_DEPTH)
}

/// [`parse_reply`] with its own nesting limit: the search's `version`
/// replies are three levels deep and parsed on small stacks.
pub fn parse_reply_depth(bytes: &[u8], max_depth: usize) -> Result<Value, Error> {
    let end = bytes
        .iter()
        .rposition(|&b| b != 0 && !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    let bytes = &bytes[..end];
    if nesting_depth(bytes, max_depth) > max_depth {
        return Err(Error::TooDeep);
    }
    let text = std::str::from_utf8(bytes).map_err(|e| Error::NotJson(e.to_string()))?;
    serde_json::from_str(text).map_err(|e| Error::NotJson(e.to_string()))
}

/// The prefix that marks a swarm member as a cgminer (Avalon) host.
pub const MEMBER_PREFIX: &str = "cgminer:";

/// A swarm member as typed or sent, normalised for the stored list:
/// `http://` and a trailing `/` dropped, the `cgminer:` prefix (any
/// case) lower-cased. An HTTP member is an IPv4 address or host name,
/// optionally with a port; a cgminer member is an IPv4 literal only, so
/// the socket the firmware opens for it cannot be steered by a name
/// that resolves elsewhere later.
pub fn normalize_member(raw: &str) -> Result<String, String> {
    let m = raw
        .trim()
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let (prefix, addr) = match m.get(..MEMBER_PREFIX.len()) {
        Some(p) if p.eq_ignore_ascii_case(MEMBER_PREFIX) => {
            (MEMBER_PREFIX, m[MEMBER_PREFIX.len()..].trim())
        }
        _ => ("", m),
    };
    if addr.is_empty() || addr.len() > 64 {
        return Err(format!("not an address: {m:?}"));
    }
    if prefix.is_empty() {
        let ok = addr
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
        if !ok {
            return Err(format!("not an address: {m:?}"));
        }
    } else if addr.parse::<std::net::Ipv4Addr>().is_err() {
        return Err(format!("cgminer members take an IPv4 address: {addr:?}"));
    }
    Ok(format!("{prefix}{addr}"))
}

/// The host of a member stored as `cgminer:<host>`; `None` for an HTTP member.
pub fn member_host(member: &str) -> Option<&str> {
    member.strip_prefix(MEMBER_PREFIX)
}

/// A cgminer found by the network search, from its `version` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its address.
    pub host: Ipv4Addr,
    /// `PROD` ("Avalon Q"), or the model, or "cgminer" when it names none.
    pub product: String,
    /// `MODEL` ("Q").
    pub model: String,
    /// The MM firmware (`LVERSION`), "25052801_14a19a2".
    pub firmware: String,
    /// `CGMiner` ("4.11.1") and `API` ("3.7") as "cgminer 4.11.1 · API 3.7".
    pub api: String,
}

/// What a `version` reply says about the device, if it is a cgminer's:
/// a `VERSION` array whose first object names `CGMiner` or `API`.
pub fn identify(host: Ipv4Addr, version: &Value) -> Option<Found> {
    let v = version.get("VERSION")?.as_array()?.first()?.as_object()?;
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let (cg, api) = (s("CGMiner"), s("API"));
    if cg.is_empty() && api.is_empty() {
        return None;
    }
    let model = s("MODEL");
    let product = match s("PROD") {
        p if !p.is_empty() => p,
        _ if !model.is_empty() => format!("Avalon {model}"),
        _ => "cgminer".into(),
    };
    Some(Found {
        host,
        product,
        model,
        firmware: s("LVERSION"),
        api: match (cg.is_empty(), api.is_empty()) {
            (false, false) => format!("cgminer {cg} · API {api}"),
            (false, true) => format!("cgminer {cg}"),
            _ => format!("API {api}"),
        },
    })
}

/// The hosts of `own`'s /24, without the network and broadcast addresses
/// and without `own` itself: what the search walks.
pub fn subnet_hosts(own: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, d] = own.octets();
    (1..=254u8)
        .filter(|&x| x != d)
        .map(|x| Ipv4Addr::new(a, b, c, x))
        .collect()
}

/// The search's bound on a `version` reply's length: the Avalon Q's is
/// 425 bytes; a worker's stack and the internal heap are sized for these.
pub const PROBE_MAX_REPLY: usize = 2 * 1024;
/// The search's bound on a `version` reply's nesting: real ones are
/// three levels deep.
pub const PROBE_MAX_DEPTH: usize = 4;

/// Whether an error is a shortage on this side ([`Error::Busy`]), worth
/// a retry rather than a verdict on the host.
pub fn is_busy(e: &Error) -> bool {
    matches!(e, Error::Busy(_))
}

/// The OS errors that mean a shortage on this side, not a host that is
/// not there: EMFILE 24, ENFILE 23, ENOMEM 12, ENOBUFS 105 (lwIP's
/// netconn and pcb pools answer with the last two).
fn busy_errno(code: Option<i32>) -> bool {
    matches!(code, Some(24) | Some(23) | Some(12) | Some(105))
}

/// What one host of the search turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// A cgminer, identified from its `version`.
    Cgminer(Found),
    /// Port 4028 accepted the connection and answered with something
    /// that is no cgminer document (or reset the connection).
    Other,
    /// Port 4028 accepted the connection but sent nothing within the
    /// reply time, twice.
    OpenNoAnswer,
}

/// One host of the search: connect within `connect`, ask `version`,
/// read the reply within `reply` (at most [`PROBE_MAX_REPLY`] bytes,
/// [`PROBE_MAX_DEPTH`] levels). `Err(Connect)` is nothing reachable
/// there, `Err(Busy)` a shortage on this side ([`is_busy`]). A port that
/// stays silent is asked a second time before it counts as
/// [`Probe::OpenNoAnswer`].
pub fn probe(
    host: Ipv4Addr,
    port: u16,
    connect: Duration,
    reply: Duration,
) -> Result<Probe, Error> {
    let mut silent = 0;
    loop {
        let addr = std::net::SocketAddr::from((host, port));
        let mut s = TcpStream::connect_timeout(&addr, connect).map_err(|e| {
            if busy_errno(e.raw_os_error()) {
                Error::Busy(e.to_string())
            } else {
                Error::Connect(e.to_string())
            }
        })?;
        let _ = s.set_nodelay(true);
        return match query_bounded(
            &mut s,
            "version",
            reply,
            reply,
            PROBE_MAX_REPLY,
            PROBE_MAX_DEPTH,
        ) {
            Ok(v) => Ok(identify(host, &v).map_or(Probe::Other, Probe::Cgminer)),
            Err(Error::Busy(e)) => Err(Error::Busy(e)),
            // Nothing came at all: once more, then "open, no answer".
            Err(Error::Read(w))
                if w.contains("not complete")
                    || w.contains("timed out")
                    || w.contains("WouldBlock")
                    || w.contains("os error 35")
                    || w.contains("os error 11")
                    || w.contains("os error 60") =>
            {
                silent += 1;
                if silent < 2 {
                    continue;
                }
                Ok(Probe::OpenNoAnswer)
            }
            // Connected, but no cgminer document: a web server that resets
            // the connection, a chatty service, a deep or long reply.
            Err(_) => Ok(Probe::Other),
        };
    }
}

/// How far a search has come; shared with whoever started it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// Hosts tried so far.
    pub done: usize,
    /// Hosts to try in all.
    pub total: usize,
    /// Hosts not tried because the budget ran out (the highest addresses:
    /// the walk goes upwards from .1, where static miners tend to sit).
    pub skipped_time: usize,
    /// Hosts not tried because no socket, buffer or memory was free on
    /// this side three times in a row.
    pub skipped_sockets: usize,
    /// Hosts not tried because the caller's memory check said no
    /// ([`Error::NoMemory`]) three times in a row.
    pub skipped_memory: usize,
    /// Hosts whose port 4028 accepted the connection but sent nothing.
    pub open_no_answer: usize,
    /// The cgminers found.
    pub found: Vec<Found>,
    /// The search has ended (all hosts, or the budget).
    pub finished: bool,
}

/// Starts a worker thread for the search; the firmware puts their
/// stacks in PSRAM, the host uses plain threads. `None` = no thread to
/// be had.
pub type Spawner = dyn Fn(Box<dyn FnOnce() + Send>) -> Option<std::thread::JoinHandle<()>> + Sync;

/// A plain-thread [`Spawner`] with `stack` bytes each.
pub fn thread_spawner(
    stack: usize,
) -> impl Fn(Box<dyn FnOnce() + Send>) -> Option<std::thread::JoinHandle<()>> + Sync {
    move |f| {
        std::thread::Builder::new()
            .name("cgscan".into())
            .stack_size(stack)
            .spawn(f)
            .ok()
    }
}

/// Walk `hosts` upwards with `parallel` workers (started through
/// `spawn`), each trying one host at a time through `probe_one`, until
/// all are done or `budget` has passed since the start (a worker in the
/// middle of a host finishes that host). A host whose probe says "no
/// socket free" is tried again after a pause, three times, then counted
/// in `skipped_sockets`; what the budget leaves untried is
/// `skipped_time`. `progress` is updated as it goes and `finished` is
/// set at the end. Locks: `queue` and `progress` are never held at the
/// same time.
pub fn scan_with<F>(
    mut hosts: Vec<Ipv4Addr>,
    parallel: usize,
    budget: Duration,
    progress: &std::sync::Arc<std::sync::Mutex<Progress>>,
    probe_one: F,
    spawn: &Spawner,
) where
    F: Fn(Ipv4Addr) -> Result<Probe, Error> + Send + Sync + 'static,
{
    use std::sync::{Arc, Mutex};
    let total = hosts.len();
    progress.lock().unwrap().total = total;
    // pop() takes from the end: reversed, the walk goes .1 upwards.
    hosts.reverse();
    let queue = Arc::new(Mutex::new(hosts));
    let probe_one = Arc::new(probe_one);
    let deadline = std::time::Instant::now() + budget;
    let worker = {
        let (queue, progress, probe_one) = (
            Arc::clone(&queue),
            Arc::clone(progress),
            Arc::clone(&probe_one),
        );
        move || loop {
            if std::time::Instant::now() >= deadline {
                return;
            }
            let next = queue.lock().unwrap().pop();
            let Some(host) = next else {
                return;
            };
            let mut outcome = None;
            let mut why = None;
            for attempt in 0..3 {
                match probe_one(host) {
                    // No socket (or memory) free right now: let some go and
                    // try again, three times.
                    Err(e @ Error::Busy(_)) | Err(e @ Error::NoMemory) => {
                        why = Some(e);
                        if attempt < 2 {
                            std::thread::sleep(Duration::from_millis(300));
                        }
                    }
                    other => {
                        outcome = Some(other);
                        break;
                    }
                }
            }
            let mut p = progress.lock().unwrap();
            match outcome {
                Some(Ok(Probe::Cgminer(f))) => {
                    p.done += 1;
                    p.found.push(f);
                }
                Some(Ok(Probe::OpenNoAnswer)) => {
                    p.done += 1;
                    p.open_no_answer += 1;
                }
                Some(_) => p.done += 1,
                None => match why {
                    Some(Error::NoMemory) => p.skipped_memory += 1,
                    _ => p.skipped_sockets += 1,
                },
            }
        }
    };
    let mut workers = Vec::new();
    for _ in 0..parallel.max(1) {
        let w = worker.clone();
        if let Some(h) = spawn(Box::new(w)) {
            workers.push(h);
        }
    }
    if workers.is_empty() {
        // No thread to be had: the caller's thread walks alone.
        worker();
    }
    for w in workers {
        let _ = w.join();
    }
    let left = queue.lock().unwrap().len();
    let mut p = progress.lock().unwrap();
    p.skipped_time += left;
    p.found.sort_by_key(|f| f.host);
    p.finished = true;
}

/// The search: `hosts` on `port`, `parallel` connections at a time,
/// `connect` to reach a host and `reply` for its answer, `budget` for
/// the whole run, workers from `spawn`.
#[allow(clippy::too_many_arguments)]
pub fn scan(
    hosts: Vec<Ipv4Addr>,
    port: u16,
    parallel: usize,
    connect: Duration,
    reply: Duration,
    budget: Duration,
    progress: &std::sync::Arc<std::sync::Mutex<Progress>>,
    spawn: &Spawner,
) {
    scan_with(
        hosts,
        parallel,
        budget,
        progress,
        move |h| probe(h, port, connect, reply),
        spawn,
    );
}

/// Send one of [`COMMANDS`] over an open stream and read the reply.
/// Other command names are refused here, so no caller can widen what
/// goes over the wire.
pub fn query(stream: &mut TcpStream, command: &str, timeout: Duration) -> Result<Value, Error> {
    query_bounded(stream, command, timeout, READ_BUDGET, MAX_REPLY, MAX_DEPTH)
}

/// [`query`] with the bounds spelt out: `timeout` per socket read,
/// `read_budget` for the whole reply, `max_reply` bytes, `max_depth`
/// levels. The command whitelist is the same.
pub fn query_bounded(
    stream: &mut TcpStream,
    command: &str,
    timeout: Duration,
    read_budget: Duration,
    max_reply: usize,
    max_depth: usize,
) -> Result<Value, Error> {
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
    let bytes = read_reply(stream, max_reply, read_budget)?;
    parse_reply_depth(&bytes, max_depth)
}

/// The three replies of one round.
#[derive(Debug, Clone)]
pub struct Raw {
    /// The `summary` reply.
    pub summary: Value,
    /// The `estats` reply.
    pub estats: Value,
    /// The `pools` reply.
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
    let ask = |command: &str| -> Result<Value, Error> {
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
            .rsplit([']', '{', ':'])
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
    /// `DNA[…]`: the board's unique id.
    pub dna: String,
    /// `STATE[1]` while the MM works.
    pub state: Option<i64>,
    /// Seconds since the MM started.
    pub elapsed_s: Option<u64>,
    /// `TMax`: the hottest chip, °C.
    pub temp_max_c: Option<f64>,
    /// `TAvg`: the chips' mean, °C (what the Avalon's own LCD shows).
    pub temp_avg_c: Option<f64>,
    /// `TarT`: the MM's own fan target, °C.
    pub temp_target_c: Option<f64>,
    /// `ITemp`: the inlet air, °C.
    pub temp_inlet_c: Option<f64>,
    /// `Fan1`–`Fan4`, rpm.
    pub fan_rpm: Vec<u32>,
    /// `FanR`: the fans' duty, %.
    pub fan_pct: Option<f64>,
    /// `PS[err aux rail_v rail_a rail_w rail_set wall_w]`, the power
    /// supply as the MM polls it over I²C (Canaan's `Avalon_mm`,
    /// `mm_miner/mmu/mmu.c`; the same order in the Nano 3S SDK). The
    /// seventh field is the draw at the wall, what the Avalon's own LCD
    /// shows (840 W at 54 TH/s in Eco).
    pub power_w: Option<f64>,
    /// `PS` field 0: the supply's error code, 0 = none.
    pub ps_error: Option<u32>,
    /// `PS` field 1: the supply's 12 V auxiliary output, V (10 mV units).
    pub ps_vin_v: Option<f64>,
    /// `PS` field 2: the hash boards' supply rail, V (10 mV units).
    pub rail_v: Option<f64>,
    /// `PS` field 3: the rail's current, A.
    pub rail_a: Option<f64>,
    /// `PS` field 4: the supply's DC output, W.
    pub rail_w: Option<f64>,
    /// `PS` field 5: the rail voltage the MM asks for, V (10 mV units).
    pub rail_set_v: Option<f64>,
    /// `OTemp`: the control board, °C (the Nano 3S).
    pub temp_outlet_c: Option<f64>,
    /// `HBITemp`: each hash board's inlet, °C.
    pub board_temps_in: Vec<Option<f64>>,
    /// `HBOTemp`: each hash board's outlet, °C.
    pub board_temps_out: Vec<Option<f64>>,
    /// `MTmax`: each hash board's hottest chip, °C.
    pub board_temps_max: Vec<Option<f64>>,
    /// `ECMM`: the MM's error bits (see [`mm_error_names`]).
    pub mm_errcode: Option<u32>,
    /// `ECHU`: each hash board's error bits (see [`board_error_names`]).
    pub board_errcodes: Vec<u32>,
    /// `SoftOFF`: why the MM stopped hashing, 0 = it did not (see
    /// [`soft_off_reason`]).
    pub soft_off: Option<u8>,
    /// `GHSspd`: the last seconds' rate, GH/s, noisy.
    pub ghs_now: Option<f64>,
    /// `GHSavg`: the mean since the MM started, GH/s.
    pub ghs_avg: Option<f64>,
    /// `Freq`: the chips' mean clock, MHz.
    pub freq_mhz: Option<f64>,
    /// `WORKMODE`: 0 Eco, 1 Standard, 2 Super.
    pub work_mode: Option<u8>,
    /// `WORKLEVEL` within the mode.
    pub work_level: Option<u8>,
    /// `MPO`: the mode's power cap, W.
    pub mode_power_cap_w: Option<f64>,
    /// `DH[3.501%]`: the device's hardware-error share of its work;
    /// `DHspd` where a firmware sends no `DH` (Canaan's `Avalon_mm`).
    pub error_percent: Option<f64>,
    /// `Core[A3197S]`: the chip.
    pub chip_name: String,
    /// `TA`: chips in all.
    pub chip_count: Option<u32>,
    /// `SYSTEMSTATU[Work: In Work, Hash Board: 1]`.
    pub system_status: String,
    /// `PVT_T0`, `PVT_T1`, …: every chip's temperature, °C, in board
    /// order; `None` where the chip gave no reading.
    pub chip_temps: Vec<Option<f64>>,
}

/// How the MM says "no reading" for a temperature (`ASIC_TEMP_NONE` in
/// Canaan's Nano 3S SDK, set for chips that did not answer and for the
/// inlet sensor a Nano 3S does not have).
pub const NO_READING: f64 = -273.0;

fn num(v: &str) -> Option<f64> {
    v.trim().trim_end_matches('%').parse().ok()
}

/// A temperature, `None` for the MM's "no reading".
fn temp(v: &str) -> Option<f64> {
    num(v).filter(|t| *t > NO_READING)
}

fn temps(v: &str) -> Vec<Option<f64>> {
    v.split_whitespace().map(temp).collect()
}

/// The names of the bits set in `ECMM` (`mm_miner/mmu/mmu_interior.h`
/// in Canaan's `Avalon_mm`); an unknown bit as `bit<N>`.
pub fn mm_error_names(code: u32) -> Vec<String> {
    bit_names(
        code,
        &[
            (1, "lan_damaged"),
            (2, "power"),
            (3, "fan"),
            (4, "ip_lost"),
            (5, "overheat"),
            (6, "pool_inactive"),
            (7, "core_link"),
            (8, "inlet_hot"),
            (11, "hash_board_mcu"),
        ],
    )
}

/// The names of the bits set in one board's `ECHU` value.
pub fn board_error_names(code: u32) -> Vec<String> {
    bit_names(code, &[(1, "enum"), (2, "comm"), (3, "mcu")])
}

fn bit_names(code: u32, names: &[(u32, &str)]) -> Vec<String> {
    (0..32)
        .filter(|b| code & (1 << b) != 0)
        .map(|b| {
            names
                .iter()
                .find(|(n, _)| *n == b)
                .map_or_else(|| format!("bit{b}"), |(_, name)| (*name).to_string())
        })
        .collect()
}

/// Why the MM stopped hashing (`softoff_rsn` in Canaan's `Avalon_mm`);
/// `None` for 0, the MM did not stop.
pub fn soft_off_reason(code: u8) -> Option<&'static str> {
    Some(match code {
        0 => return None,
        1 => "power_fault",
        2 => "hash_fault",
        3 => "chip_overheat",
        4 => "api",
        5 => "fan_error",
        6 => "lan_damaged",
        7 => "hash_board_mcu",
        8 => "hardware_error",
        9 => "overheat_at_start",
        _ => "unknown",
    })
}

/// A JSON value as the text a `Key[Value]` token would carry: arrays
/// as their items with spaces between.
fn as_token(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(u8::from(*b).to_string()),
        Value::Array(a) => Some(a.iter().filter_map(as_token).collect::<Vec<_>>().join(" ")),
        _ => None,
    }
}

/// The key/value pairs of the first MM module, from either form of
/// `estats`: the token strings of the Avalon Q (`MM ID0:Summary` and
/// `HBinfo` as one string each) or the objects a Nano 3S on Canaan's
/// newer SDK sends for a JSON request (`MMID0Summary` with `Sysinfo`
/// and `Hashinfo`, `HBinfo` with `HB_0_Info`, …).
fn estats_pairs(estats: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(obj) = estats
        .get("STATS")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_object)
    else {
        return out;
    };
    for (k, v) in obj {
        let other_module = (k.starts_with("MM ID") && !k.starts_with("MM ID0"))
            || (k.starts_with("MMID") && !k.starts_with("MMID0"));
        if other_module {
            continue;
        }
        match v {
            Value::String(text) => {
                for (key, value) in mm_tokens(text) {
                    out.push((key.to_string(), value.to_string()));
                }
            }
            Value::Object(o) if k.starts_with("MMID0") || k == "HBinfo" => {
                // One level of sections (`Sysinfo`, `HB_0_Info`), or the
                // values right here.
                for (sk, sv) in o {
                    match sv {
                        Value::Object(section) => {
                            for (key, value) in section {
                                if let Some(t) = as_token(value) {
                                    out.push((key.clone(), t));
                                }
                            }
                        }
                        other => {
                            if let Some(t) = as_token(other) {
                                out.push((sk.clone(), t));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// What the first MM module (`MM ID0` / `MMID0`) reports. An Avalon
/// with several modules would carry `MM ID1…` beside it; those are left
/// out rather than overwrite the first one's values.
pub fn parse_estats(estats: &Value) -> Mm {
    let mut mm = Mm::default();
    let mut dh_spd = None;
    for (key, value) in estats_pairs(estats) {
        let value = value.as_str();
        match key.as_str() {
            "Ver" => mm.version = value.to_string(),
            "DNA" => mm.dna = value.to_string(),
            "STATE" => mm.state = value.trim().parse().ok(),
            "Elapsed" => mm.elapsed_s = value.trim().parse().ok(),
            "TMax" => mm.temp_max_c = temp(value),
            "TAvg" => mm.temp_avg_c = temp(value),
            "TarT" => mm.temp_target_c = temp(value),
            "ITemp" => mm.temp_inlet_c = temp(value),
            "OTemp" => mm.temp_outlet_c = temp(value),
            "HBITemp" => mm.board_temps_in = temps(value),
            "HBOTemp" => mm.board_temps_out = temps(value),
            "MTmax" => mm.board_temps_max = temps(value),
            "FanR" => mm.fan_pct = num(value),
            "Fan1" | "Fan2" | "Fan3" | "Fan4" => {
                if let Some(rpm) = num(value) {
                    mm.fan_rpm.push(rpm as u32);
                }
            }
            "PS" => {
                let f: Vec<f64> = value.split_whitespace().filter_map(num).collect();
                if f.len() >= 7 {
                    mm.ps_error = Some(f[0] as u32);
                    mm.ps_vin_v = Some(f[1] / 100.0);
                    mm.rail_v = Some(f[2] / 100.0);
                    mm.rail_a = Some(f[3]);
                    mm.rail_w = Some(f[4]);
                    mm.rail_set_v = Some(f[5] / 100.0);
                    mm.power_w = Some(f[6]);
                }
            }
            "GHSspd" => mm.ghs_now = num(value),
            "GHSavg" => mm.ghs_avg = num(value),
            "Freq" => mm.freq_mhz = num(value),
            "WORKMODE" => mm.work_mode = value.trim().parse().ok(),
            "WORKLEVEL" => mm.work_level = value.trim().parse().ok(),
            "MPO" => mm.mode_power_cap_w = num(value),
            "DH" => mm.error_percent = num(value),
            "DHspd" => dh_spd = num(value),
            "Core" => mm.chip_name = value.trim().to_string(),
            "TA" => mm.chip_count = value.trim().parse().ok(),
            "SYSTEMSTATU" => mm.system_status = value.trim().to_string(),
            "ECMM" => mm.mm_errcode = value.trim().parse().ok(),
            "ECHU" => {
                mm.board_errcodes = value
                    .split_whitespace()
                    .filter_map(|c| c.parse().ok())
                    .collect();
            }
            "SoftOFF" => mm.soft_off = value.trim().parse().ok(),
            k if k.starts_with("PVT_T") && k[5..].chars().all(|c| c.is_ascii_digit()) => {
                mm.chip_temps.extend(temps(value));
            }
            _ => {}
        }
    }
    if mm.error_percent.is_none() {
        mm.error_percent = dh_spd;
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
    let mut v = json!({
        "cgminer": { "api": api, "commands": &COMMANDS[..3], "host": pf("URL") },
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
        "psError": mm.ps_error, "railVoltage": mm.rail_v, "railVoltageSet": mm.rail_set_v,
        "railCurrent": mm.rail_a, "railPower": mm.rail_w,
        "temp": mm.temp_avg_c, "tempMax": mm.temp_max_c, "tempTarget": mm.temp_target_c, "tempInlet": mm.temp_inlet_c,
        "tempBoard": mm.temp_outlet_c, "boardTempsIn": mm.board_temps_in,
        "boardTempsOut": mm.board_temps_out, "boardTempsMax": mm.board_temps_max,
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
    });
    // The MM's own error state, beside the document above (the `json!`
    // above is near the macro's recursion limit).
    let board_errors: Vec<Vec<String>> = mm
        .board_errcodes
        .iter()
        .map(|c| board_error_names(*c))
        .collect();
    let extra = json!({
        "mmErrorCode": mm.mm_errcode,
        "mmErrors": mm.mm_errcode.map(mm_error_names),
        "boardErrorCodes": mm.board_errcodes,
        "boardErrors": board_errors,
        "softOffCode": mm.soft_off,
        "softOffReason": mm.soft_off.and_then(soft_off_reason),
    });
    if let (Some(doc), Value::Object(more)) = (v.as_object_mut(), extra) {
        doc.extend(more);
    }
    v
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
        assert_eq!(mm.chip_temps[0], Some(55.0));
        assert_eq!(mm.chip_temps[159], Some(57.0));
        // PS[0 1208 2295 34 792 2296 840]: no supply error, 12.08 V aux,
        // a 22.95 V rail at 34 A, 792 W out, 22.96 V asked, 840 W in.
        assert_eq!(mm.ps_error, Some(0));
        assert_eq!(mm.rail_v, Some(22.95));
        assert_eq!(mm.rail_a, Some(34.0));
        assert_eq!(mm.rail_w, Some(792.0));
        assert_eq!(mm.rail_set_v, Some(22.96));
        assert_eq!(mm.temp_inlet_c, Some(33.0));
        assert_eq!(mm.board_temps_in, vec![Some(61.0)]);
        assert_eq!(mm.board_temps_out, vec![Some(63.0)]);
        assert_eq!(mm.mm_errcode, Some(256));
        assert_eq!(mm_error_names(256), vec!["inlet_hot"]);
        assert_eq!(mm.board_errcodes, vec![0]);
        assert_eq!(mm.soft_off, Some(0));
        assert_eq!(soft_off_reason(0), None);
    }

    #[test]
    fn no_reading_is_none_not_minus_273() {
        let v = json!({"STATS": [{"MM ID0:Summary": "ITemp[-273] OTemp[41] TMax[-273] TAvg[60] HBITemp[-273 50]",
            "HBinfo": "'HB0':{PVT_T0[ 55 -273  60]}"}]});
        let mm = parse_estats(&v);
        assert_eq!(mm.temp_inlet_c, None);
        assert_eq!(mm.temp_outlet_c, Some(41.0));
        assert_eq!(mm.temp_max_c, None);
        assert_eq!(mm.temp_avg_c, Some(60.0));
        assert_eq!(mm.board_temps_in, vec![None, Some(50.0)]);
        assert_eq!(mm.chip_temps, vec![Some(55.0), None, Some(60.0)]);
        let t = translate(&Raw {
            summary: json!({}),
            estats: v,
            pools: json!({}),
        });
        assert!(t["tempInlet"].is_null());
        assert_eq!(t["chipTemps"], json!([55.0, null, 60.0]));
        assert_eq!(t["asicCount"], 3);
    }

    #[test]
    fn the_json_form_of_a_nano_3s_reads_like_the_token_form() {
        // The shape Canaan's Nano 3S SDK (`mc_api_estats.c`) sends for a
        // JSON request: numbers and arrays instead of token strings.
        let v = json!({"STATS": [{"STATS": 0, "ID": "AVALON0", "Elapsed": 120,
            "MMID0Summary": {
                "Sysinfo": {"Ver": "Nano3s-25021401_56abae7", "DNA": "020100004d8b4c01",
                    "SYSTEMSTATU": "Work: In Work, Hash Board: 1", "Elapsed": 120,
                    "ITemp": -273, "OTemp": 45, "Fan1": 2100, "FanR": 38,
                    "PS": [0, 1200, 1350, 9, 120, 1350, 140], "SoftOFF": 0, "ECMM": 0},
                "Hashinfo": {"DH": 1.25, "TMax": 70, "TAvg": 64, "TarT": 65,
                    "GHSspd": 6012.5, "GHSavg": 5980.1, "Freq": 450.0,
                    "MTmax": [70], "TA": 10, "Core": "A3197S", "ECHU": [4],
                    "WORKMODE": 1, "WORKLEVEL": 0, "MPO": 140}},
            "HBinfo": {"HB_0_Info": {"PVT_T0": [60, 62, -273, 61], "PVT_V0": [300, 301, 0, 299]}},
            "MMID1Summary": {"Hashinfo": {"TAvg": 99}}}]});
        let mm = parse_estats(&v);
        assert_eq!(mm.version, "Nano3s-25021401_56abae7");
        assert_eq!(mm.temp_inlet_c, None);
        assert_eq!(mm.temp_outlet_c, Some(45.0));
        assert_eq!(mm.temp_avg_c, Some(64.0));
        assert_eq!(mm.fan_rpm, vec![2100]);
        assert_eq!(mm.power_w, Some(140.0));
        assert_eq!(mm.rail_v, Some(13.5));
        assert_eq!(mm.ghs_now, Some(6012.5));
        assert_eq!(mm.error_percent, Some(1.25));
        assert_eq!(mm.chip_name, "A3197S");
        assert_eq!(mm.chip_count, Some(10));
        assert_eq!(mm.work_mode, Some(1));
        assert_eq!(mm.board_temps_max, vec![Some(70.0)]);
        assert_eq!(mm.board_errcodes, vec![4]);
        assert_eq!(board_error_names(4), vec!["comm"]);
        assert_eq!(
            mm.chip_temps,
            vec![Some(60.0), Some(62.0), None, Some(61.0)]
        );
        let t = translate(&Raw {
            summary: json!({}),
            estats: v,
            pools: json!({}),
        });
        assert_eq!(t["deviceModel"], "Avalon Nano3s");
        assert_eq!(t["boardErrors"], json!([["comm"]]));
        assert_eq!(t["mmErrors"], json!([]));
        assert!(t["softOffReason"].is_null());
    }

    #[test]
    fn dhspd_stands_in_for_a_missing_dh_and_error_bits_are_named() {
        let mm = parse_estats(
            &json!({"STATS": [{"MM ID0:Summary": "DHspd[2.500%] SoftOFF[3] ECMM[72]"}]}),
        );
        assert_eq!(mm.error_percent, Some(2.5));
        let both =
            parse_estats(&json!({"STATS": [{"MM ID0:Summary": "DH[1.000%] DHspd[2.500%]"}]}));
        assert_eq!(both.error_percent, Some(1.0));
        assert_eq!(soft_off_reason(3), Some("chip_overheat"));
        assert_eq!(soft_off_reason(42), Some("unknown"));
        // 72 = bits 3 and 6.
        assert_eq!(mm_error_names(72), vec!["fan", "pool_inactive"]);
        assert_eq!(mm_error_names(1 << 20), vec!["bit20"]);
        assert_eq!(mm_error_names(0), Vec::<String>::new());
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

    /// A device that hands out one byte per read, slowly.
    struct Trickle(u64);
    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(self.0));
            buf[0] = b'[';
            Ok(1)
        }
    }

    #[test]
    fn a_trickling_device_is_dropped_after_the_budget() {
        let t0 = std::time::Instant::now();
        let r = read_reply(&mut Trickle(20), 1024, Duration::from_millis(200));
        assert!(
            matches!(r, Err(Error::Read(ref w)) if w.contains("200 ms")),
            "{r:?}"
        );
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn nesting_beyond_the_limit_is_refused_before_parsing() {
        let deep_arrays = "[".repeat(200);
        assert_eq!(parse_reply(deep_arrays.as_bytes()), Err(Error::TooDeep));
        let deep_objects = "{\"a\":".repeat(127);
        assert_eq!(parse_reply(deep_objects.as_bytes()), Err(Error::TooDeep));
        // Brackets inside strings do not count; the real replies are 3–4 deep.
        assert_eq!(nesting_depth(br#"{"s":"[[[[[[[[[[[[[[[[[[["}"#, 8), 1);
        assert_eq!(nesting_depth(b"{\"s\":\"a\\\"[[[[[[[[[[[\\\"[[\"}", 8), 1);
        let estats = std::fs::read(format!(
            "{}/fixtures/avalonq-estats.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        assert_eq!(nesting_depth(&estats, 8), 3);
        assert!(parse_reply(&estats).is_ok());
    }

    #[test]
    fn members_are_normalised_and_cgminer_ones_are_ipv4_only() {
        assert_eq!(
            normalize_member(" http://192.168.178.42/ ").unwrap(),
            "192.168.178.42"
        );
        assert_eq!(
            normalize_member("miner.local:8080").unwrap(),
            "miner.local:8080"
        );
        assert_eq!(
            normalize_member("CGMiner:192.168.178.43").unwrap(),
            "cgminer:192.168.178.43"
        );
        assert_eq!(
            normalize_member("cgminer: 192.168.178.43").unwrap(),
            "cgminer:192.168.178.43"
        );
        assert!(normalize_member("cgminer:avalon.local").is_err());
        assert!(normalize_member("cgminer:192.168.178.43:4028").is_err());
        assert!(normalize_member("cgminer:").is_err());
        assert!(normalize_member("").is_err());
        assert!(normalize_member("bad host").is_err());
        assert!(normalize_member(&"a".repeat(65)).is_err());
        assert_eq!(member_host("cgminer:1.2.3.4"), Some("1.2.3.4"));
        assert_eq!(member_host("1.2.3.4"), None);
    }

    #[test]
    fn only_the_first_mm_module_is_read() {
        let v = json!({"STATS": [{"MM ID0:Summary": "TAvg[65] WORKMODE[0]", "MM ID1:Summary": "TAvg[90] WORKMODE[2]"}, {"MM ID0:Summary": "TAvg[99]"}]});
        let mm = parse_estats(&v);
        assert_eq!(mm.temp_avg_c, Some(65.0));
        assert_eq!(mm.work_mode, Some(0));
    }

    #[test]
    fn reader_stops_at_nul_bounds_the_length_and_rejects_fragments() {
        let mut ok: &[u8] = b"{\"a\":1}\0garbage after the nul";
        assert_eq!(
            read_reply(&mut ok, 64, Duration::from_secs(1)).unwrap(),
            b"{\"a\":1}"
        );
        let mut eof: &[u8] = b"{\"a\":1}\n";
        assert_eq!(
            parse_reply(&read_reply(&mut eof, 64, Duration::from_secs(1)).unwrap()).unwrap(),
            json!({"a": 1})
        );
        let long = vec![b'x'; 2000];
        assert_eq!(
            read_reply(&mut long.as_slice(), 1024, Duration::from_secs(1)),
            Err(Error::TooLong)
        );
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
    fn the_version_reply_of_the_avalon_q_identifies_it() {
        let f = identify("192.168.178.43".parse().unwrap(), &fixture("version")).unwrap();
        assert_eq!(f.product, "Avalon Q");
        assert_eq!(f.model, "Q");
        assert_eq!(f.firmware, "25052801_14a19a2");
        assert_eq!(f.api, "cgminer 4.11.1 · API 3.7");
        let h: Ipv4Addr = "10.0.0.1".parse().unwrap();
        assert!(identify(h, &json!({"VERSION": [{"MODEL": "Q"}]})).is_none());
        assert_eq!(
            identify(h, &json!({"VERSION": [{"API": "3.7", "MODEL": "A15"}]}))
                .unwrap()
                .product,
            "Avalon A15"
        );
        assert!(identify(h, &json!({"STATUS": "S"})).is_none());
        assert!(identify(h, &json!({"hashRate": 1})).is_none());
    }

    #[test]
    fn the_search_walks_the_own_slash_24_without_itself() {
        let hosts = subnet_hosts("192.168.178.41".parse().unwrap());
        assert_eq!(hosts.len(), 253);
        assert_eq!(hosts[0], "192.168.178.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(hosts[252], "192.168.178.254".parse::<Ipv4Addr>().unwrap());
        assert!(!hosts.contains(&"192.168.178.41".parse().unwrap()));
        assert!(!hosts.contains(&"192.168.178.0".parse().unwrap()));
        assert!(!hosts.contains(&"192.168.178.255".parse().unwrap()));
    }

    #[test]
    fn the_budget_ends_a_search_and_the_rest_counts_as_skipped_for_time() {
        use std::sync::{Arc, Mutex};
        let hosts = subnet_hosts("10.9.8.7".parse().unwrap());
        let progress = Arc::new(Mutex::new(Progress::default()));
        let order = Arc::new(Mutex::new(Vec::new()));
        let o2 = Arc::clone(&order);
        let t0 = std::time::Instant::now();
        // 100 ms per host, four at a time, 350 ms in all: about 12–16 hosts.
        scan_with(
            hosts,
            4,
            Duration::from_millis(350),
            &progress,
            move |h| {
                o2.lock().unwrap().push(h);
                std::thread::sleep(Duration::from_millis(100));
                Err(Error::Connect("refused".into()))
            },
            &thread_spawner(64 * 1024),
        );
        let p = progress.lock().unwrap().clone();
        assert!(
            t0.elapsed() < Duration::from_millis(900),
            "{:?}",
            t0.elapsed()
        );
        assert!(p.finished);
        assert_eq!(p.total, 253);
        assert!(p.done >= 8 && p.done <= 24, "{p:?}");
        assert_eq!(p.done + p.skipped_time, 253);
        assert_eq!(p.skipped_sockets + p.skipped_memory, 0);
        assert!(p.found.is_empty());
        // The walk went upwards: the first four hosts are .1–.4.
        let first: Vec<u8> = order
            .lock()
            .unwrap()
            .iter()
            .take(4)
            .map(|h| h.octets()[3])
            .collect();
        let mut sorted = first.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, [1, 2, 3, 4], "{first:?}");
    }

    #[test]
    fn a_full_slash_24_of_dead_hosts_fits_the_budget() {
        use std::sync::{Arc, Mutex};
        // The firmware's shape scaled by ten: 253 hosts that each cost the
        // full connect timeout (40 ms for 400), six workers, 3 s for 30.
        let hosts = subnet_hosts("10.9.8.7".parse().unwrap());
        let progress = Arc::new(Mutex::new(Progress::default()));
        scan_with(
            hosts,
            6,
            Duration::from_secs(3),
            &progress,
            |_h| {
                std::thread::sleep(Duration::from_millis(40));
                Err(Error::Connect("timed out".into()))
            },
            &thread_spawner(64 * 1024),
        );
        let p = progress.lock().unwrap().clone();
        assert_eq!(p.done, 253, "{p:?}");
        assert_eq!(p.skipped_time + p.skipped_sockets + p.skipped_memory, 0);
    }

    #[test]
    fn a_host_without_a_free_socket_three_times_is_skipped_for_sockets() {
        use std::sync::{Arc, Mutex};
        let hosts: Vec<Ipv4Addr> = vec!["10.1.1.1".parse().unwrap(), "10.1.1.2".parse().unwrap()];
        let progress = Arc::new(Mutex::new(Progress::default()));
        let tries = Arc::new(Mutex::new(0));
        let t2 = Arc::clone(&tries);
        scan_with(
            hosts,
            1,
            Duration::from_secs(5),
            &progress,
            move |h| {
                if h.octets()[3] == 1 {
                    *t2.lock().unwrap() += 1;
                    Err(Error::Busy("Too many open files (os error 24)".into()))
                } else {
                    Err(Error::Connect("refused".into()))
                }
            },
            &thread_spawner(64 * 1024),
        );
        let p = progress.lock().unwrap().clone();
        assert_eq!(*tries.lock().unwrap(), 3);
        assert_eq!(p.skipped_sockets, 1);
        assert_eq!(p.done, 1);
        assert_eq!(p.skipped_time, 0);
        assert!(p.finished);
    }

    #[test]
    fn a_port_that_answers_with_something_else_is_not_a_cgminer() {
        use std::io::Write;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().take(3).flatten() {
                let mut s = s;
                let _ = s.write_all(b"HTTP/1.0 400 Bad Request\\r\\n\\r\\nnot json");
            }
        });
        let h: Ipv4Addr = "127.0.0.1".parse().unwrap();
        let r = probe(
            h,
            port,
            Duration::from_millis(500),
            Duration::from_millis(500),
        );
        assert_eq!(r, Ok(Probe::Other));
        // A deep JSON reply on that port is not one either, and is refused
        // before the parser runs.
        let l2 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port2 = l2.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l2.incoming().take(1).flatten() {
                let mut s = s;
                let _ = s.write_all("[".repeat(40).as_bytes());
                let _ = s.write_all(b"\\0");
            }
        });
        assert_eq!(
            probe(
                h,
                port2,
                Duration::from_millis(500),
                Duration::from_millis(500)
            ),
            Ok(Probe::Other)
        );
        // Nothing listening: not reachable, and not "busy".
        let r = probe(h, 1, Duration::from_millis(300), Duration::from_millis(300));
        assert!(matches!(r, Err(Error::Connect(_))), "{r:?}");
        assert!(!is_busy(&r.unwrap_err()));
        // A port that accepts and stays silent: open, no answer (asked twice).
        let l3 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port3 = l3.local_addr().unwrap().port();
        let accepted = std::sync::Arc::new(std::sync::Mutex::new(0));
        let acc2 = std::sync::Arc::clone(&accepted);
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            for s in l3.incoming().take(2).flatten() {
                *acc2.lock().unwrap() += 1;
                kept.push(s);
            }
            std::thread::sleep(Duration::from_secs(3));
            drop(kept);
        });
        let t0 = std::time::Instant::now();
        assert_eq!(
            probe(
                h,
                port3,
                Duration::from_millis(500),
                Duration::from_millis(200)
            ),
            Ok(Probe::OpenNoAnswer)
        );
        assert!(t0.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*accepted.lock().unwrap(), 2);
        // The caller's memory check: three refusals skip the host for memory.
        use std::sync::{Arc, Mutex};
        let progress = Arc::new(Mutex::new(Progress::default()));
        scan_with(
            vec![h],
            1,
            Duration::from_secs(5),
            &progress,
            |_h| Err(Error::NoMemory),
            &thread_spawner(64 * 1024),
        );
        let p = progress.lock().unwrap().clone();
        assert_eq!(p.skipped_memory, 1);
        assert_eq!(p.skipped_sockets, 0);
        assert!(is_busy(&Error::Busy(
            "Too many open files (os error 24)".into()
        )));
    }

    #[test]
    fn a_full_search_finds_the_simulated_avalon_and_retries_busy_sockets() {
        use std::sync::{Arc, Mutex};
        let dev = Arc::new(Mutex::new(warpaxe_sim::AvalonQ::settled(0, 23.0, 1)));
        let addr = warpaxe_cgminer::spawn("127.0.0.1:0", Arc::clone(&dev)).unwrap();
        let port = addr.port();
        let hosts: Vec<Ipv4Addr> = vec!["127.0.0.1".parse().unwrap(), "127.0.0.2".parse().unwrap()];
        let progress = Arc::new(Mutex::new(Progress::default()));
        let busy = Arc::new(Mutex::new(0));
        let b2 = Arc::clone(&busy);
        scan_with(
            hosts,
            2,
            Duration::from_secs(5),
            &progress,
            move |h| {
                // The first try on the real host says "no socket free".
                if h == "127.0.0.1".parse::<Ipv4Addr>().unwrap() {
                    let mut n = b2.lock().unwrap();
                    *n += 1;
                    if *n == 1 {
                        return Err(Error::Busy("Too many open files (os error 24)".into()));
                    }
                }
                probe(
                    h,
                    port,
                    Duration::from_millis(800),
                    Duration::from_millis(800),
                )
            },
            &thread_spawner(64 * 1024),
        );
        let p = progress.lock().unwrap().clone();
        assert!(p.finished);
        assert_eq!(p.found.len(), 1, "{p:?}");
        assert_eq!(p.found[0].product, "Avalon Q");
        assert_eq!(p.found[0].host, "127.0.0.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(p.skipped_time + p.skipped_sockets + p.skipped_memory, 0);
        assert_eq!(p.open_no_answer, 0);
        assert_eq!(dev.lock().unwrap().mode().index, 0, "nothing was written");
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
        for cmd in [
            "setpool",
            "restart",
            "quit",
            "devs",
            "stats",
            "ascset|0,workmode,set,2",
        ] {
            let mut s = TcpStream::connect(addr).unwrap();
            assert_eq!(
                query(&mut s, cmd, Duration::from_secs(1)),
                Err(Error::Shape("a read command")),
                "{cmd}"
            );
        }
        assert_eq!(COMMANDS, ["summary", "estats", "pools", "version"]);
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
