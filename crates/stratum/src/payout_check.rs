//! The payout check (docs/23): a solo pool whose coinbase stops paying
//! the miner's address is left, as a pool that stops answering is.
//!
//! [`crate::payout`] reads who a job pays. A pool the check knows to be
//! a solo pool for this miner, because one of its coinbases paid the
//! miner's address more than half of the reward, is *armed*; from then
//! on the jobs of two different blocks in a row that pay the address
//! nothing are taken as the pool keeping the block (a misconfigured or
//! hijacked pool, a pool that changed its terms) and the firmware warns
//! and switches to the other pool. Everything here is the rule, without
//! I/O, so it is host-tested.
//!
//! Why "more than half" and "two blocks": a pool that pays its miners in
//! the coinbase by their share of the last blocks (Ocean's TIDES,
//! P2Pool) lists a small miner only while the miner's balance is over
//! its threshold, so the miner's output comes and goes from block to
//! block. Such a coinbase never pays one small miner most of the reward
//! and so never arms; and a solo pool's template is rebuilt within a
//! block several times, so one odd template is one block, not two.
//!
//! - [`pool_id`]: which pool configuration (host, port, worker) a mark
//!   belongs to. A mark for another id does not apply, so editing the
//!   pool clears its marks without anyone having to.
//! - [`evidence`]: what one job says: paid, nothing to the address, or no
//!   evidence (not decoded, a pool account instead of an address, an
//!   address of another coin than the one configured).
//! - [`Check`]: one session's verdicts: arm on the first solo-shaped
//!   paying job, act when the jobs of [`BLOCKS_TO_ACT`] different blocks
//!   in a row pay nothing.
//! - [`Memory`]: per pool slot (main, fallback), the pool seen paying
//!   (stored) and the pool left for not paying (RAM): the probe does not
//!   take the miner back to a main pool left this way.

use crate::payout::Payout;

/// Different blocks in a row whose decoded jobs pay the miner's address
/// nothing before the firmware acts: one odd template (a pool
/// restarting, a template built in a hurry) is one block, not enough.
pub const BLOCKS_TO_ACT: u8 = 2;

/// The pool slots of [`Memory`].
pub const MAIN: usize = 0;
/// The fallback pool's slot.
pub const FALLBACK: usize = 1;

/// The identity of a pool configuration: FNV-1a over the host (case
/// ignored), the port and the worker name (both trimmed). Never 0, which
/// stands for "none".
pub fn pool_id(host: &str, port: u16, user: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    let mut eat = |b: u8| {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    };
    for b in host.trim().bytes() {
        eat(b.to_ascii_lowercase());
    }
    eat(0);
    for b in port.to_le_bytes() {
        eat(b);
    }
    eat(0);
    for b in user.trim().bytes() {
        eat(b);
    }
    h.max(1)
}

/// Whether an address of this kind (`bitcoin`: a Bitcoin address) is
/// for the coin the miner is set to (`ticker`, `coinTicker`): a Bitcoin
/// address on BTC (or no ticker), another coin's address on any other
/// ticker. Where the two disagree the check stays out.
pub fn coin_matches(ticker: &str, bitcoin: bool) -> bool {
    let t = ticker.trim();
    let btc = t.is_empty() || t.eq_ignore_ascii_case("BTC");
    btc == bitcoin
}

/// What one job says about the payout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The coinbase pays the miner's address something; `solo` when
    /// that is more than half of the coinbase (a solo pool, perhaps with
    /// a fee or a donation output), which alone arms a pool.
    Paid {
        /// The miner gets more than half of the coinbase.
        solo: bool,
    },
    /// The coinbase pays the miner's address nothing; the pool keeps
    /// `kept_sat` (the whole coinbase).
    Missing {
        /// The coinbase's total, satoshis.
        kept_sat: u64,
    },
    /// No evidence either way: the coinbase was not decoded, the worker
    /// is a pool account, or the address is for another coin.
    None,
}

/// What a job's payout (`None`: not decoded) says, for a miner set to
/// the coin `ticker`.
pub fn evidence(pay: Option<&Payout>, ticker: &str) -> Evidence {
    match pay {
        Some(p) if p.own_address && coin_matches(ticker, p.bitcoin) && p.total > 0 => {
            if p.user > 0 {
                Evidence::Paid {
                    solo: u128::from(p.user) * 2 > u128::from(p.total),
                }
            } else {
                Evidence::Missing { kept_sat: p.total }
            }
        }
        _ => Evidence::None,
    }
}

/// What the firmware does after a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing new.
    Nothing,
    /// The pool paid the address for the first time: armed now (the
    /// firmware stores the mark).
    Arm,
    /// The armed pool paid nothing in the jobs of [`BLOCKS_TO_ACT`]
    /// different blocks in a row: warn, and switch where there is
    /// another pool.
    Act {
        /// What the pool keeps, satoshis.
        kept_sat: u64,
    },
    /// The pool pays the address again after [`Step::Act`].
    Recovered,
}

/// One session's verdicts. A new session starts a new one, armed when
/// [`Memory::armed`] says so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Check {
    armed: bool,
    /// Blocks in a row with a job that paid nothing.
    misses: u8,
    /// The block of the last miss counted.
    miss_block: Option<u64>,
    missing: bool,
}

impl Check {
    /// A session's check; `armed` when this pool has paid the address
    /// before.
    pub const fn new(armed: bool) -> Self {
        Self {
            armed,
            misses: 0,
            miss_block: None,
            missing: false,
        }
    }

    /// The pool in use is armed.
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// The pool in use has been found keeping the payout and has not
    /// paid since.
    pub fn missing(&self) -> bool {
        self.missing
    }

    /// The next job's evidence; `block` names the block the job builds
    /// on (its previous block's hash, or part of it), so misses are
    /// counted per block, not per job. `enabled` is the `payoutCheck`
    /// setting: without it a pool is still armed (so that turning the
    /// check on later finds it armed) but never acted on.
    pub fn job(&mut self, e: Evidence, block: u64, enabled: bool) -> Step {
        match e {
            Evidence::None => Step::Nothing,
            Evidence::Paid { solo } => {
                self.misses = 0;
                self.miss_block = None;
                if !self.armed {
                    if !solo {
                        // A share of the reward among others (a pool that
                        // pays its miners in the coinbase): not a solo
                        // pool, never armed by this.
                        return Step::Nothing;
                    }
                    self.armed = true;
                    Step::Arm
                } else if self.missing {
                    self.missing = false;
                    Step::Recovered
                } else {
                    Step::Nothing
                }
            }
            Evidence::Missing { kept_sat } => {
                if !self.armed || !enabled || self.missing {
                    return Step::Nothing;
                }
                if self.miss_block == Some(block) {
                    // Another template of the same block.
                    return Step::Nothing;
                }
                self.miss_block = Some(block);
                self.misses = self.misses.saturating_add(1);
                if self.misses >= BLOCKS_TO_ACT {
                    self.missing = true;
                    Step::Act { kept_sat }
                } else {
                    Step::Nothing
                }
            }
        }
    }
}

/// A block's key for [`Check::job`]: the first eight bytes of the hash
/// of the block a job builds on.
pub fn block_key(prev_hash: &[u8; 32]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&prev_hash[..8]);
    u64::from_le_bytes(b)
}

/// Per pool slot ([`MAIN`], [`FALLBACK`]): the pool seen paying the
/// address, and the pool left for not paying it, by [`pool_id`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory {
    seen: [u32; 2],
    left: [u32; 2],
}

impl Memory {
    /// Nothing seen, nothing left.
    pub const fn new() -> Self {
        Self {
            seen: [0; 2],
            left: [0; 2],
        }
    }

    /// From the stored word ([`Memory::stored`]); nothing left.
    pub const fn from_stored(v: u64) -> Self {
        Self {
            seen: [(v >> 32) as u32, v as u32],
            left: [0; 2],
        }
    }

    /// What is stored across a reboot: the pools seen paying, main in
    /// the high half. The marks for pools left stay in RAM: after a
    /// reboot such a pool gets its two jobs again.
    pub fn stored(&self) -> u64 {
        (u64::from(self.seen[MAIN]) << 32) | u64::from(self.seen[FALLBACK])
    }

    /// The pool `id` in `slot` has paid the address before.
    pub fn armed(&self, slot: usize, id: u32) -> bool {
        id != 0 && self.seen.get(slot) == Some(&id)
    }

    /// A job of the pool `id` in `slot` paid the address: armed from now
    /// on, and no longer left. True when the stored part changed.
    pub fn paid(&mut self, slot: usize, id: u32) -> bool {
        if slot > FALLBACK || id == 0 {
            return false;
        }
        if self.left[slot] == id {
            self.left[slot] = 0;
        }
        let changed = self.seen[slot] != id;
        self.seen[slot] = id;
        changed
    }

    /// The pool `id` in `slot` was left for not paying.
    pub fn leave(&mut self, slot: usize, id: u32) {
        if let Some(l) = self.left.get_mut(slot) {
            *l = id;
        }
    }

    /// The pool `id` in `slot` was left for not paying and has not paid
    /// since (nor been edited: another id does not match).
    pub fn is_left(&self, slot: usize, id: u32) -> bool {
        id != 0 && self.left.get(slot) == Some(&id)
    }

    /// Whether a pool found keeping the payout can be left for the other
    /// one: `other_ok` says the other pool is configured (the fallback
    /// switched on and filled in; the main pool always is), `other_id`
    /// is its identity. Not for an other pool itself left for not paying.
    pub fn can_switch(&self, on_fallback: bool, other_ok: bool, other_id: u32) -> bool {
        let other = if on_fallback { MAIN } else { FALLBACK };
        other_ok && !self.is_left(other, other_id)
    }

    /// The probe may take the miner back to the main pool `main_id`:
    /// not while it is left for not paying and the check is on.
    pub fn probe_allowed(&self, main_id: u32, enabled: bool) -> bool {
        !(enabled && self.is_left(MAIN, main_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pay(total: u64, user: u64) -> Payout {
        Payout {
            outputs: Vec::new(),
            total,
            user,
            own_address: true,
            bitcoin: true,
        }
    }

    #[test]
    fn pool_ids_follow_host_port_and_worker() {
        let a = pool_id("solo.example.org", 3333, "bc1qxyz.rig1");
        assert_eq!(a, pool_id(" SOLO.example.org ", 3333, "bc1qxyz.rig1 "));
        assert_ne!(a, pool_id("solo.example.org", 3334, "bc1qxyz.rig1"));
        assert_ne!(a, pool_id("solo.example.org", 3333, "bc1qxyz.rig2"));
        assert_ne!(a, pool_id("other.example.org", 3333, "bc1qxyz.rig1"));
        // The fields cannot run into each other.
        assert_ne!(pool_id("ab", 1, "c"), pool_id("a", 1, "bc"));
        assert_ne!(pool_id("", 0, ""), 0);
    }

    const PAID: Evidence = Evidence::Paid { solo: true };

    #[test]
    fn only_a_decoded_coinbase_to_an_address_of_the_coin_counts() {
        assert_eq!(evidence(Some(&pay(100, 99)), "BTC"), PAID);
        assert_eq!(evidence(Some(&pay(100, 99)), ""), PAID);
        assert_eq!(
            evidence(Some(&pay(100, 0)), "btc"),
            Evidence::Missing { kept_sat: 100 }
        );
        // Not decoded (Sv2 standard channel, a coinbase that does not parse).
        assert_eq!(evidence(None, "BTC"), Evidence::None);
        // A pool account: the pool pays, not the coinbase.
        let account = Payout {
            own_address: false,
            ..pay(100, 0)
        };
        assert_eq!(evidence(Some(&account), "BTC"), Evidence::None);
        // A Bitcoin address on a BCH pool setting, a CashAddr on BTC.
        assert_eq!(evidence(Some(&pay(100, 0)), "BCH"), Evidence::None);
        let cash = Payout {
            bitcoin: false,
            ..pay(100, 0)
        };
        assert_eq!(evidence(Some(&cash), "BTC"), Evidence::None);
        assert_eq!(
            evidence(Some(&cash), "BCH"),
            Evidence::Missing { kept_sat: 100 }
        );
        // An empty coinbase says nothing.
        assert_eq!(evidence(Some(&pay(0, 0)), "BTC"), Evidence::None);
    }

    #[test]
    fn solo_shaped_means_more_than_half_to_the_miner() {
        // A solo pool with a 2 % fee, or a donation output.
        assert_eq!(evidence(Some(&pay(100, 98)), "BTC"), PAID);
        assert_eq!(evidence(Some(&pay(100, 51)), "BTC"), PAID);
        // Half or less: a share among others.
        let shared = Evidence::Paid { solo: false };
        assert_eq!(evidence(Some(&pay(100, 50)), "BTC"), shared);
        assert_eq!(evidence(Some(&pay(312_500_000, 40_000)), "BTC"), shared);
        // No overflow at the largest sums.
        let max = 21_000_000 * 100_000_000;
        assert_eq!(evidence(Some(&pay(max, max)), "BTC"), PAID);
    }

    #[test]
    fn a_pool_never_seen_paying_is_never_acted_on() {
        let mut c = Check::new(false);
        for b in 0..10 {
            assert_eq!(
                c.job(Evidence::Missing { kept_sat: 5 }, b, true),
                Step::Nothing
            );
        }
        assert!(!c.armed());
        assert!(!c.missing());
    }

    #[test]
    fn a_coinbase_paying_pool_never_arms_on_a_small_share() {
        // Ocean/TIDES, P2Pool: the miner's output comes and goes with its
        // balance; a small share of the reward never arms the pool.
        let mut c = Check::new(false);
        let small = Evidence::Paid { solo: false };
        let miss = Evidence::Missing {
            kept_sat: 312_500_000,
        };
        for b in 0..6 {
            assert_eq!(c.job(small, b * 2, true), Step::Nothing);
            assert_eq!(c.job(miss, b * 2 + 1, true), Step::Nothing);
        }
        assert!(!c.armed());
    }

    #[test]
    fn two_blocks_in_a_row_without_payout_act_once() {
        let mut c = Check::new(false);
        assert_eq!(c.job(PAID, 1, true), Step::Arm);
        assert!(c.armed());
        assert_eq!(c.job(PAID, 1, true), Step::Nothing);
        let miss = Evidence::Missing {
            kept_sat: 312_500_000,
        };
        assert_eq!(c.job(miss, 2, true), Step::Nothing);
        // More templates of the same block are one miss.
        for _ in 0..5 {
            assert_eq!(c.job(miss, 2, true), Step::Nothing);
        }
        assert!(!c.missing());
        assert_eq!(
            c.job(miss, 3, true),
            Step::Act {
                kept_sat: 312_500_000
            }
        );
        assert!(c.missing());
        // Once only, however many follow.
        assert_eq!(c.job(miss, 4, true), Step::Nothing);
        assert_eq!(c.job(PAID, 5, true), Step::Recovered);
        assert!(!c.missing());
        assert_eq!(c.job(PAID, 5, true), Step::Nothing);
    }

    #[test]
    fn any_payment_counts_as_paid_on_an_armed_pool() {
        let mut c = Check::new(true);
        let miss = Evidence::Missing { kept_sat: 1 };
        assert_eq!(c.job(miss, 1, true), Step::Nothing);
        assert_eq!(
            c.job(Evidence::Paid { solo: false }, 2, true),
            Step::Nothing
        );
        assert_eq!(c.job(miss, 3, true), Step::Nothing);
        assert!(!c.missing());
    }

    #[test]
    fn one_odd_block_between_paying_ones_is_forgiven() {
        let mut c = Check::new(true);
        let miss = Evidence::Missing { kept_sat: 1 };
        for b in 0..5 {
            assert_eq!(c.job(miss, b, true), Step::Nothing);
            assert_eq!(c.job(PAID, b, true), Step::Nothing);
        }
        assert!(!c.missing());
    }

    #[test]
    fn jobs_without_evidence_neither_count_nor_break_the_row() {
        let mut c = Check::new(true);
        let miss = Evidence::Missing { kept_sat: 1 };
        assert_eq!(c.job(miss, 1, true), Step::Nothing);
        assert_eq!(c.job(Evidence::None, 2, true), Step::Nothing);
        assert_eq!(c.job(Evidence::None, 3, true), Step::Nothing);
        assert_eq!(c.job(miss, 4, true), Step::Act { kept_sat: 1 });
    }

    #[test]
    fn switched_off_it_still_arms_but_never_acts() {
        let mut c = Check::new(false);
        assert_eq!(c.job(PAID, 1, false), Step::Arm);
        let miss = Evidence::Missing { kept_sat: 1 };
        for b in 2..6 {
            assert_eq!(c.job(miss, b, false), Step::Nothing);
        }
        assert!(!c.missing());
        // Turned on: the row starts from there.
        assert_eq!(c.job(miss, 6, true), Step::Nothing);
        assert_eq!(c.job(miss, 7, true), Step::Act { kept_sat: 1 });
    }

    #[test]
    fn block_keys_tell_blocks_apart() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        a[0] = 1;
        b[0] = 2;
        assert_ne!(block_key(&a), block_key(&b));
        assert_eq!(block_key(&a), block_key(&a));
    }

    #[test]
    fn memory_arms_per_slot_and_pool() {
        let mut m = Memory::new();
        let main = pool_id("solo.a", 3333, "bc1qa");
        let fb = pool_id("solo.b", 3333, "bc1qa");
        assert!(!m.armed(MAIN, main));
        assert!(m.paid(MAIN, main));
        assert!(!m.paid(MAIN, main), "the same pool again changes nothing");
        assert!(m.armed(MAIN, main));
        assert!(!m.armed(FALLBACK, main), "per slot");
        assert!(!m.armed(MAIN, fb), "another pool in the slot");
        assert!(m.paid(FALLBACK, fb));
        // Stored and read back: the seen marks survive, left ones do not.
        m.leave(MAIN, main);
        let back = Memory::from_stored(m.stored());
        assert!(back.armed(MAIN, main) && back.armed(FALLBACK, fb));
        assert!(!back.is_left(MAIN, main));
        // An edited pool (another id) is not armed.
        let edited = pool_id("solo.a", 3333, "bc1qother");
        assert!(!back.armed(MAIN, edited));
        assert!(!m.armed(MAIN, 0) && !m.paid(MAIN, 0) && !m.paid(2, main));
    }

    #[test]
    fn a_pool_left_for_not_paying_stays_left_until_it_pays_or_is_edited() {
        let mut m = Memory::new();
        let main = pool_id("solo.a", 3333, "bc1qa");
        let fb = pool_id("solo.b", 3333, "bc1qa");
        m.paid(MAIN, main);
        m.leave(MAIN, main);
        assert!(m.is_left(MAIN, main));
        assert!(!m.probe_allowed(main, true));
        // The check switched off: the probe goes as before.
        assert!(m.probe_allowed(main, false));
        // The pool edited: another id, the mark no longer applies.
        let edited = pool_id("solo.a", 3334, "bc1qa");
        assert!(m.probe_allowed(edited, true));
        // Back on it by other means (an outage round), and it pays.
        m.paid(MAIN, main);
        assert!(!m.is_left(MAIN, main));
        assert!(m.probe_allowed(main, true));
        // Paying in the other slot does not clear it.
        m.leave(MAIN, main);
        m.paid(FALLBACK, fb);
        assert!(m.is_left(MAIN, main));
    }

    #[test]
    fn switching_needs_another_pool_that_was_not_left_itself() {
        let mut m = Memory::new();
        let main = pool_id("solo.a", 3333, "bc1qa");
        let fb = pool_id("solo.b", 3333, "bc1qa");
        // On the main pool: to the fallback when one is set up.
        assert!(m.can_switch(false, true, fb));
        assert!(!m.can_switch(false, false, fb));
        // On the fallback: back to the main pool, unless it was left.
        assert!(m.can_switch(true, true, main));
        m.leave(MAIN, main);
        assert!(!m.can_switch(true, true, main));
        // Both left: warn only.
        m.leave(FALLBACK, fb);
        assert!(!m.can_switch(false, true, fb));
    }
}
