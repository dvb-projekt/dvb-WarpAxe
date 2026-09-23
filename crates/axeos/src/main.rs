//! `warpaxe-axeos`: an AxeOS-compatible miner that is not there.
//!
//! ```text
//! warpaxe-axeos [--port 8080] [--seed 1] [--ambient 23] [--time-scale 1]
//!               [--pool-diff 256] [--hostname warpaxe-sim] [--mac 02:57:41:52:50:00]
//! ```
//!
//! Point a tuner at `http://localhost:<port>` and it sees a Bitaxe Gamma:
//! `/api/system/info`, `/api/system/asic`, `PATCH /api/system`,
//! `POST /api/system/restart`. Then break it over `/sim/fault`.
//!
//! Simulated time runs at `time-scale` × wall-clock. At 1 the device
//! behaves like hardware; at 60 an hour of thermal settling takes a
//! minute. A tuner's own timers are unaffected — only the device gets
//! faster.

#![forbid(unsafe_code)]

use warpaxe_axeos::{device, server};

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use warpaxe_core::chip::bm1370::BM1370;
use warpaxe_sim::BoardSpec;

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let port: u16 = arg("--port", 8080);
    let seed: u64 = arg("--seed", 1);
    let ambient: f64 = arg("--ambient", 23.0);
    let scale: f64 = arg("--time-scale", 1.0);
    let pool_diff: u32 = arg("--pool-diff", 256);
    let hostname: String = arg("--hostname", "warpaxe-sim".to_string());
    let mac: String = arg("--mac", "02:57:41:52:50:00".to_string());

    let mut device = device::Device::new(&BM1370, BoardSpec::gamma(), ambient, seed);
    device.set_pool_difficulty(pool_diff);
    device.set_identity(device::Identity {
        hostname: hostname.clone(),
        mac,
        stratum_user: format!("bc1qsimulator.{hostname}"),
        ..device::Identity::default()
    });
    let dev = Arc::new(Mutex::new(device));

    // Wall-clock ticker.
    let ticker = Arc::clone(&dev);
    std::thread::spawn(move || {
        let mut last = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let now = Instant::now();
            let dt_ms = ((now - last).as_secs_f64() * 1000.0 * scale) as u64;
            last = now;
            ticker.lock().unwrap().tick(dt_ms);
        }
    });

    let server = tiny_http::Server::http(("0.0.0.0", port)).expect("bind");
    eprintln!("warpaxe-axeos: Bitaxe Gamma (BM1370) \"{hostname}\" on http://0.0.0.0:{port}  seed={seed} ambient={ambient} time-scale={scale} pool-diff={pool_diff}");
    server::serve(&server, &dev);
}
