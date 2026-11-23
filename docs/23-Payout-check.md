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
  at least once. That marks the pool *armed*. The mark belongs to the
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
- **Evidence.** Only a job whose coinbase was fully decoded counts. A job
  without one (decode failed, a coinbase without outputs) is no evidence
  either way. It neither counts as a miss nor breaks a run of misses.
- **When it acts.** On an armed pool, **2 decoded jobs in a row** that pay
  the address 0 sat (`MISSES_TO_ACT`). One odd job between paying ones is
  forgiven. It acts once per session, and the next paying job clears the
  state.
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
    shows a red note while the state lasts. A reconnect to the same pool
    brings no second event or message while the pool still does not pay.
- **No automatic return.** A main pool left for not paying is marked
  *left* (RAM). The probe that normally takes the miner back from the
  fallback skips it. The mark clears when one of that pool's jobs pays
  the address again (for example when an outage round brings the miner
  there), when the pool is edited, or when `payoutCheck` is switched off.
- **Off switch.** With `payoutCheck` off, pools are still armed (so that
  switching it on later finds them armed), but nothing ever acts.

## Limits

- The *left* mark is RAM only. After a reboot the miner starts on the
  main pool again, and a pool that still keeps the payout is left again
  after its first two jobs.
- A fallback left for not paying does not block the normal switch to it
  after three failures on the main pool. If it still does not pay, it is
  left again after two jobs.
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
