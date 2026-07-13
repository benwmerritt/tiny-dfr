# Ben's tiny-dfr fork

This fork experiments with a richer Touch Bar UX for Ben's Arch/T2 MacBook setup.

## Target UX

- Workspace buttons on the left.
- A spacer between workspaces and controls.
- Collapsed control groups on the right for brightness, keyboard/Touch Bar backlight, volume, and media.
- Tap a group to reveal expanded controls.
- Active Niri workspace highlighting through a safe state interface.
- A Now Playing widget (track, artist, album art) in the middle region; tapping it focuses the player window.
- No date, battery, time, or screenshot buttons in the preferred layout.

## Current state

The live strip, anchored overlay groups, sliders, and the Now Playing
widget are all built and hardware-verified. Read the newest handoff in
`handoff/` first (currently `2026-07-09-now-playing-widget.md`) — it holds
the shipped-state summary, the appletbdrm USB-wedge post-mortem, and the
recovery ladder. Protocol contract: `docs/helper-protocol.md`. Vocabulary:
`CONTEXT.md`.

## Architecture direction

- Keep tiny-dfr as the sole owner of Touch Bar DRM/input/uinput devices.
- Add overlay/group state inside tiny-dfr rather than rewriting config at runtime.
- Add generic per-button active state.
- Add a narrow Unix control socket for state messages only.
- Keep Niri-specific logic in a user-session companion watcher that reads Niri IPC and sends tiny state updates to tiny-dfr.

## Safety model

The fork should remain easy to roll back from. Any live install should use a distinct binary/service name, such as `tiny-dfr-ben`, and leave the stock `tiny-dfr.service` recoverable.

Do not live-install or swap services without explicit approval.

## Approved plan

See:

```text
/home/ben/plans/md/2026-07-01-tiny-dfr-niri-touchbar-fork-plan.md
```
