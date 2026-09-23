//! `warpaxe-simuart --port /dev/cu.usbserial-XXXX [--chips 1] [--seed 7]`
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
use warpaxe_sim::{ChipState, SimChain};

const INITIAL_BAUD: u32 = 115_200;
/// Silence this long while addressed, then traffic, means the board
/// restarted its chain: start the chips over.
const RESET_GAP: Duration = Duration::from_secs(3);

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
    if port_name.is_empty() {
        eprintln!("usage: warpaxe-simuart --port /dev/cu.usbserial-XXXX [--chips N] [--seed S]");
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
    let mut chain = SimChain::new(&BM1370, chips, seed);
    let mut baud = INITIAL_BAUD;
    eprintln!(
        "warpaxe-simuart: {chips} x {} on {port_name} at {baud} baud",
        BM1370.name
    );

    let mut buf = [0u8; 1024];
    let mut last_tick = Instant::now();
    let mut last_rx = Instant::now();
    let mut last_report = Instant::now();
    let mut nonces_at_report = 0u32;
    loop {
        // Bytes from the board.
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                let addressed = chain
                    .chips()
                    .iter()
                    .all(|c| c.state() == ChipState::Addressed);
                if addressed && last_rx.elapsed() > RESET_GAP {
                    eprintln!(
                        "quiet {:.1} s then traffic: chips reset",
                        last_rx.elapsed().as_secs_f32()
                    );
                    chain = SimChain::new(&BM1370, chips, seed);
                    if baud != INITIAL_BAUD {
                        baud = INITIAL_BAUD;
                        let _ = port.set_baud_rate(baud);
                    }
                }
                last_rx = Instant::now();
                chain.feed(&buf[..n]);
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
