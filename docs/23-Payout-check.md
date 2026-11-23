# Payout check

WarpAxe decodes the coinbase of every job and shows on the Pool page who
a block found on it would pay (`crates/stratum/src/payout.rs`). Until
now that was only shown. A solo pool that stops paying the miner's
address (misconfigured, taken over, or changed its terms) went unnoticed
until someone looked. The payout check acts on it: an event, a Discord
message, and a switch to the other pool.

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
- **What it does.**
  - It records the event `payout_missing` (pool host `p`, what the pool
    keeps `sat`, and `sw`: 1 = to the fallback, 2 = back to the main
    pool, 0 = no other pool).
  - It sends a Discord message if a webhook is set and `alertPayout` is
    on. The message is its own and is never folded with pool switches.
    It does not name the pool host (docs/22, Privacy).
  - On the main pool with a fallback set up, the session ends and the
    miner moves to the fallback. On the fallback, it goes back to the
    main pool. No failure is counted for either.
  - If there is no other pool, or the other pool was itself left for not
    paying, it only warns. The miner keeps mining, and the Pool page
    shows a red note while the state lasts.
  - A pool found keeping the payout once is not reported again until it
    pays or is edited. This holds after a reconnect to it, and also when
    the miner comes back to it because the other pool is down. The
    switch still happens, without a second event or message.
- **No automatic return.** A main pool left for not paying is marked
  *left* (RAM). The probe that normally takes the miner back from the
  fallback skips it. The mark clears when one of that pool's jobs pays
  the address again (for example when an outage round brings the miner
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
- No maximum-fee rule (a pool that keeps part of the reward). That may
  come later.

## API

- `PATCH /api/system`: `payoutCheck` (bool, default `true`), `alertPayout`
  (bool, default `true`; `0`/`1` taken too).
- `GET /api/system/info`:
  - `payoutCheck`: the setting.
  - `payoutArmed`: the pool in use has paid the address.
  - `payoutMissing`: the pool in use is found keeping the payout now
    (only while the check is on).
  - `payoutLastMissing`: `null`, or `{"host": …, "uptime_s": …}` for the
    last pool found so since boot.
- Event log: code `payout_missing`, kind `pool`. The text is translated
  in the web UI (`evd_payout_missing`).
- NerdOS's `alrt_disc_cb_en` (its coinbase check's Discord switch) sets
  `alertPayout` on the first start.

## Tested

By host tests only (`cargo test -p warpaxe-stratum payout_check`, plus
the event and alert texts in `warpaxe-diag` and `warpaxe-alerts`). The
firmware build passes. Not yet run on a device or against a pool that
stops paying.
