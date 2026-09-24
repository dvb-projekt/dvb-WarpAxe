//! Device configuration, one JSON document in NVS. The AxeOS field names
//! are kept where AxeOS has them so `PATCH /api/system` maps 1:1.

use std::sync::Mutex;

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub hostname: String,
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
    /// Chips on the chain; 0 = discover.
    #[serde(rename = "asicCount")]
    pub asic_count: u8,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: "warpaxe".into(),
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
            asic_count: 0,
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
    *NVS.lock().unwrap() = Some(nvs);
    Ok(())
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
