//! Turning a UART byte stream into chip responses, with resync.
//!
//! The chip answers in fixed 11-byte frames (`AA 55` + 8 payload + flags:crc),
//! but the UART delivers bytes, not frames: a byte can be lost, a burst can
//! arrive mid-frame after a baud switch, a payload can contain `AA 55` by
//! chance. The reference driver reads exactly eleven bytes and, when they
//! do not check out, flushes the UART — which drops whatever came after,
//! frame or not, and reports nothing about it.
//!
//! [`RxDecoder`] instead keeps a window of at most eleven bytes and
//! resynchronises **one byte at a time**: when the window is full and does
//! not verify, the first byte is dropped and the search for `AA 55` resumes
//! inside the remaining bytes. Every frame boundary is tried, so a valid
//! frame is found wherever it starts, and every byte that was not part of
//! a valid frame is counted. That count is the desync metric: a rising
//! number of dropped bytes per accepted frame is a signal-integrity
//! problem announcing itself before the hashrate does.
//!
//! No allocation, no clock: feed bytes, get frames. The decoder knows
//! framing only; what a payload means is the chip profile's business.

use crate::frame::{decode_response, Response, RESPONSE_LEN, RX_PREAMBLE};

/// A response frame the decoder found, and how many bytes it had to
/// discard since the previous frame to get there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The verified response.
    pub response: Response,
    /// Bytes dropped between the previous accepted frame (or the start)
    /// and this one. Zero on a clean stream; anything else is a desync.
    pub dropped_before: u32,
}

/// Running totals since the decoder was created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames accepted.
    pub frames: u32,
    /// Bytes that were not part of an accepted frame.
    pub dropped: u32,
    /// Full windows that started with `AA 55` and failed their CRC. Each
    /// costs at least one dropped byte; the count is kept separately
    /// because a CRC failure on a well-framed reply is a different
    /// symptom from garbage between frames.
    pub crc_failures: u32,
}

/// Incremental frame decoder for the chip's UART.
#[derive(Debug, Clone)]
pub struct RxDecoder {
    buf: [u8; RESPONSE_LEN],
    len: usize,
    /// Bytes per frame right now: 11, or 9 while a BM1373 chain answers
    /// the chip-id read.
    frame_len: usize,
    dropped_since_frame: u32,
    stats: Stats,
}

impl Default for RxDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RxDecoder {
    /// A decoder with an empty window.
    pub const fn new() -> Self {
        RxDecoder {
            buf: [0; RESPONSE_LEN],
            len: 0,
            frame_len: RESPONSE_LEN,
            dropped_since_frame: 0,
            stats: Stats {
                frames: 0,
                dropped: 0,
                crc_failures: 0,
            },
        }
    }

    /// Running totals.
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Expect frames of `len` bytes (9 or 11) from now on; anything
    /// half-received is dropped. Chip-id replies of the BM1373 are 9
    /// bytes, everything else 11.
    pub fn set_frame_len(&mut self, len: usize) {
        let len = len.clamp(crate::frame::SHORT_RESPONSE_LEN, RESPONSE_LEN);
        if len != self.frame_len {
            self.frame_len = len;
            self.reset();
        }
    }

    /// Bytes currently held that may still become a frame.
    pub const fn pending(&self) -> usize {
        self.len
    }

    /// Bytes dropped since the last accepted frame. Useful when no frame
    /// arrives at all: a chip that has gone quiet leaves this at zero, a
    /// chip that sends garbage makes it climb.
    pub const fn dropped_since_frame(&self) -> u32 {
        self.dropped_since_frame
    }

    /// Feed one byte. Returns a frame when this byte completes one.
    pub fn push(&mut self, byte: u8) -> Option<Frame> {
        // Below two bytes we are hunting for the preamble; nothing else
        // is worth keeping.
        if self.len == 0 {
            if byte == RX_PREAMBLE[0] {
                self.buf[0] = byte;
                self.len = 1;
            } else {
                self.drop_bytes(1);
            }
            return None;
        }
        if self.len == 1 {
            if byte == RX_PREAMBLE[1] {
                self.buf[1] = byte;
                self.len = 2;
            } else if byte == RX_PREAMBLE[0] {
                // `AA AA`: the second one may start a real preamble.
                self.drop_bytes(1);
            } else {
                // Both the held `AA` and this byte are gone.
                self.drop_bytes(2);
                self.len = 0;
            }
            return None;
        }

        self.buf[self.len] = byte;
        self.len += 1;
        if self.len < self.frame_len {
            return None;
        }

        // Window full and starts with the preamble: verify.
        match decode_response(&self.buf[..self.frame_len]) {
            Ok(response) => {
                let dropped_before = self.dropped_since_frame;
                self.dropped_since_frame = 0;
                self.stats.frames += 1;
                self.len = 0;
                Some(Frame {
                    response,
                    dropped_before,
                })
            }
            Err(_) => {
                self.stats.crc_failures += 1;
                self.resync();
                None
            }
        }
    }

    /// Feed a slice, calling `on_frame` for each frame completed. The
    /// result is identical to pushing the bytes one at a time.
    pub fn push_slice(&mut self, bytes: &[u8], mut on_frame: impl FnMut(Frame)) {
        for &b in bytes {
            if let Some(f) = self.push(b) {
                on_frame(f);
            }
        }
    }

    /// Discard everything held. Use after a deliberate break in the
    /// stream (a baud-rate switch, a chip reset), when the bytes in the
    /// window belong to the old world. They count as dropped.
    pub fn reset(&mut self) {
        let n = self.len as u32;
        self.len = 0;
        self.drop_bytes(n);
    }

    fn drop_bytes(&mut self, n: u32) {
        self.dropped_since_frame = self.dropped_since_frame.saturating_add(n);
        self.stats.dropped = self.stats.dropped.saturating_add(n);
    }

    /// The window is full and invalid. Drop its first byte, then slide
    /// until the window starts with the preamble again or has fewer than
    /// two bytes. Every byte slid out counts as dropped.
    fn resync(&mut self) {
        debug_assert_eq!(self.len, self.frame_len);
        let mut start = 1;
        while self.len - start >= 2 && self.buf[start..start + 2] != RX_PREAMBLE {
            start += 1;
        }
        // Fewer than two bytes left that could be a preamble: a lone
        // `AA` at the very end is worth keeping, anything else is not.
        if self.len - start < 2 {
            while start < self.len && self.buf[start] != RX_PREAMBLE[0] {
                start += 1;
            }
        }
        self.drop_bytes(start as u32);
        self.buf.copy_within(start..self.len, 0);
        self.len -= start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{encode_response, encode_short_response, FLAG_JOB, SHORT_RESPONSE_LEN};

    #[test]
    fn short_mode_decodes_nine_byte_chip_id_replies_then_returns_to_eleven() {
        let mut d = RxDecoder::new();
        d.set_frame_len(SHORT_RESPONSE_LEN);
        let mut short = [0u8; SHORT_RESPONSE_LEN];
        encode_short_response(&mut short, &[0x13, 0x72, 0x00, 0x10, 0, 0], 0x00);
        let mut got = Vec::new();
        d.push_slice(&short, |f| got.push(f));
        assert_eq!(got.len(), 1);
        assert_eq!(&got[0].response.payload[..4], &[0x13, 0x72, 0x00, 0x10]);
        assert!(!got[0].response.is_job());
        // Back to full frames: an eleven-byte job reply decodes whole.
        d.set_frame_len(RESPONSE_LEN);
        let mut full = [0u8; RESPONSE_LEN];
        encode_response(&mut full, &[1, 2, 3, 4, 5, 6, 7, 8], FLAG_JOB);
        got.clear();
        d.push_slice(&full, |f| got.push(f));
        assert_eq!(got.len(), 1);
        assert!(got[0].response.is_job());
        assert_eq!(d.stats().crc_failures, 0);
    }

    fn frame_bytes(payload: [u8; 8], flags: u8) -> [u8; RESPONSE_LEN] {
        let mut b = [0u8; RESPONSE_LEN];
        encode_response(&mut b, &payload, flags);
        b
    }

    fn collect(d: &mut RxDecoder, bytes: &[u8]) -> Vec<Frame> {
        let mut out = Vec::new();
        d.push_slice(bytes, |f| out.push(f));
        out
    }

    #[test]
    fn a_clean_frame_comes_out_with_nothing_dropped() {
        let mut d = RxDecoder::new();
        let f = frame_bytes([1, 2, 3, 4, 5, 6, 7, 8], FLAG_JOB);
        let got = collect(&mut d, &f);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].dropped_before, 0);
        assert!(got[0].response.is_job());
        assert_eq!(got[0].response.payload, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            d.stats(),
            Stats {
                frames: 1,
                dropped: 0,
                crc_failures: 0
            }
        );
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn garbage_before_a_frame_is_counted_on_that_frame() {
        let mut d = RxDecoder::new();
        let mut stream = vec![0x00, 0xFF, 0x13, 0x55, 0x55];
        stream.extend_from_slice(&frame_bytes([9; 8], 0));
        let got = collect(&mut d, &stream);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].dropped_before, 5);
        assert_eq!(d.stats().dropped, 5);
    }

    #[test]
    fn back_to_back_frames_are_all_found() {
        let mut d = RxDecoder::new();
        let mut stream = Vec::new();
        for i in 0..5u8 {
            stream.extend_from_slice(&frame_bytes([i; 8], if i % 2 == 0 { FLAG_JOB } else { 0 }));
        }
        let got = collect(&mut d, &stream);
        assert_eq!(got.len(), 5);
        for (i, f) in got.iter().enumerate() {
            assert_eq!(f.response.payload, [i as u8; 8]);
            assert_eq!(f.dropped_before, 0);
        }
        assert_eq!(d.stats().frames, 5);
    }

    /// The case the one-byte resync exists for: a payload that happens to
    /// contain the preamble must not derail the frame after it.
    #[test]
    fn a_preamble_inside_a_payload_does_not_derail_the_next_frame() {
        let mut d = RxDecoder::new();
        let first = frame_bytes([0x11, 0xAA, 0x55, 0x22, 0xAA, 0x55, 0x33, 0x44], FLAG_JOB);
        let second = frame_bytes([0xFF; 8], 0);
        let mut stream = first.to_vec();
        stream.extend_from_slice(&second);
        let got = collect(&mut d, &stream);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].response.payload[1..3], [0xAA, 0x55]);
        assert_eq!(got[1].response.payload, [0xFF; 8]);
        assert_eq!(d.stats().dropped, 0);
    }

    /// A corrupted frame followed by a good one: the good one is found,
    /// the corrupted bytes are all charged as dropped, and the CRC
    /// failure is recorded once.
    #[test]
    fn a_corrupt_frame_is_dropped_byte_by_byte_and_the_next_one_recovered() {
        let mut d = RxDecoder::new();
        let mut bad = frame_bytes([0x10; 8], FLAG_JOB);
        bad[6] ^= 0x01;
        let good = frame_bytes([0x20; 8], 0);
        let mut stream = bad.to_vec();
        stream.extend_from_slice(&good);
        let got = collect(&mut d, &stream);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].response.payload, [0x20; 8]);
        assert_eq!(got[0].dropped_before, RESPONSE_LEN as u32);
        assert_eq!(d.stats().crc_failures, 1);
    }

    /// Lose one byte in the middle of a frame — the classic UART hiccup.
    /// The truncated frame is unrecoverable, but the next one must be.
    #[test]
    fn a_frame_with_a_missing_byte_does_not_cost_the_next_frame() {
        let mut d = RxDecoder::new();
        let mut truncated = frame_bytes([0x30; 8], FLAG_JOB).to_vec();
        truncated.remove(5);
        let good = frame_bytes([0x40; 8], FLAG_JOB);
        let mut stream = truncated.clone();
        stream.extend_from_slice(&good);
        let got = collect(&mut d, &stream);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].response.payload, [0x40; 8]);
        assert_eq!(got[0].dropped_before, truncated.len() as u32);
    }

    /// A stream that joins mid-frame (after a baud switch, say): the tail
    /// of the old frame is dropped, the first complete one is found.
    #[test]
    fn joining_mid_frame_recovers_at_the_next_boundary() {
        let mut d = RxDecoder::new();
        let old = frame_bytes([0x50; 8], 0);
        let new = frame_bytes([0x60; 8], FLAG_JOB);
        let mut stream = old[4..].to_vec();
        stream.extend_from_slice(&new);
        let got = collect(&mut d, &stream);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].response.payload, [0x60; 8]);
        assert_eq!(got[0].dropped_before, 7);
    }

    /// `AA AA 55 …`: the first `AA` is noise, the second starts a frame.
    #[test]
    fn a_doubled_preamble_byte_resyncs_on_the_second() {
        let mut d = RxDecoder::new();
        let mut stream = vec![0xAA];
        stream.extend_from_slice(&frame_bytes([0x70; 8], 0));
        let got = collect(&mut d, &stream);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].dropped_before, 1);
    }

    #[test]
    fn reset_discards_the_window_and_counts_it() {
        let mut d = RxDecoder::new();
        let f = frame_bytes([0x80; 8], 0);
        assert!(collect(&mut d, &f[..6]).is_empty());
        assert_eq!(d.pending(), 6);
        d.reset();
        assert_eq!(d.pending(), 0);
        assert_eq!(d.stats().dropped, 6);
        // and a frame after the reset is clean
        let got = collect(&mut d, &f);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].dropped_before, 6);
    }

    #[test]
    fn dropped_since_frame_climbs_on_garbage_and_stays_zero_on_silence() {
        let mut d = RxDecoder::new();
        assert_eq!(d.dropped_since_frame(), 0);
        collect(&mut d, &[0x01, 0x02, 0x03]);
        assert_eq!(d.dropped_since_frame(), 3);
        collect(&mut d, &frame_bytes([0; 8], 0));
        assert_eq!(d.dropped_since_frame(), 0);
    }

    proptest::proptest! {
        /// Frames separated by junk that cannot contain a preamble byte:
        /// every frame is found, in order, and the junk is exactly what
        /// gets charged as dropped.
        #[test]
        fn every_frame_is_found_between_preamble_free_junk(
            frames in proptest::collection::vec(
                (proptest::array::uniform8(0u8..=255), 0u8..=255), 0..8),
            gaps in proptest::collection::vec(
                proptest::collection::vec(0u8..=255, 0..16), 9),
        ) {
            let mut stream = Vec::new();
            let mut expected_drop = Vec::new();
            for (i, (payload, flags)) in frames.iter().enumerate() {
                let junk: Vec<u8> = gaps[i].iter().map(|&b| if b == 0xAA { 0xAB } else { b }).collect();
                expected_drop.push(junk.len() as u32);
                stream.extend_from_slice(&junk);
                stream.extend_from_slice(&frame_bytes(*payload, *flags));
            }
            let trailing: Vec<u8> = gaps[frames.len()].iter().map(|&b| if b == 0xAA { 0xAB } else { b }).collect();
            stream.extend_from_slice(&trailing);

            let mut d = RxDecoder::new();
            let got = collect(&mut d, &stream);
            proptest::prop_assert_eq!(got.len(), frames.len());
            for (i, f) in got.iter().enumerate() {
                proptest::prop_assert_eq!(f.response.payload, frames[i].0);
                proptest::prop_assert_eq!(f.response.flags, frames[i].1 & 0xE0);
                proptest::prop_assert_eq!(f.dropped_before, expected_drop[i]);
            }
            let total_junk: u32 = expected_drop.iter().sum::<u32>() + trailing.len() as u32;
            proptest::prop_assert_eq!(d.stats().dropped, total_junk);
            proptest::prop_assert_eq!(d.stats().frames as usize, frames.len());
        }

        /// Any byte stream at all: no panic, every frame produced verifies,
        /// and bytes are conserved — each one is either in an accepted
        /// frame, dropped, or still pending.
        #[test]
        fn arbitrary_bytes_never_panic_and_bytes_are_conserved(
            stream in proptest::collection::vec(0u8..=255, 0..200),
        ) {
            let mut d = RxDecoder::new();
            let got = collect(&mut d, &stream);
            for f in &got {
                let mut b = [0u8; RESPONSE_LEN];
                encode_response(&mut b, &f.response.payload, f.response.flags);
                proptest::prop_assert!(decode_response(&b).is_ok());
            }
            let s = d.stats();
            let accounted = s.frames as usize * RESPONSE_LEN + s.dropped as usize + d.pending();
            proptest::prop_assert_eq!(accounted, stream.len());
        }

        /// Chunking does not matter: one byte at a time and all at once
        /// give the same frames and the same statistics.
        #[test]
        fn chunking_is_irrelevant(
            stream in proptest::collection::vec(0u8..=255, 0..120),
            cut in 0usize..120,
        ) {
            let cut = cut.min(stream.len());
            let mut a = RxDecoder::new();
            let got_a = collect(&mut a, &stream);
            let mut b = RxDecoder::new();
            let mut got_b = collect(&mut b, &stream[..cut]);
            got_b.extend(collect(&mut b, &stream[cut..]));
            proptest::prop_assert_eq!(got_a, got_b);
            proptest::prop_assert_eq!(a.stats(), b.stats());
            proptest::prop_assert_eq!(a.pending(), b.pending());
        }
    }
}
