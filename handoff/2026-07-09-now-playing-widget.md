# Handoff: now-playing widget shipped + device-loss recovery

Date: 2026-07-09. Supersedes `2026-07-02-live-strip-shipped.md` as the
read-first handoff (that doc's architecture/recovery sections still apply
except where amended below).

## What's on `ben/now-playing-widget` (was live on hardware until the 2026-07-06 wedge)

A **Now Playing widget** in the bar's middle region (the space the parked
critters used to own), between the workspace strip and the control groups:

- Renders current track title + artist (and album art when available) in a
  rounded container matching the control-button frame. Sized to content,
  capped at 440px, hidden when the free region is narrower than 180px or
  while an overlay is open (`src/now_playing.rs`).
- Render-only state from the helper: `media` field on the `state` message
  (title/artist/art_path, sanitized daemon-side in `src/helper_proto.rs`).
  Present only while a player reports `Playing`.
- **Album art pipeline** (helper-side, archdots
  `.config/tiny-dfr/helper/tiny_dfr_helper.py`): playerctl artUrl →
  local file or http(s) fetch (2MB cap) → ffmpeg normalize to 96x96 PNG →
  content-addressed `art-<sha256/16>.png` in `/run/tiny-dfr-ben/media/`.
  The daemon creates that dir pre-privdrop and chowns it to HelperUid
  (`src/helper_link.rs`); the daemon's loader re-validates the direct-child
  path, rejects symlinks/non-regular files without blocking, and bounds both
  compressed size and decoded dimensions (`safe_art_path`). NOTE:
  `/run/tiny-dfr-ben` is
  a systemd RuntimeDirectory — it vanishes whenever the daemon is down, so
  "no art files" while the service is crash-looping is normal, not a bug.
- **Tap-to-focus**: tapping the widget sends a `focus-now-playing` intent;
  the helper matches the playing player to a niri window (app-id token
  match, title/artist scoring) and focuses it. Protocol v1 unchanged
  otherwise: `docs/helper-protocol.md`.

Commits: 834b9d9 (render) → cd8d02a, ce114ef, 1628dc3, 886f698, 8268908
(art sharing, alignment, frame, sizing, padding) → 90c4e82 (tap-to-focus)
→ 3a9a53f (hide under overlays) → 4d851c7 (device-loss fix, below).
Daemon tests: 112 (`cargo test`; pre-commit hook runs the full gates).

## The 2026-07-06 incident (why the bar is/was dead)

Symptom as experienced: "clicking the now-playing widget crashes the bar."
**The widget was innocent.** Post-mortem from journalctl:

- 10:59:11 — kernel starts spamming `appletbdrm ... Failed to send
  message (-110)`: the known USB display-channel wedge. Critters were OFF,
  so **baseline traffic alone can wedge the channel** — this is not a
  critter-only failure mode anymore.
- 10:59:45 — the next DRM write panics with ENODEV (device gone). A tap on
  the widget triggers a redraw, so a tap is often the first write after
  the channel dies — that's why it *looks* like the click crashed it.
- The daemon then double-panicked on the dead card (crash-bitmap
  `drm.map().unwrap()` in `main()`, then `DrmBackend::drop`'s destroy
  unwraps during unwinding) → SIGABRT + core dump.
- 10:59:47 — the bar re-enumerated in **USB config 1 (HID-only)**: input
  present, no DRM display interface. Config force-switching is forbidden
  on this machine, so recovery needs a **full power-off** (see the
  2026-07-09 note below — a warm reboot is NOT enough)
  (`scripts/unwedge-touchbar.sh` detects and reports the config-1 case).
- Aftermath: the service's patient restart (every 2s, never gives up)
  found no touchbar card and panicked with a full backtrace each try —
  15k+ journal entries over the day.

## The fix (commit 4d851c7, deployed 2026-07-09)

Device loss is now a clean exit instead of an abort:

- `src/display.rs` — `Drop for DrmBackend` ignores destroy errors (a
  panic there during unwinding aborts the whole process).
- `src/main.rs` — if the card is unwritable after `real_main` dies, log
  one line and `exit(1)` for the service restart; a missing card at
  startup also exits with the probe error but no panic backtrace.

Deployed 2026-07-09 after the config-1 recovery below: `rtouchbar`
installed the fixed binary, daemon came up clean (`helper connected
(uid 1000)`, restart counter reset). Before that the installed binary
still aborted on device loss.

## Amendments to the recovery ladder (2026-07-02 handoff §Recovery)

Ladder unchanged, three clarifications:

1. The wedge is NOT critter-specific; it recurred with `EnableCritters`
   off. Any sustained damage traffic (or plain bad luck) can trigger it.
2. If the device re-attaches in config 1 (check
   `cat /sys/bus/usb/devices/*/bConfigurationValue` for the 05ac:8302
   device, or just run `unwedge-touchbar` and read its verdict), skip
   straight to a power-off — deauth/reauth cannot restore the display
   config.
3. **A warm reboot does NOT clear the config-1 wedge — only a full
   power-off does** (learned 2026-07-09; see note below). The Touch Bar
   lives on the T2 coprocessor, which keeps its hung USB state across a
   `systemctl reboot`/restart. Once wedged, the T2 must actually lose
   power.

## 2026-07-09: the config-1 wedge that survived a reboot

The bar stayed dead from the 2026-07-06 wedge across the whole next day.
What we learned recovering it:

- The device sat **unconfigured** on the bus (`bConfigurationValue`
  empty), no `appletbdrm` card, no interfaces bound. The T2's USB stack
  was hung at the protocol level: `unwedge-touchbar --force` deauth/reauth
  produced `usb 5-6: can't set config #1, error -110` — the firmware
  refused *any* set-config. (We did NOT force-switch the config; that's
  forbidden and would just re-error / harden the wedge.)
- A `systemctl reboot` did **not** fix it — same unconfigured state on the
  next boot. The T2 retained the hang.
- **A full power-off (shut down, ~30s off, cold boot) DID fix it**: the bar
  came back with `card0 -> appletbdrm`, `card1 -> i915`, USB config 2. If a
  plain power-off ever fails, the next rung is an SMC reset (shut down,
  hold right-Shift + left-Control + left-Option 7s, add the power button
  for another 7s, release, then boot).
- After the card returned, `card0` briefly showed EBUSY — that was only the
  old binary's 2s crash-loop fighting itself for DRM master (niri holds
  card1/i915, never the Touch Bar). `rtouchbar` stops the service before
  installing, so the clean install cleared it.

## 2026-07-09: wedge root cause found + a flush throttle

We read the appletbdrm driver source to answer "can we stop the wedge at
all?" Full write-up: **`docs/appletbdrm-wedge.md`**. Short version:

- The wedge is **not** a userspace rate or overlap bug. Every DRM flush is a
  blocking ~1 s request/response USB handshake, already serialized by the
  kernel. The T2 occasionally misses the deadline (`-110`), and the driver
  **never resynchronizes**, so one late response desyncs the stream
  permanently. It's a driver/firmware fault; the real fix is kernel-side.
- The ioctl returns 0 even on `-110`, so there is **no backpressure** we can
  detect. We can only lower the odds: fewer round-trips, tighter damage.
- Shipped mitigation (not a cure): a global flush throttle in `src/main.rs`
  (`MIN_FLUSH_INTERVAL`, ~30 Hz) that coalesces all redraws and collapses a
  slider drag's 100+/s per-event flush storm into ~30/s with the same tight
  damage. Applies to every redraw source, not just the parked critters.
- **Actual cure since built (2026-07-09): a kernel-side driver fix**, in its
  own repo at `~/dev/projects/appletbdrm-fix` (patched appletbdrm + DKMS). It
  resynchronizes the USB endpoints after a failed flush so a `-110` recovers
  instead of wedging permanently. Validated on hardware and DKMS-installed
  (survives kernel updates). Details: `docs/appletbdrm-wedge.md`. With this in
  place a normal display-channel timeout should recover while the device
  remains display-capable. A config-1 or unconfigured re-enumeration still
  requires the power-off/SMC-reset ladder above. If the bar ever wedges again,
  first check the fix is loaded (`cat
  /sys/module/appletbdrm/parameters/recover` → 1; a stock module means a kernel
  update dropped the DKMS build — rebuild per that repo's README).

Diagnosis one-liners:

```sh
systemctl status tiny-dfr-ben.service        # activating auto-restart = no card
journalctl -k --since -10min | grep appletbdrm   # wedge in progress?
journalctl -u tiny-dfr-ben.service -n 20     # what the daemon last said
```

## Safety rules (unchanged)

No sudo/service restarts without Ben's explicit approval per action; never
force-switch USB 05ac:8302 config; ProtectHome=true stays; no shell exec
in the daemon; helper state is render-input only; intents originate only
from physical touch.
