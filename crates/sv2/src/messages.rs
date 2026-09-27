//! The mining-protocol messages the client sends and receives (spec §7),
//! encoded by hand in wire order. Targets and hashes are copied as the
//! 32 bytes on the wire (little-endian numbers); the [`crate::channel`]
//! layer knows what to do with them.

use crate::wire::{Frame, Reader, Writer};
use crate::{msg, Error};

/// Client → pool, first message after the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupConnection {
    /// 0 = mining protocol.
    pub protocol: u8,
    /// Lowest protocol version the client speaks.
    pub min_version: u16,
    /// Highest protocol version the client speaks.
    pub max_version: u16,
    /// Bit 0 requires standard jobs, bit 1 work selection, bit 2 version
    /// rolling. An extended-channel miner sets none.
    pub flags: u32,
    /// Host the client connected to (informational).
    pub endpoint_host: String,
    /// Port the client connected to.
    pub endpoint_port: u16,
    /// Device vendor.
    pub vendor: String,
    /// Hardware version.
    pub hardware_version: String,
    /// Firmware version.
    pub firmware: String,
    /// Device identity (a serial, or empty).
    pub device_id: String,
}

impl SetupConnection {
    /// Encode as a frame.
    pub fn frame(&self) -> Frame {
        let mut w = Writer::new();
        w.u8(self.protocol)
            .u16(self.min_version)
            .u16(self.max_version)
            .u32(self.flags)
            .str0_255(&self.endpoint_host)
            .u16(self.endpoint_port)
            .str0_255(&self.vendor)
            .str0_255(&self.hardware_version)
            .str0_255(&self.firmware)
            .str0_255(&self.device_id);
        Frame::new(msg::SETUP_CONNECTION, w.finish())
    }

    /// Decode a payload (for tests and tools).
    pub fn decode(payload: &[u8]) -> Result<SetupConnection, Error> {
        let mut r = Reader::new(payload);
        Ok(SetupConnection {
            protocol: r.u8()?,
            min_version: r.u16()?,
            max_version: r.u16()?,
            flags: r.u32()?,
            endpoint_host: r.str0_255()?,
            endpoint_port: r.u16()?,
            vendor: r.str0_255()?,
            hardware_version: r.str0_255()?,
            firmware: r.str0_255()?,
            device_id: r.str0_255()?,
        })
    }
}

/// Pool → client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupConnectionSuccess {
    /// Protocol version in use.
    pub used_version: u16,
    /// Pool capabilities.
    pub flags: u32,
}

impl SetupConnectionSuccess {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(SetupConnectionSuccess {
            used_version: r.u16()?,
            flags: r.u32()?,
        })
    }
}

/// Pool → client: connection refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupConnectionError {
    /// Pool capabilities.
    pub flags: u32,
    /// Why.
    pub error_code: String,
}

impl SetupConnectionError {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(SetupConnectionError {
            flags: r.u32()?,
            error_code: r.str0_255()?,
        })
    }
}

/// Client → pool.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenExtendedMiningChannel {
    /// Echoed in the answer.
    pub request_id: u32,
    /// Worker name (address, or address.worker).
    pub user_identity: String,
    /// Expected hashrate in H/s, seeds the pool's first difficulty.
    pub nominal_hash_rate: f32,
    /// Largest target the miner accepts, little-endian. All 0xFF = any.
    pub max_target: [u8; 32],
    /// Smallest extranonce the miner needs to roll (bytes).
    pub min_extranonce_size: u16,
}

impl OpenExtendedMiningChannel {
    /// Encode as a frame.
    pub fn frame(&self) -> Frame {
        let mut w = Writer::new();
        w.u32(self.request_id)
            .str0_255(&self.user_identity)
            .f32(self.nominal_hash_rate)
            .u256(&self.max_target)
            .u16(self.min_extranonce_size);
        Frame::new(msg::OPEN_EXTENDED_MINING_CHANNEL, w.finish())
    }
}

/// Pool → client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenExtendedMiningChannelSuccess {
    /// The request this answers.
    pub request_id: u32,
    /// The channel's id, carried by every channel message.
    pub channel_id: u32,
    /// Share target, little-endian.
    pub target: [u8; 32],
    /// Bytes of extranonce the miner rolls itself.
    pub extranonce_size: u16,
    /// Bytes the pool fixes in front of the miner's extranonce.
    pub extranonce_prefix: Vec<u8>,
    /// Group the channel belongs to.
    pub group_channel_id: u32,
}

impl OpenExtendedMiningChannelSuccess {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        let request_id = r.u32()?;
        let channel_id = r.u32()?;
        let target = r.u256()?;
        let extranonce_size = r.u16()?;
        let extranonce_prefix = r.b0_255()?;
        if extranonce_prefix.len() > 32 {
            return Err(Error::TooLong);
        }
        let group_channel_id = r.u32()?;
        Ok(OpenExtendedMiningChannelSuccess {
            request_id,
            channel_id,
            target,
            extranonce_size,
            extranonce_prefix,
            group_channel_id,
        })
    }
}

/// Pool → client: channel refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenMiningChannelError {
    /// The request this answers.
    pub request_id: u32,
    /// Why.
    pub error_code: String,
}

impl OpenMiningChannelError {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(OpenMiningChannelError {
            request_id: r.u32()?,
            error_code: r.str0_255()?,
        })
    }
}

/// Pool → client: a job for an extended channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExtendedMiningJob {
    /// Channel.
    pub channel_id: u32,
    /// Job id, echoed in submits.
    pub job_id: u32,
    /// `None` = future job, activated by the next `SetNewPrevHash`;
    /// `Some(ntime)` = mine now on the current prev hash.
    pub min_ntime: Option<u32>,
    /// Block version.
    pub version: u32,
    /// The miner may roll BIP320 version bits.
    pub version_rolling_allowed: bool,
    /// Merkle branches beside the coinbase.
    pub merkle_path: Vec<[u8; 32]>,
    /// Coinbase bytes before the extranonce.
    pub coinbase_tx_prefix: Vec<u8>,
    /// Coinbase bytes after the extranonce.
    pub coinbase_tx_suffix: Vec<u8>,
}

impl NewExtendedMiningJob {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(NewExtendedMiningJob {
            channel_id: r.u32()?,
            job_id: r.u32()?,
            min_ntime: r.option_u32()?,
            version: r.u32()?,
            version_rolling_allowed: r.bool()?,
            merkle_path: r.seq0_255_u256()?,
            coinbase_tx_prefix: r.b0_64k()?,
            coinbase_tx_suffix: r.b0_64k()?,
        })
    }

    /// Encode as a frame (for tests and tools).
    pub fn frame(&self) -> Frame {
        let mut w = Writer::new();
        w.u32(self.channel_id).u32(self.job_id);
        match self.min_ntime {
            None => w.u8(0),
            Some(t) => w.u8(1).u32(t),
        };
        w.u32(self.version).bool(self.version_rolling_allowed);
        w.u8(self.merkle_path.len() as u8);
        for h in &self.merkle_path {
            w.u256(h);
        }
        w.u16(self.coinbase_tx_prefix.len() as u16);
        let mut b = w.finish();
        b.extend_from_slice(&self.coinbase_tx_prefix);
        b.extend_from_slice(&(self.coinbase_tx_suffix.len() as u16).to_le_bytes());
        b.extend_from_slice(&self.coinbase_tx_suffix);
        Frame::channel(msg::NEW_EXTENDED_MINING_JOB, b)
    }
}

/// Pool → client: a new block, activate the named future job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetNewPrevHash {
    /// Channel.
    pub channel_id: u32,
    /// The future job to mine now.
    pub job_id: u32,
    /// Previous block hash, little-endian (header byte order).
    pub prev_hash: [u8; 32],
    /// Smallest ntime allowed.
    pub min_ntime: u32,
    /// Compact network target.
    pub nbits: u32,
}

impl SetNewPrevHash {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(SetNewPrevHash {
            channel_id: r.u32()?,
            job_id: r.u32()?,
            prev_hash: r.u256()?,
            min_ntime: r.u32()?,
            nbits: r.u32()?,
        })
    }

    /// Encode as a frame (for tests and tools).
    pub fn frame(&self) -> Frame {
        let mut w = Writer::new();
        w.u32(self.channel_id)
            .u32(self.job_id)
            .u256(&self.prev_hash)
            .u32(self.min_ntime)
            .u32(self.nbits);
        Frame::channel(msg::SET_NEW_PREV_HASH, w.finish())
    }
}

/// Pool → client: new share target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTarget {
    /// Channel.
    pub channel_id: u32,
    /// Largest hash that counts as a share, little-endian.
    pub maximum_target: [u8; 32],
}

impl SetTarget {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(SetTarget {
            channel_id: r.u32()?,
            maximum_target: r.u256()?,
        })
    }
}

/// Pool → client: new extranonce prefix from the next job on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetExtranoncePrefix {
    /// Channel.
    pub channel_id: u32,
    /// The new prefix.
    pub extranonce_prefix: Vec<u8>,
}

impl SetExtranoncePrefix {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        let channel_id = r.u32()?;
        let extranonce_prefix = r.b0_255()?;
        if extranonce_prefix.len() > 32 {
            return Err(Error::TooLong);
        }
        Ok(SetExtranoncePrefix {
            channel_id,
            extranonce_prefix,
        })
    }
}

/// Client → pool: a share on an extended channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitSharesExtended {
    /// Channel.
    pub channel_id: u32,
    /// Counts up per channel.
    pub sequence_number: u32,
    /// The job the share is for.
    pub job_id: u32,
    /// Header nonce.
    pub nonce: u32,
    /// Header time.
    pub ntime: u32,
    /// Full header version, rolled bits included.
    pub version: u32,
    /// The miner's extranonce bytes (without the pool's prefix).
    pub extranonce: Vec<u8>,
}

impl SubmitSharesExtended {
    /// Encode as a frame.
    pub fn frame(&self) -> Result<Frame, Error> {
        let mut w = Writer::new();
        w.u32(self.channel_id)
            .u32(self.sequence_number)
            .u32(self.job_id)
            .u32(self.nonce)
            .u32(self.ntime)
            .u32(self.version)
            .b0_255(&self.extranonce)?;
        Ok(Frame::channel(msg::SUBMIT_SHARES_EXTENDED, w.finish()))
    }
}

/// Pool → client: shares accepted since the last success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitSharesSuccess {
    /// Channel.
    pub channel_id: u32,
    /// Highest sequence number accepted.
    pub last_sequence_number: u32,
    /// Shares accepted since the last success message.
    pub new_submits_accepted_count: u32,
    /// Difficulty sum of those shares.
    pub new_shares_sum: u64,
}

impl SubmitSharesSuccess {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(SubmitSharesSuccess {
            channel_id: r.u32()?,
            last_sequence_number: r.u32()?,
            new_submits_accepted_count: r.u32()?,
            new_shares_sum: r.u64()?,
        })
    }
}

/// Pool → client: one share rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitSharesError {
    /// Channel.
    pub channel_id: u32,
    /// The rejected share.
    pub sequence_number: u32,
    /// Why (`stale-share`, `difficulty-too-low`, …).
    pub error_code: String,
}

impl SubmitSharesError {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(SubmitSharesError {
            channel_id: r.u32()?,
            sequence_number: r.u32()?,
            error_code: r.str0_255()?,
        })
    }
}

/// Pool → client: the channel is closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseChannel {
    /// Channel.
    pub channel_id: u32,
    /// Why.
    pub reason_code: String,
}

impl CloseChannel {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(CloseChannel {
            channel_id: r.u32()?,
            reason_code: r.str0_255()?,
        })
    }
}

/// Pool → client: connect to another endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconnect {
    /// New host, empty = the same.
    pub new_host: String,
    /// New port, 0 = the same.
    pub new_port: u16,
}

impl Reconnect {
    /// Decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(payload);
        Ok(Reconnect {
            new_host: r.str0_255()?,
            new_port: r.u16()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_connection_round_trips_with_the_nerdos_layout() {
        let s = SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: 0,
            endpoint_host: "".into(),
            endpoint_port: 0,
            vendor: "NerdQAxe++".into(),
            hardware_version: "".into(),
            firmware: "".into(),
            device_id: "".into(),
        };
        let f = s.frame();
        assert_eq!(f.message_type, msg::SETUP_CONNECTION);
        assert_eq!(f.extension_type, 0);
        // 1+2+2+4 + (1+0) + 2 + (1+10) + 1 + 1 + 1 = 26
        assert_eq!(f.payload.len(), 26);
        assert_eq!(SetupConnection::decode(&f.payload).unwrap(), s);
    }

    #[test]
    fn open_extended_channel_is_ninety_six_bytes_like_the_live_trace() {
        // NerdQAxe → WarpPool: 96 B with a 53-character user identity.
        let o = OpenExtendedMiningChannel {
            request_id: 1,
            user_identity: "x".repeat(53),
            nominal_hash_rate: 1e12,
            max_target: [0xFF; 32],
            min_extranonce_size: 6,
        };
        let f = o.frame();
        assert_eq!(f.payload.len(), 4 + 1 + 53 + 4 + 32 + 2);
        assert_eq!(f.payload.len(), 96);
        assert_eq!(&f.payload[..4], &[1, 0, 0, 0]);
        assert_eq!(f.payload[4], 53);
        assert_eq!(&f.payload[94..], &[6, 0]);
    }

    #[test]
    fn open_extended_channel_success_needs_the_group_id() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&7u32.to_le_bytes());
        p.extend_from_slice(&[0xAB; 32]);
        p.extend_from_slice(&4u16.to_le_bytes());
        p.push(4);
        p.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(p.len(), 47);
        assert_eq!(
            OpenExtendedMiningChannelSuccess::decode(&p),
            Err(Error::Truncated)
        );
        p.extend_from_slice(&7u32.to_le_bytes());
        let ok = OpenExtendedMiningChannelSuccess::decode(&p).unwrap();
        assert_eq!(ok.channel_id, 7);
        assert_eq!(ok.extranonce_size, 4);
        assert_eq!(ok.extranonce_prefix, vec![1, 2, 3, 4]);
        assert_eq!(ok.group_channel_id, 7);
    }

    #[test]
    fn extended_job_round_trips_with_and_without_min_ntime() {
        for min_ntime in [None, Some(1_700_000_000u32)] {
            let j = NewExtendedMiningJob {
                channel_id: 3,
                job_id: 9,
                min_ntime,
                version: 0x2000_0000,
                version_rolling_allowed: true,
                merkle_path: vec![[1u8; 32], [2u8; 32]],
                coinbase_tx_prefix: vec![0xAA; 50],
                coinbase_tx_suffix: vec![0xBB; 60],
            };
            let f = j.frame();
            assert_eq!(f.extension_type, crate::CHANNEL_MSG_FLAG);
            let back = NewExtendedMiningJob::decode(&f.payload).unwrap();
            assert_eq!(back, j);
            // Every truncation is an error, never a panic.
            for n in 0..f.payload.len() {
                assert!(NewExtendedMiningJob::decode(&f.payload[..n]).is_err());
            }
        }
    }

    #[test]
    fn set_new_prev_hash_is_forty_eight_bytes() {
        let m = SetNewPrevHash {
            channel_id: 1,
            job_id: 2,
            prev_hash: [3u8; 32],
            min_ntime: 4,
            nbits: 0x1703_0000,
        };
        let f = m.frame();
        assert_eq!(f.payload.len(), 48);
        assert_eq!(SetNewPrevHash::decode(&f.payload).unwrap(), m);
    }

    #[test]
    fn submit_extended_carries_the_miner_extranonce_only() {
        let s = SubmitSharesExtended {
            channel_id: 1,
            sequence_number: 5,
            job_id: 9,
            nonce: 0xdead_beef,
            ntime: 1_700_000_000,
            version: 0x2000_0000,
            extranonce: vec![0, 0, 0, 1],
        };
        let f = s.frame().unwrap();
        assert_eq!(f.extension_type, crate::CHANNEL_MSG_FLAG);
        assert_eq!(f.payload.len(), 24 + 1 + 4);
        assert_eq!(f.payload[24], 4);
        let long = SubmitSharesExtended {
            extranonce: vec![0; 300],
            ..s
        };
        assert_eq!(long.frame(), Err(Error::TooLong));
    }

    #[test]
    fn results_and_errors_decode() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&9u32.to_le_bytes());
        p.extend_from_slice(&2u32.to_le_bytes());
        p.extend_from_slice(&512u64.to_le_bytes());
        let s = SubmitSharesSuccess::decode(&p).unwrap();
        assert_eq!(
            (s.last_sequence_number, s.new_submits_accepted_count),
            (9, 2)
        );
        assert_eq!(s.new_shares_sum, 512);
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&3u32.to_le_bytes());
        p.push(11);
        p.extend_from_slice(b"stale-share");
        let e = SubmitSharesError::decode(&p).unwrap();
        assert_eq!(
            (e.sequence_number, e.error_code.as_str()),
            (3, "stale-share")
        );
        let mut p = 0u32.to_le_bytes().to_vec();
        p.push(5);
        p.extend_from_slice(b"unsup");
        assert_eq!(
            SetupConnectionError::decode(&p).unwrap().error_code,
            "unsup"
        );
        let mut p = Vec::new();
        p.push(4);
        p.extend_from_slice(b"host");
        p.extend_from_slice(&3333u16.to_le_bytes());
        let r = Reconnect::decode(&p).unwrap();
        assert_eq!((r.new_host.as_str(), r.new_port), ("host", 3333));
    }
}
