//! The core dump as it leaves the miner.
//!
//! The dump is the task stacks and registers at the crash, written to
//! flash as the ESP-IDF writes it, and a task may have had a password,
//! the Wi-Fi name or the worker's address in hand at that moment. People
//! send their dumps to support or post them, so the download replaces
//! every such text with `x` of the same length (the ELF's offsets stay
//! where they are) and puts the CRC32 the dump ends with right again, so
//! `esp-coredump` still loads it. The copy in flash stays as written.
//!
//! Layout (ESP-IDF, ELF format with `CONFIG_ESP_COREDUMP_CHECKSUM_CRC32`):
//! a header, the ELF, and a little-endian CRC32 (zlib's) over everything
//! before it, header included.

/// What a masked byte becomes.
pub const MASK_BYTE: u8 = b'x';
/// Texts shorter than this are not searched for (the pool default "x").
pub const MIN_LEN: usize = crate::scrub::MIN_SECRET_LEN;
/// The trailing checksum's size.
pub const CRC_LEN: usize = 4;

/// Masks a dump streamed through it in pieces of any size. Feed the
/// dump without its last [`CRC_LEN`] bytes through [`push`](Self::push),
/// then [`finish`](Self::finish) gives the rest and the new checksum.
#[derive(Debug)]
pub struct DumpMasker {
    texts: Vec<Vec<u8>>,
    /// Bytes not yet given out: a text may start in them and end in the
    /// next piece.
    pending: Vec<u8>,
    /// Positions in `pending` before this are done (a mask may reach
    /// past `limit`; the scan goes on after it).
    scanned: usize,
    crc: Crc32,
    masked: usize,
}

impl DumpMasker {
    /// A masker for these texts; empty ones and ones shorter than
    /// [`MIN_LEN`] bytes are left out.
    pub fn new<I, S>(texts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut texts: Vec<Vec<u8>> = texts
            .into_iter()
            .map(|s| s.as_ref().as_bytes().to_vec())
            .filter(|t| t.len() >= MIN_LEN)
            .collect();
        // Longest first, so a text that contains another goes whole.
        texts.sort_by_key(|t| std::cmp::Reverse(t.len()));
        texts.dedup();
        DumpMasker {
            texts,
            pending: Vec::new(),
            scanned: 0,
            crc: Crc32::new(),
            masked: 0,
        }
    }

    fn keep(&self) -> usize {
        self.texts.first().map_or(0, |t| t.len() - 1)
    }

    /// Masks every text that starts before `limit` in `pending`, longest
    /// first at each position, scanning left to right: a short text is
    /// only taken where no longer one starts, and a match that starts
    /// before `limit` ends inside `pending` because `limit` leaves room
    /// for the longest text.
    fn mask_from(&mut self, start: usize, limit: usize) {
        let mut i = start;
        while i < limit {
            let hit = self.texts.iter().find(|t| {
                i + t.len() <= self.pending.len() && self.pending[i..i + t.len()] == t[..]
            });
            match hit.map(|t| t.len()) {
                Some(n) => {
                    self.pending[i..i + n].fill(MASK_BYTE);
                    self.masked += 1;
                    i += n;
                }
                None => i += 1,
            }
        }
        self.scanned = limit.max(i);
    }

    /// The next piece of the dump; returns what may go out now.
    pub fn push(&mut self, piece: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(piece);
        // Only the last `keep` bytes may still be the beginning of a text
        // whose end has not arrived yet.
        let cut = self.pending.len().saturating_sub(self.keep());
        let from = self.scanned;
        if cut > from {
            self.mask_from(from, cut);
        }
        let out: Vec<u8> = self.pending.drain(..cut).collect();
        self.scanned = self.scanned.saturating_sub(cut);
        self.crc.update(&out);
        out
    }

    /// The rest of the dump and the CRC32 to send in place of the stored
    /// one, little-endian.
    pub fn finish(&mut self) -> (Vec<u8>, [u8; CRC_LEN]) {
        let (from, end) = (self.scanned, self.pending.len());
        if end > from {
            self.mask_from(from, end);
        }
        let rest = std::mem::take(&mut self.pending);
        self.crc.update(&rest);
        (rest, self.crc.finish().to_le_bytes())
    }

    /// How many texts were masked so far.
    pub fn masked(&self) -> usize {
        self.masked
    }
}

/// CRC-32 as zlib computes it (reflected, polynomial 0xEDB88320), the
/// checksum the ESP-IDF closes a core dump with.
#[derive(Debug, Clone)]
pub struct Crc32 {
    table: [u32; 256],
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A checksum over nothing yet.
    pub fn new() -> Self {
        let mut table = [0u32; 256];
        for (i, e) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        Crc32 {
            table,
            state: 0xFFFF_FFFF,
        }
    }

    /// Adds `data`.
    pub fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.state = self.table[((self.state ^ b as u32) & 0xFF) as usize] ^ (self.state >> 8);
        }
    }

    /// The checksum of everything added.
    pub fn finish(&self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc(data: &[u8]) -> u32 {
        let mut c = Crc32::new();
        c.update(data);
        c.finish()
    }

    /// A dump as the IDF writes it: body plus its CRC32.
    fn dump(body: &[u8]) -> Vec<u8> {
        let mut d = body.to_vec();
        d.extend_from_slice(&crc(body).to_le_bytes());
        d
    }

    /// Stream `d` (body + stored CRC) through a masker in pieces of
    /// `step`, as the API handler does.
    fn stream(m: DumpMasker, d: &[u8], step: usize) -> (Vec<u8>, usize) {
        let mut m = m;
        let body = &d[..d.len() - CRC_LEN];
        let mut out = Vec::new();
        for piece in body.chunks(step) {
            out.extend(m.push(piece));
        }
        let (rest, c) = m.finish();
        let n = m.masked();
        out.extend(rest);
        out.extend_from_slice(&c);
        (out, n)
    }

    #[test]
    fn crc32_matches_zlib() {
        assert_eq!(crc(b""), 0);
        assert_eq!(crc(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn masks_every_text_and_keeps_length_and_a_valid_crc() {
        let body = b"HDR\x00wifi=hunter2pass;user=bc1qexampleaddress.rig;ssid=MyHome;again hunter2pass end";
        let d = dump(body);
        for step in [1, 3, 7, 16, 4096] {
            let m = DumpMasker::new(["hunter2pass", "bc1qexampleaddress.rig", "MyHome", "x", ""]);
            let (out, n) = stream(m, &d, step);
            assert_eq!(out.len(), d.len(), "step {step}");
            let text = String::from_utf8_lossy(&out);
            assert!(!text.contains("hunter2pass"), "step {step}");
            assert!(!text.contains("bc1qexample"), "step {step}");
            assert!(!text.contains("MyHome"), "step {step}");
            assert!(text.contains("wifi=xxxxxxxxxxx;"), "step {step}");
            assert!(text.contains("HDR"), "step {step}");
            assert_eq!(n, 4, "step {step}: masked count");
            let (b, c) = out.split_at(out.len() - CRC_LEN);
            assert_eq!(crc(b).to_le_bytes(), c, "step {step}: crc");
        }
    }

    #[test]
    fn a_text_across_piece_boundaries_is_masked() {
        let secret = "correct-horse-battery";
        let mut body = vec![0u8; 4090];
        body.extend_from_slice(secret.as_bytes());
        body.extend_from_slice(&[0u8; 100]);
        let d = dump(&body);
        let (out, n) = stream(DumpMasker::new([secret]), &d, 4096);
        assert_eq!(n, 1);
        assert!(!out.windows(secret.len()).any(|w| w == secret.as_bytes()));
    }

    #[test]
    fn nothing_to_mask_leaves_the_dump_as_it_was() {
        let d = dump(b"just a plain dump without anything personal in it");
        let (out, n) = stream(DumpMasker::new(["absent-secret", "ab"]), &d, 5);
        assert_eq!(n, 0);
        assert_eq!(out, d);
        let (out, _) = stream(DumpMasker::new(Vec::<String>::new()), &d, 5);
        assert_eq!(out, d);
    }

    #[test]
    fn a_text_inside_a_longer_one_goes_with_it() {
        let d = dump(b"..mypassword123..");
        let (out, _) = stream(DumpMasker::new(["password", "mypassword123"]), &d, 2);
        assert!(String::from_utf8_lossy(&out).contains("..xxxxxxxxxxxxx.."));
    }
}
