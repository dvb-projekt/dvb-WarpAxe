//! The firmware's Sv2 client against the WarpPool Sv2 server, in-process:
//! handshake with certificate check, connection setup, extended channel,
//! future job plus prev hash, template refresh, a real share found on the
//! host and accepted by the pool's own proof-of-work check, a share for a
//! job the pool never issued, and the authority key round trip.
//!
//! The server is the `warppool-stratum-v2` crate at the pinned tag: what
//! the NerdQAxe++ and the Bitaxe talk to live. The client is `warpaxe-sv2`
//! over a blocking socket, the way the firmware drives it.

use bytes::Bytes;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use warpaxe_stratum::job::hash_difficulty;
use warpaxe_stratum::sha256d;
use warpaxe_sv2::{authority_key_from_b58, Client, ClientConfig, Event, Handshake, Job, Template};
use warppool_stratum_v1::vardiff::{VarDiffConfig, VarDiffSnapshot};
use warppool_stratum_v2::session::Sv2VarDiffStore;
use warppool_stratum_v2::{
    authority_pubkey_b58, generate_authority_keypair, AcceptAllValidator, JobUpdate,
    ShareValidator, Sv2Server, Sv2ServerConfig, Sv2ServerHandle, DEFAULT_CERT_VALIDITY,
};

/// Low enough that the host finds a share in a fraction of a second.
const TEST_DIFF: f64 = 1.0 / 65536.0;

/// Hands every channel the test difficulty as its "restored" state: the
/// pool's initial difficulty floor (4096) would take the host hours.
struct TinyDiff;

impl Sv2VarDiffStore for TinyDiff {
    fn load(&self, _worker: &str) -> Option<VarDiffSnapshot> {
        Some(VarDiffSnapshot::initial(TEST_DIFF))
    }
    fn save(&self, _worker: &str, _snapshot: VarDiffSnapshot) {}
}

struct Pool {
    addr: SocketAddr,
    authority: [u8; 32],
    handle: Sv2ServerHandle,
}

async fn start_pool() -> Pool {
    let (auth_priv, auth_pub) = generate_authority_keypair();
    let cfg = Sv2ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        authority_pub_key: auth_pub,
        authority_priv_key: auth_priv,
        cert_validity: DEFAULT_CERT_VALIDITY,
        max_connections: 8,
        job_channel_capacity: 16,
        vardiff_cfg: VarDiffConfig {
            min_diff: TEST_DIFF,
            ..VarDiffConfig::default()
        },
        address_allowlist: Arc::new(HashSet::new()),
        per_worker_network: None,
        connect_limiter: None,
        reuse_port: false,
    };
    let validator: Arc<dyn ShareValidator> = Arc::new(AcceptAllValidator);
    let (server, handle) =
        Sv2Server::with_sinks_and_store(cfg, validator, None, None, Some(Arc::new(TinyDiff)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.serve_with_listener(listener, handle.clone()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    Pool {
        addr,
        authority: auth_pub,
        handle,
    }
}

fn job_update(job_id: u32, prev: u8, clean: bool, extranonce_total: usize) -> JobUpdate {
    JobUpdate {
        job_id,
        prev_hash_be: [prev; 32],
        version: 0x2000_0000,
        nbits: 0x1d00_ffff,
        min_ntime: 1_700_000_000 + job_id,
        clean_jobs: clean,
        merkle_path: vec![[0x11; 32], [0x22; 32], [0x33; 32]],
        coinbase_prefix: Bytes::from(vec![0x01, 0x00, 0x00, 0x00, 0x01, 0xAA, 0xBB]),
        coinbase_suffix: Bytes::from(vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0xCC]),
        extranonce_total_size: extranonce_total,
        version_rolling_allowed: true,
        template: None,
    }
}

fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32
}

/// A connected, handshaken client the way the firmware holds one.
struct Conn {
    sock: TcpStream,
    transport: warpaxe_sv2::Transport,
    client: Client,
    buf: [u8; 2048],
}

impl Conn {
    fn connect(addr: SocketAddr, authority: Option<[u8; 32]>) -> Result<Conn, warpaxe_sv2::Error> {
        let mut sock = TcpStream::connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let (hs, act1) = Handshake::start(authority)?;
        sock.write_all(&act1).unwrap();
        let mut act2 = [0u8; warpaxe_sv2::ACT_2_LEN];
        sock.read_exact(&mut act2).unwrap();
        let transport = hs.finish(act2, now())?;
        let client = Client::new(ClientConfig {
            user: "bc1qtestworker.warpaxe".into(),
            vendor: "dvb-projekt".into(),
            hardware_version: "bitaxe-gamma".into(),
            firmware: "warpaxe test".into(),
            device_id: "".into(),
            endpoint_host: "127.0.0.1".into(),
            endpoint_port: addr.port(),
            nominal_hash_rate: 1.0e12,
            min_extranonce_size: 4,
        });
        let mut c = Conn {
            sock,
            transport,
            client,
            buf: [0; 2048],
        };
        let hello = c.client.hello();
        c.send(&hello);
        Ok(c)
    }

    fn send(&mut self, f: &warpaxe_sv2::Frame) {
        let bytes = self.transport.seal(f).unwrap();
        self.sock.write_all(&bytes).unwrap();
    }

    /// Block until the next event from the pool.
    fn next_events(&mut self) -> Vec<Event> {
        loop {
            if let Some(frame) = self.transport.next_frame().unwrap() {
                let mut out = Vec::new();
                let ev = self.client.handle(&frame, &mut out).unwrap();
                for f in &out {
                    self.send(f);
                }
                if !ev.is_empty() {
                    return ev;
                }
                continue;
            }
            let n = self.sock.read(&mut self.buf).expect("pool went silent");
            assert!(n > 0, "pool closed the connection");
            self.transport.feed(&self.buf[..n]);
        }
    }

    /// Wait for one event matching `want`.
    fn wait(&mut self, what: &str, mut want: impl FnMut(&Event) -> bool) -> Event {
        for _ in 0..50 {
            for e in self.next_events() {
                if want(&e) {
                    return e;
                }
            }
        }
        panic!("no {what} from the pool");
    }
}

/// Grind a nonce on the host that meets the pool difficulty.
fn mine(job: &Job, difficulty: f64) -> u32 {
    (0u32..)
        .find(|&nonce| hash_difficulty(&sha256d(&job.header(job.version, nonce))) >= difficulty)
        .expect("a share below 2^32 hashes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn firmware_client_mines_a_share_the_pool_accepts() {
    let pool = start_pool().await;
    let (tx, rx) = mpsc::channel::<(usize, usize)>();
    let (addr, authority) = (pool.addr, pool.authority);
    let miner = std::thread::spawn(move || {
        let mut c = Conn::connect(addr, Some(authority)).unwrap();
        let ev = c.wait("setup", |e| matches!(e, Event::SetupDone { .. }));
        assert_eq!(
            ev,
            Event::SetupDone {
                used_version: 2,
                flags: 0
            }
        );
        let ev = c.wait("channel", |e| matches!(e, Event::ChannelOpen { .. }));
        let Event::ChannelOpen {
            extranonce2_size,
            extranonce_prefix,
            ..
        } = ev
        else {
            unreachable!()
        };
        assert!(extranonce2_size >= 4, "{extranonce2_size}");
        assert!(c.client.difficulty() > 0.0);
        tx.send((extranonce_prefix.len(), extranonce2_size))
            .unwrap();

        // Job 1: a new block → future job + SetNewPrevHash → clean template.
        let ev = c.wait("job 1", |e| matches!(e, Event::Job(_)));
        let Event::Job(t1) = ev else { unreachable!() };
        assert!(t1.clean);
        assert_eq!(t1.job_id, 1);
        assert_eq!(t1.prev_hash, [0xA1; 32]); // wire = header order
        assert_eq!((t1.nbits, t1.ntime), (0x1d00_ffff, 1_700_000_001));
        assert_eq!(t1.version_mask, warpaxe_sv2::channel::BIP320_MASK);
        assert_eq!(t1.extranonce2_size, extranonce2_size);

        // A real share: built like the firmware does, found on the host.
        let difficulty = c.client.difficulty();
        assert!(
            (difficulty - TEST_DIFF).abs() / TEST_DIFF < 0.01,
            "pool difficulty {difficulty}"
        );
        let mut extranonce2 = vec![0u8; extranonce2_size];
        extranonce2[extranonce2_size - 1] = 1;
        let job = t1.build(&extranonce2).unwrap();
        let nonce = mine(&job, difficulty);
        let f = c
            .client
            .submit(t1.job_id, &extranonce2, job.ntime, nonce, job.version)
            .unwrap();
        c.send(&f);
        let ev = c.wait("verdict", |e| {
            matches!(e, Event::Accepted { .. } | Event::Rejected { .. })
        });
        assert_eq!(
            ev,
            Event::Accepted {
                count: 1,
                last_sequence: 1
            },
            "the pool's own PoW check must agree with the client's header"
        );
        assert_eq!(c.client.accepted(), 1);
        tx.send((0, 0)).unwrap();

        // Job 2: a refresh on the same block → immediate, not clean.
        let ev = c.wait("job 2", |e| matches!(e, Event::Job(_)));
        let Event::Job(t2) = ev else { unreachable!() };
        assert!(!t2.clean);
        assert_eq!(t2.job_id, 2);
        assert_eq!(t2.prev_hash, t1.prev_hash);
        assert_eq!(t2.ntime, 1_700_000_002);
        tx.send((0, 0)).unwrap();

        // Job 3: the next block → clean again, new prev hash.
        let ev = c.wait("job 3", |e| matches!(e, Event::Job(_)));
        let Event::Job(t3) = ev else { unreachable!() };
        assert!(t3.clean);
        assert_eq!(t3.prev_hash, [0xA3; 32]);

        // A share for a job the pool never issued is rejected, not dropped.
        let f = c
            .client
            .submit(999, &extranonce2, t3.ntime, 1, t3.version)
            .unwrap();
        c.send(&f);
        let ev = c.wait("rejection", |e| {
            matches!(e, Event::Accepted { .. } | Event::Rejected { .. })
        });
        let Event::Rejected { job_id, code, .. } = ev else {
            panic!("{ev:?}")
        };
        assert_eq!(job_id, Some(999));
        // v1.34.0 files an unknown job under "stale-share"; older and
        // newer pools may say "job-not-found". Either is a verdict.
        assert!(
            code == "stale-share" || code == "job-not-found",
            "unexpected reason {code}"
        );
        assert_eq!(c.client.rejected(), 1);
        t3
    });

    // The pool side, paced by the miner thread.
    let (prefix_len, size) = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("channel open");
    let total = prefix_len + size;
    pool.handle.push_job(job_update(1, 0xA1, true, total));
    rx.recv_timeout(Duration::from_secs(20))
        .expect("share accepted");
    pool.handle.push_job(job_update(2, 0xA1, false, total));
    rx.recv_timeout(Duration::from_secs(20))
        .expect("job 2 seen");
    pool.handle.push_job(job_update(3, 0xA3, true, total));
    let t3: Template = tokio::task::spawn_blocking(move || miner.join().unwrap())
        .await
        .unwrap();
    assert_eq!(t3.job_id, 3);
    pool.handle.shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_authority_key_fails_the_handshake_and_none_skips_the_check() {
    let pool = start_pool().await;
    let (addr, authority) = (pool.addr, pool.authority);
    let r = tokio::task::spawn_blocking(move || {
        let mut wrong = authority;
        wrong[0] ^= 0x40;
        let bad = match Conn::connect(addr, Some(wrong)) {
            Ok(_) => panic!("a wrong authority key must not verify"),
            Err(e) => e,
        };
        // A flipped bit gives a key that is not on the curve about half
        // the time (refused before the handshake), else a certificate
        // signed by another authority.
        assert!(
            matches!(
                bad,
                warpaxe_sv2::Error::Certificate(_) | warpaxe_sv2::Error::BadKey
            ),
            "{bad}"
        );
        let mut c = Conn::connect(addr, None).expect("no key: pool taken as it is");
        c.wait("setup", |e| matches!(e, Event::SetupDone { .. }))
    })
    .await
    .unwrap();
    assert!(matches!(r, Event::SetupDone { .. }));
    pool.handle.shutdown.cancel();
}

#[test]
fn authority_key_base58_matches_the_pools_encoder() {
    let (_priv, pub_) = generate_authority_keypair();
    let s = authority_pubkey_b58(&pub_).unwrap();
    assert_eq!(authority_key_from_b58(&s).unwrap(), pub_);
    let mut broken = s.clone();
    broken.truncate(s.len() - 1);
    assert!(authority_key_from_b58(&broken).is_err());
}
