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
pub mod bm1397;

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
    /// Switch the UART in the middle of the list: write the profile's
    /// fast-UART word ([`ChipProfile::fast_uart`]) to every chip, then
    /// move the host to that rate (ESP-Miner's BM1373 init, which goes
    /// to 3 MBaud between the IO-strength block and the soft reset).
    /// Renders as the write followed by [`Rendered::HostBaud`]. A profile
    /// with this step gets no second switch from the driver after the
    /// lists, and the enabled version-rolling mask after the ramp.
    SwitchBaud,
}

/// The hash counting number (register 0x10): how far each core rolls
/// the nonce before the chip moves on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hcn {
    /// Computed from clock, cores and chip count
    /// ([`crate::regs::hash_counting_number`]) minus an erratum margin
    /// (2·134 on the BM1370).
    Computed {
        /// Counts subtracted from the computed value.
        errata: u32,
    },
    /// A fixed value (BM1373: the S21 Pro stock register).
    Fixed(u32),
    /// Not written at all: the BM1397 drivers (ESP-Miner, NerdNOS,
    /// cgminer) never touch register 0x10.
    None,
}

/// What one chip is, and how to bring it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipProfile {
    /// Human name, e.g. `"BM1370"`.
    pub name: &'static str,
    /// The id the chip answers with on a register-0 read.
    pub chip_id: u16,
    /// Length of that reply on the wire (11 on BM1370, 9 on BM1373 and
    /// BM1397).
    pub chip_id_reply_len: usize,
    /// Length of every other response (nonces, register reads): 11 on
    /// the BM1366/68/70/73, 9 on the BM1397.
    pub response_len: usize,
    /// Midstates the host computes and sends per job: 0 = the chip hashes
    /// the header from scratch and rolls the version itself (the 82-byte
    /// job); 4 = the BM1397, which gets one midstate per host-rolled
    /// version ([`crate::job::MidstateJob`]) and echoes the midstate
    /// number in its nonce.
    pub host_midstates: u8,
    /// A second chip id the same driver accepts (BM1373 answers 0x1372
    /// or 0x1373); 0 = none.
    pub chip_id_alias: u16,
    /// Fixed chain address stride (BM1373: 16); 0 = `256 / chips`.
    pub address_stride: u8,
    /// Single-chip commands go to the chip's address shifted right by
    /// this (BM1373: 2); replies carry the full address.
    pub command_address_shift: u8,
    /// The hash counting number: computed, fixed, or never written.
    pub hcn: Hcn,
    /// Hash cores per chip.
    pub cores: u16,
    /// Small cores per chip: what the nominal hashrate multiplies by.
    pub small_cores: u16,
    /// Hash domains per chip, each with a counter register
    /// ([`crate::regs::reg::domain_count`]): 4 on the BM1370, 8 on the
    /// BM1373 (measured, see `DOMAIN_4_COUNT`).
    pub domains: u8,
    /// A register the chip reports its own hash rate in, instead of
    /// counters: the BM1397's 0x04 (units of 2²⁰ H/s, bit 31 = long
    /// window; [`crate::regs::bm1397_hashrate`]). `None` on the chips
    /// with domain counters.
    pub rate_register: Option<u8>,
    /// What a domain counter reading is multiplied by to give the
    /// domain's hashrate. 1. ESP-Miner uses 2 on the BM1373
    /// (`main/device_config.h`, `domain_hashrate_scale` 2.0) because it
    /// reads four of the eight domains and sees two of those (its
    /// reads go out as a burst, see `Chain::request_counters`).
    pub domain_hashrate_scale: u8,
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
    /// Step between job ids (24 on BM1368/70/73, 8 on BM1366, 4 on the
    /// BM1397).
    pub job_id_step: u8,
    /// How long the driver collects chip-id replies after the read-all,
    /// ms. ESP-Miner and NerdOS wait up to 1 s per reply; 500 ms has
    /// always been enough for a BM1370 chain.
    pub count_window_ms: u32,
    /// Switch the chips' UART to the fast rate only after the clock ramp,
    /// the hash counting number and the version-rolling enable, then wait
    /// this long before the host follows (NerdOS `BM1373::init` +
    /// `setMaxBaud`: 500 ms). `None` = before the ramp, right away
    /// (ESP-Miner's BM1370 order, proven on the Gamma).
    pub baud_switch_after_ramp_ms: Option<u32>,
    /// Fresh work this often when the configuration sets no job interval;
    /// 0 = only on a notify or when the nonce space runs out. NerdOS sends
    /// every 1200 ms (NerdQAxe) / 1500 ms (Gaia); a BM1373 with its fixed
    /// hash counting number re-found the same nonces on the Gaia without
    /// it (87 % duplicates, 2026-09-29).
    pub job_interval_ms: u16,
    /// The register that sets the chip's UART rate: 0x28
    /// (`FAST_UART_CONFIGURATION`) on the BM1366/68/70/73, 0x18
    /// (`MISC_CONTROL`, divider bits 9–13) on the BM1397.
    pub fast_uart_reg: u8,
    /// Value for that register that raises the baud rate, and the host
    /// rate to switch to afterwards.
    pub fast_uart: ([u8; 4], u32),
    /// A register written (twice, like the reference) before every PLL0
    /// write: the BM1397's PLL0 divider `0x70 ← 0F 0F 0F 00` (cgminer
    /// `prefreq`, WantClue `prefreq1`, ESP-Miner `prefreq1`). `None` on
    /// the chips whose drivers write PLL0 alone.
    pub pll_prewrite: Option<(u8, [u8; 4])>,
    /// Whose init sequence the three lists below transcribe: the
    /// reference firmware of the boards the profile is chosen for
    /// ("ESP-Miner", "NerdOS", …). Logged at chain start.
    pub init_source: &'static str,
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

    /// Distinguishable jobs in flight: the bits of the job id the chip
    /// echoes. Sixteen on the BM1366/68/70/73 (bits 7..4 of the id byte,
    /// `(id & 0xF0) >> 1` of a step-24 id), thirty-two on the BM1397
    /// (bits 7..2, ids in steps of 4).
    pub const fn job_slots(&self) -> usize {
        if self.host_midstates > 0 {
            32
        } else {
            16
        }
    }

    /// The slot a chip-side job id (as handed out and as echoed) belongs
    /// to, below [`ChipProfile::job_slots`].
    pub const fn job_slot(&self, job_id: u8) -> usize {
        if self.host_midstates > 0 {
            ((job_id >> 2) & 0x1F) as usize
        } else {
            ((job_id >> 3) & 0x0F) as usize
        }
    }

    /// How many versions one job lets the chip search: 2¹⁶ with
    /// hardware rolling (the BIP 320 mask), else the midstates sent.
    pub const fn versions_per_job(&self) -> u32 {
        if self.host_midstates > 0 {
            self.host_midstates as u32
        } else {
            1 << 16
        }
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
    /// Move the host UART to this rate: the chips were just told it
    /// (the write before this item). Nothing on the wire.
    HostBaud(u32),
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
            InitStep::SwitchBaud => {
                // Two items: the write to the chips, then the host's move.
                let (word, baud) = p.fast_uart;
                let item = match self.sub {
                    0 => Rendered::write(Group::All, 0x00, p.fast_uart_reg, word),
                    1 => Rendered::HostBaud(baud),
                    _ => {
                        self.sub = 0;
                        return None;
                    }
                };
                self.sub += 1;
                return Some(item);
            }
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
                let expanding = matches!(step, InitStep::SetAddress | InitStep::SwitchBaud);
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

    /// NerdOS `BM1370::init` (develop, `components/bm1397/bm1370.cpp`):
    /// the same frames as ESP-Miner's except ClockDelayCtrl 0x0C, and no
    /// `14 …FF` or `54 …03` write. Checked on the payloads of the whole
    /// rendered sequence for one chip.
    #[test]
    fn bm1370_nerd_init_is_nerdos_frame_for_frame() {
        let nerd: Vec<Vec<u8>> = render(&bm1370::BM1370_NERD, 1)
            .filter(|r| !matches!(r, Rendered::Delay(_) | Rendered::CountChips))
            .map(|r| r.payload().to_vec())
            .collect();
        // Its clock delay, chain-wide and per chip, is NerdOS's 0x0C.
        let clock_delay: Vec<&Vec<u8>> = nerd
            .iter()
            .filter(|p| p.get(1) == Some(&reg::CORE_REGISTER_CONTROL) && p.get(4) == Some(&0x80))
            .collect();
        assert_eq!(
            clock_delay,
            vec![
                &vec![0x00, 0x3C, 0x80, 0x00, 0x80, 0x0C],
                &vec![0x00, 0x3C, 0x80, 0x00, 0x80, 0x0C],
            ],
            "chain-wide, then for chip 0"
        );
        // One ticket mask only (the profile's), no S21 XP literal.
        let tickets: Vec<&Vec<u8>> = nerd
            .iter()
            .filter(|p| p.get(1) == Some(&reg::TICKET_MASK))
            .collect();
        assert_eq!(tickets, vec![&vec![0x00, 0x14, 0x00, 0x00, 0x00, 0xFF]]);
        // The analog mux is written once, after the per-chip block, with 02.
        let mux: Vec<&Vec<u8>> = nerd
            .iter()
            .filter(|p| p.get(1) == Some(&reg::ANALOG_MUX_CONTROL))
            .collect();
        assert_eq!(mux, vec![&vec![0x00, 0x54, 0x00, 0x00, 0x00, 0x02]]);
        // Otherwise the same writes as the ESP-Miner init, in order.
        let esp: Vec<Vec<u8>> = render(&bm1370::BM1370, 1)
            .filter(|r| !matches!(r, Rendered::Delay(_) | Rendered::CountChips))
            .map(|r| r.payload().to_vec())
            .filter(|p| p != &[0x00, 0x54, 0x00, 0x00, 0x00, 0x03])
            .map(|mut p| {
                if p.get(1) == Some(&reg::CORE_REGISTER_CONTROL) && p.get(4) == Some(&0x80) {
                    p[5] = 0x0C;
                }
                p
            })
            .collect();
        // ESP-Miner's literal ticket line and the TicketMask step render
        // the same bytes; the Nerd list has the step only.
        let esp: Vec<Vec<u8>> = {
            let mut seen_ticket = false;
            esp.into_iter()
                .filter(|p| {
                    if p.get(1) == Some(&reg::TICKET_MASK) {
                        let keep = seen_ticket;
                        seen_ticket = true;
                        return keep;
                    }
                    true
                })
                .collect()
        };
        assert_eq!(nerd, esp);
        assert_eq!(bm1370::BM1370_NERD.init_source, "NerdOS");
        assert_eq!(bm1370::BM1370_NERD.init_post, bm1370::BM1370.init_post);
        assert_eq!(bm1370::BM1370_NERD.pll, bm1370::BM1370.pll);
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
    fn job_slots_follow_the_echoed_bits() {
        let p = &bm1370::BM1370;
        assert_eq!(p.job_slots(), 16);
        assert_eq!(p.job_slot(24), 3);
        assert_eq!(p.job_slot(120), 15);
        assert_eq!(p.job_slot(16), 2);
        let q = &bm1397::BM1397;
        assert_eq!(q.job_slots(), 32);
        assert_eq!(q.job_slot(4), 1);
        assert_eq!(q.job_slot(124), 31);
        assert_eq!(
            q.job_slot(0x18 | 2),
            6,
            "the midstate bits do not move the slot"
        );
        assert_eq!(p.versions_per_job(), 65536);
        assert_eq!(q.versions_per_job(), 4);
    }

    #[test]
    fn bm1370_profile_numbers() {
        let p = &bm1370::BM1370;
        assert_eq!(p.chip_id, 0x1370);
        assert_eq!((p.cores, p.small_cores, p.domains), (128, 2040, 4));
        assert_eq!(p.stock, (525_000_000, 1150));
        assert_eq!(p.job_id_step, 24);
        assert_eq!(p.hcn, Hcn::Computed { errata: 268 });
        assert_eq!(p.response_len, 11);
        assert_eq!(p.host_midstates, 0);
        assert_eq!(p.fast_uart_reg, reg::FAST_UART_CONFIGURATION);
        assert_eq!(p.pll_prewrite, None);
        assert_eq!(p.rate_register, None);
        assert_eq!(
            p.frequency_options_mhz,
            &[400, 490, 525, 550, 600, 625, 690]
        );
        assert_eq!(p.voltage_options_mv, &[1000, 1060, 1100, 1150, 1200, 1250]);
        assert_eq!(p.fast_uart.1, 1_000_000);
    }
}
