# Discord notifications

A miner nobody looks at should still say when something worth a look
happens. WarpAxe posts a short message to a Discord channel through a
webhook the owner enters on the Settings page (asked for in issue #1).
NerdOS has the same thing (`discord.cpp`, NerdQAxe+ v1.1); WarpAxe
follows it where it fits and takes its settings over on the first start.

## Setting it up

1. In Discord: *Server settings → Integrations → Webhooks → New webhook*,
   pick the channel, *Copy webhook URL*.
2. In WarpAxe: *Settings → Notifications*, paste the URL, *Save*.
3. *Send test message*. The line under the switches says whether Discord
   took it.

Only Discord's own webhook URLs are taken:
`https://discord.com/api/webhooks/<id>/<token>`, also on `discordapp.com`
and the `canary.`/`ptb.` hosts, with an optional API version
(`/api/v10/webhooks/…`) and an optional `?thread_id=<id>`. Anything else
(plain `http`, another host, a user or port in the URL, another path) is
refused when saved, with the reason (`warpaxe_alerts::webhook`).

## What is sent, and when

One line in English, starting with the miner's hostname:

| Switch (default on) | Message |
|---|---|
| Block found (`alertBlockFound`) | `🎉 bitaxe602: BLOCK FOUND at height 978480, difficulty 128.1T (network 102.3T)` (a coin other than BTC is named after the height) |
| New best difficulty (`alertBestDiff`) | `⛏️ bitaxe602: new best difficulty 14.8M (network 102.3T)` |
| Pool switch (`alertPoolSwitch`) | `↪️ bitaxe602: main pool failed 3 times, mining on the fallback pool`, `↪️ bitaxe602: main pool sent no new job for 300 s, mining on the fallback pool`, `↩️ bitaxe602: back on the main pool`, `↩️ bitaxe602: fallback pool sent no new job for 600 s either, back to the main pool` |
| Solo pool stops paying your address (`alertPayout`, docs/23) | `⚠️ bitaxe602: the main pool's coinbase stopped paying this miner's address, mining on the fallback pool` (or `… back to the main pool`, or `… still mining on it (no other pool to switch to)`); for the fee limit `⚠️ bitaxe602: the main pool keeps 3.5 % of the coinbase (limit 2 %), mining on the fallback pool` |
| Restart after a crash (`alertCrash`) | `⚠️ bitaxe602: restarted after a crash (int-wdt)` (the reset reason: `panic`, `int-wdt`, `task-wdt`, `wdt`) |
| (the test button) | `✅ bitaxe602: test message from WarpAxe` |

- **Block found**: once per block, as the event log counts it (a find
  that repeats a block on record sends nothing).
- **Best difficulty**: the all-time best (`bestDiff`), at most one
  message per ten minutes, saying the newest value. After a reset of
  the best, every share is a new best for a while; those are folded.
- **Pool switch**: the same events the event log records (`pool_fallback`,
  `pool_stale_fallback`, `pool_back`, `fallback_stale`). At most one a
  minute: switches in between are folded into the next message, which
  says how many it stands for. A reconnect to the same pool and the
  "no pool answers" stop are not a switch and send nothing.
- **Crash**: when the boot finds that the last run ended in a panic or a
  watchdog (`diag::last_run_crashed`, so a flashing tool's watchdog
  reset does not count), sent once the network is up.

## How it is sent

The places where these things happen (the mining loops, the stratum
thread through `events::record`, the boot, the test button's handler)
only put a small value into RAM behind a mutex. One thread, `alerts`,
with a 10 KB stack in PSRAM (no internal RAM, docs/20), does the rest:

- it waits until Wi-Fi or Ethernet has been up for five seconds;
- one HTTPS POST at a time (`{"content": …, "allowed_mentions":
  {"parse": []}}`: no one is pinged, whatever the hostname says), 10 s
  timeout, the server checked against the firmware's pinned CA bundle
  (Discord's chain ends at GTS Root R4, which the bundle has; a server
  that sends the cross-signed copy of that root is handled by
  `tls_trust`);
- a send that got no answer or a 5xx is tried once more after 5 s; a 429
  once more after Discord's `retry_after` if that is at most 60 s, else
  the message is dropped and nothing goes out until the wait is over;
  anything else (a deleted webhook answers 404) is dropped with a log
  line. Never more than one retry;
- at least a second between two messages; at most eight blocks, crashes
  and tests wait while the network is down, more are dropped;
- it never writes flash: the last result is RAM only.

## API

- `PATCH /api/system`: `discordWebhook` (a URL, `""` removes it, `"***"`
  keeps it), `alertBlockFound`, `alertBestDiff`, `alertPoolSwitch`,
  `alertPayout`, `alertCrash` (booleans; `0`/`1` taken too).
- `GET /api/v1/state` → `config`: the same fields, `discordWebhook` as
  `"***"` when one is set.
- `GET /api/system/info`: `discordWebhookSet` (bool) and
  `discordLastResult`: `null` before the first send, else
  `{"ok": true, "code": 204, "error": null, "what": "test", "t": <unix
  or 0>, "uptime": <s>}`; `what` is `test`, `block`, `bestDiff`, `pool`,
  `payout` or `crash`.
- `POST /api/v1/alerts/test`: `202 {"queued": true}`, or `409
  {"queued": false, "error": …}` without a webhook or within five
  seconds of the last test.

## Privacy

- The miner sends to the URL you entered and nowhere else, and only when
  one is set. Without one, nothing about this feature leaves the miner.
- A message carries the hostname, the event and its figures (height,
  difficulties, seconds, the reset reason). No IP or MAC address, no
  worker name or address, no pool host. (NerdOS adds the IP and MAC
  address to each message; WarpAxe does not.)
- The URL holds the webhook's token: anyone who has it can post to the
  channel. So it is never shown back (the API says `***` and
  `discordWebhookSet`), never logged, left out of the diagnostics file,
  scrubbed from every section of it and masked in a core dump download,
  like the passwords. The config patch record keeps key names only.
- To stop: switch the events off, or *Remove webhook*.

## Moving from NerdOS

On the first start WarpAxe takes NerdOS's `alrt_disc_url` when it is a
webhook URL it accepts, with `alrt_disc_bf_en` (block found),
`alrt_disc_bd_en` (best difficulty) and `alrt_disc_en` (NerdOS's restart
after an hour without a share; here the restart after a crash). NerdOS
has no pool-switch message; that switch stays on. Its coinbase-check
alert (`alrt_disc_cb_en`) sets `alertPayout` (docs/23).

## Not yet

- Telegram (also asked for in issue #1).
- A message when no pool answers at all (`pools_out`): the network may
  well be what is down then.
