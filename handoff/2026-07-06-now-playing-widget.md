# Handoff: now-playing widget shipped + device-loss crash fix

Date: 2026-07-06. Supersedes `2026-07-02-live-strip-shipped.md` as the
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
  (`src/helper_link.rs`); the daemon's loader re-validates path, root,
  symlink escape, and size (`safe_art_path`). NOTE: `/run/tiny-dfr-ben` is
  a systemd RuntimeDirectory — it vanishes whenever the daemon is down, so
  "no art files" while the service is crash-looping is normal, not a bug.
- **Tap-to-focus**: tapping the widget sends a `focus-now-playing` intent;
  the helper matches the playing player to a niri window (app-id token
  match, title/artist scoring) and focuses it. Protocol v1 unchanged
  otherwise: `docs/helper-protocol.md`.

Commits: 834b9d9 (render) → cd8d02a, ce114ef, 1628dc3, 886f698, 8268908
(art sharing, alignment, frame, sizing, padding) → 90c4e82 (tap-to-focus)
→ 3a9a53f (hide under overlays) → 4d851c7 (device-loss fix, below).
Daemon tests: 105 (`cargo test`; pre-commit hook runs the full gates).

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
  on this machine, so **reboot is the only recovery from config 1**
  (`scripts/unwedge-touchbar.sh` detects and reports this case).
- Aftermath: the service's patient restart (every 2s, never gives up)
  found no touchbar card and panicked with a full backtrace each try —
  15k+ journal entries over the day.

## The fix (commit 4d851c7, built + tested, NOT yet deployed)

Device loss is now a clean exit instead of an abort:

- `src/display.rs` — `Drop for DrmBackend` ignores destroy errors (a
  panic there during unwinding aborts the whole process).
- `src/main.rs` — if the card is unwritable after `real_main` dies, log
  one line and `exit(1)` for the service restart; a missing card at
  startup also exits with the probe error but no panic backtrace.

Deploy: reboot first (to leave config 1), then `rtouchbar` (sudo,
Ben-run). The installed binary until then still has the abort behavior.

## Amendments to the recovery ladder (2026-07-02 handoff §Recovery)

Ladder unchanged, two clarifications:

1. The wedge is NOT critter-specific; it recurred with `EnableCritters`
   off. Any sustained damage traffic (or plain bad luck) can trigger it.
2. If the device re-attaches in config 1 (check
   `cat /sys/bus/usb/devices/*/bConfigurationValue` for the 05ac:8302
   device, or just run `unwedge-touchbar` and read its verdict), skip
   straight to reboot — deauth/reauth cannot restore the display config.

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
