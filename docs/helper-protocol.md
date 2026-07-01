# tiny-dfr-ben helper protocol v1

The single source of truth for the socket contract between the tiny-dfr-ben
daemon and the user-session helper (`tiny_dfr_helper.py` in archdots).

## Transport

- Unix stream socket at `/run/tiny-dfr-ben/helper.sock`.
- The **daemon listens** (created pre-privdrop inside `RuntimeDirectory=tiny-dfr-ben`,
  chowned to `HelperUid` and chmod `0600`); the helper connects as a client.
  The reverse is impossible: the post-privdrop daemon (`nobody`) cannot reach
  into `/run/user/1000`, and `ProtectHome=true` stays.
- On accept the daemon checks `SO_PEERCRED` and drops any peer whose uid is
  not `HelperUid` (default 1000) or 0.
- **Single client, newest wins**: a new validated connection replaces the old
  one, so helper restarts never get locked out.

## Framing

- NDJSON: one JSON object per line, UTF-8, `\n`-terminated.
- **Max 4096 bytes per line.** Receiver treats an overlong line as a protocol
  error (a legitimate `state` is < 600 bytes).
- **Must-ignore rule**: unknown *fields* in a known message are silently
  ignored (this is the forward-compatibility seam — e.g. a future
  `"claude": {"on": true}` presence field rides `state` with no version bump).
  Unknown *types* are dropped and count against the error budget.

## Helper → daemon (state in)

`hello` — must be the first line after connect:

```json
{"t":"hello","v":1,"src":"tiny-dfr-helper"}
```

- `v` (int, required): protocol version; the daemon supports `{1}` and closes
  the connection on anything else. The helper treats close-after-hello as
  version skew and backs off at max interval.
- `src` (string, optional, informational, ≤ 64 chars).
- If the first line is not a valid `hello`, the daemon closes immediately.

`state` — full snapshot, idempotent, doubles as heartbeat:

```json
{"t":"state","ws":[{"idx":1,"occ":true,"foc":false},{"idx":3,"occ":false,"foc":true}],"vol":{"level":0.60,"muted":true}}
```

- `ws` (array, required): the workspace strip for the **focused output**,
  sorted by `idx` ascending.
  - Entries: `idx` int in `1..=32`, `occ` bool, `foc` bool.
  - The helper sends at most 9 entries. The daemon accepts up to 16, renders
    at most `Workspaces.MaxButtons` (≤ 9), and uses the first `foc:true` if
    several are set.
  - An empty array means "no usable view" → the daemon renders the fallback
    `[1]` strip.
- `vol` (object or absent): `level` finite float, receiver clamps to
  `[0.0, 1.0]` (wpctl can report > 1.0 on boost); `muted` bool. Absent →
  daemon keeps last-known volume.
- **Full snapshots only, no deltas**: any single message fully repairs the
  daemon's view after drops or reconnects.
- Sent: (a) debounced (~40 ms) on any derived-state change, (b) every **2 s**
  as heartbeat, (c) immediately after `hello`.

## Daemon → helper (intents out)

`set-volume` — the only intent:

```json
{"t":"set-volume","level":0.42}
```

- `level` finite float; the helper clamps to `[0.0, 1.0]` and applies
  `wpctl set-volume @DEFAULT_AUDIO_SINK@ <level> -l 1.0`.
- Because the bar has no mute button, the helper also runs
  `wpctl set-mute @DEFAULT_AUDIO_SINK@ 0` on every set-volume intent — the
  slider is the single volume authority and dragging it un-mutes.
- No acks: the resulting volume-change event → fresh `state` push is the ack.
- The daemon rate-limits to one send per **40 ms, latest wins**, during drags.

## Robustness rules

- The daemon never crashes on socket input: parse errors, out-of-range or
  non-finite numbers, and oversize lines are dropped and counted. After **8
  consecutive** invalid lines, any line > 4096 bytes, or > **200 msg/s**
  sustained over 1 s, the daemon closes the connection. The counter resets on
  any valid message.
- **Staleness**: state is stale when no client is connected or no valid
  `state` arrived for **6 s** (three missed heartbeats). Stale → the strip
  falls back to the static `[1]` button and the volume slider goes inert
  (drags emit nothing). Freshness returning restores the live strip.
- **Echo-fight rule**: while a volume-slider touch is active, the daemon
  ignores incoming `vol` for rendering (the finger wins) and resumes tracking
  pushed state on touch-up.
- Helper reconnect: exponential backoff 0.5 → 1 → 2 → 4 → 8 s cap, ±20%
  jitter, retrying forever. On every connect: `hello` then an immediate full
  `state`. The niri socket path embeds the compositor PID and changes on every
  niri restart — the helper re-globs `/run/user/<uid>/niri.*.sock` and
  reconnects (verified live when niri was OOM-killed mid-session).

## Explicit non-goals

No brightness messages (daemon-local sysfs), no key or command execution, no
config, no queries/RPC, no niri actions. Workspace switching stays uinput
`LeftAlt+NumN`, physical-touch-driven only. **State is render-input only;
intents originate only from touch.** Anything not specified here is invalid.
