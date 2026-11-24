//! What a NerdMiner v2 leaves in the devkit's flash, and how WarpAxe
//! takes it over on its first start: the Wi-Fi from ESP-IDF's own Wi-Fi
//! storage and the wallet from NerdMiner's `/config.json`.
//!
//! Read from a full backup of the T-Display-S3 (2026-09-23), never
//! printing a value:
//!
//! * **Wi-Fi**: the Wi-Fi library's NVS namespace `nvs.net80211`, blobs
//!   `sta.ssid` (36 bytes: a `u32` length, then up to 32 bytes) and
//!   `sta.pswd` (65 bytes, a C string). NerdMiner's NVS partition sits at
//!   the same offset as WarpAxe's (0x9000) but is one page shorter
//!   (0x5000 vs 0x6000): WarpAxe's sixth page holds NerdMiner's old OTA
//!   data, whose first word fails the NVS page-header CRC, so ESP-IDF
//!   marks that page corrupt and erases *it*, nothing else (nvs_page.cpp
//!   `load`, nvs_pagemanager.cpp). The other five pages carry on.
//! * **Wallet and pool**: NerdMiner's SPIFFS partition (flash 0x310000,
//!   0xE0000 bytes) holds `/config.json`, one 256-byte SPIFFS page per
//!   version; the page header is `obj_id u16, span u16, flags u8`, the
//!   JSON starts at byte 5. Every rewrite leaves the old page behind
//!   with its "deleted" flag cleared (bit 7 = 0); the live one keeps
//!   it set. The backup had twelve copies, eleven deleted, with
//!   different wallets. After WarpAxe's first install the region lies
//!   partly in the tail of `ota_0` (0x20000 + 0x300000, the first 64 KB)
//!   and partly in the head of `ota_1`. The first install writes only
//!   `ota_0`, up to where the app ends (the release script keeps that
//!   below 0x310000), and the devkit takes no OTA, so the file stays;
//!   the import still runs on the first start only, with the profile.

use serde::Deserialize;

/// Flash address of NerdMiner's SPIFFS partition.
pub const SPIFFS_FLASH_ADDR: u32 = 0x31_0000;
/// Its size.
pub const SPIFFS_SIZE: u32 = 0xE_0000;
/// SPIFFS logical page size on the ESP32.
pub const SPIFFS_PAGE: usize = 256;
/// Bytes of page header before the data.
const PAGE_HEADER: usize = 5;
/// Page flag "deleted": set (1) while the page is valid, cleared when it
/// is deleted (SPIFFS clears bits, flash erases to ones).
const FLAG_DELET: u8 = 0x80;

/// NVS namespace of the ESP-IDF Wi-Fi library.
pub const WIFI_NAMESPACE: &str = "nvs.net80211";
/// Station SSID blob key.
pub const WIFI_SSID_KEY: &str = "sta.ssid";
/// Station password blob key.
pub const WIFI_PASSWORD_KEY: &str = "sta.pswd";

/// The pool every WarpAxe devkit mines to (2026-10-05): the
/// NerdMiner's own pool setting is not taken over.
pub const POOL_HOST: &str = "pool.warppool.org";
/// Its Stratum V1 port.
pub const POOL_PORT: u16 = 3333;
/// The worker name appended to the wallet.
pub const WORKER: &str = "WarpMiner";

/// NerdMiner's `/config.json`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct NerdConfig {
    /// `poolString`: the pool host (not taken over).
    #[serde(rename = "poolString")]
    pub pool: String,
    /// `portNumber`.
    #[serde(rename = "portNumber")]
    pub port: u16,
    /// `poolPassword`.
    #[serde(rename = "poolPassword")]
    pub pool_password: String,
    /// `btcString`: the wallet, possibly with a `.worker` suffix.
    #[serde(rename = "btcString")]
    pub btc: String,
    /// `gmtZone`: hours east of UTC.
    #[serde(rename = "gmtZone")]
    pub gmt_zone: i32,
}

/// The JSON that starts at `data` if it is a `/config.json` body.
fn parse_page(data: &[u8]) -> Option<NerdConfig> {
    if !data.starts_with(b"{\"poolString\"") {
        return None;
    }
    let end = data.iter().position(|&b| b == b'}')?;
    serde_json::from_slice(&data[..=end]).ok()
}

/// Scans the SPIFFS region for `/config.json` pages, any number of
/// chunks at a time; chunks must be multiples of the page size (a page
/// never straddles a 4 KB block, so 4 KB flash reads are fine).
#[derive(Debug, Default)]
pub struct ConfigScan {
    /// The last page seen with the valid flag set.
    live: Option<NerdConfig>,
    /// The last page seen at all, for a region whose flags are unreadable.
    last: Option<NerdConfig>,
    /// Pages that parsed.
    pub copies: u32,
}

impl ConfigScan {
    /// Look at one chunk of the region.
    pub fn feed(&mut self, chunk: &[u8]) {
        for page in chunk.chunks(SPIFFS_PAGE) {
            if page.len() <= PAGE_HEADER {
                continue;
            }
            let Some(cfg) = parse_page(&page[PAGE_HEADER..]) else {
                continue;
            };
            self.copies += 1;
            if page[4] & FLAG_DELET != 0 {
                self.live = Some(cfg.clone());
            }
            self.last = Some(cfg);
        }
    }

    /// The config to take: the live page, else the last copy in flash
    /// order (writes move upwards within a block).
    pub fn result(self) -> Option<NerdConfig> {
        self.live.or(self.last)
    }
}

/// The station SSID from the `sta.ssid` blob.
pub fn wifi_ssid(blob: &[u8]) -> Option<String> {
    if blob.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    if len == 0 || len > 32 || blob.len() < 4 + len {
        return None;
    }
    String::from_utf8(blob[4..4 + len].to_vec()).ok()
}

/// The station password from the `sta.pswd` blob (a C string; empty
/// for an open network).
pub fn wifi_password(blob: &[u8]) -> Option<String> {
    let end = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
    String::from_utf8(blob[..end].to_vec()).ok()
}

/// The worker name a NerdNOS gets instead of [`WORKER`]: the pool keys
/// its VarDiff and worker stats by the full name, and a 150 GH/s chip
/// under the devkit's name would inherit a sub-1 difficulty (Warppool
/// #2, 2026-10-08).
pub const WORKER_NERDNOS: &str = "NerdNOS";

/// The stratum user for the devkit: the wallet without any worker
/// suffix NerdMiner may have had, plus `.WarpMiner`. `None` without a
/// wallet.
pub fn stratum_user(btc: &str) -> Option<String> {
    stratum_user_as(btc, WORKER)
}

/// The same with a given worker name.
pub fn stratum_user_as(btc: &str, worker: &str) -> Option<String> {
    let wallet = btc.trim().split('.').next().unwrap_or("").trim();
    if wallet.is_empty() || !wallet.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!("{wallet}.{worker}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(flags: u8, json: &str) -> Vec<u8> {
        let mut p = vec![0xffu8; SPIFFS_PAGE];
        p[0] = 1;
        p[1] = 0;
        p[2] = 0;
        p[3] = 0;
        p[4] = flags;
        p[5..5 + json.len()].copy_from_slice(json.as_bytes());
        p
    }

    const OLD: &str = r#"{"poolString":"public-pool.io","portNumber":21496,"poolPassword":"x","btcString":"bc1qold","gmtZone":2,"saveStatsToNVS":true,"invertColors":false}"#;
    const NEW: &str = r#"{"poolString":"public-pool.io","portNumber":21496,"poolPassword":"x","btcString":"bc1qnew.devkit","gmtZone":2,"saveStatsToNVS":true,"invertColors":false,"newKey":1}"#;

    #[test]
    fn live_page_wins_over_deleted_copies() {
        let mut region = Vec::new();
        region.extend(page(0x7c, OLD)); // deleted (bit 7 clear), as in the backup
        region.extend(vec![0xffu8; SPIFFS_PAGE]); // empty page
        region.extend(page(0xfc, NEW)); // live
        region.extend(page(0x7c, OLD)); // a deleted copy after the live one
        let mut scan = ConfigScan::default();
        // Chunked like the firmware's flash reads.
        for chunk in region.chunks(2 * SPIFFS_PAGE) {
            scan.feed(chunk);
        }
        assert_eq!(scan.copies, 3);
        let cfg = scan.result().unwrap();
        assert_eq!(cfg.btc, "bc1qnew.devkit");
        assert_eq!(cfg.port, 21496);
        assert_eq!(cfg.gmt_zone, 2);
    }

    #[test]
    fn without_flags_the_last_copy_is_taken() {
        let mut region = page(0x7c, OLD);
        region.extend(page(0x7c, NEW));
        let mut scan = ConfigScan::default();
        scan.feed(&region);
        assert_eq!(scan.result().unwrap().btc, "bc1qnew.devkit");
        assert!(ConfigScan::default().result().is_none());
    }

    #[test]
    fn a_json_cut_by_the_page_end_is_skipped() {
        let mut p = page(0xfc, &NEW[..100]);
        p.truncate(SPIFFS_PAGE);
        let mut scan = ConfigScan::default();
        scan.feed(&p);
        assert_eq!(scan.copies, 0);
    }

    #[test]
    fn wifi_blobs() {
        let mut ssid = vec![0u8; 36];
        ssid[0] = 7;
        ssid[4..11].copy_from_slice(b"MyWlan7");
        assert_eq!(wifi_ssid(&ssid).as_deref(), Some("MyWlan7"));
        ssid[0] = 0;
        assert_eq!(wifi_ssid(&ssid), None);
        ssid[0] = 33;
        assert_eq!(wifi_ssid(&ssid), None);
        let mut pw = vec![0u8; 65];
        pw[..6].copy_from_slice(b"secret");
        assert_eq!(wifi_password(&pw).as_deref(), Some("secret"));
        assert_eq!(wifi_password(&[0u8; 65]).as_deref(), Some(""));
    }

    #[test]
    fn worker_name() {
        assert_eq!(
            stratum_user("bc1qabc").as_deref(),
            Some("bc1qabc.WarpMiner")
        );
        assert_eq!(
            stratum_user(" bc1qabc.nerd1 ").as_deref(),
            Some("bc1qabc.WarpMiner")
        );
        assert_eq!(
            stratum_user_as("bc1qabc.old", WORKER_NERDNOS).as_deref(),
            Some("bc1qabc.NerdNOS")
        );
        assert_eq!(stratum_user(""), None);
        assert_eq!(stratum_user(".worker"), None);
        assert_eq!(stratum_user("bc1q abc"), None);
    }

    /// Against the real backup when `NERDMINER_BACKUP` names it: the
    /// scan finds exactly one live copy among several, the Wi-Fi blobs
    /// decode. Prints nothing from the image.
    #[test]
    fn real_backup_if_present() {
        let Ok(path) = std::env::var("NERDMINER_BACKUP") else {
            return;
        };
        let image = std::fs::read(path).expect("backup readable");
        let start = SPIFFS_FLASH_ADDR as usize;
        let region = &image[start..start + SPIFFS_SIZE as usize];
        let mut scan = ConfigScan::default();
        for chunk in region.chunks(4096) {
            scan.feed(chunk);
        }
        assert!(scan.copies > 1);
        let live = scan.live.clone().expect("a live copy");
        assert_eq!(scan.result().as_ref(), Some(&live));
        assert!(stratum_user(&live.btc).is_some());
        assert!(live.port > 0);
    }
}
