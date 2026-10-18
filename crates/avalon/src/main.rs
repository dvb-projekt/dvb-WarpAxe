//! `warpaxe-avalon <host> [port]`: read an Avalon once over the cgminer
//! API (summary, estats, pools; nothing is written) and print the
//! AxeOS-like document the firmware's swarm endpoint would serve.
//! `--raw` prints the three replies as they came instead.

#![forbid(unsafe_code)]

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let raw_out = args.iter().any(|a| a == "--raw");
    let rest: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let Some(host) = rest.first() else {
        eprintln!("usage: warpaxe-avalon <host> [port] [--raw]");
        std::process::exit(2);
    };
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
