//! Device configuration, one JSON document in NVS. The AxeOS field names
//! are kept where AxeOS has them so `PATCH /api/system` maps 1:1.

use std::sync::Mutex;

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use serde::{Deserialize, Serialize};
use warpaxe_stratum::block;

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
    /// The main pool's confirmed public key for a certificate no public
    /// authority issued: SHA-256 of its SubjectPublicKeyInfo as
    /// `AB:CD:…:EF` (`warpaxe_trust::pin`); empty = none. Only consulted
    /// when the bundle check fails; set by a click on the Pool page.
    #[serde(rename = "stratumTLSPin")]
    pub stratum_tls_pin: String,
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
    /// As `stratum_tls_pin`, for the fallback pool.
    #[serde(rename = "fallbackStratumTLSPin")]
    pub fallback_tls_pin: String,
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
    /// Show the chips' hash counters (per chip and domain) on the web UI's
    /// dashboard. Off by default; `hashrateMonitor` is in the API either way.
    #[serde(rename = "showHashCounters")]
    pub show_hash_counters: bool,
    /// The coin the main pool mines, as its ticker (BTC, BCH, FIX …):
    /// only the unit the web UI writes the coinbase amounts in.
    #[serde(rename = "coinTicker")]
    pub coin_ticker: String,
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
    /// Both fan outputs at the chip fan's speed: M1 follows M2 1:1.
    /// On, as NerdOS's second channel defaults to LINKED (mirrors
    /// channel 0): on the NerdOctaxe both headers cool the chips, and a
    /// second fan left to the regulator's temperature idled at 25 %
    /// while the chips overheated (v2.2, 2026-10-01). Off = the second
    /// output follows the regulator temperature (`autofanspeed2`).
    #[serde(rename = "linkFans")]
    pub link_fans: bool,
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
    /// Which init brings up a BM1373: empty (the default) for the
    /// board's own — ESP-Miner's on the Bitaxe Naja Duo, NerdOS's on the
    /// NerdAxe Gaia — or by name: `espminer` (ESP-Miner's, 3 MBaud
    /// before the ramp), `espminer-1m` (the same registers, 1 MBaud
    /// after the ramp as NerdOS paces it; the Naja Duo's fallback should
    /// 3 MBaud not hold across its level shifters) or `nerdos`. Read at
    /// every chain start, so a change takes effect after chips off/on
    /// or a restart. A chain that does not come up moves one init down
    /// that list until the next boot (`board::chip_init_fall_back`).
    /// Ignored on every other chip. `docs/05`.
    #[serde(rename = "chipInit")]
    pub chip_init: String,
    /// The USB network: the device is 192.168.<usbSubnet>.1. Default 7, as
    /// the docs and the web flasher say; a second WarpAxe on the same
    /// computer gets another number by hand (two links on one subnet
    /// leave the computer unable to reach either). Applied at boot.
    #[serde(rename = "usbSubnet")]
    pub usb_subnet: u8,
    /// Pool connections may use IPv6: the station takes an IPv6 address
    /// from the router (SLAAC) and a pool with AAAA records is tried over
    /// IPv6 first, IPv4 a quarter second later (Happy Eyeballs,
    /// `net::dial`). On by default; an IPv4-only network never tries
    /// IPv6. Switched on, the address follows within seconds. Off: pool
    /// names only over IPv4 at once (a pool typed as an IPv6 literal is
    /// still used as typed); the address itself goes at the next start.
    /// `docs/17`.
    #[serde(rename = "ipv6")]
    pub ipv6: bool,
    /// The rated output of the power supply that feeds this miner, whole
    /// watts; 0 = unknown. Only reported: WarpTune keeps a miner below
    /// 80 % of it and searches no higher than the stock point without it.
    #[serde(rename = "psuWatts")]
    pub psu_watts: u16,
    /// The LCD pages the page key steps through, in order; names from
    /// `warpaxe_displaycfg::PAGES`. An empty list = the factory pages.
    #[serde(rename = "displayPages")]
    pub display_pages: Vec<String>,
    /// The page shown at boot and after a minute without a key press.
    #[serde(rename = "displayStartPage")]
    pub display_start_page: String,
    /// The cockpit's large number: hashrate, efficiency or power.
    #[serde(rename = "cockpitBig")]
    pub cockpit_big: String,
    /// The three places of the cockpit's right column
    /// (`warpaxe_displaycfg::SLOTS`); an empty list = the board's default.
    #[serde(rename = "cockpitSlots")]
    pub cockpit_slots: Vec<String>,
    /// The cockpit's footer line (`warpaxe_displaycfg::FOOTERS`).
    #[serde(rename = "cockpitFooter")]
    pub cockpit_footer: String,
    /// The OLED slides that rotate, in order
    /// (`warpaxe_displaycfg::OLED_SLIDES`); an empty list = the factory
    /// slides.
    #[serde(rename = "oledSlides")]
    pub oled_slides: Vec<String>,
    /// Seconds an OLED slide stays, 1–60.
    #[serde(rename = "oledInterval")]
    pub oled_interval: u8,
    /// Night mode: inside the window the display shows the clock screen
    /// instead of the pages (colour displays: time, date, hashrate and
    /// temperature; OLED: its clock slide), dimmed to `night_brightness`,
    /// and the screen timeout does not apply. Needs the clock (SNTP); off by default.
    #[serde(rename = "nightMode")]
    pub night_mode: bool,
    /// The window, minutes of the local day; it may run past midnight.
    #[serde(rename = "nightStart")]
    pub night_start: u16,
    #[serde(rename = "nightEnd")]
    pub night_end: u16,
    /// Backlight (LCD) or contrast (OLED) at night, percent; 0 = dark.
    #[serde(rename = "nightBrightness")]
    pub night_brightness: u8,
    /// The display clock: "auto" (12 h with AM/PM in US, Canadian,
    /// Australian and New Zealand time zones, else 24 h), "24h", "12h".
    #[serde(rename = "clockFormat")]
    pub clock_format: String,
    /// Discord notifications (docs/22): the webhook the miner posts to,
    /// empty = none. A secret (its token lets anyone post to the
    /// channel): the API shows `***` when one is set and
    /// `discordWebhookSet`, never the URL; `***` coming back in a PATCH
    /// keeps it. Only Discord's own https URLs are taken
    /// (`warpaxe_alerts::webhook`).
    #[serde(rename = "discordWebhook")]
    pub discord_webhook: String,
    /// A message when a block is found.
    #[serde(rename = "alertBlockFound")]
    pub alert_block: bool,
    /// A message on a new all-time best difficulty (at most one per ten
    /// minutes, the newest value).
    #[serde(rename = "alertBestDiff")]
    pub alert_best: bool,
    /// A message when the miner moves to the fallback pool or back.
    #[serde(rename = "alertPoolSwitch")]
    pub alert_pool: bool,
    /// A message after a restart that a crash or a watchdog caused.
    #[serde(rename = "alertCrash")]
    pub alert_crash: bool,
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
            stratum_tls_pin: String::new(),
            use_fallback: false,
            fallback_url: String::new(),
            fallback_port: 3333,
            fallback_user: String::new(),
            fallback_pass: "x".into(),
            fallback_tls: 0,
            fallback_tls_pin: String::new(),
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
            show_hash_counters: false,
            price_currency: "EUR".into(),
            coin_ticker: "BTC".into(),
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
            link_fans: true,
            suggested_difficulty: 0,
            job_interval_ms: 0,
            asic_count: 0,
            asic_difficulty: 0,
            chip_init: String::new(),
            usb_subnet: 7,
            ipv6: true,
            psu_watts: 0,
            // The screens of v0.1.30, spelt out: a device updated from
            // there shows no difference.
            display_pages: warpaxe_displaycfg::DEFAULT_PAGES
                .iter()
                .map(|p| p.to_string())
                .collect(),
            display_start_page: "cockpit".into(),
            cockpit_big: warpaxe_displaycfg::DEFAULT_BIG.into(),
            cockpit_slots: Vec::new(),
            cockpit_footer: warpaxe_displaycfg::DEFAULT_FOOTER.into(),
            oled_slides: warpaxe_displaycfg::DEFAULT_OLED_SLIDES
                .iter()
                .map(|p| p.to_string())
                .collect(),
            oled_interval: warpaxe_displaycfg::DEFAULT_OLED_INTERVAL_S,
            night_mode: false,
            night_start: warpaxe_displaycfg::DEFAULT_NIGHT_START_MIN,
            night_end: warpaxe_displaycfg::DEFAULT_NIGHT_END_MIN,
            night_brightness: warpaxe_displaycfg::DEFAULT_NIGHT_BRIGHTNESS,
            clock_format: warpaxe_displaycfg::DEFAULT_CLOCK_FORMAT.into(),
            discord_webhook: String::new(),
            alert_block: true,
            alert_best: true,
            alert_pool: true,
            alert_crash: true,
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
    // The count (under 160 bytes, as every version writes it) and the
    // last block's hash and status (about 200); both buffers with room.
    let mut buf = [0u8; 256];
    let mut info_buf = [0u8; 384];
    let count = nvs
        .get_str(BLOCKS_KEY, &mut buf)
        .ok()
        .flatten()
        .map(|s| s.trim_end_matches('\0'));
    let info = nvs
        .get_str(BLOCK_INFO_KEY, &mut info_buf)
        .ok()
        .flatten()
        .map(|s| s.trim_end_matches('\0'));
    *BLOCKS.lock().unwrap() = blocks_from(count, info);
    *BEST_DIFF_F.lock().unwrap() = nvs
        .get_u64(BEST_F_KEY)
        .ok()
        .flatten()
        .map(f64::from_bits)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(0.0);
    // The clock offsets per chip.
    let mut buf = [0u8; CHIP_OFFSETS_BYTES];
    if let Ok(Some(bytes)) = nvs.get_raw(CHIP_OFFSETS_KEY, &mut buf) {
        *CHIP_OFFSETS.lock().unwrap() = ChipOffsets::from_bytes(bytes);
    }
    let mut buf = [0u8; OffsetsSet::BYTES];
    if let Ok(Some(bytes)) = nvs.get_raw(CHIP_OFFSETS_SET_KEY, &mut buf) {
        let sum = CHIP_OFFSETS.lock().unwrap().sum();
        *CHIP_OFFSETS_SET.lock().unwrap() = OffsetsSet::from_bytes(bytes, sum);
    }
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

/// The default NVS partition, for reading another firmware's namespace
/// (the devkit's NerdMiner import).
pub fn partition() -> Option<EspDefaultNvsPartition> {
    PART.lock().unwrap().clone()
}

/// All-time best nonce difficulty, kept across reboots (AxeOS `bestDiff`).
/// A mutex, not an atomic: Xtensa has no 64-bit atomics.
static BEST_DIFF: Mutex<u64> = Mutex::new(0);
const BEST_KEY: &str = "bestdiff";
/// The same with its decimals, for the devkit, whose shares sit below 1
/// (the integer above stays 0 there). Stored as the f64's bits.
static BEST_DIFF_F: Mutex<f64> = Mutex::new(0.0);
const BEST_F_KEY: &str = "bestdiff_f";

pub fn best_diff() -> u64 {
    *BEST_DIFF.lock().unwrap()
}

/// All-time best difficulty with its decimals (the devkit's figure).
pub fn best_diff_f() -> f64 {
    *BEST_DIFF_F.lock().unwrap()
}

/// Record a difficulty with its decimals; NVS only when the best rises.
pub fn raise_best_diff_f(diff: f64) {
    {
        let mut best = BEST_DIFF_F.lock().unwrap();
        if !(diff > *best) {
            return;
        }
        *best = diff;
    }
    crate::alerts::best_diff(diff);
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_u64(BEST_F_KEY, diff.to_bits()) {
            log::warn!("config: storing best diff: {e}");
        }
    }
}

/// Blocks found, kept across reboots: how many, and the last one
/// (height, unix time or 0 without a clock, coin ticker) for the web UI's
/// banner. One NVS string in the form every version since the first
/// reads (under 160 bytes), written when a block is found. The last
/// block's hash, its parent's and what became of it are in a second
/// string ([`BlockInfo`]), so a firmware from before v0.1.42 keeps the
/// count after a downgrade.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Blocks {
    pub n: u32,
    pub h: u64,
    pub t: u64,
    pub coin: String,
    /// The block's hash, hex, as explorers show it; empty for a block
    /// found before v0.1.42. From [`BlockInfo`], not part of this record.
    #[serde(skip)]
    pub hash: String,
    /// The hash of the block it builds on, the same way.
    #[serde(skip)]
    pub prev: String,
    /// `pending`, `confirmed`, `orphaned` or `unknown`
    /// (`warpaxe_stratum::block::Status`); empty without a hash.
    #[serde(skip)]
    pub status: String,
}

/// The last found block's hash, its parent's and its status, with the
/// height and time of the block they belong to (a block counted by an
/// older firmware after a downgrade has none of this, and must not get
/// the one before's). One NVS string, written when a block is found and
/// when its status changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct BlockInfo {
    h: u64,
    t: u64,
    hash: String,
    prev: String,
    status: String,
}

static BLOCKS: Mutex<Option<Blocks>> = Mutex::new(None);
/// The parents of the blocks recorded lately (RAM only): a find on one
/// of them is a leftover, not a new block. Locked inside [`BLOCKS`].
static RECENT_PARENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
const BLOCKS_KEY: &str = "blocks";
const BLOCK_INFO_KEY: &str = "blockinfo";

/// The blocks found so far, all time.
pub fn blocks() -> Blocks {
    BLOCKS.lock().unwrap().clone().unwrap_or_default()
}

/// The stored records as read at boot: the count, with the last block's
/// hash and status where the second record is that block's.
fn blocks_from(count: Option<&str>, info: Option<&str>) -> Option<Blocks> {
    let mut b: Blocks = serde_json::from_str(count?).ok()?;
    if let Some(i) = info.and_then(|s| serde_json::from_str::<BlockInfo>(s).ok()) {
        if i.h == b.h && i.t == b.t {
            b.hash = i.hash;
            b.prev = i.prev;
            b.status = i.status;
        }
    }
    Some(b)
}

/// Count a found block and store it with its hash and its parent's
/// (hex, as explorers show them), pending: two writes. `false`, and
/// nothing written or counted, for a find that is a block already on
/// record or a leftover of one (`warpaxe_stratum::block::repeats`): on
/// a chain at minimum difficulty (testnet4, regtest) every nonce meets
/// the target, hundreds a second, also for the job before the block
/// just found. From a thread with an internal stack (the miner loops,
/// like `raise_best_diff`).
pub fn record_block(height: u64, unix: u64, hash: &str, prev: &str) -> bool {
    let coin = get().coin_ticker;
    let b = {
        let mut g = BLOCKS.lock().unwrap();
        let b = g.get_or_insert_with(Blocks::default);
        let mut recent = RECENT_PARENTS.lock().unwrap();
        let last = (b.n > 0).then(|| block::OnRecord {
            height: b.h,
            coin: &b.coin,
            prev: &b.prev,
            has_hash: !b.hash.is_empty(),
            orphaned: b.status == "orphaned",
        });
        if block::repeats(last.as_ref(), &recent, height, &coin, prev) {
            return false;
        }
        block::remember_parent(&mut recent, prev);
        drop(recent);
        b.n += 1;
        b.h = height;
        b.t = unix;
        b.coin = coin;
        b.hash = hash.into();
        b.prev = prev.into();
        b.status = "pending".into();
        b.clone()
    };
    if let Ok(json) = serde_json::to_string(&b) {
        nvs_store(BLOCKS_KEY, &json);
    }
    store_block_info(&b);
    true
}

/// The status of the last found block, whose hash is `hash`, changed:
/// stored (one write). `false` when the stored block is another one
/// (found meanwhile on another thread) or already has that status. From
/// a thread with an internal stack.
pub fn set_block_status(hash: &str, status: &str) -> bool {
    let b = {
        let mut g = BLOCKS.lock().unwrap();
        let Some(b) = g
            .as_mut()
            .filter(|b| !b.hash.is_empty() && b.hash == hash && b.status != status)
        else {
            return false;
        };
        b.status = status.into();
        b.clone()
    };
    store_block_info(&b);
    true
}

fn store_block_info(b: &Blocks) {
    let info = BlockInfo {
        h: b.h,
        t: b.t,
        hash: b.hash.clone(),
        prev: b.prev.clone(),
        status: b.status.clone(),
    };
    if let Ok(json) = serde_json::to_string(&info) {
        nvs_store(BLOCK_INFO_KEY, &json);
    }
}

fn nvs_store(key: &str, json: &str) {
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_str(key, json) {
            log::warn!("config: storing {key}: {e}");
        }
    }
}

/// Forget the all-time best difficulty.
pub fn reset_best_diff() {
    *BEST_DIFF.lock().unwrap() = 0;
    *BEST_DIFF_F.lock().unwrap() = 0.0;
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_u64(BEST_KEY, 0) {
            log::warn!("config: resetting best diff: {e}");
        }
        if let Err(e) = nvs.set_u64(BEST_F_KEY, 0) {
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
    let t0 = std::time::Instant::now();
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_str(LAST_INSTALL_KEY, record) {
            log::warn!("config: storing the install record: {e}");
        }
    }
    slow_write("the install record", t0);
}

/// A flash write on the restart path that took long enough for a page
/// erase is worth a line: the interrupt watchdog is not fed while a
/// flash operation runs (docs/12), so the figure says how close it came.
fn slow_write(what: &str, t0: std::time::Instant) {
    let ms = t0.elapsed().as_millis();
    if ms >= 100 {
        log::warn!("config: storing {what} took {ms} ms (a page erase; the interrupt watchdog allows 1000)");
    }
}

const CHIP_OFFSETS_KEY: &str = "chipoffs";
const CHIP_OFFSETS_BYTES: usize = 1 + 2 * warpaxe_chain::MAX_CHIPS;
/// The stored offsets, read once at boot (`init`): the mining loop
/// must not read flash on a PSRAM stack.
static CHIP_OFFSETS: Mutex<ChipOffsets> = Mutex::new(ChipOffsets::NONE);

/// Clock offsets per chip, MHz: `mhz[..len]` count, the rest is 0.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ChipOffsets {
    pub mhz: [i16; warpaxe_chain::MAX_CHIPS],
    pub len: usize,
}

impl ChipOffsets {
    /// No chip has an offset.
    pub const NONE: Self = Self {
        mhz: [0; warpaxe_chain::MAX_CHIPS],
        len: 0,
    };

    /// The entries that count.
    pub fn list(&self) -> &[i16] {
        &self.mhz[..self.len.min(self.mhz.len())]
    }

    /// A checksum of the stored form (FNV-1a), kept with who set them
    /// ([`OffsetsSet`]): an older image that changed the offsets without
    /// knowing that record leaves one that no longer fits them.
    fn sum(&self) -> u32 {
        self.bytes().iter().fold(0x811c_9dc5u32, |h, &b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        })
    }

    /// As stored: the count, then every entry in two bytes.
    fn bytes(&self) -> [u8; CHIP_OFFSETS_BYTES] {
        let mut out = [0u8; CHIP_OFFSETS_BYTES];
        out[0] = self.len as u8;
        for i in 0..warpaxe_chain::MAX_CHIPS {
            let [lo, hi] = self.mhz[i].to_le_bytes();
            out[1 + 2 * i] = lo;
            out[2 + 2 * i] = hi;
        }
        out
    }

    /// From the stored bytes; none for a record of another length.
    fn from_bytes(b: &[u8]) -> Self {
        let mut out = Self::NONE;
        if b.len() == CHIP_OFFSETS_BYTES && usize::from(b[0]) <= warpaxe_chain::MAX_CHIPS {
            out.len = usize::from(b[0]);
            for i in 0..warpaxe_chain::MAX_CHIPS {
                out.mhz[i] = i16::from_le_bytes([b[1 + 2 * i], b[2 + 2 * i]]);
            }
        }
        out
    }
}

/// A clock of its own per chip (`chipFreqOffsets` in the API): MHz
/// added to `frequency` for the chip at that place in the chain (from
/// 0), negative for a slower chip. Empty (the default) = every chip at
/// `frequency`; otherwise one entry per chip
/// (`warpaxe_chain::offsets::normalise`), so a list from a board with
/// another chip count is told apart and dropped. Carried out by the
/// chain driver without a restart; the search for the values is a
/// tuner's, not the firmware's. Its own NVS key (the count, then two
/// bytes per chip): a tuner changes it often and the config document
/// stays as it is.
pub fn chip_offsets() -> ChipOffsets {
    *CHIP_OFFSETS.lock().unwrap()
}

/// Store the offsets (already in their stored form) with who set them;
/// `false` and no flash write when they are the ones in force. A flash
/// write otherwise: internal-stack callers only (the API).
pub fn set_chip_offsets(new: ChipOffsets, by: OffsetsBy) -> bool {
    let mut g = CHIP_OFFSETS.lock().unwrap();
    if *g == new {
        return false;
    }
    let set = OffsetsSet {
        by,
        unix: crate::clock::unix_now()
            .and_then(|t| u32::try_from(t).ok())
            .unwrap_or(0),
    };
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if nvs.set_raw(CHIP_OFFSETS_KEY, &new.bytes()).is_err() {
            log::warn!("config: storing the clock offsets failed");
        }
        if nvs.set_raw(CHIP_OFFSETS_SET_KEY, &set.bytes(new.sum())).is_err() {
            log::warn!("config: storing who set the clock offsets failed");
        }
    }
    *g = new;
    *CHIP_OFFSETS_SET.lock().unwrap() = set;
    true
}

const CHIP_OFFSETS_SET_KEY: &str = "chipoffset";
/// Who set the offsets in force and when, read once at boot like them.
static CHIP_OFFSETS_SET: Mutex<OffsetsSet> = Mutex::new(OffsetsSet::UNKNOWN);

/// Who sent the PATCH that set the clock offsets per chip
/// (`chipFreqOffsetsBy`): the web UI says `manual`, WarpTune `warptune`;
/// anything else, or nothing, is `api`. The web UI shows it next to
/// the offsets (docs/18).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OffsetsBy {
    /// Nothing stored: offsets from before v0.1.47, or none ever set.
    Unknown,
    Api,
    Manual,
    WarpTune,
}

impl OffsetsBy {
    /// From the PATCH's `chipFreqOffsetsBy`.
    pub fn from_patch(v: Option<&str>) -> Self {
        match v {
            Some("manual") => Self::Manual,
            Some("warptune") => Self::WarpTune,
            _ => Self::Api,
        }
    }

    /// As the API names it; `None` when unknown.
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::Unknown => return None,
            Self::Api => "api",
            Self::Manual => "manual",
            Self::WarpTune => "warptune",
        })
    }
}

/// Who set the offsets in force, and when (seconds since 1970; 0 when
/// the clock was not set then, or before v0.1.47 stored it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OffsetsSet {
    pub by: OffsetsBy,
    pub unix: u32,
}

impl OffsetsSet {
    /// Who, when and the offsets' checksum; v0.1.47 and v0.1.48 stored
    /// the first [`Self::BYTES_V1`] alone.
    const BYTES: usize = 9;
    const BYTES_V1: usize = 5;
    /// Nothing stored: offsets from before v0.1.47, or none ever set.
    const UNKNOWN: Self = Self {
        by: OffsetsBy::Unknown,
        unix: 0,
    };

    /// As stored: who (0 api, 1 manual, 2 warptune), the time, then the
    /// checksum of the offsets set ([`ChipOffsets::sum`]), LE.
    fn bytes(&self, sum: u32) -> [u8; Self::BYTES] {
        let [a, b, c, d] = self.unix.to_le_bytes();
        let [s0, s1, s2, s3] = sum.to_le_bytes();
        let by = match self.by {
            OffsetsBy::Unknown => 0xFF,
            OffsetsBy::Api => 0,
            OffsetsBy::Manual => 1,
            OffsetsBy::WarpTune => 2,
        };
        [by, a, b, c, d, s0, s1, s2, s3]
    }

    /// From the stored record, against `sum`, the checksum of the
    /// offsets in force: a record made for other offsets (they were
    /// changed by an image from before the checksum, after a downgrade)
    /// is unknown. A record without a checksum is taken as it is.
    fn from_bytes(b: &[u8], sum: u32) -> Self {
        let b = match b.len() {
            Self::BYTES_V1 => b,
            Self::BYTES if b[5..9] == sum.to_le_bytes() => &b[..Self::BYTES_V1],
            _ => return Self::UNKNOWN,
        };
        let Ok(&[by, a, b2, c, d]) = <&[u8; Self::BYTES_V1]>::try_from(b) else {
            return Self::UNKNOWN;
        };
        let by = match by {
            0 => OffsetsBy::Api,
            1 => OffsetsBy::Manual,
            2 => OffsetsBy::WarpTune,
            _ => OffsetsBy::Unknown,
        };
        Self {
            by,
            unix: u32::from_le_bytes([a, b2, c, d]),
        }
    }
}

/// Who set the offsets in force, and when.
pub fn chip_offsets_set() -> OffsetsSet {
    *CHIP_OFFSETS_SET.lock().unwrap()
}

const SWARM_KEY: &str = "swarm";
/// At most this many miners in the swarm list, and this many bytes stored.
pub const SWARM_MAX: usize = 32;
const SWARM_MAX_BYTES: usize = 1536;

/// The other miners the web UI's swarm page shows: addresses only, the
/// browser asks each of them for its state. A member stored as
/// `cgminer:<host>` is an Avalon (or another cgminer): the browser
/// cannot open its socket, so this miner reads it for the page
/// (`GET /api/v1/swarm/cgminer?host=`, read-only). Its own NVS key, so
/// the config document stays small.
pub fn swarm() -> Vec<String> {
    let mut buf = [0u8; SWARM_MAX_BYTES + 64];
    NVS.lock()
        .unwrap()
        .as_ref()
        .and_then(|nvs| nvs.get_str(SWARM_KEY, &mut buf).ok().flatten())
        .and_then(|s| serde_json::from_str::<Vec<String>>(s.trim_end_matches('\0')).ok())
        .unwrap_or_default()
}

/// The host of a swarm member stored as `cgminer:<host>`; `None` for
/// an HTTP member.
pub fn cgminer_host(member: &str) -> Option<&str> {
    warpaxe_avalon::member_host(member)
}

/// Whether `host` is on the stored list as a cgminer member: the only
/// hosts the cgminer endpoint will ever open a socket to.
pub fn swarm_has_cgminer(host: &str) -> bool {
    swarm()
        .iter()
        .any(|m| cgminer_host(m).is_some_and(|h| h.eq_ignore_ascii_case(host)))
}

/// Store the swarm list; addresses are IPv4 or host names (with an
/// optional port for HTTP members), deduplicated. A `cgminer:` prefix
/// (any case) marks an Avalon, whose host is an IPv4 literal
/// (`warpaxe_avalon::normalize_member`, host-tested).
pub fn set_swarm(members: &[String]) -> Result<Vec<String>, String> {
    let mut clean: Vec<String> = Vec::new();
    for m in members {
        let m = warpaxe_avalon::normalize_member(m)?;
        if !clean.iter().any(|c| c.eq_ignore_ascii_case(&m)) {
            clean.push(m);
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

const POOLS_KEY: &str = "pools";
/// At most this many saved pools, and this many bytes stored.
pub const POOLS_MAX: usize = 8;
/// Below the 4000-byte limit of an NVS string.
const POOLS_MAX_BYTES: usize = 3968;

/// A saved pool: the main pool's fields under their config names, so
/// using it is a config patch, plus a name and the coin it mines.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct PoolProfile {
    pub name: String,
    #[serde(rename = "stratumURL")]
    pub url: String,
    #[serde(rename = "stratumPort")]
    pub port: u16,
    #[serde(rename = "stratumUser")]
    pub user: String,
    #[serde(rename = "stratumPassword")]
    pub pass: String,
    #[serde(rename = "stratumTLS")]
    pub tls: u8,
    /// The confirmed key of a pool on its own certificate, as in the
    /// config; stored only when set, so the eight pools stay within the
    /// NVS string (`as_patch` still clears it when switching).
    #[serde(rename = "stratumTLSPin", skip_serializing_if = "String::is_empty")]
    pub pin: String,
    #[serde(rename = "stratumProtocol")]
    pub protocol: String,
    #[serde(rename = "stratumV2AuthorityPubkey")]
    pub pubkey: String,
    #[serde(rename = "stratumV2RequireAuth")]
    pub require_auth: bool,
    #[serde(rename = "coinTicker")]
    pub coin: String,
}

impl PoolProfile {
    /// The main pool as it is configured now, under `name`.
    pub fn current(name: &str, cfg: &Config) -> PoolProfile {
        PoolProfile {
            name: name.trim().to_string(),
            url: cfg.stratum_url.clone(),
            port: cfg.stratum_port,
            user: cfg.stratum_user.clone(),
            pass: cfg.stratum_pass.clone(),
            tls: cfg.stratum_tls,
            pin: cfg.stratum_tls_pin.clone(),
            protocol: cfg.stratum_protocol.clone(),
            pubkey: cfg.stratum_v2_authority_pubkey.clone(),
            require_auth: cfg.stratum_v2_require_auth,
            coin: cfg.coin_ticker.clone(),
        }
    }

    /// The config patch that makes this the main pool.
    pub fn as_patch(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("name");
            // A pool without a pin takes the previous pool's pin away.
            o.entry("stratumTLSPin")
                .or_insert_with(|| serde_json::Value::String(String::new()));
        }
        v
    }

    /// For the API: the password is never sent back.
    pub fn redacted(&self) -> PoolProfile {
        let mut p = self.clone();
        if !p.pass.is_empty() {
            p.pass = "***".into();
        }
        p
    }
}

/// The saved pools, in the order they were saved. Their own NVS key, so
/// the config document stays small.
pub fn pools() -> Vec<PoolProfile> {
    stored_pools().unwrap_or_default()
}

/// The saved pools as stored; `Err` when something is stored but does
/// not read (damaged, or written by another version), so a save does not
/// silently replace the whole list with one pool.
fn stored_pools() -> Result<Vec<PoolProfile>, String> {
    let mut buf = vec![0u8; POOLS_MAX_BYTES + 64];
    let g = NVS.lock().unwrap();
    let raw = match g.as_ref().map(|nvs| nvs.get_str(POOLS_KEY, &mut buf)) {
        Some(Ok(Some(s))) => s.trim_end_matches('\0').to_string(),
        Some(Ok(None)) | None => return Ok(Vec::new()),
        Some(Err(e)) => {
            log::warn!("config: reading the saved pools: {e}");
            return Err("the saved pools cannot be read".into());
        }
    };
    drop(g);
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<PoolProfile>>(&raw).map_err(|e| {
        log::warn!("config: the saved pools do not read ({e}); not overwriting them");
        "the saved pools cannot be read".to_string()
    })
}

fn store_pools(list: &[PoolProfile]) -> Result<(), String> {
    let json = serde_json::to_string(list).map_err(|e| e.to_string())?;
    if json.len() > POOLS_MAX_BYTES {
        return Err("the saved pools take too much space".into());
    }
    let mut g = NVS.lock().unwrap();
    let nvs = g.as_mut().ok_or("no NVS")?;
    nvs.set_str(POOLS_KEY, &json).map_err(|e| e.to_string())
}

/// Save a pool under its name; the same name (any case) is replaced in
/// place, a new one goes at the end.
pub fn save_pool(mut p: PoolProfile) -> Result<Vec<PoolProfile>, String> {
    p.name = p.name.trim().to_string();
    let name = p.name.clone();
    if name.is_empty() || name.chars().count() > 32 {
        return Err("a name of 1 to 32 characters".into());
    }
    if p.url.trim().is_empty() || p.user.trim().is_empty() {
        return Err("the pool needs an address and a worker".into());
    }
    let mut list = stored_pools()?;
    match list.iter().position(|q| q.name.eq_ignore_ascii_case(&name)) {
        Some(i) => list[i] = p,
        None if list.len() >= POOLS_MAX => return Err(format!("at most {POOLS_MAX} pools")),
        None => list.push(p),
    }
    store_pools(&list)?;
    Ok(list)
}

pub fn delete_pool(name: &str) -> Result<Vec<PoolProfile>, String> {
    let mut list = stored_pools()?;
    let before = list.len();
    list.retain(|q| !q.name.eq_ignore_ascii_case(name.trim()));
    if list.len() == before {
        return Err("no saved pool of that name".into());
    }
    store_pools(&list)?;
    Ok(list)
}

pub fn pool(name: &str) -> Option<PoolProfile> {
    pools()
        .into_iter()
        .find(|q| q.name.eq_ignore_ascii_case(name.trim()))
}

/// The last config changes over the API: when (unix seconds, 0 before
/// NTP), from which address, which keys. Kept across reboots so "who
/// changed the worker?" has an answer after the fact.
const PATCHES_KEY: &str = "patches";
const PATCHES_MAX: usize = 12;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatchRecord {
    pub t: u64,
    pub ip: String,
    pub keys: Vec<String>,
}

pub fn patches() -> Vec<PatchRecord> {
    let mut g = NVS.lock().unwrap();
    let Some(nvs) = g.as_mut() else {
        return Vec::new();
    };
    let mut buf = vec![0u8; 2048];
    nvs.get_str(PATCHES_KEY, &mut buf)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(s.trim_end_matches('\0')).ok())
        .unwrap_or_default()
}

pub fn record_patch(ip: &str, keys: &[String]) {
    let mut list = patches();
    list.push(PatchRecord {
        t: crate::clock::unix_now().unwrap_or(0),
        ip: ip.into(),
        keys: keys.to_vec(),
    });
    while list.len() > PATCHES_MAX {
        list.remove(0);
    }
    // Secrets never go in: only key names are stored.
    if let Ok(json) = serde_json::to_string(&list) {
        let mut g = NVS.lock().unwrap();
        if let Some(nvs) = g.as_mut() {
            if let Err(e) = nvs.set_str(PATCHES_KEY, &json) {
                log::warn!("config: saving the patch record failed: {e}");
            }
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
    // The Discord message, folded to one per ten minutes (RAM only).
    crate::alerts::best_diff(diff as f64);
    if let Some(nvs) = NVS.lock().unwrap().as_mut() {
        if let Err(e) = nvs.set_u64(BEST_KEY, diff) {
            log::warn!("config: storing best diff: {e}");
        }
    }
}

pub fn get() -> Config {
    CONFIG.lock().unwrap().clone().unwrap_or_default()
}

/// A string of our NVS namespace by key, for the small records other
/// modules keep beside the config (the event log). `None` when there is
/// none or it does not read.
pub fn nvs_get_str(key: &str, max: usize) -> Option<String> {
    let g = NVS.lock().unwrap();
    let nvs = g.as_ref()?;
    let mut buf = vec![0u8; max];
    nvs.get_str(key, &mut buf)
        .ok()
        .flatten()
        .map(|s| s.trim_end_matches('\0').to_string())
}

/// Store a string under a key of our NVS namespace. A flash write: only
/// from a thread whose stack is in internal RAM (the lesson of v0.1.5).
pub fn nvs_set_str(key: &str, value: &str) -> Result<(), String> {
    let mut g = NVS.lock().unwrap();
    let nvs = g.as_mut().ok_or("no NVS")?;
    nvs.set_str(key, value).map_err(|e| e.to_string())
}

/// Remove a key of our NVS namespace (a flash write, same rule). Ok
/// whether or not it was there.
pub fn nvs_remove(key: &str) -> Result<(), String> {
    let mut g = NVS.lock().unwrap();
    let nvs = g.as_mut().ok_or("no NVS")?;
    nvs.remove(key).map(|_| ()).map_err(|e| e.to_string())
}

/// Note a restart that is about to happen (`warpaxe_diag::restart`): on
/// the main task, right before the chips go off and again right before
/// the reset. The next boot takes it (`take_restart`) and tells a
/// completed restart from one that crashed on the way.
pub fn note_restart(m: &warpaxe_diag::restart::Marker) {
    let t0 = std::time::Instant::now();
    if let Err(e) = nvs_set_str(warpaxe_diag::restart::KEY, &m.to_stored()) {
        log::warn!("config: storing the restart marker: {e}");
    }
    slow_write("the restart marker", t0);
}

/// The image that ran last, by the first nine hex digits of its ELF
/// SHA, for `warpaxe_diag::restart::crashed`: a watchdog reset on the
/// first boot of an image is the flashing tool's, not a crash.
const LAST_RUN_SHA_KEY: &str = "lastsha";

/// Note this image as the one running; true when it is not the one
/// that ran last (or none is stored: a first install over a stock
/// firmware, or an erased NVS). One flash write per new image, none
/// otherwise. On the main task at boot.
/// When the SHA cannot be stored (no NVS), the answer is false: the
/// old rule then applies and a watchdog reset stays a crash, rather
/// than every boot counting as a first one.
pub fn note_image(sha: &str) -> bool {
    if nvs_get_str(LAST_RUN_SHA_KEY, 32).is_some_and(|s| s == sha) {
        return false;
    }
    match nvs_set_str(LAST_RUN_SHA_KEY, sha) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("config: storing the image's SHA: {e}");
            false
        }
    }
}

/// The restart marker the previous run left, removed from NVS; `None`
/// without one (or with one that does not read, which is removed all
/// the same). On the main task at boot.
pub fn take_restart() -> Option<warpaxe_diag::restart::Marker> {
    use warpaxe_diag::restart::{Marker, KEY, MAX_STORED};
    let stored = nvs_get_str(KEY, MAX_STORED + 16);
    // Removed whether or not it read: an entry too long for the buffer
    // must not sit there for every boot to come.
    if let Err(e) = nvs_remove(KEY) {
        log::warn!("config: removing the restart marker: {e}");
    }
    let stored = stored?;
    let m = Marker::parse(&stored);
    if m.is_none() {
        log::warn!("config: a restart marker that does not read: {stored:?}");
    }
    m
}

/// Moves of stored settings after an update. A value above the board's
/// ceiling (the chip stop; on the Naja Duo the core voltage, v0.1.45) is
/// checked at every start and saved only when it is above. v0.1.36,
/// keyed by a marker in NVS so it runs once per device: a Nano's stored
/// fan target 60 (the old default on the old diode scale) goes back to
/// the profile's value (`warpaxe_board::profile::migrated_fan_target`).
/// On the main task (board::init_bus), before the board starts.
pub fn migrate(profile: &warpaxe_board::Profile) {
    // A chip stop above the board's ceiling (a Bitaxe's 90 from before
    // v0.1.32): stored as the ceiling it acts as anyway, so the web UI's
    // field, capped at the ceiling, does not refuse the whole form.
    let cfg = get();
    if let Some(fixed_c) = profile.stored_chip_stop_fix(cfg.overheat_temp) {
        log::warn!(
            "config: chip stop {} °C above this board's ceiling: stored as {fixed_c} °C (it acted as that already)",
            cfg.overheat_temp
        );
        let mut fixed = cfg;
        fixed.overheat_temp = fixed_c;
        if let Err(e) = save(&fixed) {
            log::warn!("config: saving the chip stop failed: {e}");
        }
    }
    // A Naja Duo's core voltage above its v0.1.45 ceiling of 1120 mV
    // (stored by hand up to 1250 before): stored as the ceiling, for the
    // same reason. Only the Naja's ceiling moved; other boards' stored
    // values are left as they are.
    let cfg = get();
    if profile.name == warpaxe_board::profile::BITAXE_NAJA_DUO.name {
        if let Some(fixed_mv) = profile.stored_core_mv_fix(cfg.core_voltage) {
            log::warn!(
                "config: core voltage {} mV above this board's ceiling: stored as {fixed_mv} mV",
                cfg.core_voltage
            );
            let mut fixed = cfg;
            fixed.core_voltage = fixed_mv;
            if let Err(e) = save(&fixed) {
                log::warn!("config: saving the core voltage failed: {e}");
            }
        }
    }
    const NANO_FAN_TARGET_KEY: &str = "mig_nanofan";
    if profile.name == warpaxe_board::profile::BITFORGE_NANO.name
        && nvs_get_str(NANO_FAN_TARGET_KEY, 8).is_none()
    {
        let cfg = get();
        // A chip stop set by hand below the line was entered on the old
        // scale too; there is no default to tell it by, so it stays, said
        // once.
        if cfg.overheat_temp != 0 && f32::from(cfg.overheat_temp) < profile.chip_stop_c {
            log::warn!(
                "config: the chip stop {} °C was set on the old diode scale (v0.1.35 and before): on the maker's scale it is about 8 °C warmer in real terms; subtract 8 or set it to 0",
                cfg.overheat_temp
            );
        }
        if let Some(target) =
            warpaxe_board::profile::migrated_fan_target(profile, cfg.pid_target_temp)
        {
            log::warn!(
                "config: the fan target {} is the old default on the old diode scale: back to the profile's {} °C (v0.1.36)",
                cfg.pid_target_temp,
                profile.fan_target_c
            );
            let mut fixed = cfg;
            fixed.pid_target_temp = target;
            if let Err(e) = save(&fixed) {
                log::warn!("config: saving the fan target failed: {e}");
                return;
            }
        }
        if let Err(e) = nvs_set_str(NANO_FAN_TARGET_KEY, "1") {
            log::warn!("config: marking the fan-target move failed: {e}");
        }
    }
}

/// The board the chips have been counted on (`board::verify_choice`):
/// its profile name, or `None` until a first start passed. A `boardModel`
/// that is not this name starts cautiously and is checked against the
/// bus first (`warpaxe_board::choice`).
const BOARD_OK_KEY: &str = "boardok";

/// What `boardok` holds for a choice made under this firmware that has
/// not passed its first start yet (never a profile name).
pub const BOARD_PENDING: &str = warpaxe_board::choice::PENDING;

pub fn verified_board() -> Option<String> {
    nvs_get_str(BOARD_OK_KEY, 48)
}

/// Record the board as verified. A flash write: internal-stack callers
/// only (the miner thread).
pub fn set_verified_board(name: &str) -> Result<(), String> {
    nvs_set_str(BOARD_OK_KEY, name)
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
        c.discord_webhook = mask(&self.discord_webhook);
        c
    }
}

/// Apply a JSON object of AxeOS-named fields, persist, return the result.
pub fn patch(v: &serde_json::Value) -> anyhow::Result<Config> {
    let mut cur = serde_json::to_value(get())?;
    if let (Some(dst), Some(src)) = (cur.as_object_mut(), v.as_object()) {
        for (k, val) in src {
            if let Some(old) = dst.get(k) {
                // null clears the power supply (it is optional): 0 = unknown.
                if k == "psuWatts" && val.is_null() {
                    dst.insert(k.clone(), serde_json::json!(0));
                    continue;
                }
                // AxeOS sends its switches as 0/1; take a number for a
                // boolean field instead of failing the whole PATCH.
                let val = match (old, val.as_f64()) {
                    (serde_json::Value::Bool(_), Some(n)) => serde_json::Value::Bool(n != 0.0),
                    _ => val.clone(),
                };
                dst.insert(k.clone(), val);
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
    // The webhook: `***` keeps it, empty clears it, anything else must
    // be a Discord webhook URL (pasted with a space around it, taken).
    if cfg.discord_webhook == "***" {
        cfg.discord_webhook = old.discord_webhook;
    } else if cfg.discord_webhook != old.discord_webhook {
        cfg.discord_webhook = cfg.discord_webhook.trim().to_string();
        if !cfg.discord_webhook.is_empty() {
            warpaxe_alerts::webhook::validate(&cfg.discord_webhook)
                .map_err(|r| anyhow::anyhow!(r.text()))?;
        }
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
pub struct Stock(Option<EspNvs<NvsDefault>>, serde_json::Value);

impl Stock {
    /// Newer NerdOS builds (and BitMaker's NerdAxe Gaia firmware) keep
    /// every setting in one JSON blob, `json_cfg`, under the same key
    /// names; older ones store each key on its own. Both are read.
    pub fn open() -> Stock {
        let part = PART.lock().unwrap().clone();
        let nvs = part.and_then(|p| EspNvs::new(p, "main", false).ok());
        let json = nvs
            .as_ref()
            .and_then(|n| {
                let mut buf = vec![0u8; 8192];
                n.get_raw("json_cfg", &mut buf)
                    .ok()
                    .flatten()
                    .map(<[u8]>::to_vec)
            })
            .and_then(|b| {
                let end = b.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
                serde_json::from_slice(&b[..end]).ok()
            })
            .unwrap_or(serde_json::Value::Null);
        Stock(nvs, json)
    }

    fn json(&self, key: &str) -> Option<&serde_json::Value> {
        self.1.get(key)
    }

    pub fn str(&self, key: &str) -> Option<String> {
        let own = self.0.as_ref().and_then(|nvs| {
            let mut buf = [0u8; 160];
            nvs.get_str(key, &mut buf)
                .ok()
                .flatten()
                .map(|s| s.trim_end_matches('\0').to_string())
        });
        own.or_else(|| self.json(key).and_then(|v| v.as_str()).map(str::to_string))
            .filter(|s| !s.is_empty())
    }

    pub fn u16(&self, key: &str) -> Option<u16> {
        self.num(key).and_then(|v| u16::try_from(v as i64).ok())
    }

    /// A number whatever width the firmware stored it in: NerdOS and
    /// AxeOS use u8, u16, u32 and i32, and AxeOS keeps floats as text
    /// ("525.000000").
    pub fn num(&self, key: &str) -> Option<f64> {
        let Some(nvs) = self.0.as_ref() else {
            return self.json(key).and_then(|v| v.as_f64());
        };
        if let Some(v) = nvs.get_u16(key).ok().flatten() {
            return Some(f64::from(v));
        }
        if let Some(v) = nvs.get_u8(key).ok().flatten() {
            return Some(f64::from(v));
        }
        if let Some(v) = nvs.get_u32(key).ok().flatten() {
            return Some(f64::from(v));
        }
        if let Some(v) = nvs.get_i32(key).ok().flatten() {
            return Some(f64::from(v));
        }
        self.str(key)
            .and_then(|s| s.trim().parse().ok())
            .or_else(|| self.json(key).and_then(|v| v.as_f64()))
    }

    /// A long string (AxeOS 2.15 keeps each pool as JSON text).
    pub fn long_str(&self, key: &str) -> Option<String> {
        let nvs = self.0.as_ref()?;
        // (AxeOS pools only; not in json_cfg.)
        let mut buf = vec![0u8; 1024];
        nvs.get_str(key, &mut buf)
            .ok()
            .flatten()
            .map(|s| s.trim_end_matches('\0').to_string())
            .filter(|s| !s.is_empty())
    }
}

/// One pool as AxeOS 2.15 stores it (`pool_1` … `pool_8`, JSON text).
#[derive(Deserialize, Default)]
#[serde(default)]
struct AxeosPool {
    #[serde(rename = "stratumProtocol")]
    protocol: String,
    #[serde(rename = "stratumURL")]
    url: String,
    #[serde(rename = "stratumPort")]
    port: u16,
    #[serde(rename = "stratumUser")]
    user: String,
    #[serde(rename = "stratumPassword")]
    pass: String,
    #[serde(rename = "stratumTLS")]
    tls: u8,
    #[serde(rename = "stratumV2AuthorityPubkey")]
    pubkey: String,
    #[serde(rename = "stratumV2ChannelType")]
    channel: String,
}

/// AxeOS 2.15's pool at a 0-based selector index, if it has a URL.
fn axeos_pool(stock: &Stock, index: Option<f64>) -> Option<AxeosPool> {
    let n = index.map_or(0, |i| i as usize) + 1;
    let text = stock.long_str(&format!("pool_{n}"))?;
    serde_json::from_str::<AxeosPool>(&text)
        .ok()
        .filter(|p| !p.url.is_empty() && !p.user.is_empty())
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
    // AxeOS 2.15 keeps its pools as JSON (`pool_N`, selectors `prim_idx`
    // and `sec_idx`) and may leave the pre-2.15 keys behind stale, so the
    // new form wins where it exists.
    if cfg.stratum_user.is_empty() {
        if let Some(p) = axeos_pool(stock, stock.num("prim_idx")) {
            cfg.stratum_url = p.url;
            cfg.stratum_user = p.user;
            cfg.stratum_port = if p.port > 0 { p.port } else { cfg.stratum_port };
            cfg.stratum_pass = if p.pass.is_empty() {
                "x".into()
            } else {
                p.pass
            };
            cfg.stratum_tls = tls_from_stock(u16::from(p.tls), "pool");
            if is_sv2(&p.protocol) {
                cfg.stratum_protocol = "SV2".into();
            }
            cfg.stratum_v2_authority_pubkey = p.pubkey;
            if !p.channel.is_empty() {
                cfg.stratum_v2_channel_type = p.channel;
            }
            took.push("pool");
        }
    }
    if cfg.fallback_url.is_empty() {
        if let Some(p) = axeos_pool(stock, Some(stock.num("sec_idx").unwrap_or(1.0))) {
            cfg.fallback_url = p.url;
            cfg.fallback_user = p.user;
            cfg.fallback_port = if p.port > 0 {
                p.port
            } else {
                cfg.fallback_port
            };
            cfg.fallback_pass = if p.pass.is_empty() {
                "x".into()
            } else {
                p.pass
            };
            cfg.fallback_tls = tls_from_stock(u16::from(p.tls), "fallback pool");
            if is_sv2(&p.protocol) {
                cfg.fallback_protocol = "SV2".into();
            }
            cfg.fallback_v2_authority_pubkey = p.pubkey;
            if !p.channel.is_empty() {
                cfg.fallback_v2_channel_type = p.channel;
            }
            if let Some(on) = stock.num("usefbstartum") {
                cfg.use_fallback = on != 0.0;
            }
            took.push("fallback");
        }
    }
    if cfg.stratum_user.is_empty() {
        if let (Some(url), Some(user)) = (stock.str("stratumurl"), stock.str("stratumuser")) {
            cfg.stratum_url = url;
            cfg.stratum_user = user;
            cfg.stratum_port = stock.u16("stratumport").unwrap_or(cfg.stratum_port);
            cfg.stratum_pass = stock.str("stratumpass").unwrap_or_else(|| "x".into());
            cfg.stratum_tls = stock_tls(stock, "stratumtls");
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
            cfg.fallback_tls = stock_tls(stock, "fbstratumtls");
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
    if cfg.discord_webhook.is_empty() && import_discord(stock, &mut cfg) {
        took.push("discord");
    }
    // After an overheat stop ForgeOS (and older AxeOS) leave their
    // emergency point in NVS: 1000 mV, 50 MHz, fans 100 % manual,
    // `overheat_mode` 1. That is no point the owner chose, so WarpAxe
    // keeps its board defaults for clock, voltage and fans instead.
    if stock.num("overheat_mode") == Some(1.0) {
        log::warn!("config: the stock firmware was in its overheat mode; clock, voltage and fans not taken over");
    } else {
        if import_tuning(stock, &mut cfg) {
            took.push("tuning");
        }
        if import_fans(stock, &mut cfg) {
            took.push("fans");
        }
    }
    if import_display(stock, &mut cfg) {
        took.push("display");
    }
    // Every pool AxeOS 2.15 kept, as saved pools, so switching between
    // them stays one click after the move.
    if pools().is_empty() {
        let mut list: Vec<PoolProfile> = Vec::new();
        for n in 0..POOLS_MAX {
            let Some(p) = axeos_pool(stock, Some(n as f64)) else {
                continue;
            };
            // Names are at most 32 characters: shorten first, then make
            // them unique, so two long URLs do not end up with one name.
            let base: String = p.url.chars().take(27).collect();
            let mut name: String = p.url.chars().take(32).collect();
            let mut k = 2;
            while list.iter().any(|q| q.name.eq_ignore_ascii_case(&name)) {
                name = format!("{base} ({k})");
                k += 1;
            }
            let profile = PoolProfile {
                name,
                port: if p.port > 0 { p.port } else { 3333 },
                pass: if p.pass.is_empty() {
                    "x".into()
                } else {
                    p.pass
                },
                tls: tls_from_stock(u16::from(p.tls), "saved pool"),
                // AxeOS's own certificate (stratumTLS 2) is not taken over
                // as a key: the owner confirms it on the Pool page.
                pin: String::new(),
                protocol: if is_sv2(&p.protocol) {
                    "SV2".into()
                } else {
                    "SV1".into()
                },
                pubkey: p.pubkey,
                coin: "BTC".into(),
                url: p.url,
                user: p.user,
                require_auth: false,
            };
            // As many as fit: a long list is cut, not dropped.
            list.push(profile);
            if serde_json::to_string(&list).map_or(true, |j| j.len() > POOLS_MAX_BYTES) {
                list.pop();
                break;
            }
        }
        if !list.is_empty() {
            match store_pools(&list) {
                Ok(()) => took.push("saved pools"),
                Err(e) => log::warn!("config: storing the stock firmware's pools: {e}"),
            }
        }
    }
    if !took.is_empty() {
        if let Err(e) = save(&cfg) {
            log::warn!("config: saving the imported settings failed: {e}");
        }
    }
    took
}

/// NerdOS's Discord alerter (`alrt_disc_url`, NerdQAxe+ v1.1): its
/// webhook when it is one WarpAxe takes, and its switches where it has
/// them: `alrt_disc_bf_en` block found, `alrt_disc_bd_en` best
/// difficulty, `alrt_disc_en` its watchdog restart (a restart without a
/// share for an hour; here the restart after a crash or a watchdog). It
/// has no pool-switch message, so that switch keeps WarpAxe's default.
/// The URL never reaches the log.
fn import_discord(stock: &Stock, cfg: &mut Config) -> bool {
    const KEY: &str = "alrt_disc_url";
    let Some(url) = stock
        .long_str(KEY)
        .or_else(|| stock.str(KEY))
        .map(|u| u.trim().to_string())
    else {
        return false;
    };
    if let Err(r) = warpaxe_alerts::webhook::validate(&url) {
        log::warn!("config: the stock firmware's Discord webhook not taken over ({})", r.text());
        return false;
    }
    cfg.discord_webhook = url;
    let flag = |k: &str, d: bool| stock.num(k).map_or(d, |v| v != 0.0);
    cfg.alert_block = flag("alrt_disc_bf_en", cfg.alert_block);
    cfg.alert_best = flag("alrt_disc_bd_en", cfg.alert_best);
    cfg.alert_crash = flag("alrt_disc_en", cfg.alert_crash);
    true
}

/// TLS from a stock pool entry: 1 = TLS with the CA bundle, 2 = TLS
/// with the owner's own certificate (AxeOS, ForgeOS). WarpAxe has no
/// place for that certificate, so 2 becomes TLS with the CA bundle: a
/// pool on a private certificate then fails visibly instead of the
/// connection quietly dropping to plain text.
fn stock_tls(stock: &Stock, key: &str) -> u8 {
    stock
        .u16(key)
        .map_or(0, |v| tls_from_stock(v, key))
}

/// The same for a value already read (the AxeOS 2.15 pool list and its
/// active entries), so no path maps the owner's certificate to plain
/// text (the audit of 2026-10-23 found the active pool did).
fn tls_from_stock(v: u16, what: &str) -> u8 {
    match v {
        1 => 1,
        2 => {
            log::warn!("config: {what} used the owner's own certificate; TLS stays on with the built-in CA bundle");
            1
        }
        _ => 0,
    }
}

/// The clock and core voltage the stock firmware ran: the owner's
/// validated point on this very device (AxeOS 2.10+ `asicfrequency_f`,
/// older AxeOS and NerdOS `asicfrequency`; `asicvoltage` in both).
fn import_tuning(stock: &Stock, cfg: &mut Config) -> bool {
    let d = Config::default();
    let mut took = false;
    if cfg.frequency == d.frequency {
        let f = stock
            .num("asicfrequency_f")
            .or_else(|| stock.num("asicfrequency"));
        if let Some(f) = f.filter(|f| (100.0..=1500.0).contains(f)) {
            cfg.frequency = f.round() as u32;
            took = true;
        }
    }
    if cfg.core_voltage == d.core_voltage {
        if let Some(mv) = stock
            .num("asicvoltage")
            .filter(|v| (700.0..=1600.0).contains(v))
        {
            cfg.core_voltage = mv as u16;
            took = true;
        }
    }
    took
}

/// Fan control and temperature limits. AxeOS: `autofanspeed` (0/1),
/// `manualfanspeed`, `minfanspeed`, `temptarget`. NerdOS: `autofanspeed`
/// (a mode, 0 = manual), `fanspeed`, `pid_temp`, `overheat_temp` and the
/// second output `fan1mode` (0 = manual), `fan1speed`, `fan1_pid_temp`.
/// Not the PID gains: they belong to the stock firmware's own loop.
fn import_fans(stock: &Stock, cfg: &mut Config) -> bool {
    let pct = |v: f64| (0.0..=100.0).contains(&v).then_some(v as u8);
    let temp = |v: f64| (35.0..=90.0).contains(&v).then_some(v as u8);
    let mut took = false;
    if let Some(m) = stock.num("autofanspeed") {
        cfg.autofanspeed = m != 0.0;
        took = true;
    }
    if let Some(v) = stock
        .num("manualfanspeed")
        .or_else(|| stock.num("fanspeed"))
        .and_then(pct)
    {
        cfg.fanspeed = v;
        took = true;
    }
    if let Some(v) = stock.num("minfanspeed").and_then(pct) {
        cfg.min_fan_speed = v;
        took = true;
    }
    if let Some(v) = stock
        .num("temptarget")
        .or_else(|| stock.num("pid_temp"))
        .and_then(temp)
    {
        cfg.pid_target_temp = v;
        took = true;
    }
    if let Some(v) = stock.num("overheat_temp").and_then(temp) {
        cfg.overheat_temp = v;
        took = true;
    }
    // NerdOS FanController::Mode: 0 manual, 2 PID on the regulator
    // temperature, 3 linked to channel 0 (its default).
    if let Some(m) = stock.num("fan1mode") {
        cfg.autofanspeed2 = m != 0.0;
        cfg.link_fans = m == 3.0;
        took = true;
    }
    if let Some(v) = stock.num("fan1speed").and_then(pct) {
        cfg.manual_fan_speed2 = v;
        took = true;
    }
    if let Some(v) = stock.num("fan1_pid_temp").and_then(temp) {
        cfg.vr_target_temp = v;
        took = true;
    }
    took
}

/// Screen orientation, colours and when it goes dark. `flipscreen` (NerdOS,
/// pre-2.15 AxeOS) means what WarpAxe's switch means: the NerdOCTAXE-γ in
/// Oliver's case runs flipped under both, and the Bitaxe Gamma's OLED
/// reads right with it on. AxeOS 2.15 turns the board's own orientation
/// by `rotation` (0 = as the board comes, which on the Gamma is WarpAxe's
/// flipped and on the Color boards its own way up; 180 = the other way;
/// `detect::flip_for_axeos_rotation`). Screen-off: AxeOS `displayTimeout` (-1
/// always on, 0 off, else minutes), NerdOS `autoscreenoff` (0 = never).
/// WarpAxe's 0 means never off.
fn import_display(stock: &Stock, cfg: &mut Config) -> bool {
    let mut took = false;
    if let Some(f) = stock.num("flipscreen") {
        cfg.flipscreen = f != 0.0;
        took = true;
    } else if let Some(r) = stock.num("rotation") {
        let board = stock.str("boardversion");
        cfg.flipscreen = warpaxe_board::detect::flip_for_axeos_rotation(board.as_deref(), r);
        took = true;
    }
    if let Some(i) = stock.num("invertscreen") {
        cfg.invertscreen = i != 0.0;
        took = true;
    }
    if let Some(t) = stock.num("displayTimeout") {
        cfg.display_timeout = if t < 0.0 { 0 } else { (t as u32).max(1) };
        took = true;
    } else if stock.num("autoscreenoff") == Some(0.0) {
        cfg.display_timeout = 0;
        took = true;
    }
    took
}

/// "SV2" (any case) or the NerdOS number 1 means Stratum V2.
pub fn is_sv2(protocol: &str) -> bool {
    protocol.trim().eq_ignore_ascii_case("sv2") || protocol.trim() == "1"
}
