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
    let mut buf = vec![0u8; 2048];
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
