//! `warpaxe-cgminer`: an Avalon Q that is not there.
//!
//! ```text
//! warpaxe-cgminer [--port 4028] [--mode 0] [--ambient 23] [--time-scale 1] [--seed 1]
//!                 [--reinit 120] [--collapse-in-mode 2] [--refuse-workmode]
//! ```
//!
//! Speaks the cgminer API: `estats`, `summary`, `pools`, `devs`,
//! `version`, `ascset|0,workmode,set,n` / `get`. Point a tuner's Avalon path at it.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use warpaxe_sim::{AvalonFault, AvalonQ};

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn main() {
    let port: u16 = arg("--port", 4028);
    let mode: u8 = arg("--mode", 0);
    let ambient: f64 = arg("--ambient", 23.0);
    let scale: f64 = arg("--time-scale", 1.0);
    let seed: u32 = arg("--seed", 1);
    let reinit: u64 = arg("--reinit", 120);
    let collapse: i32 = arg("--collapse-in-mode", -1);

    let mut q = AvalonQ::settled(mode, ambient, seed);
    q.reinit_s = reinit;
    if collapse >= 0 {
        q.inject(AvalonFault::CollapseInMode {
            mode: collapse as u8,
        });
    }
    if flag("--refuse-workmode") {
        q.inject(AvalonFault::RefuseWorkmode);
    }
    let dev = Arc::new(Mutex::new(q));

    let ticker = Arc::clone(&dev);
    std::thread::spawn(move || {
        let mut last = Instant::now();
        let mut carry = 0.0f64;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = Instant::now();
            carry += (now - last).as_secs_f64() * scale;
            last = now;
            let whole = carry.floor() as u64;
            if whole > 0 {
                carry -= whole as f64;
                ticker.lock().unwrap().tick(whole);
            }
        }
    });

    let addr = warpaxe_cgminer::spawn(&format!("0.0.0.0:{port}"), Arc::clone(&dev)).expect("bind");
    eprintln!(
        "warpaxe-cgminer: Avalon Q on {addr}  mode={mode} ambient={ambient} time-scale={scale} reinit={reinit}s collapse-in-mode={collapse}"
    );
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
