//! Device configuration, one JSON document in NVS. The AxeOS field names
//! are kept where AxeOS has them so `PATCH /api/system` maps 1:1.

use std::sync::Mutex;

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub hostname: String,
    /// Board profile name (`warpaxe_board::Profile::by_name`).
    #[serde(rename = "boardModel")]
    pub board_model: String,
    pub ssid: String,
    #[serde(rename = "wifiPass")]
    pub wifi_pass: String,
    #[serde(rename = "stratumURL")]
    pub stratum_url: String,
    #[serde(rename = "stratumPort")]
    pub stratum_port: u16,
    #[serde(rename = "stratumUser")]
    pub stratum_user: String,
    #[serde(rename = "stratumPassword")]
    pub stratum_pass: String,
    /// Stratum over TLS (AxeOS name): 0 = plain, 1 = TLS, the pool's
    /// certificate checked against the built-in CA bundle. (AxeOS's 2 =
    /// own certificate is not offered.)
    #[serde(rename = "stratumTLS")]
    pub stratum_tls: u8,
    /// Switch to the fallback pool after repeated failures on the main one
    /// (AxeOS/NerdOS name).
    #[serde(rename = "useFallbackStratum")]
    pub use_fallback: bool,
    #[serde(rename = "fallbackStratumURL")]
    pub fallback_url: String,
    #[serde(rename = "fallbackStratumPort")]
    pub fallback_port: u16,
    #[serde(rename = "fallbackStratumUser")]
    pub fallback_user: String,
    #[serde(rename = "fallbackStratumPassword")]
    pub fallback_pass: String,
    #[serde(rename = "fallbackStratumTLS")]
    pub fallback_tls: u8,
    /// Stratum protocol of the main pool (AxeOS name): "SV1" or "SV2".
    #[serde(rename = "stratumProtocol")]
    pub stratum_protocol: String,
    /// The Sv2 pool's authority public key, Base58Check as pools publish
    /// it; empty = the pool is not verified (only without RequireAuth).
    #[serde(rename = "stratumV2AuthorityPubkey")]
    pub stratum_v2_authority_pubkey: String,
    /// Channel kind (AxeOS name). WarpAxe opens extended channels only;
    /// the value is kept for tools that read it.
    #[serde(rename = "stratumV2ChannelType")]
    pub stratum_v2_channel_type: String,
    /// Connect to the Sv2 pool only when its certificate verifies
    /// against the authority key.
    #[serde(rename = "stratumV2RequireAuth")]
    pub stratum_v2_require_auth: bool,
    #[serde(rename = "fallbackStratumProtocol")]
    pub fallback_protocol: String,
    #[serde(rename = "fallbackStratumV2AuthorityPubkey")]
    pub fallback_v2_authority_pubkey: String,
    #[serde(rename = "fallbackStratumV2ChannelType")]
    pub fallback_v2_channel_type: String,
    #[serde(rename = "fallbackStratumV2RequireAuth")]
    pub fallback_v2_require_auth: bool,
    /// Set the clock by NTP (AxeOS name); the history and log show real
    /// times once it is set.
    #[serde(rename = "useNTP")]
    pub use_ntp: bool,
    #[serde(rename = "ntpServer")]
    pub ntp_server: String,
    /// POSIX TZ rule for the clock on the display (default: Central
    /// European time with summer time).
    pub timezone: String,
    /// Fetch the BTC price and fee rates from mempool.space for the
    /// display's Bitcoin page. Off by default: the miner asks no one but
    /// its pool unless this is on.
    #[serde(rename = "btcPrice")]
    pub btc_price: bool,
    /// Currency of that price (a key of mempool.space `/api/v1/prices`).
    #[serde(rename = "priceCurrency")]
    pub price_currency: String,
    /// Look for new firmware on the project's Forgejo (at boot and every
    /// 12 hours); installing is always a click in the web UI.
    #[serde(rename = "updateCheck")]
    pub update_check: bool,
    /// Target ASIC clock, MHz (AxeOS name).
    pub frequency: u32,
    /// Core voltage, mV (AxeOS name).
    #[serde(rename = "coreVoltage")]
    pub core_voltage: u16,
    pub fanspeed: u8,
    pub autofanspeed: bool,
    /// Minutes until the display goes dark (AxeOS name); 0 = never. A
    /// button press or a write on the API brings it back.
    #[serde(rename = "displayTimeout")]
    pub display_timeout: u32,
    /// OLED mounted the other way round (AxeOS/NerdOS name).
    pub flipscreen: bool,
    /// Language of the device display: "en" or "de".
    #[serde(rename = "displayLanguage")]
    pub display_language: String,
    /// OLED inverse video (AxeOS name).
    pub invertscreen: bool,
    /// Fan PWM inverted, for fans that read duty backwards (NerdOS name).
    pub invertfanpolarity: bool,
    /// Lowest duty the fan PID may choose, % (AxeOS name).
    #[serde(rename = "minFanSpeed")]
    pub min_fan_speed: u8,
    /// Chip temperature the fan PID holds, °C; 0 = the board's default
    /// (NerdOS name).
    #[serde(rename = "pidTargetTemp")]
    pub pid_target_temp: u8,
    /// Chip temperature that switches the core voltage off, °C; 0 = the
    /// board's default (NerdOS name).
    pub overheat_temp: u8,
    /// Fan PID gains (NerdOS names); the defaults are ESP-Miner's. f64:
    /// deserializing an f32 trips the Xtensa LLVM backend.
    #[serde(rename = "pidP")]
    pub pid_p: f64,
    #[serde(rename = "pidI")]
    pub pid_i: f64,
    #[serde(rename = "pidD")]
    pub pid_d: f64,
    /// Second fan output (regulator fan on the Nerd boards): automatic on
    /// the regulator temperature, or a fixed duty.
    #[serde(rename = "autofanspeed2")]
    pub autofanspeed2: bool,
    #[serde(rename = "manualFanSpeed2")]
    pub manual_fan_speed2: u8,
    /// Regulator temperature the second fan holds, °C; 0 = the default.
    #[serde(rename = "vrTargetTemp")]
    pub vr_target_temp: u8,
    /// Share difficulty asked of the pool after authorizing
    /// (`mining.suggest_difficulty`, AxeOS name); 0 = ask nothing.
    #[serde(rename = "stratumSuggestedDifficulty")]
    pub suggested_difficulty: u32,
    /// Fresh work for the chips at least every this many ms (NerdOS
    /// name); 0 = when their nonce space runs out. Never later than that.
    #[serde(rename = "jobInterval")]
    pub job_interval_ms: u16,
    /// Chips on the chain; 0 = discover.
    #[serde(rename = "asicCount")]
    pub asic_count: u8,
    /// Ticket-mask difficulty for the chips; 0 = follow the pool.
    /// A diagnostic knob: 1 makes the chips report every nonce.
    #[serde(rename = "asicDifficulty")]
    pub asic_difficulty: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: "warpaxe".into(),
            // Empty: detected on the first start (board::init_bus).
            board_model: String::new(),
            ssid: String::new(),
            wifi_pass: String::new(),
            stratum_url: "pool.warppool.org".into(),
            stratum_port: 3333,
            stratum_user: String::new(),
            stratum_pass: "x".into(),
            stratum_tls: 0,
            use_fallback: false,
            fallback_url: String::new(),
            fallback_port: 3333,
            fallback_user: String::new(),
            fallback_pass: "x".into(),
            fallback_tls: 0,
            stratum_protocol: "SV1".into(),
            stratum_v2_authority_pubkey: String::new(),
            stratum_v2_channel_type: "extended".into(),
            stratum_v2_require_auth: true,
            fallback_protocol: "SV1".into(),
            fallback_v2_authority_pubkey: String::new(),
            fallback_v2_channel_type: "extended".into(),
            fallback_v2_require_auth: true,
            use_ntp: true,
            ntp_server: "pool.ntp.org".into(),
            timezone: "CET-1CEST,M3.5.0,M10.5.0/3".into(),
            btc_price: false,
            price_currency: "EUR".into(),
            update_check: true,
            frequency: 525,
            core_voltage: 1150,
            fanspeed: 100,
            autofanspeed: true,
            display_timeout: 30,
            flipscreen: false,
            display_language: "en".into(),
            invertscreen: false,
            invertfanpolarity: false,
            min_fan_speed: 25,
            pid_target_temp: 0,
            overheat_temp: 0,
            // warpaxe_board::fan::{KP, KI, KD}, written out: 0.1f32 as
            // f64 would show as 0.10000000149.
            pid_p: 5.0,
            pid_i: 0.1,
            pid_d: 2.0,
            autofanspeed2: true,
            manual_fan_speed2: 100,
            vr_target_temp: 0,
            suggested_difficulty: 0,
            job_interval_ms: 0,
            asic_count: 0,
            asic_difficulty: 0,
        }
    }
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);
static NVS: Mutex<Option<EspNvs<NvsDefault>>> = Mutex::new(None);
static PART: Mutex<Option<EspDefaultNvsPartition>> = Mutex::new(None);
const KEY: &str = "config";

/// Load from NVS (or defaults) and remember the NVS handle for saves.
pub fn init(part: EspDefaultNvsPartition) -> anyhow::Result<()> {
    *PART.lock().unwrap() = Some(part.clone());
    let nvs = EspNvs::new(part, "warpaxe", true)?;
    let mut buf = vec![0u8; 4096];
    let cfg = match nvs.get_raw(KEY, &mut buf) {
        Ok(Some(bytes)) => match serde_json::from_slice::<Config>(bytes) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("config: stored JSON unreadable ({e}), using defaults");
                Config::default()
            }
        },
        _ => Config::default(),
    };
    log::info!(
        "config: hostname={} wifi={:?} pool={}:{}",
        cfg.hostname,
        (!cfg.ssid.is_empty()).then_some(&cfg.ssid),
        cfg.stratum_url,
        cfg.stratum_port
    );
    *CONFIG.lock().unwrap() = Some(cfg);
    *BEST_DIFF.lock().unwrap() = nvs.get_u64(BEST_KEY).ok().flatten().unwrap_or(0);
    // Read once here, on the main task: a thread whose stack is in PSRAM must
    // not touch flash (the cache is off while it reads), and the update
    // watcher is such a thread (0.1.5 rebooted every minute once the record
    // existed).
    let mut buf = [0u8; 64];
    *LAST_INSTALL.lock().unwrap() = nvs
        .get_str(LAST_INSTALL_KEY, &mut buf)
        .ok()
        .flatten()
        .map(|s| s.trim_end_matches('\0').to_string())
        .unwrap_or_default();
    *NVS.lock().unwrap() = Some(nvs);
    Ok(())
}

/// All-time best nonce difficulty, kept across reboots (AxeOS `bestDiff`).
/// A mutex, not an atomic: Xtensa has no 64-bit atomics.
static BEST_DIFF: Mutex<u64> = Mutex::new(0);
const BEST_KEY: &str = "bestdiff";

pub fn best_diff() -> u64 {
    *BEST_DIFF.lock().unwrap()
}

/// Forget the all-time best difficulty.
pub fn reset_best_diff() {
    *BEST_DIFF.lock().unwrap() = 0;
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_u64(BEST_KEY, 0) {
            log::warn!("config: resetting best diff: {e}");
        }
    }
}

/// What the last online update installed and whether its signature
/// verified ("v0.1.3 verified" / "v0.1.3 missing"), written just before
/// the restart that ends an install and read back at the next boot.
const LAST_INSTALL_KEY: &str = "lastinstall";

/// The last install's record, empty when the running image was flashed
/// by hand or before this record existed.
static LAST_INSTALL: Mutex<String> = Mutex::new(String::new());

/// The install record read at boot (from RAM: safe on any thread), if it
/// belongs to the running image: after a hand upload or a web-flasher
/// reinstall the record of an older online install stays in flash but no
/// longer describes what runs.
pub fn last_install() -> String {
    let r = install_record();
    if r.starts_with(&format!("v{} ", env!("CARGO_PKG_VERSION"))) {
        r
    } else {
        String::new()
    }
}

/// The install record as stored, whichever image it was made for.
pub fn install_record() -> String {
    LAST_INSTALL.lock().unwrap().clone()
}

/// Record what the online update is about to boot into.
pub fn set_last_install(record: &str) {
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_str(LAST_INSTALL_KEY, record) {
            log::warn!("config: storing the install record: {e}");
        }
    }
}

const SWARM_KEY: &str = "swarm";
/// At most this many miners in the swarm list, and this many bytes stored.
pub const SWARM_MAX: usize = 32;
const SWARM_MAX_BYTES: usize = 1536;

/// The other miners the web UI's swarm page shows: addresses only, the
/// browser asks each of them for its state. Its own NVS key, so the
/// config document stays small.
pub fn swarm() -> Vec<String> {
    let mut buf = [0u8; SWARM_MAX_BYTES + 64];
    NVS.lock()
        .unwrap()
        .as_ref()
        .and_then(|nvs| nvs.get_str(SWARM_KEY, &mut buf).ok().flatten())
        .and_then(|s| serde_json::from_str::<Vec<String>>(s.trim_end_matches('\0')).ok())
        .unwrap_or_default()
}

/// Store the swarm list; addresses are IPv4 or host names, deduplicated.
pub fn set_swarm(members: &[String]) -> Result<Vec<String>, String> {
    let mut clean: Vec<String> = Vec::new();
    for m in members {
        let m = m.trim().trim_start_matches("http://").trim_end_matches('/');
        let ok = !m.is_empty()
            && m.len() <= 64
            && m.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
        if !ok {
            return Err(format!("not an address: {m:?}"));
        }
        if !clean.iter().any(|c| c.eq_ignore_ascii_case(m)) {
            clean.push(m.to_string());
        }
    }
    if clean.len() > SWARM_MAX {
        return Err(format!("at most {SWARM_MAX} miners"));
    }
    let json = serde_json::to_string(&clean).map_err(|e| e.to_string())?;
    if json.len() > SWARM_MAX_BYTES {
        return Err("list too long".into());
    }
    let mut g = NVS.lock().unwrap();
    let nvs = g.as_mut().ok_or("no NVS")?;
    nvs.set_str(SWARM_KEY, &json).map_err(|e| e.to_string())?;
    Ok(clean)
}

/// Record a nonce difficulty; writes NVS only when the all-time best rises,
/// which after the first hours is rare.
pub fn raise_best_diff(diff: u64) {
    {
        let mut best = BEST_DIFF.lock().unwrap();
        if *best >= diff {
            return;
        }
        *best = diff;
    }
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_u64(BEST_KEY, diff) {
            log::warn!("config: storing best diff: {e}");
        }
    }
}

pub fn get() -> Config {
    CONFIG.lock().unwrap().clone().unwrap_or_default()
}

impl Config {
    /// The config as the API may show it: secrets replaced by a marker
    /// that says whether one is set. Never return the real ones.
    /// The main pool speaks Stratum V2.
    pub fn sv2(&self) -> bool {
        is_sv2(&self.stratum_protocol)
    }
    /// The fallback pool speaks Stratum V2.
    pub fn fallback_sv2(&self) -> bool {
        is_sv2(&self.fallback_protocol)
    }

    pub fn redacted(&self) -> Config {
        let mut c = self.clone();
        let mask = |v: &String| {
            if v.is_empty() {
                String::new()
            } else {
                "***".into()
            }
        };
        c.wifi_pass = mask(&self.wifi_pass);
        c.stratum_pass = mask(&self.stratum_pass);
        c.fallback_pass = mask(&self.fallback_pass);
        c
    }
}

/// Apply a JSON object of AxeOS-named fields, persist, return the result.
pub fn patch(v: &serde_json::Value) -> anyhow::Result<Config> {
    let mut cur = serde_json::to_value(get())?;
    if let (Some(dst), Some(src)) = (cur.as_object_mut(), v.as_object()) {
        for (k, val) in src {
            if dst.contains_key(k) {
                dst.insert(k.clone(), val.clone());
            }
        }
    }
    let mut cfg: Config = serde_json::from_value(cur)?;
    // A masked secret coming back in a PATCH means "keep what you have".
    let old = get();
    if cfg.wifi_pass == "***" {
        cfg.wifi_pass = old.wifi_pass;
    }
    if cfg.stratum_pass == "***" {
        cfg.stratum_pass = old.stratum_pass;
    }
    if cfg.fallback_pass == "***" {
        cfg.fallback_pass = old.fallback_pass;
    }
    save(&cfg)?;
    Ok(cfg)
}

pub fn save(cfg: &Config) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(cfg)?;
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        nvs.set_raw(KEY, &bytes)?;
    }
    *CONFIG.lock().unwrap() = Some(cfg.clone());
    Ok(())
}

/// What the stock firmware (AxeOS or NerdOS) left in its NVS namespace
/// `main`, read-only. WarpAxe's own settings live in `warpaxe`, so a stock
/// NVS kept through the first flash is still there.
pub struct Stock(Option<EspNvs<NvsDefault>>);

impl Stock {
    pub fn open() -> Stock {
        let part = PART.lock().unwrap().clone();
        Stock(part.and_then(|p| EspNvs::new(p, "main", false).ok()))
    }

    pub fn str(&self, key: &str) -> Option<String> {
        let nvs = self.0.as_ref()?;
        let mut buf = [0u8; 160];
        nvs.get_str(key, &mut buf)
            .ok()
            .flatten()
            .map(|s| s.trim_end_matches('\0').to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn u16(&self, key: &str) -> Option<u16> {
        let nvs = self.0.as_ref()?;
        nvs.get_u16(key)
            .ok()
            .flatten()
            .or_else(|| self.str(key).and_then(|s| s.parse().ok()))
    }
}

/// On a first start: take Wi-Fi, pools and hostname from the stock
/// firmware where WarpAxe has nothing yet. Returns what was taken (never
/// the values: passwords must not reach the log).
pub fn import_stock(stock: &Stock) -> Vec<&'static str> {
    let mut cfg = get();
    let mut took = Vec::new();
    if cfg.ssid.is_empty() {
        if let Some(ssid) = stock.str("wifissid") {
            cfg.ssid = ssid;
            cfg.wifi_pass = stock.str("wifipass").unwrap_or_default();
            took.push("wifi");
        }
    }
    if cfg.stratum_user.is_empty() {
        if let (Some(url), Some(user)) = (stock.str("stratumurl"), stock.str("stratumuser")) {
            cfg.stratum_url = url;
            cfg.stratum_user = user;
            cfg.stratum_port = stock.u16("stratumport").unwrap_or(cfg.stratum_port);
            cfg.stratum_pass = stock.str("stratumpass").unwrap_or_else(|| "x".into());
            cfg.stratum_tls = u8::from(stock.u16("stratumtls") == Some(1));
            // NerdOS keys; AxeOS stores its Sv2 settings under other names.
            if stock.u16("sv2_proto") == Some(1) {
                cfg.stratum_protocol = "SV2".into();
            }
            cfg.stratum_v2_authority_pubkey = stock.str("sv2_auth_pk").unwrap_or_default();
            cfg.stratum_v2_require_auth = stock.u16("sv2_reqauth") != Some(0);
            took.push("pool");
        }
    }
    if cfg.fallback_url.is_empty() {
        if let (Some(url), Some(user)) = (stock.str("fbstratumurl"), stock.str("fbstratumuser")) {
            cfg.fallback_url = url;
            cfg.fallback_user = user;
            cfg.fallback_port = stock.u16("fbstratumport").unwrap_or(cfg.fallback_port);
            cfg.fallback_pass = stock.str("fbstratumpass").unwrap_or_else(|| "x".into());
            cfg.fallback_tls = u8::from(stock.u16("fbstratumtls") == Some(1));
            if stock.u16("fbsv2_proto") == Some(1) {
                cfg.fallback_protocol = "SV2".into();
            }
            cfg.fallback_v2_authority_pubkey = stock.str("fbsv2_authpk").unwrap_or_default();
            cfg.fallback_v2_require_auth = stock.u16("fbsv2_reqauth") != Some(0);
            took.push("fallback");
        }
    }
    if cfg.hostname == Config::default().hostname {
        if let Some(h) = stock.str("hostname") {
            cfg.hostname = h;
            took.push("hostname");
        }
    }
    if !took.is_empty() {
        if let Err(e) = save(&cfg) {
            log::warn!("config: saving the imported settings failed: {e}");
        }
    }
    took
}

/// "SV2" (any case) or the NerdOS number 1 means Stratum V2.
pub fn is_sv2(protocol: &str) -> bool {
    protocol.trim().eq_ignore_ascii_case("sv2") || protocol.trim() == "1"
}
