//! Chip profiles: everything that differs between one BM13xx and the next,
//! as data.
//!
//! The reference driver has a `.c` file per chip, each with its own
//! init function, its own result struct and its own constants; a fix in
//! one does not reach the others. Here a chip is a [`ChipProfile`]: the
//! numbers, and the initialisation as a list of [`InitStep`]s that
//! [`render`] expands into concrete commands for a chain of `n` chips.
//! Firmware sends what `render` yields; the simulator checks that what
//! it receives is what `render` would have yielded. Same list, both
//! sides, no second copy of the sequence to drift.
//!
//! Steps carry meaning, not bytes: `SetAddress` is one step whatever the
//! chain length, `TicketMask` takes its value from the profile's
//! difficulty, `Delay` is a delay and not a comment. That is what lets a
//! simulator say "you sent SET_ADDRESS before CHAIN_INACTIVE" instead of
//! "byte 7 differs".

pub mod bm1370;
pub mod bm1373;

use crate::frame::{Group, Op};
use crate::pll::PllSpec;
use crate::regs::{self, reg, version_mask, write_payload};

/// One step of a chip's initialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitStep {
    /// Write a register on every chip (`GROUP_ALL`, address byte 0).
    WriteAll {
        /// Register address.
        reg: u8,
        /// The four value bytes as they go on the wire.
        value: [u8; 4],
    },
    /// Write a register on the chip this step is being rendered for.
    /// Only meaningful in a profile's `init_per_chip` list.
    WriteChip {
        /// Register address.
        reg: u8,
        /// The four value bytes as they go on the wire.
        value: [u8; 4],
    },
    /// Read a register on every chip; each chip answers.
    ReadAll {
        /// Register address.
        reg: u8,
    },
    /// Write the BIP 320 version-rolling mask to every chip, enabled.
    VersionMask,
    /// Write the version-rolling mask without the enable bit (BM1373
    /// pre-enumeration, NerdOS).
    VersionMaskNoEnable,
    /// Write the ticket mask for the profile's default difficulty.
    TicketMask,
    /// `CMD_INACTIVE` to the chain: stop forwarding, prepare addressing.
    ChainInactive,
    /// One `CMD_SETADDRESS` per chip, addresses `0, interval, 2·interval…`
    /// with `interval = 256 / chip_count`.
    SetAddress,
    /// Wait. The reference driver's `asic_init_core_register_delay()` is
    /// 10 ms after core-register writes.
    Delay {
        /// Milliseconds.
        ms: u16,
    },
    /// Marker: the firmware now collects chip-id replies (one per chip,
    /// [`ChipProfile::chip_id_reply_len`] bytes each) and derives the
    /// chain length. Yields nothing on the wire.
    CountChips,
}

/// What one chip is, and how to bring it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipProfile {
    /// Human name, e.g. `"BM1370"`.
    pub name: &'static str,
    /// The id the chip answers with on a register-0 read.
    pub chip_id: u16,
    /// Length of that reply on the wire (11 on BM1370, 9 on BM1373).
    pub chip_id_reply_len: usize,
    /// A second chip id the same driver accepts (BM1373 answers 0x1372
    /// or 0x1373); 0 = none.
    pub chip_id_alias: u16,
    /// Fixed chain address stride (BM1373: 16); 0 = `256 / chips`.
    pub address_stride: u8,
    /// Single-chip commands go to the chip's address shifted right by
    /// this (BM1373: 2); replies carry the full address.
    pub command_address_shift: u8,
    /// A fixed hash counting number instead of the computed one
    /// (BM1373: the S21 Pro stock value).
    pub hcn_fixed: Option<u32>,
    /// Hash cores per chip.
    pub cores: u16,
    /// Small cores per chip: what the nominal hashrate multiplies by.
    pub small_cores: u16,
    /// Voltage / clock domains per chip.
    pub domains: u8,
    /// Share difficulty the ticket mask is set to at init.
    pub default_difficulty: u32,
    /// Vendor stock operating point, (Hz, mV).
    pub stock: (u32, u16),
    /// Vendor frequency options, MHz, as the reference driver lists them.
    pub frequency_options_mhz: &'static [u16],
    /// Vendor core-voltage options, mV.
    pub voltage_options_mv: &'static [u16],
    /// The PLL's legal ranges.
    pub pll: PllSpec,
    /// Step between job ids (24 on BM1368/70/73, 8 on BM1366).
    pub job_id_step: u8,
    /// Margin subtracted from the hash counting number (2·134 on BM1370).
    pub hcn_errata: u32,
    /// Value for the UART register that raises the baud rate, and the
    /// host rate to switch to afterwards.
    pub fast_uart: ([u8; 4], u32),
    /// Initialisation before and including chain enumeration and
    /// addressing; `SetAddress` expands per chip.
    pub init_chain: &'static [InitStep],
    /// Initialisation repeated for every chip, with `WriteChip` bound to
    /// that chip's address.
    pub init_per_chip: &'static [InitStep],
    /// Initialisation after the per-chip pass. The reference driver then
    /// ramps the frequency and sets the hash counting number — those are
    /// runtime values, not profile data, and are not in this list.
    pub init_post: &'static [InitStep],
}

impl ChipProfile {
    /// Chain address interval for `chip_count` chips: the profile's
    /// fixed stride, else `256 / n`.
    pub const fn address_interval(&self, chip_count: u8) -> u8 {
        if self.address_stride != 0 {
            return self.address_stride;
        }
        if chip_count == 0 {
            return 0;
        }
        (256 / chip_count as u16) as u8
    }

    /// Chain address of chip `index` in a chain of `chip_count`.
    pub const fn chip_address(&self, chip_count: u8, index: u8) -> u8 {
        index.wrapping_mul(self.address_interval(chip_count))
    }

    /// The address a single-chip command carries for a chip at
    /// `chip_address`.
    pub const fn command_address(&self, chip_address: u8) -> u8 {
        chip_address >> self.command_address_shift
    }

    /// Chip index for an address a reply carries, if it is one of the
    /// `chip_count` chips.
    pub fn chip_index(&self, chip_count: u8, address: u8) -> Option<usize> {
        if chip_count == 0 {
            return None;
        }
        if chip_count == 1 {
            return (address == 0).then_some(0);
        }
        let interval = self.address_interval(chip_count);
        if interval == 0 || !address.is_multiple_of(interval) {
            return None;
        }
        let i = usize::from(address / interval);
        (i < usize::from(chip_count)).then_some(i)
    }

    /// Chip index for the eight address bits a nonce carries (bits
    /// 17..25): the chip's address plus whatever the chip puts in the
    /// low bits, so the reference divides by `256 / chips` and does not
    /// expect a multiple. A real BM1370 returned every value with bit
    /// patterns below its address; the simulator returned the address.
    pub fn chip_index_from_nonce(&self, chip_count: u8, address_bits: u8) -> Option<usize> {
        if chip_count == 0 {
            return None;
        }
        let interval = (256 / u16::from(chip_count)).max(1);
        let i = usize::from(address_bits) / usize::from(interval);
        (i < usize::from(chip_count)).then_some(i)
    }

    /// True for the profile's chip id or its alias.
    pub const fn is_chip_id(&self, id: u16) -> bool {
        id == self.chip_id || (self.chip_id_alias != 0 && id == self.chip_id_alias)
    }
}

/// One thing to do, as [`render`] yields it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendered {
    /// Send a command frame.
    Cmd {
        /// Addressing group.
        group: Group,
        /// Operation.
        op: Op,
        /// Payload bytes; only the first `len` are valid.
        payload: [u8; 6],
        /// Payload length (2 for reads/address ops, 6 for writes).
        len: u8,
    },
    /// Wait this many milliseconds.
    Delay(u16),
    /// Collect chip-id replies now.
    CountChips,
}

impl Rendered {
    /// The payload slice.
    pub fn payload(&self) -> &[u8] {
        match self {
            Rendered::Cmd { payload, len, .. } => &payload[..*len as usize],
            _ => &[],
        }
    }

    fn write(group: Group, chip_addr: u8, reg: u8, value: [u8; 4]) -> Self {
        Rendered::Cmd {
            group,
            op: Op::Write,
            payload: write_payload(chip_addr, reg, value),
            len: 6,
        }
    }

    fn two(group: Group, op: Op, a: u8, b: u8) -> Self {
        Rendered::Cmd {
            group,
            op,
            payload: [a, b, 0, 0, 0, 0],
            len: 2,
        }
    }
}

/// Expand a profile's initialisation for a chain of `chip_count` chips.
///
/// Yields, in order: `init_chain` (with `SetAddress` expanded to one
/// command per chip), then `init_per_chip` once per chip with that
/// chip's address, then `init_post`. Allocation-free: an iterator over
/// the lists with a small cursor.
pub fn render(profile: &ChipProfile, chip_count: u8) -> Render<'_> {
    Render {
        profile,
        chip_count,
        phase: Phase::Chain,
        idx: 0,
        chip: 0,
        sub: 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Chain,
    PerChip,
    Post,
    Done,
}

/// Iterator returned by [`render`].
#[derive(Debug, Clone)]
pub struct Render<'a> {
    profile: &'a ChipProfile,
    chip_count: u8,
    phase: Phase,
    /// Index into the current phase's list.
    idx: usize,
    /// Current chip in `PerChip`.
    chip: u8,
    /// Sub-index for steps that expand (`SetAddress`).
    sub: u8,
}

impl Render<'_> {
    fn expand(&mut self, step: InitStep, chip_addr: u8) -> Option<Rendered> {
        let p = self.profile;
        Some(match step {
            InitStep::WriteAll { reg, value } => Rendered::write(Group::All, 0x00, reg, value),
            InitStep::WriteChip { reg, value } => {
                Rendered::write(Group::Single, p.command_address(chip_addr), reg, value)
            }
            InitStep::ReadAll { reg } => Rendered::two(Group::All, Op::Read, 0x00, reg),
            InitStep::VersionMask => Rendered::write(
                Group::All,
                0x00,
                reg::VERSION_ROLLING,
                version_mask(regs::BIP320_VERSION_MASK),
            ),
            InitStep::VersionMaskNoEnable => Rendered::write(
                Group::All,
                0x00,
                reg::VERSION_ROLLING,
                regs::version_mask_no_enable(regs::BIP320_VERSION_MASK),
            ),
            InitStep::TicketMask => Rendered::write(
                Group::All,
                0x00,
                reg::TICKET_MASK,
                regs::ticket_mask(p.default_difficulty as f64),
            ),
            InitStep::ChainInactive => Rendered::two(Group::All, Op::Inactive, 0x00, 0x00),
            InitStep::SetAddress => {
                // One per chip; `sub` counts them. Return None once all
                // are out so the caller advances `idx`.
                if self.sub >= self.chip_count {
                    self.sub = 0;
                    return None;
                }
                let addr = p.chip_address(self.chip_count, self.sub);
                self.sub += 1;
                // Stay on this step: the caller must not advance.
                return Some(Rendered::two(Group::Single, Op::SetAddress, addr, 0x00));
            }
            InitStep::Delay { ms } => Rendered::Delay(ms),
            InitStep::CountChips => Rendered::CountChips,
        })
    }
}

impl Iterator for Render<'_> {
    type Item = Rendered;

    fn next(&mut self) -> Option<Rendered> {
        loop {
            let (list, chip_addr): (&[InitStep], u8) = match self.phase {
                Phase::Chain => (self.profile.init_chain, 0),
                Phase::PerChip => (
                    self.profile.init_per_chip,
                    self.profile.chip_address(self.chip_count, self.chip),
                ),
                Phase::Post => (self.profile.init_post, 0),
                Phase::Done => return None,
            };
            if self.idx < list.len() {
                let step = list[self.idx];
                let expanding = matches!(step, InitStep::SetAddress);
                match self.expand(step, chip_addr) {
                    Some(r) => {
                        if !expanding {
                            self.idx += 1;
                        }
                        return Some(r);
                    }
                    None => {
                        // An expanding step ran out: move on.
                        self.idx += 1;
                        continue;
                    }
                }
            }
            // End of this phase's list.
            self.idx = 0;
            self.phase = match self.phase {
                Phase::Chain => {
                    if self.chip_count == 0 || self.profile.init_per_chip.is_empty() {
                        Phase::Post
                    } else {
                        Phase::PerChip
                    }
                }
                Phase::PerChip => {
                    self.chip += 1;
                    if self.chip < self.chip_count {
                        Phase::PerChip
                    } else {
                        Phase::Post
                    }
                }
                Phase::Post => Phase::Done,
                Phase::Done => Phase::Done,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::bm1370::BM1370;
    use crate::frame::encode_cmd;

    /// Render to wire frames (test helper; the firmware would send them).
    fn frames(profile: &ChipProfile, chips: u8) -> Vec<Vec<u8>> {
        render(profile, chips)
            .filter_map(|r| match r {
                Rendered::Cmd {
                    group,
                    op,
                    payload,
                    len,
                    ..
                } => {
                    let mut buf = [0u8; 16];
                    let n = encode_cmd(&mut buf, group, op, &payload[..len as usize]).unwrap();
                    Some(buf[..n].to_vec())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn address_interval_splits_256_evenly() {
        assert_eq!(
            BM1370.address_interval(1),
            0,
            "256 does not fit a byte: 1 chip → interval 0 → address 0"
        );
        assert_eq!(BM1370.address_interval(2), 128);
        assert_eq!(BM1370.address_interval(4), 64);
        assert_eq!(BM1370.address_interval(8), 32);
        assert_eq!(BM1370.chip_address(4, 3), 192);
        assert_eq!(BM1370.chip_address(1, 0), 0);
    }

    /// The `bm1370.c` comment frames, in the order the init sends them,
    /// must all appear in the rendered sequence for one chip — and in
    /// that order.
    #[test]
    fn bm1370_init_contains_the_commented_frames_in_order() {
        #[rustfmt::skip]
        let expected: &[&[u8]] = &[
            &[0x55, 0xAA, 0x51, 0x09, 0x00, 0xA8, 0x00, 0x07, 0x00, 0x00, 0x03],
            &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x18, 0xF0, 0x00, 0xC1, 0x00, 0x04],
            &[0x55, 0xAA, 0x53, 0x05, 0x00, 0x00, 0x03],
            &[0x55, 0xAA, 0x40, 0x05, 0x00, 0x00, 0x1C],
            &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x8B, 0x00, 0x12],
            &[0x55, 0xAA, 0x41, 0x09, 0x00, 0xA8, 0x00, 0x07, 0x01, 0xF0, 0x15],
            &[0x55, 0xAA, 0x41, 0x09, 0x00, 0x18, 0xF0, 0x00, 0xC1, 0x00, 0x0C],
            &[0x55, 0xAA, 0x41, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x8B, 0x00, 0x1A],
            &[0x55, 0xAA, 0x41, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x82, 0xAA, 0x05],
            &[0x55, 0xAA, 0x51, 0x09, 0x00, 0xB9, 0x00, 0x00, 0x44, 0x80, 0x0D],
            &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x54, 0x00, 0x00, 0x00, 0x02, 0x18],
            &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x8D, 0xEE, 0x1B],
        ];
        let got = frames(&bm1370::BM1370, 1);
        let mut cursor = 0;
        for want in expected {
            let pos = got[cursor..]
                .iter()
                .position(|f| f == want)
                .unwrap_or_else(|| panic!("frame {want:02x?} not found after index {cursor}"));
            cursor += pos + 1;
        }
    }

    /// Structure of the one-chip init, step by step, against `BM1370_init()`.
    #[test]
    fn bm1370_init_structure_for_one_chip() {
        let r: Vec<Rendered> = render(&bm1370::BM1370, 1).collect();
        // 3 version masks, chip-id read, count, version mask, A8, 18,
        // inactive, 1 set-address, 3C+delay, 3C+delay, 14+delay,
        // ticket+delay, 54, 58, 68  = chain phase
        let is_vmask = |x: &Rendered| x.payload().get(1) == Some(&reg::VERSION_ROLLING);
        assert!(
            r[..3].iter().all(is_vmask),
            "three version-mask writes first"
        );
        assert_eq!(
            r[3].payload(),
            &[0x00, reg::CHIP_ID],
            "then the chip-id read"
        );
        assert_eq!(r[4], Rendered::CountChips);
        assert!(is_vmask(&r[5]));
        assert_eq!(r[6].payload(), &[0x00, 0xA8, 0x00, 0x07, 0x00, 0x00]);
        assert_eq!(r[7].payload(), &[0x00, 0x18, 0xF0, 0x00, 0xC1, 0x00]);
        assert_eq!(r[8].payload(), &[0x00, 0x00]);
        assert!(matches!(
            r[8],
            Rendered::Cmd {
                op: Op::Inactive,
                ..
            }
        ));
        assert!(matches!(
            r[9],
            Rendered::Cmd {
                op: Op::SetAddress,
                ..
            }
        ));
        assert_eq!(r[9].payload(), &[0x00, 0x00], "one chip: address 0");
        // The ticket-mask step carries the profile's difficulty (256 → 00 00 00 FF).
        let ticket = r
            .iter()
            .filter(|x| x.payload().get(1) == Some(&reg::TICKET_MASK))
            .map(|x| x.payload().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(
            ticket,
            vec![
                vec![0x00, 0x14, 0x00, 0x00, 0x00, 0xFF], // the literal S21 XP line
                vec![0x00, 0x14, 0x00, 0x00, 0x00, 0xFF], // TicketMask(256)
            ]
        );
        let delays = r
            .iter()
            .filter(|x| matches!(x, Rendered::Delay(10)))
            .count();
        assert_eq!(delays, 4 + 3 + 1, "chain 4, per-chip 3, post 1");
    }

    /// Two chips: two set-address commands with addresses 0 and 128, and
    /// the per-chip block twice with those addresses.
    #[test]
    fn bm1370_init_addresses_a_two_chip_chain() {
        let r: Vec<Rendered> = render(&bm1370::BM1370, 2).collect();
        let set: Vec<u8> = r
            .iter()
            .filter(|x| {
                matches!(
                    x,
                    Rendered::Cmd {
                        op: Op::SetAddress,
                        ..
                    }
                )
            })
            .map(|x| x.payload()[0])
            .collect();
        assert_eq!(set, [0, 128]);
        let per_chip_a8: Vec<u8> = r
            .iter()
            .filter(|x| {
                matches!(
                    x,
                    Rendered::Cmd {
                        group: Group::Single,
                        op: Op::Write,
                        ..
                    }
                ) && x.payload()[1] == 0xA8
            })
            .map(|x| x.payload()[0])
            .collect();
        assert_eq!(per_chip_a8, [0, 128]);
    }

    #[test]
    fn render_length_grows_with_chip_count() {
        let one = render(&bm1370::BM1370, 1).count();
        let two = render(&bm1370::BM1370, 2).count();
        let per_chip = bm1370::BM1370.init_per_chip.len();
        assert_eq!(
            two - one,
            per_chip + 1,
            "one more per-chip block and one more set-address"
        );
    }

    #[test]
    fn zero_chips_renders_chain_and_post_only() {
        let r: Vec<Rendered> = render(&bm1370::BM1370, 0).collect();
        assert!(!r.iter().any(|x| matches!(
            x,
            Rendered::Cmd {
                op: Op::SetAddress,
                ..
            }
        )));
        assert!(!r.iter().any(|x| matches!(
            x,
            Rendered::Cmd {
                group: Group::Single,
                ..
            }
        )));
    }

    #[test]
    fn bm1370_profile_numbers() {
        let p = &bm1370::BM1370;
        assert_eq!(p.chip_id, 0x1370);
        assert_eq!((p.cores, p.small_cores, p.domains), (128, 2040, 4));
        assert_eq!(p.stock, (525_000_000, 1150));
        assert_eq!(p.job_id_step, 24);
        assert_eq!(p.hcn_errata, 268);
        assert_eq!(
            p.frequency_options_mhz,
            &[400, 490, 525, 550, 600, 625, 690]
        );
        assert_eq!(p.voltage_options_mv, &[1000, 1060, 1100, 1150, 1200, 1250]);
        assert_eq!(p.fast_uart.1, 1_000_000);
    }
}
