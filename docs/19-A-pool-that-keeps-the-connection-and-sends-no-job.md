# A pool that keeps the connection and sends no job

Until v0.1.47 a miner stayed with a pool for as long as the connection
lived. A pool that stopped sending new jobs but kept talking held it
there indefinitely: the miner hashed the last job, the pool rejected the
shares as stale or ignored them, and the fallback pool never took over.

## The report

On 2026-11-20 a user on Discord found four Bitaxe Gammas at a BCH pool
hashing without accepted shares. The diagnostics file showed
the miner healthy (1.56 TH/s, no errors, no rejects) and the pool,
us-bch.solofury.com:7070, sending no `mining.notify` for about 69
minutes, only a `mining.ping` every ten seconds. ckpool does exactly
that while its node gives it no new block template, "to keep [the
miners] connected while bitcoind recovers" (`stratifier.c`,
`broadcast_ping`). The pings are data, so the read timeout of
`link::READ_TIMEOUT` (180 s) never ran out, and a fallback pool was set
but never used. All four miners were on the same pool and stalled at
the same time.

## The rule

The watch lives in `crates/stratum/src/stale.rs` (`Watch`, `Verdict`)
and is applied by `firmware/warpaxe/src/stratum.rs` (`run`, `session`
for Stratum V1, `session_v2` for Sv2).

1. **Ten minutes without a new job ends the session** (`LIMIT_S` = 600).
   Only sessions that had work count; the clock restarts with every job.
   The session ends with `StaleJob` and the miner reconnects to the same
   pool (event `pool_stale`).
2. **Five more minutes without fresh work switches pools**
   (`RETRY_LIMIT_S` = 300). A job counts as fresh only if it comes at
   least a minute after the session's first job (`FRESH_AFTER_S` = 60):
   a stuck pool hands out a job right after the connect too. On the main
   pool this switches to the fallback (event `pool_stale_fallback`); on
   the fallback it goes back to the main pool (event `fallback_stale`).
   Without a fallback the miner keeps reconnecting with the short limit.
   A stuck fallback, too, is first reconnected after ten minutes, so
   `fallback_stale` reports about 300 s. The seconds in the events are a
   little over the limit: the check runs when something arrives (on V1
   every ping, on Sv2 every frame).
3. **A pool that sends no job at all** within a minute of the connect
   (`FIRST_JOB_S` = 60), even while it pings, ends the session as a
   failed attempt, like a pool that does not answer.
4. **The main pool stays marked.** While on the fallback the probe
   (`probe_primary`) checks the main pool after 10, 20, then every 40
   minutes, counted from the switch (not from the probe before: the first
   device test saw 15 minutes instead of 20); fresh work from the main
   pool, a settings change or a session that held there starts again at
   10. A marked main pool brought back by the probe gets the short limit
   straight away; still stuck, the miner is back on the fallback after
   five minutes. Back after `fallback_stale`, it gets the full 600 s. Fresh work from the main pool clears the mark and
   sets the probe interval back. Only then does the event log say
   "main pool answers again" (`pool_back`): that the probe reached the
   pool says nothing about one that was left for sending no jobs. A
   main pool left for failed connects is reported back at once, as
   before.
5. **Pings are answered.** `mining.ping` with an id gets
   `{"id":…,"result":"pong","error":null}` through the submit thread, so
   there is one writer on the link. ckpool only logs the pong; the answer
   is for pools that might expect one.

Why ten minutes: pools send a fresh job at least every minute or so
(ckpool every 30 s, with a new block at once), so ten minutes without
one means the pool is stuck, and it is short enough that a stuck pool
costs little.

## Sv2

Sv2 sessions use the same rule. Sv2 lets the miner roll the time
itself, so a healthy Sv2 pool might in principle send a new job only
with a new block, and blocks can be an hour apart. A host probe
(`sv2watch`, the firmware's own client) measured this on 2026-11-20 for
two hours, six blocks, one gap of 77 minutes between blocks:

| Pool | Jobs | Median gap | Longest gap |
|---|---|---|---|
| Braiins (v2.stratum.braiins.com:3336) | 260 | 29 s | 30 s |
| Blitzpool (blitzpool.yourdevice.ch:3333) | 152 | 48 s | 121 s |

Both refreshed the job throughout the block gap, so the V1 limits hold:
ten minutes is five times the longest gap seen. A stuck Sv2 pool keeps
answering shares instead of pinging, so the check runs on every frame;
a pool that sends nothing at all ends the session by the read timeout.
The Sv2 thread now hands its error back as it is instead of as text, so
`run` can tell a `StaleJob` from other ends (it could not before).

### The group-channel bug the measurement found

The same probe showed that the Sv2 client dropped messages a pool sends
to the channel's group (`crates/sv2/src/channel.rs`): it read
`group_channel_id` from `OpenExtendedMiningChannelSuccess` and then
accepted only its own channel id. Blitzpool sends the first job to the
channel and every later job and every new block to group 2: v0.1.47
hashed the first job and kept hashing it after the next block (the old
client, run alongside, accepted 1 of 154 jobs and lost all five block
changes it saw in two hours).
Braiins sends the prev hash to group 0: no job ever started. WarpPool
sends everything to the channel id, so v0.1.47 was not affected there.

Jobs, prev hashes and targets sent to the group now count as the
channel's own, group 0 included (ESP-Miner reads 0 as "no group" (as of November 2026) and
would drop Braiins' prev hash; with one channel per connection nothing
else can come for it). A prev hash activates the future job of its own
sender first, in case a pool counts job ids per channel and per group.
The extranonce prefix stays per channel.

## Device test

On the .42 (Bitaxe Gamma), 2026-11-20, with a relay on the Mac that
passes the first job of a session and then only pings (`stuckpool.py`,
upstream WarpPool V1), fallback atlaspool:

| Step | Expected | Measured |
|---|---|---|
| Reconnect | after 600 s, `pool_stale` | 600 s |
| To the fallback | after 300 s more, `pool_stale_fallback` | 300 s |
| First probe | 10 min after the switch | 10 min 0 s |
| Marked main left again | after 300 s | 300 s |
| Second probe | 20 min after that switch | 20 min 0 s |
| `pool_back` | none at the probe; with the first fresh job | none at the probe; 1 s after the relay passed jobs again |

Then Sv2 at Blitzpool for an hour: nine blocks, each picked up at once
(two of them 20 s apart), 751 shares accepted, none rejected, no false
alarm. While the relay held the jobs back, WarpPool rejected the
miner's shares for the old job as stale after about four minutes: the
harm the watch limits.

Not covered on a device: a stuck Sv2 pool (Sv2 is encrypted, the relay
cannot drop its messages); host tests cover it.
