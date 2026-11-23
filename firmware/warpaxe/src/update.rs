//! Firmware updates from the project's Forgejo releases.
//!
//! The miner itself asks (at boot and every hour, while `updateCheck`
//! is on) for the latest release; the web UI shows it. Installing is
//! always a click: the miner downloads the image, checks it against the
//! release's SHA-256 file and against the release signature, and writes
//! it with the same path as an uploaded update. A release carries three
//! assets: `warpaxe.bin` (one app image for every board; the board is
//! configuration), `warpaxe.bin.sha256` and `warpaxe.bin.sig` (BIP-340
//! Schnorr over the SHA-256, checked with [`RELEASE_KEY`]). A release
//! without a signature still installs for now, marked unsigned; that
//! tolerance goes once every published release carries one.
//!
//! The T-Display-S3 (the devkit, experimental) updates the same way but
//! is offered only a release that ran on it: the release's fourth asset
//! `tested-on.txt` lists the profile ids the image was soaked on, one
//! per line (`tools/release-firmware.sh` writes it, the devkit's line
//! only after a soak on a devkit in the same run). Without the file, or
//! without its profile in it, the devkit shows the release as a notice
//! and installs nothing. Boards with chips are offered every release.
//!
//! Writing the image to flash is the main task's job ([`serve_writes`]):
//! its stack sits in internal RAM from boot, so a write never waits for
//! a 16 KB piece of a fragmented heap (the v0.1.31 writer thread did, and
//! a miner up for hours could not update until restarted; docs/09). The
//! online update and the upload (`POST /api/system/OTA`) hand their image
//! over with [`submit`] and are told at once when another write is
//! under way.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use warpaxe_diag::restart::{Marker, Step};

use esp_idf_svc::http::client::Configuration;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{config, diag, display};

/// The release feed (Forgejo API, no login: the repository must be public).
const LATEST: &str =
    "https://git.warppool.org/api/v1/repos/dvb-projekt/dvb-WarpAxe/releases/latest";
const ASSET: &str = "warpaxe.bin";
/// The boards a release ran on, one profile id per line; only the devkit
/// reads it (see the module doc).
const TESTED_ASSET: &str = "tested-on.txt";
/// The release signing key (x-only secp256k1). The private half lives on
/// the maintainer's machine only; `tools/release-firmware.sh` signs.
const RELEASE_KEY: [u8; 32] = [
    0xec, 0x42, 0xcf, 0x9c, 0xe2, 0xe7, 0x36, 0x2e, 0x52, 0xab, 0xa1, 0x67, 0x5d, 0xc2, 0x70, 0x53,
    0x05, 0x8e, 0x31, 0x3c, 0x15, 0xbf, 0x4c, 0x30, 0xb5, 0x27, 0xd4, 0x50, 0xae, 0xf3, 0x27, 0x42,
];
const EVERY: Duration = Duration::from_secs(3600);
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
    #[serde(skip)]
    pub tested_url: Option<String>,
    /// The release carries a signature file.
    pub signed: bool,
    pub size: u64,
    /// The release may be installed on this board: always on a board
    /// with chips; on the devkit only when the release's `tested-on.txt`
    /// names its profile. `available` stays false without it.
    pub tested_here: bool,
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
    /// The install record as stored in flash, also when it belongs to an
    /// older image (tools/soak-test.py wants a miner that has one).
    pub install_record: String,
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
            install_record: config::install_record(),
            phase: "idle".into(),
            ..Default::default()
        })
}

fn update(f: impl FnOnce(&mut UpdateState)) {
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(|| UpdateState {
        installed: env!("CARGO_PKG_VERSION").into(),
        last_install: config::last_install(),
        install_record: config::install_record(),
        phase: "idle".into(),
        ..Default::default()
    });
    f(s);
}

pub fn request_check() {
    REQUEST.store(1, Ordering::Relaxed);
}

/// Start an install of the latest release. Refused while one is queued
/// or under way (a second click would otherwise queue a retry that runs
/// as soon as a failed first attempt ends), if no release is known, or
/// if the release did not run on this board (the devkit, see the module
/// doc): the watcher decides that at the check, this is the second lock.
pub fn request_install() -> Result<(), &'static str> {
    let s = state();
    if REQUEST.load(Ordering::Relaxed) == 2
        || ["downloading", "verifying", "flashing"].contains(&s.phase.as_str())
        || writing()
    {
        return Err("an update is already running");
    }
    let Some(r) = s.latest else {
        return Err("no release known yet: check first");
    };
    if !r.tested_here {
        log::warn!(
            "update: {} refused, its {TESTED_ASSET} does not name the {}",
            r.tag,
            crate::board::profile().name
        );
        return Err("this release was not tested on this board");
    }
    REQUEST.store(2, Ordering::Relaxed);
    Ok(())
}

/// Whether `tested-on.txt` (one profile id per line, `#` starts a
/// comment) names `profile`.
fn names_profile(text: &str, profile: &str) -> bool {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .any(|l| l == profile)
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
    } else if !config::install_record().is_empty() {
        log::info!(
            "update: running image flashed by hand (last online install: {})",
            config::install_record()
        );
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
        Ok(mut r) => {
            let newer = semver(&r.version) > semver(env!("CARGO_PKG_VERSION"));
            if crate::board::devkit() {
                r.tested_here = tested_here(&r);
            }
            log::info!(
                "update: latest {} ({}{})",
                r.tag,
                if newer { "newer" } else { "installed or older" },
                if r.tested_here {
                    ""
                } else {
                    ", not released for this board"
                }
            );
            update(|s| {
                s.available = newer && r.tested_here;
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

/// The devkit's gate: the release's `tested-on.txt` names this board's
/// profile. A release without the file, or one that cannot be read,
/// is not offered (and says why in the log).
fn tested_here(r: &Release) -> bool {
    let profile = crate::board::profile().name;
    let Some(url) = &r.tested_url else {
        log::info!(
            "update: {} carries no {TESTED_ASSET}: not offered on the {profile}",
            r.tag
        );
        return false;
    };
    match get(url, 4 * 1024, |_| {}) {
        Ok(b) => {
            let yes = names_profile(&String::from_utf8_lossy(&b), profile);
            if !yes {
                log::info!(
                    "update: {} did not run on the {profile}: not offered",
                    r.tag
                );
            }
            yes
        }
        Err(e) => {
            log::warn!(
                "update: {} {TESTED_ASSET}: {e}: not offered on the {profile}",
                r.tag
            );
            false
        }
    }
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
    let tested_url = find(TESTED_ASSET).map(|(u, _)| u);
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
        tested_url,
        size,
        // The devkit decides at the check (tested_here); every other
        // board takes every release.
        tested_here: true,
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
        // Over IPv6 when the pool's rule allows it (docs/17).
        let mut c = crate::net::http::get(
            &url,
            &Configuration {
                crt_bundle_attach: Some(crate::tls_trust::attach),
                timeout: Some(Duration::from_secs(20)),
                buffer_size: Some(2048),
                ..Default::default()
            },
            &[("Accept", "application/json, application/octet-stream")],
        )?;
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
    if let Err(why) = submit(Job {
        image,
        origin: "update",
        record: Some(record),
        delay: Duration::ZERO,
    }) {
        fail(format!("cannot write: {why}"));
    }
}

/// An image for the writer: where it came from (for the log), the
/// install record to store once it is written (the online update's; an
/// upload leaves none), and a pause before the write starts (the upload
/// answers its request first).
pub struct Job {
    pub image: Vec<u8>,
    pub origin: &'static str,
    pub record: Option<String>,
    pub delay: Duration,
}

/// What the main task does for others: write an image, or restart
/// (into the app or into ROM download mode) with the chips parked
/// first. The restarts used to spawn a thread with an 8 KB internal
/// stack; on the fragmented heap that stops an update, `std::thread::spawn`
/// panics instead, and the panic resets the board without
/// `prepare_reset` (the chips stay powered through it; docs/04).
pub enum Work {
    Write(Job),
    /// `POST /api/system/restart`, with the reason for the log.
    Restart(&'static str),
    /// `/bootloader`: ROM download mode, the PHY on USB-Serial/JTAG.
    Bootloader,
}

/// The hand-off to the main task (`warpaxe_ota::Handoff`, host-tested)
/// and the bell that wakes it.
static WRITER: Mutex<warpaxe_ota::Handoff<Work>> = Mutex::new(warpaxe_ota::Handoff::new());
static WRITER_BELL: Condvar = Condvar::new();

/// Queue an image for the main task to write. Refused, with the reason
/// for the user, while another job is queued or under way; the image
/// is dropped then, and the caller says so.
pub fn submit(job: Job) -> Result<(), &'static str> {
    let (origin, bytes) = (job.origin, job.image.len());
    let mut w = WRITER.lock().unwrap();
    match w.offer(Work::Write(job)) {
        Ok(()) => {
            log::info!(
                "{origin}: {bytes} bytes handed to the writer ({})",
                diag::heap_line()
            );
            WRITER_BELL.notify_all();
            Ok(())
        }
        Err((why, _)) => {
            log::warn!("{origin}: refused, {}", why.reason());
            Err(why.reason())
        }
    }
}

/// Queue a restart (or the way into download mode) for the main task.
/// While a write is queued or under way the request is dropped: that
/// write ends in a restart of its own, and must not be cut short.
pub fn submit_restart(work: Work) {
    let mut w = WRITER.lock().unwrap();
    match w.offer(work) {
        Ok(()) => WRITER_BELL.notify_all(),
        Err((why, _)) => log::warn!("restart: not now, {}", why.reason()),
    }
}

/// A job (a write or a restart) is queued or under way.
pub fn writing() -> bool {
    WRITER.lock().unwrap().phase() != warpaxe_ota::Phase::Idle
}

/// The writer: runs on the main task once boot is done and never
/// returns. The main task's stack (CONFIG_ESP_MAIN_TASK_STACK_SIZE, in
/// internal RAM, allocated before anything else) is what writing flash
/// needs, and it exists whatever the heap looks like after hours of
/// mining. A written image ends in a restart; a failed write reports
/// and the task waits for the next job.
/// The priority a firmware write runs at: ESP-IDF's pthread default,
/// what the writer thread had up to v0.1.31.
const WRITER_PRIORITY: u32 = 5;

pub fn serve_writes() -> ! {
    loop {
        let work = {
            let mut w = WRITER.lock().unwrap();
            loop {
                if let Some(work) = w.take() {
                    break work;
                }
                w = WRITER_BELL.wait(w).unwrap();
            }
        };
        let job = match work {
            Work::Write(job) => job,
            Work::Restart(why) => {
                // The answer goes out first.
                std::thread::sleep(Duration::from_millis(300));
                restart_now(why);
            }
            Work::Bootloader => {
                std::thread::sleep(Duration::from_millis(300));
                // Nothing runs in ROM download mode: the chips must not
                // sit powered at their last clock meanwhile.
                let marker = note_restart("download mode");
                crate::board::prepare_reset("download mode");
                config::note_restart(&marker.at(Step::Reset));
                diag::reboot_to_bootloader();
            }
        };
        let Job {
            image,
            origin,
            record,
            delay,
        } = job;
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        log::info!("{origin}: writing {} bytes on the main task", image.len());
        // The main task runs at priority 1 on CPU 0, below the mining and
        // network threads: a write took ~100 s on a NerdQAxe++ instead of
        // the ~35 s of the old writer thread at the pthread default of 5
        // (2026-10-20). The write gets that priority back for its length;
        // the task waits for the next job at its own again.
        let written = {
            let _boost = diag::PriorityBoost::raise(WRITER_PRIORITY);
            let t0 = std::time::Instant::now();
            let r = crate::api::flash_image(&image);
            log::info!("{origin}: write took {} s", t0.elapsed().as_secs());
            r
        };
        match written {
            Ok(()) => {
                log::info!(
                    "{origin}: written and verified, restarting ({} bytes of the writer's stack never used)",
                    diag::stack_unused()
                );
                drop(image);
                if let Some(record) = &record {
                    config::set_last_install(record);
                }
                std::thread::sleep(Duration::from_millis(500));
                restart_now(origin);
            }
            Err(e) => {
                drop(image);
                display::OTA_STATE.store(0, Ordering::Relaxed);
                log::error!("{origin}: flashing failed: {e}");
                if origin == "update" {
                    update(|s| {
                        s.phase = "failed".into();
                        s.error = format!("flashing: {e}");
                    });
                }
                WRITER.lock().unwrap().done();
            }
        }
    }
}

/// The restart marker for `why`, stored before the chips go off
/// (`warpaxe_diag::restart`, docs/12): the next boot reports a restart
/// that ended in a crash, and an installed image on any bootloader.
fn note_restart(why: &str) -> Marker {
    let m = Marker::new(why, crate::api::VERSION, diag::uptime_s());
    config::note_restart(&m);
    m
}

/// The intentional restart, on the main task: the marker, the chips
/// off (`board::prepare_reset`), the marker again at its second step,
/// the reset. Nothing on this path can panic (docs/12): the NVS writes
/// log their errors, the locks cannot be poisoned (the firmware aborts
/// on a panic, so no thread ever unwinds while holding one), and the
/// I²C errors of the rail switch-off are logged.
fn restart_now(why: &str) -> ! {
    let marker = note_restart(why);
    crate::board::prepare_reset(why);
    config::note_restart(&marker.at(Step::Reset));
    diag::restart()
}

static MARKED_VALID: AtomicBool = AtomicBool::new(false);

/// The running app has proven itself: tell the bootloader so, so it keeps
/// booting this image. With CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE an OTA'd
/// image boots once as "pending verify"; a reset before this call (a
/// crash, a boot loop) makes the bootloader fall back to the previous app
/// — the difference between a cased NerdNOS that heals itself and one
/// that needs its case opened for a USB cable. Called at chain ready and,
/// as a fallback for boards without a chain, after three minutes. A no-op
/// when the image is already valid or the bootloader predates rollback.
pub fn mark_app_valid(why: &str) {
    if MARKED_VALID.swap(true, Ordering::Relaxed) {
        return;
    }
    let rc = unsafe { esp_idf_svc::sys::esp_ota_mark_app_valid_cancel_rollback() };
    if rc == esp_idf_svc::sys::ESP_OK {
        if *OTA_STATE.lock().unwrap() == "pending-verify" {
            *OTA_STATE.lock().unwrap() = "valid";
        }
        log::info!("update: app marked valid ({why}); no rollback to the previous image");
    } else {
        log::warn!("update: mark app valid ({why}) returned {rc}");
    }
}

/// The bootloader's view of the running image, read ONCE at boot on the
/// main task and kept: esp_ota_get_last_invalid_partition and
/// esp_ota_get_state_partition read the otadata partition from flash,
/// and a flash read from a thread whose stack sits in PSRAM (the HTTP
/// server's) hangs it — the devkit's API went silent 35 s after a
/// rollback on 2026-10-08 while the device still answered pings.
/// "aborted" when the last update never proved itself and the bootloader
/// came back to this image (the aborted mark sits on the other slot);
/// otherwise the running image's own state; "valid" once marked.
static OTA_STATE: Mutex<&'static str> = Mutex::new("unknown");
/// The running app slot's label ("ota_0"), read with the state above.
static RUNNING: Mutex<String> = Mutex::new(String::new());

pub fn init_ota_state() {
    use esp_idf_svc::sys;
    let running = unsafe { sys::esp_ota_get_running_partition() };
    if !running.is_null() {
        let label = unsafe { core::ffi::CStr::from_ptr((*running).label.as_ptr()) };
        *RUNNING.lock().unwrap() = label.to_string_lossy().into_owned();
    }
    let state = if !unsafe { sys::esp_ota_get_last_invalid_partition() }.is_null() {
        "aborted"
    } else {
        let part = unsafe { sys::esp_ota_get_running_partition() };
        let mut st: sys::esp_ota_img_states_t = 0;
        if part.is_null()
            || unsafe { sys::esp_ota_get_state_partition(part, &mut st) } != sys::ESP_OK
        {
            "unknown"
        } else {
            match st {
                sys::esp_ota_img_states_t_ESP_OTA_IMG_NEW => "new",
                sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY => "pending-verify",
                sys::esp_ota_img_states_t_ESP_OTA_IMG_VALID => "valid",
                sys::esp_ota_img_states_t_ESP_OTA_IMG_INVALID => "invalid",
                sys::esp_ota_img_states_t_ESP_OTA_IMG_ABORTED => "aborted",
                _ => "undefined",
            }
        }
    };
    *OTA_STATE.lock().unwrap() = state;
    log::info!("update: image state at boot: {state}");
}

pub fn ota_state() -> &'static str {
    *OTA_STATE.lock().unwrap()
}

/// The running app slot ("ota_0" / "ota_1"), from the same boot-time read;
/// "?" only if the OTA API named none. The HTTP handler used to ask the
/// OTA API on every request while the otadata was erased.
pub fn running_partition() -> String {
    let r = RUNNING.lock().unwrap();
    if r.is_empty() {
        "?".into()
    } else {
        r.clone()
    }
}
