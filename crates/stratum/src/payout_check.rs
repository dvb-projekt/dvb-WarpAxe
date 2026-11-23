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
//! I/O, so it is host-tested. The same run of two blocks also counts a
//! coinbase that pays the address but keeps more than `payoutMaxFee`
//! for the pool ([`take_bp`], [`fee_over`]): one rule for both, and a
//! block within the limit starts the run over.
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

/// After a pool is reported for keeping the payout (event, Discord), no
/// further report for the same pool for this long, in uptime seconds,
/// even if a paying job cleared its *left* mark in between: a hostile
/// pool cannot turn the check into an alert loop by alternating one
/// paying job with two jobs on fake block hashes. The switch itself is
/// not held back.
pub const REPORT_COOLDOWN_S: u64 = 600;

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

/// The highest `payoutMaxFee` (tenths of a percent): 100 %, no limit.
pub const MAX_FEE_LIMIT: u16 = 1000;
/// The default `payoutMaxFee`: 2.0 %.
pub const DEFAULT_MAX_FEE: u16 = 20;

/// What the pool keeps of a coinbase, in hundredths of a percent
/// (`(total − user) × 10000 / total`, rounded down), as nerd-os-rs
/// figures it. `total` 0 keeps nothing.
pub fn take_bp(user: u64, total: u64) -> u16 {
    if total == 0 {
        return 0;
    }
    let take = u128::from(total.saturating_sub(user));
    (take * 10_000 / u128::from(total)) as u16
}

/// Whether a take of `take_bp` (hundredths of a percent) is over the
/// limit `max_fee` (tenths of a percent): `take_bp > max_fee × 10`, the
/// comparison nerd-os-rs makes. At [`MAX_FEE_LIMIT`] nothing is over.
pub fn fee_over(take_bp: u16, max_fee: u16) -> bool {
    u32::from(take_bp) > u32::from(max_fee) * 10
}

/// A take as shown (tenths of a percent), rounded up so that a fee
/// shown is over the limit exactly when [`fee_over`] says so: 2.04 % is
/// 2.1 % against a limit of 2.0 %.
pub fn fee_tenths(take_bp: u16) -> u16 {
    take_bp.div_ceil(10)
}

/// What one job says about the payout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The coinbase pays the miner's address something; `solo` when
    /// that is more than half of the coinbase (a solo pool, perhaps with
    /// a fee or a donation output), which alone arms a pool. `take_bp`
    /// is what the pool keeps ([`take_bp`]).
    Paid {
        /// The miner gets more than half of the coinbase.
        solo: bool,
        /// The pool's take, hundredths of a percent.
        take_bp: u16,
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

impl Evidence {
    /// The pool's fee as shown (tenths of a percent); 100 % for a
    /// coinbase that pays the address nothing, `None` without evidence.
    pub fn fee(&self) -> Option<u16> {
        match self {
            Evidence::Paid { take_bp, .. } => Some(fee_tenths(*take_bp)),
            Evidence::Missing { .. } => Some(MAX_FEE_LIMIT),
            Evidence::None => None,
        }
    }

    /// The job pays the address and keeps the pool within `max_fee`.
    pub fn good(&self, max_fee: u16) -> bool {
        matches!(self, Evidence::Paid { take_bp, .. } if !fee_over(*take_bp, max_fee))
    }
}

/// What a job's payout (`None`: not decoded) says, for a miner set to
/// the coin `ticker`. Data outputs (the witness commitment) carry 0 sat
/// and do not change the take.
pub fn evidence(pay: Option<&Payout>, ticker: &str) -> Evidence {
    match pay {
        Some(p) if p.own_address && coin_matches(ticker, p.bitcoin) && p.total > 0 => {
            if p.user > 0 {
                Evidence::Paid {
                    solo: u128::from(p.user) * 2 > u128::from(p.total),
                    take_bp: take_bp(p.user, p.total),
                }
            } else {
                Evidence::Missing { kept_sat: p.total }
            }
        }
        _ => Evidence::None,
    }
}

/// What was found wrong with an armed pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Its coinbase pays the address nothing; `kept_sat` is the total.
    Missing {
        /// The coinbase's total, satoshis.
        kept_sat: u64,
    },
    /// It keeps more than the limit: `fee` and `limit` in tenths of a
    /// percent.
    Fee {
        /// The fee, tenths of a percent ([`fee_tenths`]).
        fee: u16,
        /// `payoutMaxFee` at the time.
        limit: u16,
    },
}

impl Problem {
    /// For the fee (else for paying nothing).
    pub fn is_fee(&self) -> bool {
        matches!(self, Problem::Fee { .. })
    }
}

/// What the firmware does after a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing new.
    Nothing,
    /// The pool paid the address most of the coinbase for the first
    /// time: armed now (the firmware stores the mark).
    Arm,
    /// The armed pool paid nothing, or kept more than the limit, in the
    /// jobs of [`BLOCKS_TO_ACT`] different blocks in a row: warn, and
    /// switch where there is another pool. The problem is the last
    /// block's.
    Act(Problem),
    /// The pool pays the address within the limit again after
    /// [`Step::Act`].
    Recovered,
}

/// One session's verdicts. A new session starts a new one, armed when
/// [`Memory::armed`] says so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Check {
    armed: bool,
    /// Blocks in a row with a job that paid nothing or kept too much.
    misses: u8,
    /// The block of the last such job counted.
    miss_block: Option<u64>,
    /// What was found and acted on, until a good job.
    problem: Option<Problem>,
}

impl Check {
    /// A session's check; `armed` when this pool has paid the address
    /// before.
    pub const fn new(armed: bool) -> Self {
        Self {
            armed,
            misses: 0,
            miss_block: None,
            problem: None,
        }
    }

    /// The pool in use is armed.
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// The pool in use has been found paying the address nothing and has
    /// not paid within the limit since.
    pub fn missing(&self) -> bool {
        matches!(self.problem, Some(Problem::Missing { .. }))
    }

    /// The pool in use has been found keeping more than the limit and
    /// has not paid within it since.
    pub fn fee_over(&self) -> bool {
        matches!(self.problem, Some(Problem::Fee { .. }))
    }

    /// The next job's evidence; `block` names the block the job builds
    /// on (its previous block's hash, or part of it), so bad jobs are
    /// counted per block, not per job. `enabled` is the `payoutCheck`
    /// setting: without it a pool is still armed (so that turning the
    /// check on later finds it armed) but never acted on. `max_fee` is
    /// `payoutMaxFee`, tenths of a percent.
    pub fn job(&mut self, e: Evidence, block: u64, enabled: bool, max_fee: u16) -> Step {
        if e == Evidence::None {
            return Step::Nothing;
        }
        let mut step = Step::Nothing;
        if let Evidence::Paid { solo: true, .. } = e {
            if !self.armed {
                self.armed = true;
                step = Step::Arm;
            }
        }
        if !self.armed {
            // A share of the reward among others (a pool that pays its
            // miners in the coinbase), or nothing: not a solo pool.
            return Step::Nothing;
        }
        if !enabled || e.good(max_fee) {
            self.misses = 0;
            self.miss_block = None;
            if enabled && self.problem.take().is_some() {
                return Step::Recovered;
            }
            return step;
        }
        let problem = match e {
            Evidence::Missing { kept_sat } => Problem::Missing { kept_sat },
            Evidence::Paid { take_bp, .. } => Problem::Fee {
                fee: fee_tenths(take_bp),
                limit: max_fee,
            },
            Evidence::None => return step,
        };
        if let Some(old) = self.problem {
            if old.is_fee() == problem.is_fee() {
                // Acted on already, the same reason: the figures follow,
                // no second act, and a run for the other reason starts
                // over.
                self.problem = Some(problem);
                self.misses = 0;
                self.miss_block = None;
                return step;
            }
            // The other reason: acted on again once it, too, holds for
            // two blocks in a row (below).
        }
        if self.miss_block == Some(block) {
            // Another template of the same block.
            return step;
        }
        self.miss_block = Some(block);
        self.misses = self.misses.saturating_add(1);
        if self.misses >= BLOCKS_TO_ACT {
            self.problem = Some(problem);
            self.misses = 0;
            self.miss_block = None;
            Step::Act(problem)
        } else {
            step
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
    /// Why each was left: for the fee (else for paying nothing).
    left_fee: [bool; 2],
    /// The pool last reported in each slot and the uptime second of
    /// that report ([`REPORT_COOLDOWN_S`]); RAM only.
    reported: [(u32, u64); 2],
}

impl Memory {
    /// Nothing seen, nothing left.
    pub const fn new() -> Self {
        Self {
            seen: [0; 2],
            left: [0; 2],
            left_fee: [false; 2],
            reported: [(0, 0); 2],
        }
    }

    /// From the stored word ([`Memory::stored`]); nothing left.
    pub const fn from_stored(v: u64) -> Self {
        Self {
            seen: [(v >> 32) as u32, v as u32],
            left: [0; 2],
            left_fee: [false; 2],
            reported: [(0, 0); 2],
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

    /// A job of the armed pool `id` in `slot` paid the address but kept
    /// more than the limit: armed (stored), the mark for leaving it
    /// stays. True when the stored part changed.
    pub fn see(&mut self, slot: usize, id: u32) -> bool {
        if slot > FALLBACK || id == 0 {
            return false;
        }
        let changed = self.seen[slot] != id;
        self.seen[slot] = id;
        changed
    }

    /// The pool `id` in `slot` was left for not paying (`fee`: for
    /// keeping more than the limit).
    pub fn leave(&mut self, slot: usize, id: u32, fee: bool) {
        if slot <= FALLBACK {
            self.left[slot] = id;
            self.left_fee[slot] = fee;
        }
    }

    /// The armed pool `id` in `slot` is acted on (two blocks without the
    /// payout, `fee`: or over the fee limit) at uptime `now_s`: it is
    /// marked left for that reason. True when the act is to be reported
    /// (event, Discord): not when it was already left for the same
    /// reason and has not paid within the limit since, and not within
    /// [`REPORT_COOLDOWN_S`] of the last report on this pool in this
    /// slot, whatever happened in between (a paying job, another
    /// reason).
    pub fn act(&mut self, slot: usize, id: u32, fee: bool, now_s: u64) -> bool {
        if slot > FALLBACK || id == 0 {
            return false;
        }
        let same = self.left_reason(slot, id) == Some(fee);
        self.leave(slot, id, fee);
        let (last_id, last_s) = self.reported[slot];
        let cooling = last_id == id && now_s.saturating_sub(last_s) < REPORT_COOLDOWN_S;
        if same || cooling {
            return false;
        }
        self.reported[slot] = (id, now_s);
        true
    }

    /// Why the pool `id` in `slot` was left: `Some(true)` for the fee,
    /// `Some(false)` for paying nothing, `None` when it is not left.
    pub fn left_reason(&self, slot: usize, id: u32) -> Option<bool> {
        self.is_left(slot, id).then(|| self.left_fee[slot])
    }

    /// The payout check's settings changed: `all` (`payoutCheck`
    /// switched) forgets every left mark, otherwise (`payoutMaxFee`
    /// changed) those left for the fee only. True when the main pool
    /// `main_id` was left and no longer is: a miner on the fallback for
    /// it should judge the main pool again.
    pub fn settings_changed(&mut self, main_id: u32, all: bool) -> bool {
        let was = self.is_left(MAIN, main_id);
        for slot in [MAIN, FALLBACK] {
            if all || self.left_fee[slot] {
                self.left[slot] = 0;
                self.left_fee[slot] = false;
            }
        }
        was && !self.is_left(MAIN, main_id)
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

    const PAID: Evidence = Evidence::Paid {
        solo: true,
        take_bp: 0,
    };
    const MAX: u16 = DEFAULT_MAX_FEE;

    #[test]
    fn only_a_decoded_coinbase_to_an_address_of_the_coin_counts() {
        let p99 = Evidence::Paid {
            solo: true,
            take_bp: 100,
        };
        assert_eq!(evidence(Some(&pay(100, 99)), "BTC"), p99);
        assert_eq!(evidence(Some(&pay(100, 99)), ""), p99);
        assert_eq!(evidence(Some(&pay(100, 100)), "BTC"), PAID);
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
        let solo = |e: Evidence| matches!(e, Evidence::Paid { solo: true, .. });
        // A solo pool with a 2 % fee, or a donation output.
        assert!(solo(evidence(Some(&pay(100, 98)), "BTC")));
        assert!(solo(evidence(Some(&pay(100, 51)), "BTC")));
        // Half or less: a share among others.
        assert!(!solo(evidence(Some(&pay(100, 50)), "BTC")));
        assert!(!solo(evidence(Some(&pay(312_500_000, 40_000)), "BTC")));
        // No overflow at the largest sums.
        let max = 21_000_000 * 100_000_000;
        assert_eq!(evidence(Some(&pay(max, max)), "BTC"), PAID);
    }

    #[test]
    fn a_pool_never_seen_paying_is_never_acted_on() {
        let mut c = Check::new(false);
        for b in 0..10 {
            assert_eq!(
                c.job(Evidence::Missing { kept_sat: 5 }, b, true, MAX),
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
        let small = Evidence::Paid {
            solo: false,
            take_bp: 9000,
        };
        let miss = Evidence::Missing {
            kept_sat: 312_500_000,
        };
        for b in 0..6 {
            assert_eq!(c.job(small, b * 2, true, MAX), Step::Nothing);
            assert_eq!(c.job(miss, b * 2 + 1, true, MAX), Step::Nothing);
        }
        assert!(!c.armed());
    }

    #[test]
    fn two_blocks_in_a_row_without_payout_act_once() {
        let mut c = Check::new(false);
        assert_eq!(c.job(PAID, 1, true, MAX), Step::Arm);
        assert!(c.armed());
        assert_eq!(c.job(PAID, 1, true, MAX), Step::Nothing);
        let miss = Evidence::Missing {
            kept_sat: 312_500_000,
        };
        assert_eq!(c.job(miss, 2, true, MAX), Step::Nothing);
        // More templates of the same block are one miss.
        for _ in 0..5 {
            assert_eq!(c.job(miss, 2, true, MAX), Step::Nothing);
        }
        assert!(!c.missing());
        assert_eq!(
            c.job(miss, 3, true, MAX),
            Step::Act(Problem::Missing {
                kept_sat: 312_500_000
            })
        );
        assert!(c.missing());
        // Once only, however many follow.
        assert_eq!(c.job(miss, 4, true, MAX), Step::Nothing);
        assert_eq!(c.job(PAID, 5, true, MAX), Step::Recovered);
        assert!(!c.missing());
        assert_eq!(c.job(PAID, 5, true, MAX), Step::Nothing);
    }

    fn paid(take_bp: u16) -> Evidence {
        Evidence::Paid {
            solo: take_bp < 5000,
            take_bp,
        }
    }

    #[test]
    fn a_small_payment_on_an_armed_pool_is_a_fee_without_a_limit_paid() {
        let mut c = Check::new(true);
        let miss = Evidence::Missing { kept_sat: 1 };
        // Without a limit (100 %) any payment counts as paid.
        assert_eq!(c.job(miss, 1, true, MAX_FEE_LIMIT), Step::Nothing);
        assert_eq!(c.job(paid(9000), 2, true, MAX_FEE_LIMIT), Step::Nothing);
        assert_eq!(c.job(miss, 3, true, MAX_FEE_LIMIT), Step::Nothing);
        assert!(!c.missing() && !c.fee_over());
    }

    #[test]
    fn fee_math_matches_nerd_os() {
        // take × 10000 / total, rounded down, against limit × 10.
        assert_eq!(take_bp(98, 100), 200);
        assert_eq!(take_bp(312_500_000, 312_500_000), 0);
        assert_eq!(take_bp(0, 312_500_000), 10_000);
        assert_eq!(take_bp(5, 0), 0);
        // 2.04 % keeps 204: over a 2.0 % limit, within 2.1 %.
        let total = 312_500_000u64;
        let bp = take_bp(total - total * 204 / 10_000, total);
        assert_eq!(bp, 204);
        assert!(fee_over(bp, 20));
        assert!(!fee_over(bp, 21));
        // Exactly the limit is within it.
        assert!(!fee_over(200, 20));
        assert!(fee_over(201, 20));
        // Rounding down: 1.9999 % is 199.
        assert_eq!(take_bp(980_001, 1_000_000), 199);
        // 0 allows no fee at all; 1000 (100 %) nothing is over.
        assert!(fee_over(1, 0) && !fee_over(0, 0));
        assert!(!fee_over(10_000, MAX_FEE_LIMIT));
        // Shown rounded up, so shown over the limit exactly when over.
        assert_eq!(fee_tenths(204), 21);
        assert_eq!(fee_tenths(200), 20);
        assert_eq!(fee_tenths(0), 0);
        assert_eq!(fee_tenths(1), 1);
        for bp in [0u16, 1, 9, 10, 11, 199, 200, 201, 209, 210, 9999, 10_000] {
            for limit in [0u16, 1, 20, 21, 500, 1000] {
                assert_eq!(fee_over(bp, limit), fee_tenths(bp) > limit, "{bp} {limit}");
            }
        }
        // No overflow at the largest sums.
        let max = 21_000_000 * 100_000_000;
        assert_eq!(take_bp(1, max), 9999);
        assert_eq!(paid(300).fee(), Some(30));
        assert_eq!(Evidence::Missing { kept_sat: 1 }.fee(), Some(1000));
        assert_eq!(Evidence::None.fee(), None);
    }

    #[test]
    fn two_blocks_over_the_fee_act_once() {
        let mut c = Check::new(false);
        // Arms on a solo-shaped coinbase even with a fee over the limit;
        // that block already counts.
        assert_eq!(c.job(paid(300), 1, true, MAX), Step::Arm);
        assert!(c.armed() && !c.fee_over());
        assert_eq!(c.job(paid(300), 1, true, MAX), Step::Nothing);
        assert_eq!(
            c.job(paid(350), 2, true, MAX),
            Step::Act(Problem::Fee { fee: 35, limit: 20 })
        );
        assert!(c.fee_over() && !c.missing());
        assert_eq!(c.job(paid(350), 3, true, MAX), Step::Nothing);
        // Back within the limit: recovered.
        assert_eq!(c.job(paid(200), 4, true, MAX), Step::Recovered);
        assert!(!c.fee_over());
    }

    #[test]
    fn a_block_within_the_limit_resets_the_fee_run() {
        let mut c = Check::new(true);
        for b in 0..5 {
            assert_eq!(c.job(paid(300), b * 2, true, MAX), Step::Nothing);
            assert_eq!(c.job(paid(100), b * 2 + 1, true, MAX), Step::Nothing);
        }
        // A good template within a block resets too.
        assert_eq!(c.job(paid(300), 20, true, MAX), Step::Nothing);
        assert_eq!(c.job(paid(0), 20, true, MAX), Step::Nothing);
        assert_eq!(c.job(paid(300), 21, true, MAX), Step::Nothing);
        assert!(!c.fee_over());
    }

    #[test]
    fn missing_and_fee_count_in_one_run() {
        let miss = Evidence::Missing { kept_sat: 7 };
        // A block paying nothing, then one over the fee: the last says why.
        let mut c = Check::new(true);
        assert_eq!(c.job(miss, 1, true, MAX), Step::Nothing);
        assert_eq!(
            c.job(paid(500), 2, true, MAX),
            Step::Act(Problem::Fee { fee: 50, limit: 20 })
        );
        // Now nothing at all: another reason, acted on again once it
        // holds for two blocks.
        assert_eq!(c.job(miss, 3, true, MAX), Step::Nothing);
        assert!(c.fee_over());
        assert_eq!(
            c.job(miss, 4, true, MAX),
            Step::Act(Problem::Missing { kept_sat: 7 })
        );
        assert!(c.missing() && !c.fee_over());
        // And the same reason again: no act.
        assert_eq!(c.job(miss, 5, true, MAX), Step::Nothing);
        assert_eq!(c.job(miss, 6, true, MAX), Step::Nothing);
        // The other way round.
        let mut c = Check::new(true);
        assert_eq!(c.job(paid(500), 1, true, MAX), Step::Nothing);
        assert_eq!(
            c.job(miss, 2, true, MAX),
            Step::Act(Problem::Missing { kept_sat: 7 })
        );
        // A payment over the limit is no recovery; a single block of it
        // is no new reason, and one of the old reason breaks its run.
        assert_eq!(c.job(paid(500), 3, true, MAX), Step::Nothing);
        assert!(c.missing());
        assert_eq!(c.job(miss, 4, true, MAX), Step::Nothing);
        assert_eq!(c.job(paid(500), 5, true, MAX), Step::Nothing);
        assert!(c.missing());
        assert_eq!(c.job(paid(20), 6, true, MAX), Step::Recovered);
    }

    #[test]
    fn switched_off_the_fee_is_not_acted_on_either() {
        let mut c = Check::new(true);
        for b in 0..4 {
            assert_eq!(c.job(paid(900), b, false, MAX), Step::Nothing);
        }
        assert!(!c.fee_over());
    }

    #[test]
    fn one_odd_block_between_paying_ones_is_forgiven() {
        let mut c = Check::new(true);
        let miss = Evidence::Missing { kept_sat: 1 };
        for b in 0..5 {
            assert_eq!(c.job(miss, b, true, MAX), Step::Nothing);
            assert_eq!(c.job(PAID, b, true, MAX), Step::Nothing);
        }
        assert!(!c.missing());
    }

    #[test]
    fn jobs_without_evidence_neither_count_nor_break_the_row() {
        let mut c = Check::new(true);
        let miss = Evidence::Missing { kept_sat: 1 };
        assert_eq!(c.job(miss, 1, true, MAX), Step::Nothing);
        assert_eq!(c.job(Evidence::None, 2, true, MAX), Step::Nothing);
        assert_eq!(c.job(Evidence::None, 3, true, MAX), Step::Nothing);
        assert_eq!(
            c.job(miss, 4, true, MAX),
            Step::Act(Problem::Missing { kept_sat: 1 })
        );
    }

    #[test]
    fn switched_off_it_still_arms_but_never_acts() {
        let mut c = Check::new(false);
        assert_eq!(c.job(PAID, 1, false, MAX), Step::Arm);
        let miss = Evidence::Missing { kept_sat: 1 };
        for b in 2..6 {
            assert_eq!(c.job(miss, b, false, MAX), Step::Nothing);
        }
        assert!(!c.missing());
        // Turned on: the row starts from there.
        assert_eq!(c.job(miss, 6, true, MAX), Step::Nothing);
        assert_eq!(
            c.job(miss, 7, true, MAX),
            Step::Act(Problem::Missing { kept_sat: 1 })
        );
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
        m.leave(MAIN, main, false);
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
        m.leave(MAIN, main, false);
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
        m.leave(MAIN, main, false);
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
        m.leave(MAIN, main, false);
        assert!(!m.can_switch(true, true, main));
        // Both left: warn only.
        m.leave(FALLBACK, fb, false);
        assert!(!m.can_switch(false, true, fb));
    }

    #[test]
    fn left_marks_keep_their_reason_and_settings_clear_them() {
        let main = pool_id("solo.a", 3333, "bc1qa");
        let fb = pool_id("solo.b", 3333, "bc1qa");
        let mut m = Memory::new();
        m.leave(MAIN, main, true);
        m.leave(FALLBACK, fb, false);
        assert_eq!(m.left_reason(MAIN, main), Some(true));
        assert_eq!(m.left_reason(FALLBACK, fb), Some(false));
        assert_eq!(m.left_reason(MAIN, fb), None);
        // The reason changes with a new leave.
        m.leave(MAIN, main, false);
        assert_eq!(m.left_reason(MAIN, main), Some(false));
        m.leave(MAIN, main, true);
        // A new fee limit: the fee marks go, the main pool is to be
        // judged again.
        assert!(m.settings_changed(main, false));
        assert!(!m.is_left(MAIN, main));
        assert!(m.is_left(FALLBACK, fb));
        // Again: nothing left for the fee, nothing to do.
        assert!(!m.settings_changed(main, false));
        // Main left for paying nothing stays left on a fee change ...
        m.leave(MAIN, main, false);
        assert!(!m.settings_changed(main, false));
        assert!(m.is_left(MAIN, main));
        // ... and goes when the check is switched.
        assert!(m.settings_changed(main, true));
        assert!(!m.is_left(MAIN, main) && !m.is_left(FALLBACK, fb));
        // Only the fallback left: nothing for the main pool to redo.
        m.leave(FALLBACK, fb, true);
        assert!(!m.settings_changed(main, true));
    }

    #[test]
    fn reports_are_held_back_for_ten_minutes_per_pool() {
        let main = pool_id("solo.a", 3333, "bc1qa");
        let other = pool_id("solo.c", 3333, "bc1qa");
        let fb = pool_id("solo.b", 3333, "bc1qa");
        let mut m = Memory::new();
        m.paid(MAIN, main);
        // The first act is reported, at any uptime.
        assert!(m.act(MAIN, main, false, 5));
        assert!(m.is_left(MAIN, main));
        // Same reason, still left: never a second report.
        assert!(!m.act(MAIN, main, false, 5 + REPORT_COOLDOWN_S));
        // A hostile loop: one paying job clears the mark, two jobs on
        // fake block hashes act again, seconds later. Left again, but
        // not reported.
        for t in [20, 40, 300, 604] {
            m.paid(MAIN, main);
            assert!(!m.is_left(MAIN, main));
            assert!(!m.act(MAIN, main, false, t));
            assert!(m.is_left(MAIN, main));
        }
        // A new reason within the cooldown: left for it, not reported.
        assert!(!m.act(MAIN, main, true, 605 - 1));
        assert_eq!(m.left_reason(MAIN, main), Some(true));
        // After the cooldown a new reason is reported again ...
        assert!(m.act(MAIN, main, false, 5 + REPORT_COOLDOWN_S));
        // ... and so is the same reason after a paying job.
        m.paid(MAIN, main);
        assert!(!m.act(MAIN, main, false, 4 + 2 * REPORT_COOLDOWN_S));
        m.paid(MAIN, main);
        assert!(m.act(MAIN, main, false, 5 + 2 * REPORT_COOLDOWN_S));
        // Another pool in the slot, or the other slot, is its own.
        assert!(m.act(MAIN, other, false, 6 + 2 * REPORT_COOLDOWN_S));
        assert!(m.act(FALLBACK, fb, true, 6 + 2 * REPORT_COOLDOWN_S));
        // No slot, no pool: nothing.
        assert!(!m.act(2, main, false, 0) && !m.act(MAIN, 0, false, 0));
    }
}
