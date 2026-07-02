# tiny-dfr-ben helper protocol v1

The single source of truth for the socket contract between the tiny-dfr-ben
daemon and the user-session helper (`tiny_dfr_helper.py` in archdots).

Revised 2026-07-02 pre-deployment (still v1, nothing had shipped): workspace
state became output-grouped with opaque niri ids, and the `focus-workspace`
intent was added so taps can jump across monitors.

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
  error (a legitimate `state` is comfortably under 1 KiB).
- **Must-ignore rule**: unknown *fields* in a known message are silently
  ignored (the forward-compatibility seam — e.g. a future
  `"claude": {"on": true}` presence field rides `state` with no version
  bump). Unknown *types* are dropped and count against the error budget.

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
{"t":"state",
 "outs":[
   {"name":"eDP-1","ws":[{"id":7,"idx":1,"occ":true,"foc":false},
                          {"id":9,"idx":2,"occ":true,"foc":true}]},
   {"name":"DP-3","ws":[{"id":3,"idx":1,"occ":true,"foc":false}]}
 ],
 "vol":{"level":0.60,"muted":false}}
```

- `outs` (array, required): one group per output, in a **stable,
  focus-independent order** — built-in `eDP-1` first, then externals sorted
  by name — so strip buttons never reorder when focus moves. `name` is
  informational.
- `ws` entries, sorted by `idx` ascending:
  - `id` — the niri workspace id: an opaque u64 and the tap currency (see
    `focus-workspace` below). Globally unique across outputs, which is what
    makes two outputs both having an "idx 1" unambiguous.
  - `idx` — per-output 1-based index (the button label), int in `1..=32`.
  - `occ` — has windows; `foc` — the globally focused workspace (at most one
    true across all groups; daemon uses the first if several arrive).
  - Included workspaces: occupied **or per-output active** — every output
    contributes at least one entry, so no divider ever borders an empty group.
- Bounds: helper sends ≤ 9 entries per output; daemon accepts ≤ 16 per group
  and ≤ 4 groups, renders at most `Workspaces.MaxButtons` (≤ 9) buttons
  total (truncating trailing groups). `outs: []` → daemon renders the
  fallback `[1]` strip.
- `vol` (object or absent): `level` finite float clamped by the receiver to
  `[0.0, 1.0]`; `muted` bool. Absent → daemon keeps last-known volume.
- **Full snapshots only, no deltas**: any single message fully repairs the
  daemon's view after drops or reconnects.
- Sent: (a) debounced (~40 ms) on any derived-state change, (b) every **2 s**
  as heartbeat, (c) immediately after `hello`.

## Daemon → helper (intents out)

`set-volume`:

```json
{"t":"set-volume","level":0.42}
```

- `level` finite float; helper clamps to `[0.0, 1.0]` and applies
  `wpctl set-volume @DEFAULT_AUDIO_SINK@ <level> -l 1.0`.
- Because the bar has no mute button, the helper also runs
  `wpctl set-mute @DEFAULT_AUDIO_SINK@ 0` on every set-volume intent — the
  slider is the single volume authority and dragging it un-mutes.
- No acks: the resulting volume-change event → fresh `state` push is the ack.
- The daemon rate-limits to one send per **50 ms, latest wins**, during drags
  (the slider emission throttle), always flushing the final drag position.

`focus-workspace`:

```json
{"t":"focus-workspace","id":9}
```

- `id` is a niri workspace id previously delivered in `state`. The helper
  **validates it against its live model** — unknown ids are logged and
  dropped (covers daemon-renders-stale-strip races and forged input alike).
- Execution: if the target workspace's output is not the focused output,
  `niri msg action focus-monitor-next` first (deterministic for two
  monitors; more than two outputs is best-effort with a single hop), then
  `niri msg action focus-workspace <idx>` using the workspace's **current**
  idx from the live model — never the idx the daemon rendered.
- Focus intents queue FIFO (consecutive duplicates collapsed) beside the
  volume latest-wins slot, so a tap can't be coalesced away by a concurrent
  drag. No acks: the resulting niri event → fresh `state` is the ack.

## Robustness rules

- The daemon never crashes on socket input: parse errors, out-of-range or
  non-finite numbers, and oversize lines are dropped and counted. After **8
  consecutive** invalid lines, any line > 4096 bytes, or > **200 msg/s**
  sustained over 1 s, the daemon closes the connection. The counter resets on
  any valid message.
- **Staleness**: state is stale when no client is connected or no valid
  `state` arrived for **6 s** (three missed heartbeats). Stale → the strip
  falls back to the static `[1]` button (plain `Alt+Num1` uinput keys, no
  helper needed) and the volume slider goes inert. Freshness returning
  restores the live strip.
- **Echo-fight rule**: while a volume-slider touch is active, the daemon
  ignores incoming `vol` for rendering (the finger wins) and resumes tracking
  pushed state on touch-up.
- Helper reconnect: exponential backoff 0.5 → 8 s cap, ±20% jitter, forever.
  On every connect: `hello` then an immediate full `state`. The niri socket
  path embeds the compositor PID and changes on every niri restart — the
  helper re-globs `/run/user/<uid>/niri.*.sock` and reconnects.

## Explicit non-goals

No brightness messages (daemon-local sysfs), no key or command execution, no
config, no queries/RPC. The intent set is exactly
`{set-volume, focus-workspace}` — **typed, helper-validated, and originating
only from physical touch**; state is render-input only and never triggers
key emission, sysfs writes, or execution. Anything not specified here is
invalid.

Threat note: a process that can write this socket already runs as uid 1000
and could call `wpctl`/`niri msg action` directly — the protocol grants
nothing beyond that baseline, and forged state can only change what the bar
*renders*; the intents a tap then sends are still validated by the helper
against live niri before anything executes.
