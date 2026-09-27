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
    /// Set the clock by NTP (AxeOS name); the history and log show real
    /// times once it is set.
    #[serde(rename = "useNTP")]
    pub use_ntp: bool,
    #[serde(rename = "ntpServer")]
    pub ntp_server: String,
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
            board_model: "t-display-s3".into(),
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
            use_ntp: true,
            ntp_server: "pool.ntp.org".into(),
            frequency: 525,
            core_voltage: 1150,
            fanspeed: 100,
            autofanspeed: true,
            display_timeout: 30,
            flipscreen: false,
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
const KEY: &str = "config";

/// Load from NVS (or defaults) and remember the NVS handle for saves.
pub fn init(part: EspDefaultNvsPartition) -> anyhow::Result<()> {
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
