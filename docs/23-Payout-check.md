# Payout check

WarpAxe decodes the coinbase of every job and shows on the Pool page who
a block found on it would pay (`crates/stratum/src/payout.rs`). Until
now that was only shown. A solo pool that stops paying the miner's
address (misconfigured, taken over, or changed its terms) went unnoticed
until someone looked. The payout check acts on it: an event, a Discord
message, and a switch to the other pool. It does the same for a solo
pool that keeps more of the reward than the fee limit the owner set
(`payoutMaxFee`, default 2.0 %).

## The rule

`warpaxe_stratum::payout_check`, host-tested; the firmware feeds it in
`stratum.rs` (`pay_session`, `pay_job`).

- **Which pools.** Only a pool whose coinbase has paid the miner's address
  more than half of the coinbase at least once (`user * 2 > total`: a
  solo pool, with a fee or a donation output if it has one). That marks
  the pool *armed*. A pool that pays its miners in the coinbase by
  share (Ocean's TIDES, P2Pool, P2Poolv2) lists a small miner only while
  the miner's balance is over a threshold. Such a coinbase never pays one
  small miner most of the reward, so it never arms. The mark belongs to the
  pool's configuration: host (case ignored), port and worker name,
  hashed (`pool_id`). It is kept per slot (main, fallback) in NVS
  (`payseen`, one u64), so it survives a reboot. Editing host, port or
  worker gives another id, and the old mark no longer applies.
- **Never checked**: a worker name that is not an address (a pool account:
  `Wallet::parse` fails, the pool pays outside the coinbase), a pool that
  has never paid the address, and an address for a coin other than the
  one set in `coinTicker`. A Bitcoin address counts on `BTC` (or no
  ticker), any other coin's address on any other ticker. Where the two
  disagree, the check stays out.
- **Evidence.** Only a job whose coinbase was fully decoded counts, and
  only once the session is authorized (V1 `mining.authorize` answered
  true, Sv2 channel open). A pool may hand out templates for its own
  address until it knows the worker. A job without evidence (decode
  failed, a coinbase without outputs, before authorize) neither counts
  as a miss nor breaks a run of misses. On an armed pool any payment to
  the address counts as paid, even a small one.
- **When it acts.** On an armed pool, when the jobs of **2 different blocks
  in a row** pay the address 0 sat (`BLOCKS_TO_ACT`). Blocks are told
  apart by the hash of the block the job builds on. More templates of
  the same block count as one miss, and a paying job in between starts
  the count over. It acts once per session, and the next paying job
  clears the state.
- **Fee limit.** The same run also counts a block whose job pays the
  address but leaves the pool more than `payoutMaxFee`. The fee is
  `(total − user) / total`, figured as nerd-os-rs does:
  `take × 10000 / total` (rounded down, in hundredths of a percent) is
  over the limit when it is greater than `payoutMaxFee × 10`. Exactly
  the limit is within it. Data outputs (the witness commitment) carry
  0 sat and change nothing. A block paying nothing and a block over the
  fee count in the same run; the event says what the last one was. A
  block within the limit starts the run over, and only such a block
  counts as "paying again". A payment over the limit is no recovery.
  The fee is shown rounded up to a tenth, so a fee shown is over the
  limit exactly when it is: 2.04 % shows as 2.1 % against 2.0 %.
  `payoutMaxFee` is in tenths of a percent, 0 to 1000. 0 allows no fee
  at all, and 1000 (100 %) is no limit, which leaves only the
  missing-payout rule. Arming is unchanged: a coinbase that pays the
  miner more than half arms the pool even when its fee is over the
  limit, and that block already counts.
- **What it does.**
  - It records the event `payout_missing` (pool host `p`, what the pool
    keeps `sat`, and `sw`: 1 = to the fallback, 2 = back to the main
    pool, 0 = no other pool). For the fee it records `payout_fee`: pool
    host `p`, the fee `fee` and the limit `max` in tenths of a percent,
    and `sw`.
  - It sends a Discord message if a webhook is set and `alertPayout` is
    on. The message is its own and is never folded with pool switches.
    It does not name the pool host (docs/22, Privacy).
  - On the main pool with a fallback set up, the session ends and the
    miner moves to the fallback. On the fallback, it goes back to the
    main pool. No failure is counted for either.
  - If there is no other pool, or the other pool was itself left for not
    paying, it only warns. The miner keeps mining, and the Pool page
    shows a red note while the state lasts.
  - A pool found keeping the payout once is not reported again for the
    same reason until it pays within the limit or is edited. The mark
    keeps its reason (paying nothing, or the fee). If the pool later
    fails for the other reason, for two blocks again, that is a new
    event and message. This holds after a reconnect to it, and also when
    the miner comes back to it because the other pool is down. The
    switch still happens, without a second event or message.
  - Reports on one pool are at least ten minutes of uptime apart
    (`REPORT_COOLDOWN_S`), per slot, even when a paying job cleared the
    mark in between or the reason changed. Without that, a pool could
    alternate one paying job with two jobs on made-up block hashes and
    raise an event and a Discord message every few seconds. Within the
    cooldown the pool is still marked left and the switch still
    happens; only the event and the message are held back. A change
    of `payoutMaxFee` or `payoutCheck` ends the cooldown, so the next
    act after it is reported at once.
- **No automatic return.** A main pool left for not paying is marked
  *left* (RAM). The probe that normally takes the miner back from the
  fallback skips it. The same mark is set for a pool left for its fee.
  The mark clears when one of that pool's jobs pays the address within
  the limit again (for example when an outage round brings the miner
  there) or when the pool is edited.
- **Off switch.** With `payoutCheck` off, pools are still armed (so that
  switching it on later finds them armed), but nothing ever acts, and
  the probe ignores the *left* mark. The mark is not cleared: switched
  on again, the check honours it.

## Limits

- The *left* mark is RAM only. After a reboot the miner starts on the
  main pool again, and a pool that still keeps the payout is left again
  after two blocks.
- A fallback left for not paying does not block the normal switch to it
  after three failures on the main pool. If it still does not pay, it is
  left again after two blocks, without a second event.
- Leaving takes two blocks, about 20 minutes on Bitcoin. Jobs mined
  meanwhile pay the pool if one of them finds a block.
- A pool armed by one paying job that later pays through another script
  of the same address (P2PKH and P2WPKH of the same key) counts as not
  paying. `Wallet::pays` matches the exact address the worker name
  starts with.
- Stratum V2: WarpAxe opens extended channels only, whose jobs carry the
  coinbase, so they are checked like V1. A standard channel (not used)
  would carry no coinbase and so no evidence.
- Changing the settings clears *left* marks. A new `payoutMaxFee` clears
  the marks of pools left for their fee. Switching `payoutCheck`, on or
  off, clears every mark. If the miner is on the fallback because the
  main pool was left, and that mark is gone, the session is rebuilt at
  once (the same reconnect a pool change makes). The main pool is then
  judged again under the new settings.
- The 2.0 % default applies to every armed pool from this release on,
  also to pools armed before the update. That is intended: a solo pool
  that keeps more than 2 % is left unless the owner raises the limit
  (release note).
- The fee field on the Pool page shows only while the `payoutCheck`
  switch is on. It follows the switch as it is flipped, before saving.

## API

- `PATCH /api/system`: `payoutCheck` (bool, default `true`), `alertPayout`
  (bool, default `true`; `0`/`1` taken too), `payoutMaxFee` (tenths of a
  percent, default 20 = 2.0 %, 0 to 1000; more is refused). The Pool
  page shows it in percent with 0.1 steps.
- `GET /api/system/info`:
  - `payoutCheck`: the setting.
  - `payoutArmed`: the pool in use has paid the address.
  - `payoutMissing`: the pool in use is found keeping the payout now
    (only while the check is on).
  - `payoutFeeOver`: the pool in use is found keeping more than the
    limit now (only while the check is on).
  - `payoutMaxFee`: the limit, tenths of a percent.
  - `payoutFee`: the pool's fee in the session's last job with evidence,
    tenths of a percent, rounded up. It is 1000 for a coinbase that pays
    the address nothing, and `null` before such a job (or for a pool
    account). The Pool page's green mark shows it
    ("✓ payout checked · fee 0 %").
  - `payoutLastMissing`: `null`, or `{"host": …, "uptime_s": …, "why":
    "missing" | "fee"}` for the last pool found so since boot.
- Event log: codes `payout_missing` and `payout_fee`, kind `pool`. The
  texts are translated in the web UI (`evd_payout_missing`,
  `evd_payout_fee`).
- NerdOS's `alrt_disc_cb_en` (its coinbase check's Discord switch) sets
  `alertPayout` on the first start.

## Tested

By host tests only (`cargo test -p warpaxe-stratum payout_check`, plus
the event and alert texts in `warpaxe-diag` and `warpaxe-alerts`). The
firmware build passes. Not yet run on a device or against a pool that
stops paying.
