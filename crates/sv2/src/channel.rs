//! The client state machine: connection setup, one extended mining
//! channel, jobs, targets and share results. Feed it every decrypted
//! [`Frame`], send what it puts in `out`, act on the [`Event`]s.
//!
//! Jobs: a `NewExtendedMiningJob` without `min_ntime` is a future job,
//! held until a `SetNewPrevHash` names it (a new block: clean). One with
//! `min_ntime` is mined at once on the current prev hash (a template
//! refresh: not clean). Either way the miner gets a [`Template`] and
//! rolls its own extranonce through [`Template::build`].

use crate::messages::*;
use crate::wire::{target_to_difficulty, Frame, CHANNEL_MSG_FLAG};
use crate::{msg, Error, Job};
use std::collections::VecDeque;
use warpaxe_stratum::sha256d;

/// BIP320 version-rolling mask, what the pool allows when it allows any.
pub const BIP320_MASK: u32 = 0x1fff_e000;
/// Future jobs kept while waiting for their prev hash.
const MAX_FUTURE_JOBS: usize = 8;
/// Shares awaiting a verdict.
const MAX_PENDING_SHARES: usize = 64;

/// What the client tells the pool about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientConfig {
    /// Worker name: the payout address, or address.worker.
    pub user: String,
    /// Device vendor for `SetupConnection`.
    pub vendor: String,
    /// Hardware version for `SetupConnection`.
    pub hardware_version: String,
    /// Firmware version for `SetupConnection`.
    pub firmware: String,
    /// Device identity for `SetupConnection`.
    pub device_id: String,
    /// The host the client connected to.
    pub endpoint_host: String,
    /// The port the client connected to.
    pub endpoint_port: u16,
    /// Expected hashrate in H/s; the pool picks its first difficulty
    /// from it.
    pub nominal_hash_rate: f32,
    /// Smallest extranonce the miner wants to roll, bytes.
    pub min_extranonce_size: u16,
}

/// A job the miner can roll its extranonce through: the Sv2 counterpart
/// of a V1 notify plus session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// Pool job id.
    pub job_id: u32,
    /// Block version.
    pub version: u32,
    /// Version bits the miner may roll (0 = none).
    pub version_mask: u32,
    /// Merkle branches beside the coinbase.
    pub merkle_path: Vec<[u8; 32]>,
    /// Coinbase bytes before the extranonce.
    pub coinbase_prefix: Vec<u8>,
    /// Coinbase bytes after the extranonce.
    pub coinbase_suffix: Vec<u8>,
    /// The pool's fixed part of the extranonce.
    pub extranonce_prefix: Vec<u8>,
    /// Bytes the miner rolls.
    pub extranonce2_size: usize,
    /// Previous block hash in header byte order.
    pub prev_hash: [u8; 32],
    /// Compact network target.
    pub nbits: u32,
    /// Block time to mine with.
    pub ntime: u32,
    /// A new block: drop older jobs.
    pub clean: bool,
}

impl Template {
    /// Assemble the coinbase with `extranonce2`, fold the merkle path,
    /// return the job. `None` if the extranonce has the wrong size.
    pub fn build(&self, extranonce2: &[u8]) -> Option<Job> {
        if extranonce2.len() != self.extranonce2_size {
            return None;
        }
        let mut h = sha256d(&self.coinbase(extranonce2));
        for b in &self.merkle_path {
            let mut both = [0u8; 64];
            both[..32].copy_from_slice(&h);
            both[32..].copy_from_slice(b);
            h = sha256d(&both);
        }
        Some(Job {
            job_id: self.job_id.to_string(),
            extranonce2: extranonce2.to_vec(),
            merkle_root: h,
            prev_hash: self.prev_hash,
            version: self.version,
            nbits: self.nbits,
            ntime: self.ntime,
            clean_jobs: self.clean,
        })
    }

    /// The full coinbase transaction for `extranonce2`. Built for the
    /// txid and dropped; a job does not keep it.
    pub fn coinbase(&self, extranonce2: &[u8]) -> Vec<u8> {
        let mut coinbase = Vec::with_capacity(
            self.coinbase_prefix.len()
                + self.extranonce_prefix.len()
                + extranonce2.len()
                + self.coinbase_suffix.len(),
        );
        coinbase.extend_from_slice(&self.coinbase_prefix);
        coinbase.extend_from_slice(&self.extranonce_prefix);
        coinbase.extend_from_slice(extranonce2);
        coinbase.extend_from_slice(&self.coinbase_suffix);
        coinbase
    }
}

/// What happened, for the firmware to log, count or act on.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The pool accepted the connection.
    SetupDone {
        /// Protocol version in use.
        used_version: u16,
        /// Pool capability flags.
        flags: u32,
    },
    /// The channel is open; jobs follow.
    ChannelOpen {
        /// Channel id.
        channel_id: u32,
        /// Bytes the miner rolls.
        extranonce2_size: usize,
        /// The pool's fixed part.
        extranonce_prefix: Vec<u8>,
    },
    /// A job to mine.
    Job(Template),
    /// The share difficulty changed.
    Difficulty(f64),
    /// The pool changed the extranonce prefix for coming jobs.
    ExtranoncePrefix(Vec<u8>),
    /// Shares accepted.
    Accepted {
        /// How many since the last success message.
        count: u32,
        /// Highest sequence number covered.
        last_sequence: u32,
    },
    /// A share rejected.
    Rejected {
        /// Its sequence number.
        sequence: u32,
        /// The job it was for, if still known.
        job_id: Option<u32>,
        /// The pool's reason.
        code: String,
    },
    /// The pool wants the client elsewhere.
    Reconnect {
        /// New host, empty = the same.
        host: String,
        /// New port, 0 = the same.
        port: u16,
    },
    /// Something the client does not act on, with the message type.
    Ignored(u8),
    /// Something odd but survivable.
    Warn(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Setup,
    Opening,
    Mining,
}

/// The client.
pub struct Client {
    cfg: ClientConfig,
    phase: Phase,
    channel_id: u32,
    extranonce_prefix: Vec<u8>,
    extranonce2_size: usize,
    target: [u8; 32],
    difficulty: f64,
    /// (prev_hash, nbits, min_ntime) of the current block.
    prev: Option<([u8; 32], u32, u32)>,
    future: Vec<NewExtendedMiningJob>,
    sequence: u32,
    /// (sequence, job_id) of shares without a verdict.
    pending: VecDeque<(u32, u32)>,
    accepted: u64,
    rejected: u64,
}

impl Client {
    /// A client that has not said hello yet.
    pub fn new(cfg: ClientConfig) -> Client {
        Client {
            cfg,
            phase: Phase::Setup,
            channel_id: 0,
            extranonce_prefix: Vec::new(),
            extranonce2_size: 0,
            target: [0xFF; 32],
            difficulty: 0.0,
            prev: None,
            future: Vec::new(),
            sequence: 0,
            pending: VecDeque::new(),
            accepted: 0,
            rejected: 0,
        }
    }

    /// The first frame to send after the handshake.
    pub fn hello(&self) -> Frame {
        SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: 0,
            endpoint_host: self.cfg.endpoint_host.clone(),
            endpoint_port: self.cfg.endpoint_port,
            vendor: self.cfg.vendor.clone(),
            hardware_version: self.cfg.hardware_version.clone(),
            firmware: self.cfg.firmware.clone(),
            device_id: self.cfg.device_id.clone(),
        }
        .frame()
    }

    /// Current share difficulty (0 before the channel is open).
    pub fn difficulty(&self) -> f64 {
        self.difficulty
    }
    /// The channel id once open.
    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }
    /// Bytes the miner rolls once the channel is open.
    pub fn extranonce2_size(&self) -> usize {
        self.extranonce2_size
    }
    /// The channel is open and jobs may arrive.
    pub fn mining(&self) -> bool {
        self.phase == Phase::Mining
    }
    /// Shares accepted so far.
    pub fn accepted(&self) -> u64 {
        self.accepted
    }
    /// Shares rejected so far.
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    /// A share for the pool; the frame goes out, the sequence number is
    /// remembered for the verdict.
    pub fn submit(
        &mut self,
        job_id: u32,
        extranonce2: &[u8],
        ntime: u32,
        nonce: u32,
        version: u32,
    ) -> Result<Frame, Error> {
        if self.phase != Phase::Mining {
            return Err(Error::Protocol("submit before the channel is open".into()));
        }
        self.sequence = self.sequence.wrapping_add(1);
        let frame = SubmitSharesExtended {
            channel_id: self.channel_id,
            sequence_number: self.sequence,
            job_id,
            nonce,
            ntime,
            version,
            extranonce: extranonce2.to_vec(),
        }
        .frame()?;
        if self.pending.len() >= MAX_PENDING_SHARES {
            self.pending.pop_front();
        }
        self.pending.push_back((self.sequence, job_id));
        Ok(frame)
    }

    /// Process one frame from the pool. Replies go to `out`.
    pub fn handle(&mut self, frame: &Frame, out: &mut Vec<Frame>) -> Result<Vec<Event>, Error> {
        let mut ev = Vec::new();
        let ty = frame.message_type;
        let p = &frame.payload[..];
        match (self.phase, ty) {
            (Phase::Setup, msg::SETUP_CONNECTION_SUCCESS) => {
                let s = SetupConnectionSuccess::decode(p)?;
                self.phase = Phase::Opening;
                out.push(
                    OpenExtendedMiningChannel {
                        request_id: 1,
                        user_identity: self.cfg.user.clone(),
                        nominal_hash_rate: self.cfg.nominal_hash_rate,
                        max_target: [0xFF; 32],
                        min_extranonce_size: self.cfg.min_extranonce_size,
                    }
                    .frame(),
                );
                ev.push(Event::SetupDone {
                    used_version: s.used_version,
                    flags: s.flags,
                });
            }
            (_, msg::SETUP_CONNECTION_ERROR) => {
                let e = SetupConnectionError::decode(p)?;
                return Err(Error::Refused(format!("setup: {}", e.error_code)));
            }
            (Phase::Opening, msg::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS) => {
                let s = OpenExtendedMiningChannelSuccess::decode(p)?;
                // Sv2 caps the whole extranonce at 32 bytes; anything else
                // is a broken or hostile pool, and the miner would allocate
                // whatever it says for every job.
                if !(1..=32).contains(&s.extranonce_size) {
                    return Err(Error::Refused(format!(
                        "channel: extranonce size {}",
                        s.extranonce_size
                    )));
                }
                self.phase = Phase::Mining;
                self.channel_id = s.channel_id;
                self.extranonce_prefix = s.extranonce_prefix.clone();
                self.extranonce2_size = usize::from(s.extranonce_size);
                self.target = s.target;
                self.difficulty = target_to_difficulty(&s.target);
                ev.push(Event::ChannelOpen {
                    channel_id: s.channel_id,
                    extranonce2_size: self.extranonce2_size,
                    extranonce_prefix: s.extranonce_prefix,
                });
                ev.push(Event::Difficulty(self.difficulty));
            }
            (_, msg::OPEN_MINING_CHANNEL_ERROR) => {
                let e = OpenMiningChannelError::decode(p)?;
                return Err(Error::Refused(format!("channel: {}", e.error_code)));
            }
            (Phase::Mining, _) if frame.extension_type & CHANNEL_MSG_FLAG != 0 => {
                self.channel_message(ty, p, &mut ev)?;
            }
            (_, msg::RECONNECT) => {
                let r = Reconnect::decode(p)?;
                ev.push(Event::Reconnect {
                    host: r.new_host,
                    port: r.new_port,
                });
            }
            (_, msg::CLOSE_CHANNEL) => {
                let c = CloseChannel::decode(p)?;
                return Err(Error::Refused(format!("channel closed: {}", c.reason_code)));
            }
            (Phase::Mining, _) => {
                // A channel message without the flag: tolerate it, the
                // flag is what some pools forget rather than the payload.
                self.channel_message(ty, p, &mut ev)?;
            }
            (_, other) => ev.push(Event::Ignored(other)),
        }
        Ok(ev)
    }

    fn channel_message(&mut self, ty: u8, p: &[u8], ev: &mut Vec<Event>) -> Result<(), Error> {
        match ty {
            msg::NEW_EXTENDED_MINING_JOB => {
                let j = NewExtendedMiningJob::decode(p)?;
                if j.channel_id != self.channel_id {
                    ev.push(Event::Warn(format!("job for channel {}", j.channel_id)));
                    return Ok(());
                }
                match (j.min_ntime, self.prev) {
                    (Some(ntime), Some((prev_hash, nbits, _))) => {
                        ev.push(Event::Job(
                            self.template(&j, prev_hash, nbits, ntime, false),
                        ));
                    }
                    _ => {
                        // Future job, or an immediate one before any prev
                        // hash: wait for SetNewPrevHash to name it.
                        if self.future.len() >= MAX_FUTURE_JOBS {
                            self.future.remove(0);
                        }
                        self.future.push(j);
                    }
                }
            }
            msg::SET_NEW_PREV_HASH => {
                let s = SetNewPrevHash::decode(p)?;
                if s.channel_id != self.channel_id {
                    ev.push(Event::Warn(format!(
                        "prev hash for channel {}",
                        s.channel_id
                    )));
                    return Ok(());
                }
                self.prev = Some((s.prev_hash, s.nbits, s.min_ntime));
                match self.future.iter().position(|j| j.job_id == s.job_id) {
                    Some(i) => {
                        let j = self.future.remove(i);
                        // Older future jobs were for a block that is gone.
                        self.future.retain(|f| f.job_id > s.job_id);
                        ev.push(Event::Job(self.template(
                            &j,
                            s.prev_hash,
                            s.nbits,
                            s.min_ntime,
                            true,
                        )));
                    }
                    None => ev.push(Event::Warn(format!(
                        "prev hash names unknown job {}",
                        s.job_id
                    ))),
                }
            }
            msg::SET_TARGET => {
                let t = SetTarget::decode(p)?;
                if t.channel_id == self.channel_id {
                    self.target = t.maximum_target;
                    self.difficulty = target_to_difficulty(&t.maximum_target);
                    ev.push(Event::Difficulty(self.difficulty));
                }
            }
            msg::SET_EXTRANONCE_PREFIX => {
                let e = SetExtranoncePrefix::decode(p)?;
                if e.channel_id == self.channel_id {
                    self.extranonce_prefix = e.extranonce_prefix.clone();
                    ev.push(Event::ExtranoncePrefix(e.extranonce_prefix));
                }
            }
            msg::SUBMIT_SHARES_SUCCESS => {
                let s = SubmitSharesSuccess::decode(p)?;
                self.pending
                    .retain(|(seq, _)| *seq > s.last_sequence_number);
                self.accepted += u64::from(s.new_submits_accepted_count);
                ev.push(Event::Accepted {
                    count: s.new_submits_accepted_count,
                    last_sequence: s.last_sequence_number,
                });
            }
            msg::SUBMIT_SHARES_ERROR => {
                let e = SubmitSharesError::decode(p)?;
                let job_id = self
                    .pending
                    .iter()
                    .position(|(seq, _)| *seq == e.sequence_number)
                    .and_then(|i| self.pending.remove(i))
                    .map(|(_, job)| job);
                self.rejected += 1;
                ev.push(Event::Rejected {
                    sequence: e.sequence_number,
                    job_id,
                    code: e.error_code,
                });
            }
            msg::CLOSE_CHANNEL => {
                let c = CloseChannel::decode(p)?;
                return Err(Error::Refused(format!("channel closed: {}", c.reason_code)));
            }
            other => ev.push(Event::Ignored(other)),
        }
        Ok(())
    }

    fn template(
        &self,
        j: &NewExtendedMiningJob,
        prev_hash: [u8; 32],
        nbits: u32,
        ntime: u32,
        clean: bool,
    ) -> Template {
        Template {
            job_id: j.job_id,
            version: j.version,
            version_mask: if j.version_rolling_allowed {
                BIP320_MASK
            } else {
                0
            },
            merkle_path: j.merkle_path.clone(),
            coinbase_prefix: j.coinbase_tx_prefix.clone(),
            coinbase_suffix: j.coinbase_tx_suffix.clone(),
            extranonce_prefix: self.extranonce_prefix.clone(),
            extranonce2_size: self.extranonce2_size,
            prev_hash,
            nbits,
            ntime,
            clean,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Writer;

    fn cfg() -> ClientConfig {
        ClientConfig {
            user: "bc1qtest.warpaxe".into(),
            vendor: "dvb-projekt".into(),
            hardware_version: "bitaxe-gamma".into(),
            firmware: "warpaxe 0.1.1".into(),
            device_id: "".into(),
            endpoint_host: "pool.warppool.org".into(),
            endpoint_port: 34254,
            nominal_hash_rate: 1.4e12,
            min_extranonce_size: 4,
        }
    }

    fn setup_success() -> Frame {
        let mut w = Writer::new();
        w.u16(2).u32(0);
        Frame::new(msg::SETUP_CONNECTION_SUCCESS, w.finish())
    }

    fn open_success(prefix: &[u8], size: u16) -> Frame {
        let mut t = [0u8; 32];
        t[26] = 0xFF;
        t[27] = 0xFF; // difficulty 1
        let mut w = Writer::new();
        w.u32(1).u32(7).u256(&t).u16(size).b0_255(prefix).unwrap();
        w.u32(7);
        Frame::new(msg::OPEN_EXTENDED_MINING_CHANNEL_SUCCESS, w.finish())
    }

    fn job(id: u32, min_ntime: Option<u32>) -> Frame {
        NewExtendedMiningJob {
            channel_id: 7,
            job_id: id,
            min_ntime,
            version: 0x2000_0000,
            version_rolling_allowed: true,
            merkle_path: vec![[0x11; 32], [0x22; 32]],
            coinbase_tx_prefix: vec![1, 2, 3],
            coinbase_tx_suffix: vec![9, 9],
        }
        .frame()
    }

    fn prev(job_id: u32) -> Frame {
        SetNewPrevHash {
            channel_id: 7,
            job_id,
            prev_hash: [0xAB; 32],
            min_ntime: 1_700_000_000,
            nbits: 0x1703_0000,
        }
        .frame()
    }

    /// A client with the channel open.
    fn open() -> Client {
        let mut c = Client::new(cfg());
        let mut out = Vec::new();
        let ev = c.handle(&setup_success(), &mut out).unwrap();
        assert!(matches!(
            ev[0],
            Event::SetupDone {
                used_version: 2,
                ..
            }
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].message_type, msg::OPEN_EXTENDED_MINING_CHANNEL);
        let ev = c.handle(&open_success(&[1, 2, 3, 4], 4), &mut out).unwrap();
        assert_eq!(
            ev[0],
            Event::ChannelOpen {
                channel_id: 7,
                extranonce2_size: 4,
                extranonce_prefix: vec![1, 2, 3, 4]
            }
        );
        assert_eq!(ev[1], Event::Difficulty(1.0));
        assert!(c.mining());
        c
    }

    #[test]
    fn channel_with_an_extranonce_size_out_of_range_is_refused() {
        for size in [0u16, 33, u16::MAX] {
            let mut c = Client::new(cfg());
            let mut out = Vec::new();
            c.handle(&setup_success(), &mut out).unwrap();
            assert!(matches!(
                c.handle(&open_success(&[1, 2, 3, 4], size), &mut out),
                Err(Error::Refused(_))
            ));
            assert!(!c.mining());
        }
    }

    #[test]
    fn hello_is_a_setup_connection_for_the_mining_protocol() {
        let c = Client::new(cfg());
        let f = c.hello();
        let s = SetupConnection::decode(&f.payload).unwrap();
        assert_eq!(
            (s.protocol, s.min_version, s.max_version, s.flags),
            (0, 2, 2, 0)
        );
        assert_eq!(s.endpoint_port, 34254);
        assert_eq!(s.vendor, "dvb-projekt");
    }

    #[test]
    fn open_channel_carries_user_hashrate_and_extranonce_wish() {
        let mut c = Client::new(cfg());
        let mut out = Vec::new();
        c.handle(&setup_success(), &mut out).unwrap();
        let p = &out[0].payload;
        assert_eq!(&p[4..5], &[16]);
        assert_eq!(&p[5..21], b"bc1qtest.warpaxe");
        assert_eq!(&p[21..25], &1.4e12f32.to_le_bytes());
        assert_eq!(&p[25..57], &[0xFF; 32]);
        assert_eq!(&p[57..59], &[4, 0]);
    }

    #[test]
    fn future_job_waits_for_its_prev_hash_and_comes_out_clean() {
        let mut c = open();
        let mut out = Vec::new();
        let ev = c.handle(&job(10, None), &mut out).unwrap();
        assert!(ev.is_empty(), "{ev:?}");
        let ev = c.handle(&prev(10), &mut out).unwrap();
        let Event::Job(t) = &ev[0] else {
            panic!("{ev:?}")
        };
        assert!(t.clean);
        assert_eq!(t.job_id, 10);
        assert_eq!(t.prev_hash, [0xAB; 32]);
        assert_eq!((t.nbits, t.ntime), (0x1703_0000, 1_700_000_000));
        assert_eq!(t.version_mask, BIP320_MASK);
        assert_eq!(t.extranonce_prefix, vec![1, 2, 3, 4]);
        assert_eq!(t.extranonce2_size, 4);
        // A refresh on the same block is immediate and not clean.
        let ev = c.handle(&job(11, Some(1_700_000_050)), &mut out).unwrap();
        let Event::Job(t) = &ev[0] else {
            panic!("{ev:?}")
        };
        assert!(!t.clean);
        assert_eq!((t.job_id, t.ntime), (11, 1_700_000_050));
        assert!(out.is_empty());
    }

    #[test]
    fn template_builds_the_same_job_as_the_v1_path() {
        let t = Template {
            job_id: 5,
            version: 0x2000_0000,
            version_mask: BIP320_MASK,
            merkle_path: vec![[0x11; 32]],
            coinbase_prefix: vec![1, 2, 3],
            coinbase_suffix: vec![9, 9],
            extranonce_prefix: vec![0xAA, 0xBB],
            extranonce2_size: 2,
            prev_hash: [0xCD; 32],
            nbits: 0x1703_0000,
            ntime: 1234,
            clean: true,
        };
        let j = t.build(&[0, 1]).unwrap();
        let coinbase = t.coinbase(&[0, 1]);
        assert_eq!(coinbase, vec![1, 2, 3, 0xAA, 0xBB, 0, 1, 9, 9]);
        let mut both = [0u8; 64];
        both[..32].copy_from_slice(&sha256d(&coinbase));
        both[32..].copy_from_slice(&[0x11; 32]);
        assert_eq!(j.merkle_root, sha256d(&both));
        assert_eq!(j.job_id, "5");
        assert_eq!(j.prev_hash, [0xCD; 32]);
        assert!(j.clean_jobs);
        assert!(t.build(&[0]).is_none());
    }

    #[test]
    fn shares_are_sequenced_and_verdicts_attributed() {
        let mut c = open();
        let mut out = Vec::new();
        c.handle(&job(10, None), &mut out).unwrap();
        c.handle(&prev(10), &mut out).unwrap();
        let f1 = c.submit(10, &[0, 0, 0, 1], 1, 2, 0x2000_0000).unwrap();
        let f2 = c.submit(10, &[0, 0, 0, 2], 1, 3, 0x2000_0000).unwrap();
        assert_eq!(&f1.payload[4..8], &1u32.to_le_bytes());
        assert_eq!(&f2.payload[4..8], &2u32.to_le_bytes());
        assert_eq!(f1.extension_type, CHANNEL_MSG_FLAG);
        let mut w = Writer::new();
        w.u32(7).u32(1).u32(1).u32(0);
        w.u32(0);
        let ok = Frame::channel(msg::SUBMIT_SHARES_SUCCESS, w.finish());
        let ev = c.handle(&ok, &mut out).unwrap();
        assert_eq!(
            ev[0],
            Event::Accepted {
                count: 1,
                last_sequence: 1
            }
        );
        let mut w = Writer::new();
        w.u32(7).u32(2).str0_255("stale-share");
        let err = Frame::channel(msg::SUBMIT_SHARES_ERROR, w.finish());
        let ev = c.handle(&err, &mut out).unwrap();
        assert_eq!(
            ev[0],
            Event::Rejected {
                sequence: 2,
                job_id: Some(10),
                code: "stale-share".into()
            }
        );
        assert_eq!((c.accepted(), c.rejected()), (1, 1));
        assert!(c.pending.is_empty());
    }

    #[test]
    fn set_target_changes_the_difficulty() {
        let mut c = open();
        let mut out = Vec::new();
        let mut t = [0u8; 32];
        t[26] = 0xFF;
        t[27] = 0x0F; // difficulty 16
        let mut w = Writer::new();
        w.u32(7).u256(&t);
        let ev = c
            .handle(&Frame::channel(msg::SET_TARGET, w.finish()), &mut out)
            .unwrap();
        let Event::Difficulty(d) = ev[0] else {
            panic!("{ev:?}")
        };
        assert!((d - 65535.0 / 4095.0).abs() < 1e-6);
        assert!((c.difficulty() - d).abs() < 1e-9);
    }

    #[test]
    fn refusals_are_errors_and_stray_messages_are_ignored() {
        let mut c = Client::new(cfg());
        let mut out = Vec::new();
        let mut w = Writer::new();
        w.u32(0).str0_255("unsupported-protocol");
        let e = c
            .handle(
                &Frame::new(msg::SETUP_CONNECTION_ERROR, w.finish()),
                &mut out,
            )
            .unwrap_err();
        assert_eq!(e, Error::Refused("setup: unsupported-protocol".into()));
        let mut c = Client::new(cfg());
        let ev = c.handle(&job(1, None), &mut out).unwrap();
        assert_eq!(ev, vec![Event::Ignored(msg::NEW_EXTENDED_MINING_JOB)]);
        assert!(c.submit(1, &[0; 4], 0, 0, 0).is_err());
        let mut c = open();
        let mut w = Writer::new();
        w.u32(7).str0_255("bye");
        let e = c
            .handle(&Frame::new(msg::CLOSE_CHANNEL, w.finish()), &mut out)
            .unwrap_err();
        assert!(matches!(e, Error::Refused(_)));
        let mut c = open();
        let ev = c.handle(&prev(99), &mut out).unwrap();
        assert!(matches!(ev[0], Event::Warn(_)));
        let mut w = Writer::new();
        w.str0_255("other.pool").u16(3333);
        let ev = c
            .handle(&Frame::new(msg::RECONNECT, w.finish()), &mut out)
            .unwrap();
        assert_eq!(
            ev[0],
            Event::Reconnect {
                host: "other.pool".into(),
                port: 3333
            }
        );
    }

    #[test]
    fn a_truncated_job_is_an_error_not_a_panic() {
        let mut c = open();
        let mut out = Vec::new();
        let mut f = job(10, None);
        f.payload.truncate(20);
        assert_eq!(c.handle(&f, &mut out), Err(Error::Truncated));
    }
}
