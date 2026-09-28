//! Firmware updates from the project's Forgejo releases.
//!
//! The miner itself asks (at boot and every 12 hours, while `updateCheck`
//! is on) for the latest release; the web UI shows it. Installing is
//! always a click: the miner downloads the image, checks it against the
//! release's SHA-256 file and against the release signature, and writes
//! it with the same path as an uploaded update. A release carries three
//! assets: `warpaxe.bin` (one app image for every board; the board is
//! configuration), `warpaxe.bin.sha256` and `warpaxe.bin.sig` (BIP-340
//! Schnorr over the SHA-256, checked with [`RELEASE_KEY`]). A release
//! without a signature still installs for now, marked unsigned; that
//! tolerance goes once every published release carries one.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{config, diag, display};

/// The release feed (Forgejo API, no login: the repository must be public).
const LATEST: &str =
    "https://git.warppool.org/api/v1/repos/dvb-projekt/dvb-WarpAxe/releases/latest";
const ASSET: &str = "warpaxe.bin";
/// The release signing key (x-only secp256k1). The private half lives on
/// the maintainer's machine only; `tools/release-firmware.sh` signs.
const RELEASE_KEY: [u8; 32] = [
    0xec, 0x42, 0xcf, 0x9c, 0xe2, 0xe7, 0x36, 0x2e, 0x52, 0xab, 0xa1, 0x67, 0x5d, 0xc2, 0x70, 0x53,
    0x05, 0x8e, 0x31, 0x3c, 0x15, 0xbf, 0x4c, 0x30, 0xb5, 0x27, 0xd4, 0x50, 0xae, 0xf3, 0x27, 0x42,
];
const EVERY: Duration = Duration::from_secs(12 * 3600);
/// Largest image accepted (the app slot is 3 MiB).
const MAX_IMAGE: usize = 3 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub name: String,
    pub notes: String,
    pub published: String,
    #[serde(skip)]
    pub bin_url: String,
    #[serde(skip)]
    pub sha_url: String,
    #[serde(skip)]
    pub sig_url: Option<String>,
    /// The release carries a signature file.
    pub signed: bool,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct UpdateState {
    pub installed: String,
    pub latest: Option<Release>,
    pub available: bool,
    /// Unix time of the last check, if the clock was set.
    pub checked_unix: Option<u64>,
    pub checked_uptime_s: u64,
    pub error: String,
    /// idle, checking, downloading, verifying, flashing, failed
    pub phase: String,
    /// Download progress, percent.
    pub progress: u8,
    /// Outcome of the signature check of the last install: "" (none
    /// yet), "verified", "missing" (installed unsigned), "invalid".
    pub signature: String,
    /// What the running image was installed as by the online update and
    /// how its signature checked, e.g. "v0.1.3 verified"; empty for an
    /// image flashed by hand.
    pub last_install: String,
}

static STATE: Mutex<Option<UpdateState>> = Mutex::new(None);
/// 1 = check now, 2 = install now.
static REQUEST: AtomicU8 = AtomicU8::new(0);

pub fn state() -> UpdateState {
    STATE
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| UpdateState {
            installed: env!("CARGO_PKG_VERSION").into(),
            last_install: config::last_install(),
            phase: "idle".into(),
            ..Default::default()
        })
}

fn update(f: impl FnOnce(&mut UpdateState)) {
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(|| UpdateState {
        installed: env!("CARGO_PKG_VERSION").into(),
        last_install: config::last_install(),
        phase: "idle".into(),
        ..Default::default()
    });
    f(s);
}

pub fn request_check() {
    REQUEST.store(1, Ordering::Relaxed);
}

/// Start an install of the latest release; false if none is known.
pub fn request_install() -> bool {
    if state().latest.is_none() {
        return false;
    }
    REQUEST.store(2, Ordering::Relaxed);
    true
}

/// "v1.2.3" → (1, 2, 3, 1); "1.2.3-rc4" → (1, 2, 3, 0): a pre-release sorts
/// below its release, so a miner on a test build is offered the release
/// (SemVer precedence). Anything else sorts lowest.
fn semver(s: &str) -> (u32, u32, u32, u8) {
    let s = s.trim_start_matches('v');
    let pre = s.split('+').next().unwrap_or("").contains('-');
    let core = s.split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        u8::from(!pre),
    )
}

pub fn start() {
    let record = config::last_install();
    if !record.is_empty() {
        log::info!("update: running image installed as {record}");
    }
    // The watcher only waits and fetches: its stack can live in PSRAM.
    // The install runs on a thread of its own with an internal stack,
    // because writing flash turns the cache off.
    diag::spawn_psram("update", 10 * 1024, || {
        std::thread::sleep(Duration::from_secs(60));
        let mut last = None::<std::time::Instant>;
        loop {
            let want = REQUEST.swap(0, Ordering::Relaxed);
            let due = config::get().update_check && last.map_or(true, |t| t.elapsed() >= EVERY);
            if want == 1 || (want == 0 && due) {
                last = Some(std::time::Instant::now());
                check();
            }
            if want == 2 {
                install();
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    })
    .expect("update thread");
}

fn check() {
    update(|s| {
        s.phase = "checking".into();
        s.error.clear();
    });
    match fetch_latest() {
        Ok(r) => {
            let newer = semver(&r.version) > semver(env!("CARGO_PKG_VERSION"));
            log::info!(
                "update: latest {} ({})",
                r.tag,
                if newer { "newer" } else { "installed or older" }
            );
            update(|s| {
                s.available = newer;
                s.latest = Some(r);
                s.phase = "idle".into();
            });
        }
        Err(e) => {
            log::warn!("update: check failed: {e}");
            update(|s| {
                s.error = e.to_string();
                s.phase = "idle".into();
            });
        }
    }
    update(|s| {
        s.checked_uptime_s = diag::uptime_s();
        s.checked_unix = crate::clock::unix_now();
    });
}

fn fetch_latest() -> anyhow::Result<Release> {
    let body = get(LATEST, 64 * 1024, |_| {})?;
    let v: serde_json::Value = serde_json::from_slice(&body)?;
    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let find = |name: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .map(|a| {
                (
                    a["browser_download_url"].as_str().unwrap_or("").to_string(),
                    a["size"].as_u64().unwrap_or(0),
                )
            })
    };
    let (bin_url, size) =
        find(ASSET).ok_or_else(|| anyhow::anyhow!("release {tag} has no {ASSET}"))?;
    let (sha_url, _) = find(&format!("{ASSET}.sha256"))
        .ok_or_else(|| anyhow::anyhow!("release {tag} has no {ASSET}.sha256"))?;
    let sig_url = find(&format!("{ASSET}.sig")).map(|(u, _)| u);
    let notes: String = v["body"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(4000)
        .collect();
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        tag,
        name: v["name"].as_str().unwrap_or("").to_string(),
        notes,
        published: v["published_at"].as_str().unwrap_or("").to_string(),
        bin_url,
        sha_url,
        signed: sig_url.is_some(),
        sig_url,
        size,
    })
}

/// GET over HTTPS (built-in CA bundle), following redirects, at most
/// `max` bytes; `progress(bytes so far)` after every chunk.
/// [`get_once`] with up to three attempts, five seconds apart: a TLS
/// handshake can fail for want of memory at a bad moment (seen after an
/// hour of mining on TLS) or on a network hiccup.
fn get(url: &str, max: usize, mut progress: impl FnMut(usize)) -> anyhow::Result<Vec<u8>> {
    let mut last = None;
    for attempt in 1..=3 {
        match get_once(url, max, &mut progress) {
            Ok(b) => return Ok(b),
            Err(e) => {
                log::warn!("update: {url}: attempt {attempt} failed: {e}");
                last = Some(e);
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("{url}: no attempt made")))
}

fn get_once(url: &str, max: usize, progress: &mut impl FnMut(usize)) -> anyhow::Result<Vec<u8>> {
    let mut url = url.to_string();
    for _ in 0..5 {
        let mut c = EspHttpConnection::new(&Configuration {
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            timeout: Some(Duration::from_secs(20)),
            buffer_size: Some(2048),
            ..Default::default()
        })?;
        c.initiate_request(
            Method::Get,
            &url,
            &[("Accept", "application/json, application/octet-stream")],
        )?;
        c.initiate_response()?;
        let status = c.status();
        if (300..400).contains(&status) {
            let next = c
                .header("Location")
                .ok_or_else(|| anyhow::anyhow!("{url}: redirect without Location"))?
                .to_string();
            url = if next.starts_with("http") {
                next
            } else {
                // Same host, absolute path.
                let host_end = url
                    .find("://")
                    .map(|i| i + 3)
                    .and_then(|i| url[i..].find('/').map(|j| i + j));
                format!("{}{}", &url[..host_end.unwrap_or(url.len())], next)
            };
            continue;
        }
        if status == 404 {
            anyhow::bail!("no release found (repository private or no release yet)");
        }
        if status != 200 {
            anyhow::bail!("{url}: HTTP {status}");
        }
        let mut body = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            let n = c.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if body.len() + n > max {
                anyhow::bail!("{url}: larger than {max} bytes");
            }
            body.extend_from_slice(&buf[..n]);
            progress(body.len());
        }
        return Ok(body);
    }
    anyhow::bail!("{url}: too many redirects")
}

fn install() {
    let Some(r) = state().latest else { return };
    let fail = |e: String| {
        log::error!("update: {e}");
        update(|s| {
            s.phase = "failed".into();
            s.error = e;
        });
        display::OTA_STATE.store(0, Ordering::Relaxed);
    };
    update(|s| {
        s.phase = "downloading".into();
        s.progress = 0;
        s.error.clear();
    });
    log::info!("update: downloading {} ({} bytes)", r.tag, r.size);
    let sha = match get(&r.sha_url, 1024, |_| {}) {
        Ok(b) => String::from_utf8_lossy(&b)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase(),
        Err(e) => return fail(format!("checksum download: {e}")),
    };
    let size = r.size.max(1) as usize;
    let image = match get(&r.bin_url, MAX_IMAGE, |n| {
        let pct = (n * 100 / size).min(100) as u8;
        update(|s| s.progress = pct);
    }) {
        Ok(b) => b,
        Err(e) => return fail(format!("download: {e}")),
    };
    update(|s| s.phase = "verifying".into());
    let got: String = Sha256::digest(&image)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if sha.len() != 64 || got != sha {
        return fail(format!(
            "checksum mismatch: release says {sha}, download is {got}"
        ));
    }
    if image.first() != Some(&0xE9) {
        return fail("download is not an ESP app image".into());
    }
    // The signature covers the digest just recomputed, not the file's
    // claim of it.
    let digest = warpaxe_sv2::release::digest(&image);
    match &r.sig_url {
        Some(url) => {
            let sig = match get(url, 1024, |_| {}) {
                Ok(b) => String::from_utf8_lossy(&b).to_string(),
                Err(e) => return fail(format!("signature download: {e}")),
            };
            let Some(sig) = warpaxe_sv2::release::from_hex::<64>(&sig) else {
                update(|s| s.signature = "invalid".into());
                return fail("signature file is not 128 hex characters".into());
            };
            if !warpaxe_sv2::release::verify(&RELEASE_KEY, &digest, &sig) {
                update(|s| s.signature = "invalid".into());
                return fail(format!(
                    "signature of {} does not verify against the release key",
                    r.tag
                ));
            }
            update(|s| s.signature = "verified".into());
            log::info!("update: {} signature verified", r.tag);
        }
        None => {
            update(|s| s.signature = "missing".into());
            log::warn!(
                "update: {} carries no signature, installing unsigned",
                r.tag
            );
        }
    }
    log::info!("update: {} verified (sha256 {got}), flashing", r.tag);
    update(|s| s.phase = "flashing".into());
    // Survives the restart: the next boot reports what was installed
    // and how the signature checked.
    let record = format!("{} {}", r.tag, state().signature);
    let spawned = std::thread::Builder::new()
        .stack_size(16 * 1024)
        .spawn(move || match crate::api::flash_image(&image) {
            Ok(()) => {
                log::info!("update: written and verified, restarting");
                config::set_last_install(&record);
                std::thread::sleep(Duration::from_millis(500));
                diag::restart();
            }
            Err(e) => {
                display::OTA_STATE.store(0, Ordering::Relaxed);
                log::error!("update: flashing failed: {e}");
                update(|s| {
                    s.phase = "failed".into();
                    s.error = format!("flashing: {e}");
                });
            }
        });
    if let Err(e) = spawned {
        fail(format!(
            "cannot start the writer: {e} ({} bytes internal RAM free)",
            diag::free_heap_internal()
        ));
    }
}
