//! The Noise NX handshake and the encrypted transport, on top of
//! `noise_sv2` (the crate the WarpPool server uses on its side).
//!
//! Handshake: the client sends its 64-byte EllSwift ephemeral key
//! ([`ACT_1_LEN`]), the pool answers with 234 bytes ([`ACT_2_LEN`]): its
//! ephemeral key, its encrypted static key and the encrypted, Schnorr-
//! signed certificate. With an authority key the certificate is checked
//! against it and against the clock; without one the pool is trusted as
//! it is.
//!
//! Transport: each frame is two AEAD records, the 6-byte header (22 bytes
//! on the wire) and the payload (N + 16 bytes), each with its own nonce.
//! A frame with an empty payload is the header record alone.

use crate::wire::{Frame, HEADER_LEN};
use crate::Error;
use noise_sv2::{Initiator, NoiseCodec};

/// Bytes the client sends first.
pub const ACT_1_LEN: usize = 64;
/// Bytes the pool answers with.
pub const ACT_2_LEN: usize = 234;
/// Poly1305 tag on every record.
pub const TAG_LEN: usize = 16;
/// Largest payload the transport accepts. Jobs are a few hundred bytes;
/// the spec allows 16 MiB, the board has no such memory.
pub const MAX_PAYLOAD: usize = 8 * 1024;

const ENC_HEADER_LEN: usize = HEADER_LEN + TAG_LEN;

/// The client side of the handshake.
pub struct Handshake {
    inner: Box<Initiator>,
}

impl Handshake {
    /// Start a handshake; the pool's certificate is checked against
    /// `authority` when given. Returns the 64 bytes to send first.
    pub fn start(authority: Option<[u8; 32]>) -> Result<(Handshake, [u8; ACT_1_LEN]), Error> {
        let mut inner = match authority {
            Some(k) => {
                let pk = secp256k1::XOnlyPublicKey::from_slice(&k).map_err(|_| Error::BadKey)?;
                Initiator::new(Some(pk))
            }
            None => Initiator::without_pk().map_err(|e| Error::Handshake(format!("{e:?}")))?,
        };
        let act1 = inner
            .step_0()
            .map_err(|e| Error::Handshake(format!("{e:?}")))?;
        Ok((Handshake { inner }, act1))
    }

    /// Finish with the pool's 234 bytes; `now` is the Unix time for the
    /// certificate validity check (the pool signs a window of days, with
    /// 10 s of leeway). A certificate that fails is [`Error::Certificate`]
    /// with its window and the clock, so the caller can tell a wrong
    /// clock from a wrong pool.
    pub fn finish(mut self, act2: [u8; ACT_2_LEN], now: u32) -> Result<Transport, Error> {
        let codec = self.inner.step_2_with_now(act2, now).map_err(|e| match e {
            noise_sv2::Error::InvalidCertificate(m) => Error::Certificate(format!(
                "not verified: valid {}..{}, clock {now}",
                m.valid_from, m.not_valid_after
            )),
            e => Error::Handshake(format!("{e:?}")),
        })?;
        Ok(Transport {
            codec,
            rx: Vec::with_capacity(1024),
            pending: None,
        })
    }
}

/// The encrypted frame transport after the handshake.
pub struct Transport {
    codec: NoiseCodec,
    /// Ciphertext received and not yet consumed.
    rx: Vec<u8>,
    /// A decrypted header waiting for its payload record.
    pending: Option<(Frame, usize)>,
}

impl Transport {
    /// Encrypt a frame for the wire.
    pub fn seal(&mut self, frame: &Frame) -> Result<Vec<u8>, Error> {
        let mut out = Vec::with_capacity(ENC_HEADER_LEN + frame.payload.len() + TAG_LEN);
        let mut header = frame.header().to_vec();
        self.codec
            .encrypt(&mut header)
            .map_err(|_| Error::Decrypt)?;
        out.extend_from_slice(&header);
        if !frame.payload.is_empty() {
            let mut payload = frame.payload.clone();
            self.codec
                .encrypt(&mut payload)
                .map_err(|_| Error::Decrypt)?;
            out.extend_from_slice(&payload);
        }
        Ok(out)
    }

    /// Hand over bytes from the socket.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.rx.extend_from_slice(bytes);
    }

    /// Bytes buffered and not yet part of a complete frame.
    pub fn buffered(&self) -> usize {
        self.rx.len()
    }

    /// The next complete frame, if the buffered bytes hold one.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, Error> {
        if self.pending.is_none() {
            if self.rx.len() < ENC_HEADER_LEN {
                return Ok(None);
            }
            let mut header: Vec<u8> = self.rx.drain(..ENC_HEADER_LEN).collect();
            self.codec
                .decrypt(&mut header)
                .map_err(|_| Error::Decrypt)?;
            let mut h = [0u8; HEADER_LEN];
            h.copy_from_slice(&header[..HEADER_LEN]);
            let (ext, ty, len) = Frame::parse_header(&h);
            if len > MAX_PAYLOAD {
                return Err(Error::FrameTooLarge(len));
            }
            let frame = Frame {
                extension_type: ext,
                message_type: ty,
                payload: Vec::new(),
            };
            if len == 0 {
                return Ok(Some(frame));
            }
            self.pending = Some((frame, len + TAG_LEN));
        }
        let owed = self.pending.as_ref().map_or(0, |p| p.1);
        if self.rx.len() < owed {
            return Ok(None);
        }
        let (mut frame, _) = self.pending.take().expect("checked above");
        let mut payload: Vec<u8> = self.rx.drain(..owed).collect();
        self.codec
            .decrypt(&mut payload)
            .map_err(|_| Error::Decrypt)?;
        frame.payload = payload;
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noise_sv2::Responder;
    use std::time::Duration;

    /// A fixed authority key pair.
    fn authority() -> ([u8; 32], [u8; 32]) {
        let secp = secp256k1::Secp256k1::new();
        let secret: [u8; 32] = core::array::from_fn(|i| (i as u8) * 3 + 1);
        let kp = secp256k1::Keypair::from_seckey_slice(&secp, &secret).unwrap();
        (kp.secret_bytes(), kp.x_only_public_key().0.serialize())
    }

    fn now() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32
    }

    /// Both ends after a handshake with certificate verification.
    fn pair() -> (Transport, NoiseCodec) {
        let (priv_, pub_) = authority();
        let mut pool =
            Responder::from_authority_kp(&pub_, &priv_, Duration::from_secs(3600)).unwrap();
        let (hs, act1) = Handshake::start(Some(pub_)).unwrap();
        let (act2, pool_codec) = pool.step_1(act1).unwrap();
        assert_eq!(act2.len(), ACT_2_LEN);
        let t = hs.finish(act2, now()).unwrap();
        (t, pool_codec)
    }

    #[test]
    fn handshake_verifies_the_certificate() {
        let _ = pair();
        // Wrong authority key: the certificate does not verify.
        let (priv_, pub_) = authority();
        let mut pool =
            Responder::from_authority_kp(&pub_, &priv_, Duration::from_secs(3600)).unwrap();
        let mut wrong = pub_;
        wrong[5] ^= 0x01;
        let (hs, act1) = match Handshake::start(Some(wrong)) {
            Ok(x) => x,
            Err(Error::BadKey) => return, // not a valid x coordinate: refused earlier
            Err(e) => panic!("{e}"),
        };
        let (act2, _) = pool.step_1(act1).unwrap();
        assert!(matches!(hs.finish(act2, now()), Err(Error::Certificate(_))));
        // Clock far outside the validity window: refused too, and the
        // error names the window and the clock.
        let (hs, act1) = Handshake::start(Some(pub_)).unwrap();
        let (act2, _) = pool.step_1(act1).unwrap();
        let late = now() + 86_400 * 30;
        match hs.finish(act2, late) {
            Err(Error::Certificate(s)) => {
                assert!(s.contains(&format!("clock {late}")), "{s}");
                assert!(s.contains("valid "), "{s}");
            }
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("a certificate verified against a clock a month ahead"),
        }
        // Without a key the pool is taken as it is.
        let (hs, act1) = Handshake::start(None).unwrap();
        let (act2, _) = pool.step_1(act1).unwrap();
        assert!(hs.finish(act2, 0).is_ok());
    }

    #[test]
    fn frames_round_trip_in_any_fragmentation() {
        let (mut client, mut pool) = pair();
        // Pool → client: two records per frame, fed one byte at a time.
        let frame = Frame::channel(0x1F, (0..300u32).map(|i| i as u8).collect());
        let mut hdr = frame.header().to_vec();
        pool.encrypt(&mut hdr).unwrap();
        let mut body = frame.payload.clone();
        pool.encrypt(&mut body).unwrap();
        let wire = [hdr, body].concat();
        assert_eq!(wire.len(), 22 + 300 + 16);
        for (i, b) in wire.iter().enumerate() {
            client.feed(&[*b]);
            let got = client.next_frame().unwrap();
            if i + 1 < wire.len() {
                assert!(got.is_none(), "frame complete at byte {i}");
            } else {
                assert_eq!(got.unwrap(), frame);
            }
        }
        assert_eq!(client.buffered(), 0);
        // An empty payload is the header record alone; two frames in one read.
        let empty = Frame::new(0x01, Vec::new());
        let mut hdr = empty.header().to_vec();
        pool.encrypt(&mut hdr).unwrap();
        let mut hdr2 = frame.header().to_vec();
        pool.encrypt(&mut hdr2).unwrap();
        let mut body2 = frame.payload.clone();
        pool.encrypt(&mut body2).unwrap();
        client.feed(&[hdr, hdr2, body2].concat());
        assert_eq!(client.next_frame().unwrap().unwrap(), empty);
        assert_eq!(client.next_frame().unwrap().unwrap(), frame);
        assert_eq!(client.next_frame().unwrap(), None);
        // Client → pool: the pool decrypts what `seal` produced.
        let sealed = client.seal(&frame).unwrap();
        assert_eq!(sealed.len(), 22 + 300 + 16);
        let mut h = sealed[..22].to_vec();
        pool.decrypt(&mut h).unwrap();
        assert_eq!(h, frame.header());
        let mut b = sealed[22..].to_vec();
        pool.decrypt(&mut b).unwrap();
        assert_eq!(b, frame.payload);
        let sealed = client.seal(&empty).unwrap();
        assert_eq!(sealed.len(), 22);
    }

    #[test]
    fn tampering_and_oversize_are_errors() {
        let (mut client, mut pool) = pair();
        let frame = Frame::new(0x01, vec![1, 2, 3]);
        let mut hdr = frame.header().to_vec();
        pool.encrypt(&mut hdr).unwrap();
        hdr[3] ^= 0xFF;
        client.feed(&hdr);
        assert_eq!(client.next_frame(), Err(Error::Decrypt));
        let (mut client, mut pool) = pair();
        let big = Frame::new(0x01, vec![0; MAX_PAYLOAD + 1]);
        let mut hdr = big.header().to_vec();
        pool.encrypt(&mut hdr).unwrap();
        client.feed(&hdr);
        assert_eq!(
            client.next_frame(),
            Err(Error::FrameTooLarge(MAX_PAYLOAD + 1))
        );
    }
}
