//! The library against a session captured from WarpPool on 2026-09-23.

use serde_json::Value;
use warpaxe_stratum::message::*;
use warpaxe_stratum::{parse_line, Incoming, Job, Session};

const VECTOR: &str = include_str!("vectors/warppool-2026-09-23.json");

fn vector() -> Value {
    serde_json::from_str(VECTOR).unwrap()
}

fn notify() -> Notify {
    let v = vector();
    let line = serde_json::json!({"id": null, "method": "mining.notify", "params": v["notify"]})
        .to_string();
    match parse_line(&line) {
        Incoming::Notify(n) => n,
        other => panic!("{other:?}"),
    }
}

#[test]
fn classifies_the_captured_lines() {
    let sub = r#"{"id":2,"result":[[["mining.set_difficulty","warppool-666"],["mining.notify","warppool-666"]],"000000000000029a",4],"error":null}"#;
    match parse_line(sub) {
        Incoming::Response(r) => {
            assert_eq!(r.id, 2);
            assert_eq!(
                parse_subscribe_result(&r.result),
                Some(("000000000000029a".into(), 4))
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        parse_line(r#"{"id":null,"method":"mining.set_difficulty","params":[1.0]}"#),
        Incoming::SetDifficulty(1.0)
    );
    match parse_line(
        r#"{"id":1,"result":{"version-rolling":true,"version-rolling.mask":"1fffe000"},"error":null}"#,
    ) {
        Incoming::Response(r) => assert_eq!(r.result["version-rolling.mask"], "1fffe000"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        parse_line(r#"{"id":null,"method":"mining.set_version_mask","params":["1fffe000"]}"#),
        Incoming::SetVersionMask(0x1fff_e000)
    );
    assert!(matches!(parse_line("garbage"), Incoming::Unparsed(_)));
    assert!(matches!(
        parse_line(r#"{"id":null,"method":"client.reconnect","params":[]}"#),
        Incoming::Other { .. }
    ));
    let n = notify();
    assert_eq!(n.job_id, "j2bc1");
    assert_eq!(n.merkle_branches.len(), 13);
    assert!(n.clean_jobs);
}

#[test]
fn coinbase_and_merkle_root_match_python() {
    let v = vector();
    let session = Session::new(
        v["extranonce1"].as_str().unwrap(),
        v["extranonce2_size"].as_u64().unwrap() as usize,
    )
    .unwrap();
    let en2 = hex::decode(v["extranonce2"].as_str().unwrap()).unwrap();
    let job = Job::build(&session, &notify(), &en2).unwrap();
    assert_eq!(
        hex::encode(&job.coinbase),
        v["coinbase_hex"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(job.merkle_root),
        v["merkle_root_natural"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(job.prev_hash),
        v["prevhash_header_order"].as_str().unwrap()
    );
    assert_eq!(job.version, 0x2000_0000);
    assert_eq!(job.nbits, 0x1702_1ec5);
    assert_eq!(job.ntime, 0x6ab3_d530);
}

#[test]
fn chip_work_follows_esp_miner_byte_orders() {
    let v = vector();
    let session = Session::new(v["extranonce1"].as_str().unwrap(), 4).unwrap();
    let job = Job::build(&session, &notify(), &[0, 0, 0, 0]).unwrap();
    let w = job.work();
    // Both hashes: the eight 32-bit words in reverse order, bytes within
    // a word untouched (ESP-Miner reverse_32bit_words, d[0] = s[7]). A
    // real BM1370's nonces verify against the header only with this.
    for i in 0..8 {
        assert_eq!(
            &w.prev_block_hash[i * 4..i * 4 + 4],
            &job.prev_hash[(7 - i) * 4..(7 - i) * 4 + 4]
        );
        assert_eq!(
            &w.merkle_root[i * 4..i * 4 + 4],
            &job.merkle_root[(7 - i) * 4..(7 - i) * 4 + 4]
        );
    }
    assert_eq!(w.version, job.version);
    assert_eq!(w.ntime, job.ntime);
    assert_eq!(w.nbits, job.nbits);
}

#[test]
fn header_is_eighty_bytes_in_wire_layout() {
    let v = vector();
    let session = Session::new(v["extranonce1"].as_str().unwrap(), 4).unwrap();
    let job = Job::build(&session, &notify(), &[0, 0, 0, 0]).unwrap();
    let h = job.header(0x2000_0000, 0xdead_beef);
    assert_eq!(&h[0..4], &0x2000_0000u32.to_le_bytes());
    assert_eq!(&h[4..36], &job.prev_hash);
    assert_eq!(&h[36..68], &job.merkle_root);
    assert_eq!(&h[76..80], &0xdead_beefu32.to_le_bytes());
    // A random header hashes far below difficulty 1.
    let d = warpaxe_stratum::job::hash_difficulty(&warpaxe_stratum::sha256d(&h));
    assert!(d < 1.0, "{d}");
}

#[test]
fn requests_serialise_as_stratum_expects() {
    let r = submit_request(
        7,
        "bc1q.worker",
        "j2bc1",
        "00000001",
        0x6ab3_d530,
        0x1234_5678,
        0x0002_0000,
    );
    assert_eq!(r.to_line(), "{\"id\":7,\"method\":\"mining.submit\",\"params\":[\"bc1q.worker\",\"j2bc1\",\"00000001\",\"6ab3d530\",\"12345678\",\"00020000\"]}\n");
    assert!(configure_request(1, 0x1fff_e000)
        .to_line()
        .contains("\"version-rolling.mask\":\"1fffe000\""));
    assert!(subscribe_request(2, "warpaxe/0.1")
        .to_line()
        .contains("mining.subscribe"));
    assert!(authorize_request(3, "u", "x")
        .to_line()
        .contains("[\"u\",\"x\"]"));
}
