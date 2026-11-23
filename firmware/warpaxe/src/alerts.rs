//! Discord notifications (docs/22): a short message to the webhook the
//! owner entered when a block is found, on a new all-time best
//! difficulty, on a switch to or from the fallback pool, after a
//! restart that a crash caused and when a solo pool stops paying the
//! miner's address (docs/23). The rules (which URLs, the texts,
//! coalescing, the one retry) are `warpaxe_alerts`; this module queues
//! and sends.
//!
//! Who calls what, and why it never holds anyone up: the mining loops
//! ([`best_diff`], [`block`]), the stratum thread (pool switches, through
//! [`on_event`] from `events::record`), the boot ([`crash`]) and the
//! test button's handler ([`test`]) only put a small value into RAM
//! behind a mutex: no config read, no formatting, no I/O. One thread
//! ([`start`]) with its stack in PSRAM sends, over HTTPS against the
//! pinned CA bundle (`tls_trust::attach`; Discord's chain ends at GTS
//! Root R4, which the bundle has), one message at a time with a 10 s
//! timeout, at most one retry. It never writes flash: the last result
//! lives in RAM only (`discordLastResult` in `/api/system/info`). The
//! webhook's URL holds a token and never reaches the log.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use warpaxe_alerts::pace::{self, Coalesce, LastResult, Next};
use warpaxe_alerts::{message, webhook, Alert, Kind};

use crate::{config, diag};

/// Blocks, crashes and tests waiting; more are dropped (with a log line).
const QUEUE_MAX: usize = 8;
/// The sender's stack (PSRAM): the TLS handshake runs on it.
const STACK: usize = 10 * 1024;
/// Seconds the network has to be up before the first send (DHCP, DNS).
const SETTLE_S: u64 = 5;

static QUEUE: Mutex<VecDeque<Alert>> = Mutex::new(VecDeque::new());
static BEST: Mutex<Coalesce<f64>> = Mutex::new(Coalesce::new(pace::BEST_GAP_S));
static POOL: Mutex<Coalesce<Alert>> = Mutex::new(Coalesce::new(pace::POOL_GAP_S));
static LAST: Mutex<Option<LastResult>> = Mutex::new(None);
/// Uptime of the last test message queued.
static LAST_TEST: Mutex<Option<u64>> = Mutex::new(None);

fn push(a: Alert) -> bool {
    let mut q = QUEUE.lock().unwrap();
    if q.len() >= QUEUE_MAX {
        drop(q);
        log::warn!("alerts: queue full, {} message dropped", a.kind().name());
        return false;
    }
    q.push_back(a);
    true
}

/// A new all-time best difficulty (`config::raise_best_diff`).
pub fn best_diff(diff: f64) {
    BEST.lock().unwrap().offer(diff);
}

/// A block found and counted (`miner::block_found`): its height, the
/// hash's difficulty and the coin.
pub fn block(height: u64, diff: f64, coin: String) {
    push(Alert::Block { height, diff, coin });
}

/// The last run ended in a crash or a watchdog reset (from main at
/// boot): `reason` is the reset reason.
pub fn crash(reason: &str) {
    push(Alert::Crash {
        reason: reason.into(),
    });
}

/// An event as `events::record` notes it: a switch between the main and
/// the fallback pool becomes a message (folded to one a minute); a pool
/// found keeping the payout (docs/23) one of its own, never folded.
pub fn on_event(detail: &warpaxe_diag::events::Detail) {
    if let Some(a) = message::from_event(detail.code(), |k| detail.number(k)) {
        if a.kind() == Kind::Payout {
            push(a);
        } else {
            POOL.lock().unwrap().offer(a);
        }
    }
}

/// The test button: queued when a webhook is set and the last test is
/// more than a few seconds old.
pub fn test() -> Result<(), &'static str> {
    if config::get().discord_webhook.is_empty() {
        return Err("no Discord webhook set");
    }
    let now = diag::uptime_s();
    {
        let mut last = LAST_TEST.lock().unwrap();
        if last.is_some_and(|t| now < t + pace::TEST_GAP_S) {
            return Err("a test message was queued a moment ago");
        }
        *last = Some(now);
    }
    if push(Alert::Test) {
        Ok(())
    } else {
        Err("too many messages waiting")
    }
}

/// The last send's result, for `/api/system/info`.
pub fn last_result() -> Option<LastResult> {
    LAST.lock().unwrap().clone()
}

fn waiting() -> bool {
    !QUEUE.lock().unwrap().is_empty()
        || BEST.lock().unwrap().waiting()
        || POOL.lock().unwrap().waiting()
}

fn drop_all() {
    QUEUE.lock().unwrap().clear();
    BEST.lock().unwrap().clear();
    POOL.lock().unwrap().clear();
}

/// The next message due now, with the count it folded; what waits
/// behind a switch that is off is dropped.
fn next_due(cfg: &config::Config, now: u64) -> Option<(Alert, u32)> {
    if !cfg.alert_best {
        BEST.lock().unwrap().clear();
    }
    if !cfg.alert_pool {
        POOL.lock().unwrap().clear();
    }
    {
        let mut q = QUEUE.lock().unwrap();
        while let Some(a) = q.pop_front() {
            let on = match a.kind() {
                Kind::Test => true,
                Kind::Block => cfg.alert_block,
                Kind::Crash => cfg.alert_crash,
                Kind::BestDiff => cfg.alert_best,
                Kind::Pool => cfg.alert_pool,
                Kind::Payout => cfg.alert_payout,
            };
            if on {
                return Some((a, 0));
            }
        }
    }
    if let Some(p) = POOL.lock().unwrap().take_due(now) {
        return Some(p);
    }
    BEST.lock()
        .unwrap()
        .take_due(now)
        .map(|(diff, n)| (Alert::BestDiff { diff }, n))
}

/// Start the sender. Nothing is sent before something happens.
pub fn start() {
    let spawned = diag::spawn_psram("alerts", STACK, || {
        let mut online_since: Option<u64> = None;
        let mut quiet_until = 0u64;
        loop {
            std::thread::sleep(Duration::from_secs(pace::SPACING_S));
            if !waiting() {
                continue;
            }
            let now = diag::uptime_s();
            let n = crate::net::state();
            if !(n.wifi_connected || n.eth_connected) {
                online_since = None;
                continue;
            }
            let since = *online_since.get_or_insert(now);
            if now < since + SETTLE_S || now < quiet_until {
                continue;
            }
            let cfg = config::get();
            if cfg.discord_webhook.is_empty() {
                drop_all();
                continue;
            }
            let Some((alert, skipped)) = next_due(&cfg, now) else {
                continue;
            };
            let network = crate::stratum::state().network_difficulty;
            let text = alert.text(&cfg.hostname, network, skipped);
            quiet_until = send(&cfg.discord_webhook, alert.kind(), &text);
        }
    });
    if let Err(e) = spawned {
        log::error!("alerts: thread not started: {e}");
    }
}

/// Send `text`, with the one retry; returns the uptime before which
/// nothing else may go out (Discord's `retry_after`).
fn send(url: &str, kind: Kind, text: &str) -> u64 {
    let body = message::body(text);
    let mut attempt = 1;
    loop {
        let (status, retry_after, error) = match post(url, &body) {
            Ok((s, r)) => (Some(s), r, None),
            Err(e) => (None, None, Some(webhook::scrub(&e, url))),
        };
        let now = diag::uptime_s();
        *LAST.lock().unwrap() = Some(LastResult::new(
            kind.name(),
            status,
            error.clone(),
            crate::clock::unix_now().unwrap_or(0),
            now,
        ));
        match pace::after(status, attempt, retry_after) {
            Next::Done => {
                log::info!("alerts: {} message sent to Discord", kind.name());
                return 0;
            }
            Next::Retry(s) => {
                log::warn!(
                    "alerts: {} message not delivered ({}), once more in {s} s",
                    kind.name(),
                    error.unwrap_or_else(|| format!("HTTP {}", status.unwrap_or(0)))
                );
                std::thread::sleep(Duration::from_secs(s));
                attempt += 1;
            }
            Next::Drop { pause_s } => {
                log::warn!(
                    "alerts: {} message dropped ({})",
                    kind.name(),
                    error.unwrap_or_else(|| format!("HTTP {}", status.unwrap_or(0)))
                );
                return now + pause_s;
            }
        }
    }
}

/// One POST: the status and, on a 429, Discord's `retry_after`. The
/// error text is the transport's (never the URL: the caller scrubs it
/// all the same).
fn post(url: &str, body: &str) -> Result<(u16, Option<f64>), String> {
    let mut c = EspHttpConnection::new(&Configuration {
        crt_bundle_attach: Some(crate::tls_trust::attach),
        timeout: Some(Duration::from_secs(10)),
        // Cloudflare's answer carries long headers (cookies, reporting).
        buffer_size: Some(2048),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let len = body.len().to_string();
    c.initiate_request(
        Method::Post,
        url,
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", &len),
            ("User-Agent", "WarpAxe"),
        ],
    )
    .map_err(|e| e.to_string())?;
    c.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    c.initiate_response().map_err(|e| e.to_string())?;
    let status = c.status();
    if status != 429 {
        return Ok((status, None));
    }
    let header = c.header("Retry-After").map(str::to_string);
    let mut answer = Vec::new();
    let mut buf = [0u8; 256];
    while answer.len() < 1024 {
        match c.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => answer.extend_from_slice(&buf[..n]),
        }
    }
    Ok((status, pace::retry_after(header.as_deref(), &answer)))
}
