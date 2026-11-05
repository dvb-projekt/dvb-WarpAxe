//! Talk to an Sv2 pool from the host, the way the firmware does: Noise
//! handshake with the authority key, connection setup, an extended
//! channel, then print the first jobs. No share is ever sent.
//!
//!   cargo run -p warpaxe-sv2 --example probe -- HOST:PORT WORKER [AUTHORITY_B58]
//!
//! Without the key the pool is not verified (the firmware would refuse
//! that with "require auth" on).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};
use warpaxe_sv2::{authority_key_from_b58, Client, ClientConfig, Event, Handshake, ACT_2_LEN};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: probe HOST:PORT WORKER [AUTHORITY_B58]");
        std::process::exit(2);
    }
    let addr = args[1]
        .to_socket_addrs()
        .expect("resolve")
        .next()
        .expect("an address");
    let authority = args
        .get(3)
        .map(|k| authority_key_from_b58(k).expect("a valid Base58Check authority key"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;

    let t0 = Instant::now();
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let (hs, act1) = Handshake::start(authority).expect("handshake start");
    sock.write_all(&act1).unwrap();
    let mut act2 = [0u8; ACT_2_LEN];
    sock.read_exact(&mut act2).expect("act 2 (234 bytes)");
    let mut transport = match hs.finish(act2, now) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("handshake failed: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "handshake ok in {} ms{}",
        t0.elapsed().as_millis(),
        if authority.is_some() {
            ", certificate verified"
        } else {
            ", pool not verified"
        }
    );

    let mut client = Client::new(ClientConfig {
        user: args[2].clone(),
        vendor: "dvb-projekt".into(),
        hardware_version: "host-probe".into(),
        firmware: concat!("warpaxe-probe/", env!("CARGO_PKG_VERSION")).into(),
        device_id: String::new(),
        endpoint_host: args[1].split(':').next().unwrap_or("").into(),
        endpoint_port: addr.port(),
        nominal_hash_rate: 1.0e12,
        min_extranonce_size: 4,
    });
    let hello = client.hello();
    sock.write_all(&transport.seal(&hello).unwrap()).unwrap();

    let mut buf = [0u8; 2048];
    let mut jobs = 0;
    while jobs < 2 {
        let n = match sock.read(&mut buf) {
            Ok(0) => {
                eprintln!("pool closed the connection");
                std::process::exit(1);
            }
            Ok(n) => n,
            Err(e) => {
                eprintln!("read: {e}");
                std::process::exit(1);
            }
        };
        transport.feed(&buf[..n]);
        while let Some(frame) = transport.next_frame().expect("frame") {
            let mut out = Vec::new();
            let events = match client.handle(&frame, &mut out) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            for f in &out {
                sock.write_all(&transport.seal(f).unwrap()).unwrap();
            }
            for ev in events {
                match ev {
                    Event::Job(t) => {
                        jobs += 1;
                        let job = t.build(&vec![0u8; t.extranonce2_size]).unwrap();
                        println!(
                            "job {} clean {} ntime {} nbits {:#010x} version {:#010x} mask {:#010x} merkle branches {} coinbase {} B",
                            t.job_id,
                            t.clean,
                            t.ntime,
                            t.nbits,
                            t.version,
                            t.version_mask,
                            t.merkle_path.len(),
                            t.coinbase(&job.extranonce2).len()
                        );
                        println!(
                            "  height {:?} tag {:?}",
                            warpaxe_stratum::coinbase::block_height(&t.coinbase_prefix),
                            warpaxe_stratum::coinbase::script_text(
                                &t.coinbase_prefix,
                                &t.coinbase_suffix,
                                t.extranonce_prefix.len() + t.extranonce2_size
                            )
                        );
                    }
                    other => println!("{other:?}"),
                }
            }
        }
    }
    println!("done, no share sent");
}
