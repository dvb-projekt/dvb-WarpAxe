//! Firmware updates from the project's Forgejo releases.
//!
//! The miner itself asks (at boot and every 12 hours, while `updateCheck`
//! is on) for the latest release; the web UI shows it. Installing is
//! always a click: the miner downloads the image, checks it against the
//! release's SHA-256 file and writes it with the same path as an uploaded
//! update. A release carries two assets: `warpaxe.bin` (one app image for
//! every board; the board is configuration) and `warpaxe.bin.sha256`.

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
            phase: "idle".into(),
            ..Default::default()
        })
}

fn update(f: impl FnOnce(&mut UpdateState)) {
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(|| UpdateState {
        installed: env!("CARGO_PKG_VERSION").into(),
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

/// "v1.2.3" / "1.2.3-beta" → (1, 2, 3); anything else sorts lowest.
fn semver(s: &str) -> (u32, u32, u32) {
    let core = s
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    let mut it = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

pub fn start() {
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
        size,
    })
}

/// GET over HTTPS (built-in CA bundle), following redirects, at most
/// `max` bytes; `progress(bytes so far)` after every chunk.
fn get(url: &str, max: usize, mut progress: impl FnMut(usize)) -> anyhow::Result<Vec<u8>> {
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
    log::info!("update: {} verified (sha256 {got}), flashing", r.tag);
    update(|s| s.phase = "flashing".into());
    let spawned = std::thread::Builder::new()
        .stack_size(16 * 1024)
        .spawn(move || match crate::api::flash_image(&image) {
            Ok(()) => {
                log::info!("update: written and verified, restarting");
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
