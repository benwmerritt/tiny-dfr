# Pet Claude critters — PARKED (2026-07-03)

The feature is **complete and fully present in the tree, but disabled by
default**. It was "unplugged" rather than removed so it can be picked back up
without redoing the work. Nothing here has been deleted.

## Why it's parked

The animation wedges the **appletbdrm** USB Touch Bar display on this
hardware. When critters spawn, leave, or simply animate, the daemon issues
DRM `dirty()` ioctls frequently enough that the T2 virtual-USB display
channel jams: the kernel spams

    appletbdrm 5-6:2.1: [drm] *ERROR* Failed to send message (-110)

the daemon lands in uninterruptible **D-state**, and the bar freezes until the
display is reset (`unwedge-touchbar`) or the machine reboots.

The "gentler damage" fix (commit `a080bb4`: 10fps, exactly one merged union
damage rect per frame, degenerate-rect guard, helper aborts on >64KB write
backlog) **was not enough** — that build still wedged on 2026-07-03 when a
critter left on Ctrl-C and again when a button was pressed afterward
(115 `-110` errors in 2 minutes, daemon D-state). The reported symptom
"critter freezes and won't disappear when I close a Claude session" is this
same wedge: the erase-on-removal damage burst kills the display mid-frame, so
the half-drawn sprite is stuck on a dead screen.

So this is **not** a critter-logic bug. It's a display-throughput ceiling that
the current render cadence exceeds.

> **Root cause since found (2026-07-09): `docs/appletbdrm-wedge.md`.** It is not
> a rate/queue ceiling at all — every flush is a blocking ~1 s USB handshake,
> and the driver never resynchronizes after a `-110` timeout, so one unlucky
> late response desyncs the stream permanently. That is why the 10 fps
> merged-damage build still wedged: cadence only changes the *odds* of hitting
> the timeout, not whether a hit is survivable. Userspace can lower the odds
> (fewer round-trips, tighter damage) but cannot prevent it; the real fix is
> kernel-side resync in appletbdrm. The general flush throttle now in
> `src/main.rs` (`MIN_FLUSH_INTERVAL`) applies this odds-reduction to all
> redraws, not just critters — but re-enabling critters still needs the
> kernel-side fix (or acceptance of the wedge risk).

## How it's disabled

- Config flag `EnableCritters` (PascalCase in TOML), added in `src/config.rs`,
  **defaults to `false`** (absent key = off).
- `src/main.rs` gates the three critter wiring points on `cfg.enable_critters`:
  the session census/`reconcile`, the tick/`critters_visible`, and the render
  block (plus the erase-dirty trigger). With the flag off, no critter code
  runs on the hot path — behaviour is identical to the pre-critter bar, and
  there is zero animation `dirty()` traffic.
- Every critter method stays referenced inside those runtime branches, so
  `cargo clippy -- -D warnings` stays clean with no `#[allow(dead_code)]`.

## Where the code lives (all intact)

- `src/critters.rs` — the `CritterField` engine (spawn/reconcile, tick/bounce,
  render with single-union damage, `wait_ms`, unit tests). Tests still pass.
- `src/helper_proto.rs` — `ClaudePresence` / `ClaudeSession` types, the
  `claude` field on `StateMsg`, and their `sanitize` (session cap 8, id ≤64).
- `src/main.rs` — the gated wiring described above, and
  `CritterField::default()` construction.
- `share/tiny-dfr/claude-critter.svg` — the lobehub Claude Code sprite
  (fill patched to `#D97757`), embedded via `include_bytes!`.
- archdots `~/.config/tiny-dfr/helper/tiny_dfr_helper.py` —
  `detect_claude_sessions` / `classify_claude_process` (/proc scan every 3s)
  and the `claude` block in the state message. Harmless while the flag is off
  (the daemon ignores the field via the protocol's must-ignore seam).
- `docs/helper-protocol.md` — documents the `claude` presence field.

## How to resume experimenting

1. Set `EnableCritters = true` in `~/archdots/.config/tiny-dfr/config.toml`.
2. Redeploy (`rtouchbar`). Critters return immediately — no rebuild needed if
   only the config changed (the daemon reloads config live), but a rebuild is
   harmless.
3. **Expect the display to wedge** under sustained animation. Recover with
   `unwedge-touchbar` (never force-switch USB `05ac:8302`).

## The real problem to crack first

The `dirty()` cadence, not the sprites. Directions worth trying, roughly in
order of cheapness:

1. **Drop the frame rate hard** — try 1–2 fps and measure how long the display
   survives. If even that wedges, the problem is per-ioctl, not per-second.
2. **Detect backpressure** — watch for `-110`/`ETIMEDOUT` from `drm.dirty()`
   (or a rising socket write queue) and back the animation off / pause it.
3. **Idle-only animation** — only animate while the bar is otherwise idle;
   freeze critters the instant any button/strip/slider activity happens, so
   critter damage never competes with interaction damage.
4. **Page-flip instead of dirty-rects** — test whether full-surface flips are
   more robust than partial-damage updates on the appletbdrm driver.
5. **Global dirty budget** — one hard `dirty()` call per N ms, shared across
   ALL bar redraws (buttons + critters), so total ioctl rate is bounded no
   matter what.

Until one of these keeps the display alive for a long session, the flag stays
off by default.
