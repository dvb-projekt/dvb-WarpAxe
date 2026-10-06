//! The dashboard's history: one sample every 10 s, three hours kept
//! (NerdOS keeps a day at 5 s but serves at most three hours; the web UI
//! switches between 1 h and 3 h like its original).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use crate::{board, diag, miner};

/// Seconds between samples.
pub const INTERVAL_S: u64 = 10;
/// Samples kept: three hours.
const CAPACITY: usize = (3 * 3600 / INTERVAL_S) as usize;
/// Most points one reply carries; longer spans are averaged down.
const MAX_POINTS: usize = 360;

#[derive(Clone, Copy)]
struct Sample {
    uptime_s: u32,
    hashrate_now_ghs: f32,
    hashrate_10m_ghs: f32,
    temp_c: f32,
    vr_temp_c: f32,
    power_w: f32,
}

static SAMPLES: Mutex<Option<VecDeque<Sample>>> = Mutex::new(None);

/// Start the sampling thread.
pub fn start() {
    *SAMPLES.lock().unwrap() = Some(VecDeque::with_capacity(CAPACITY));
    diag::spawn_psram("history", 4096, || loop {
        std::thread::sleep(Duration::from_secs(INTERVAL_S));
        let m = miner::state();
        let b = board::state();
        let s = Sample {
            uptime_s: diag::uptime_s() as u32,
            hashrate_now_ghs: m.hashrate_now_ghs as f32,
            hashrate_10m_ghs: m.hashrate_ghs as f32,
            temp_c: b.temp_c.unwrap_or(f32::NAN),
            vr_temp_c: b.vr_temp_c.map_or(f32::NAN, |t| t as f32),
            power_w: b.power_w.unwrap_or(f32::NAN),
        };
        if let Some(q) = SAMPLES.lock().unwrap().as_mut() {
            if q.len() == CAPACITY {
                q.pop_front();
            }
            q.push_back(s);
        }
    })
    .expect("history thread");
}

/// The last `span_s` seconds, at most [`MAX_POINTS`] points (averaged
/// bins), as parallel arrays. Missing readings are `null`.
pub fn json(span_s: u64) -> serde_json::Value {
    let span_s = span_s.clamp(60, 3 * 3600);
    let now = diag::uptime_s();
    let from = now.saturating_sub(span_s) as u32;
    let picked: Vec<Sample> = SAMPLES
        .lock()
        .unwrap()
        .as_ref()
        .map(|q| q.iter().filter(|s| s.uptime_s >= from).copied().collect())
        .unwrap_or_default();
    let bin = picked.len().div_ceil(MAX_POINTS).max(1);
    let mean = |xs: &[Sample], f: fn(&Sample) -> f32| {
        let v: Vec<f32> = xs.iter().map(f).filter(|x| x.is_finite()).collect();
        if v.is_empty() {
            serde_json::Value::Null
        } else {
            // Two decimals of GH/s; the devkit (a few 1e-4 GH/s) would read
            // as 0.00 and draw a flat line, so it keeps seven.
            let scale = if board::devkit() {
                1e7
            } else {
                100.0
            };
            json!((v.iter().sum::<f32>() / v.len() as f32 * scale).round() / scale)
        }
    };
    let mut t = Vec::new();
    let (mut hn, mut h10, mut tc, mut tv, mut pw) = (vec![], vec![], vec![], vec![], vec![]);
    for c in picked.chunks(bin) {
        t.push(c[c.len() - 1].uptime_s);
        hn.push(mean(c, |s| s.hashrate_now_ghs));
        h10.push(mean(c, |s| s.hashrate_10m_ghs));
        tc.push(mean(c, |s| s.temp_c));
        tv.push(mean(c, |s| s.vr_temp_c));
        pw.push(mean(c, |s| s.power_w));
    }
    json!({
        "interval": INTERVAL_S * bin as u64, "uptime": now, "span": span_s,
        "unixNow": crate::clock::unix_now(),
        "t": t, "hashrate": hn, "hashrate10m": h10, "temp": tc, "vrTemp": tv, "power": pw,
    })
}

/// The last `n` samples of the short-window hashrate, GH/s, oldest first
/// (the display's sparkline).
pub fn recent_ghs(n: usize) -> Vec<f32> {
    SAMPLES
        .lock()
        .unwrap()
        .as_ref()
        .map(|q| {
            q.iter()
                .skip(q.len().saturating_sub(n))
                .map(|s| s.hashrate_now_ghs)
                .collect()
        })
        .unwrap_or_default()
}
