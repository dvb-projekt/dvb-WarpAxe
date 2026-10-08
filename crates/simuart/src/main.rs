//! `warpaxe-simuart --port /dev/cu.usbserial-XXXX [--chips 1] [--seed 7] [--chip bm1370|bm1373|bm1397]`
//!
//! Hardware-in-the-loop without the ASIC: the firmware on the board drives
//! its UART exactly as it would drive a chain; on this side `SimChain`
//! answers. The simulator follows the chips' baud switch on the port and
//! treats a quiet gap followed by new traffic as a chip reset (the real
//! reset line is not on the cable).

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use warpaxe_core::chip::bm1370::BM1370;
use warpaxe_core::chip::bm1373::BM1373;
use warpaxe_core::chip::bm1397::BM1397;
use warpaxe_core::chip::ChipProfile;
use warpaxe_sim::{ChipState, SimChain};

const INITIAL_BAUD: u32 = 115_200;
/// Silence this long while addressed, then traffic, means the board
/// restarted its chain: start the chips over.
const RESET_GAP: Duration = Duration::from_secs(3);
/// At the fast rate, bytes that decode to nothing for this long mean the
/// board restarted and talks at the initial rate again: start over.
/// (Silence alone is not a restart: a real chip waits for work without
/// forgetting anything, and a board waits seconds for its first pool job.)
const GARBAGE_WINDOW: Duration = Duration::from_secs(2);

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let port_name: String = arg("--port", String::new());
    let chips: u8 = arg("--chips", 1);
    let seed: u64 = arg("--seed", 7);
    let chip_name: String = arg("--chip", "bm1370".to_string());
    let profile: &'static ChipProfile = match chip_name.to_ascii_lowercase().as_str() {
        "bm1370" => &BM1370,
        "bm1373" => &BM1373,
        "bm1397" => &BM1397,
        other => {
            eprintln!("unknown --chip {other}: bm1370, bm1373 or bm1397");
            std::process::exit(2);
        }
    };
    if port_name.is_empty() {
        eprintln!("usage: warpaxe-simuart --port /dev/cu.usbserial-XXXX [--chips N] [--seed S] [--chip bm1370|bm1373|bm1397]");
        eprintln!("ports seen:");
        for p in serialport::available_ports().unwrap_or_default() {
            eprintln!("  {}", p.port_name);
        }
        std::process::exit(2);
    }
    let mut port = serialport::new(&port_name, INITIAL_BAUD)
        .timeout(Duration::from_millis(1))
        .open()
        .unwrap_or_else(|e| {
            eprintln!("cannot open {port_name}: {e}");
            std::process::exit(1);
        });
    let mut chain = SimChain::new(profile, chips, seed);
    let mut baud = INITIAL_BAUD;
    eprintln!(
        "warpaxe-simuart: {chips} x {} on {port_name} at {baud} baud",
        profile.name
    );

    let mut buf = [0u8; 1024];
    let mut last_tick = Instant::now();
    let mut last_rx = Instant::now();
    let mut last_report = Instant::now();
    let mut nonces_at_report = 0u32;
    let mut garbage_since: Option<Instant> = None;
    loop {
        // Bytes from the board.
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                let addressed = chain
                    .chips()
                    .iter()
                    .all(|c| c.state() == ChipState::Addressed);
                // At the initial rate the board is still initialising: a
                // pause then traffic is a restart.
                if baud == INITIAL_BAUD && addressed && last_rx.elapsed() > RESET_GAP {
                    eprintln!(
                        "quiet {:.1} s then traffic at {baud} baud: chips reset",
                        last_rx.elapsed().as_secs_f32()
                    );
                    chain = SimChain::new(profile, chips, seed);
                }
                last_rx = Instant::now();
                let before = chain.totals();
                chain.feed(&buf[..n]);
                let after = chain.totals();
                if baud != INITIAL_BAUD {
                    let decoded = after.commands != before.commands || after.jobs != before.jobs;
                    if decoded {
                        garbage_since = None;
                    } else if garbage_since.get_or_insert(Instant::now()).elapsed() > GARBAGE_WINDOW
                    {
                        eprintln!("undecodable bytes at {baud} baud for {GARBAGE_WINDOW:?}: board restarted, chips reset");
                        chain = SimChain::new(profile, chips, seed);
                        baud = INITIAL_BAUD;
                        let _ = port.set_baud_rate(baud);
                        garbage_since = None;
                    }
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                eprintln!("port error: {e}");
                std::process::exit(1);
            }
        }
        // Time passes for the chips.
        let now = Instant::now();
        let dt = now.duration_since(last_tick);
        last_tick = now;
        chain.advance(dt.as_micros() as u64);
        // Bytes to the board.
        let out = chain.drain();
        if !out.is_empty() {
            if let Err(e) = port.write_all(&out) {
                eprintln!("write error: {e}");
            }
        }
        // Follow the chips' baud rate.
        let chip_baud = chain.chips().first().map_or(INITIAL_BAUD, |c| c.baud());
        if chip_baud != baud {
            eprintln!("chips switched to {chip_baud} baud; following");
            baud = chip_baud;
            if let Err(e) = port.set_baud_rate(baud) {
                eprintln!("set_baud_rate failed: {e}");
            }
        }
        if last_report.elapsed() >= Duration::from_secs(10) {
            let t = chain.totals();
            let states: Vec<String> = chain
                .chips()
                .iter()
                .map(|c| {
                    format!(
                        "{:?}@{}MHz/d{}",
                        c.state(),
                        c.frequency_hz() / 1_000_000,
                        c.ticket_difficulty()
                    )
                })
                .collect();
            eprintln!(
                "[{:>4}s] cmds {} jobs {} nonces {} (+{}) viol {} crc {} | {}",
                last_rx.elapsed().as_secs().min(9999),
                t.commands,
                t.jobs,
                t.nonces,
                t.nonces - nonces_at_report,
                t.violations,
                chain.wire_stats().crc_failures,
                states.join(" ")
            );
            nonces_at_report = t.nonces;
            last_report = Instant::now();
        }
    }
}
