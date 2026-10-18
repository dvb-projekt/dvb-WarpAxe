//! `warpaxe-avalon <host> [port]`: read an Avalon once over the cgminer
//! API (summary, estats, pools; nothing is written) and print the
//! AxeOS-like document the firmware's swarm endpoint would serve.
//! `--raw` prints the three replies as they came instead.
//! `warpaxe-avalon --scan <own-ip>`: walk that /24 for port 4028 as the
//! firmware's search does (`version` only) and list what answers.

#![forbid(unsafe_code)]

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let raw_out = args.iter().any(|a| a == "--raw");
    let rest: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let Some(host) = rest.first() else {
        eprintln!("usage: warpaxe-avalon <host> [port] [--raw] | --scan <own-ip>");
        std::process::exit(2);
    };
    if args.iter().any(|a| a == "--scan") {
        let Ok(own) = host.parse::<std::net::Ipv4Addr>() else {
            eprintln!("--scan takes this computer's IPv4 address in the subnet to walk");
            std::process::exit(2);
        };
        let progress =
            std::sync::Arc::new(std::sync::Mutex::new(warpaxe_avalon::Progress::default()));
        let t0 = std::time::Instant::now();
        warpaxe_avalon::scan(
            warpaxe_avalon::subnet_hosts(own),
            warpaxe_avalon::PORT,
            8,
            Duration::from_millis(400),
            Duration::from_secs(1),
            Duration::from_secs(30),
            &progress,
            &warpaxe_avalon::thread_spawner(64 * 1024),
        );
        let p = progress.lock().unwrap();
        println!(
            "{} of {} hosts tried in {} s, {} skipped for time, {} for sockets",
            p.done,
            p.total,
            t0.elapsed().as_secs(),
            p.skipped_time,
            p.skipped_sockets
        );
        for f in &p.found {
            println!("{}  {}  MM {}  {}", f.host, f.product, f.firmware, f.api);
        }
        return;
    }
    let port = rest
        .get(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(warpaxe_avalon::PORT);
    match warpaxe_avalon::fetch(host, port, Duration::from_secs(3)) {
        Ok(raw) => {
            let v = if raw_out {
                serde_json::json!({"summary": raw.summary, "estats": raw.estats, "pools": raw.pools})
            } else {
                warpaxe_avalon::translate(&raw)
            };
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        Err(e) => {
            eprintln!("{host}:{port}: {e}");
            std::process::exit(1);
        }
    }
}
