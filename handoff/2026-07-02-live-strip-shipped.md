# Handoff: full custom bar shipped (live strip, sliders, anchored groups)

Date: 2026-07-02 (evening). Supersedes `2026-07-02-overlay-continuation-handoff.md`.

## What is live on hardware (all Ben-verified)

The complete original project goal plus the field revisions:

- **Live dual-monitor workspace strip** (left): one group per output with a
  thin divider, occupied-or-active workspaces, per-output idx labels, the
  globally focused workspace highlighted. Keyboard switches move the
  highlight without redraw flicker (decoration tier). Tapping ANY square —
  either monitor — jumps to that monitor + workspace via a socket intent.
- **Anchored overlay groups** (right): base is exactly
  `[brightness][sound]`; groups expand folder-style AT the tapped launcher,
  nest (`sound → vol → slider`), close on tap-outside or 8s idle
  (never under a held finger). No close buttons.
- **Real sliders** in the standard button container: screen brightness
  (sysfs, 1% floor), keyboard backlight (sysfs, 0 allowed — this FIXED the
  previously-dead kbd backlight), volume (true PipeWire value via the
  helper; drags send set-volume intents and un-mute).
- **Fallback**: helper dead/stale (6s) → single static `[1]` (plain
  Alt+Num1 keys); volume slider inert. Self-heals on reconnect.

## Architecture

- Daemon (`tiny-dfr-ben.service`, root → drops to nobody): binds
  `/run/tiny-dfr-ben/helper.sock` pre-privdrop (RuntimeDirectory, chown
  HelperUid=1000, 0600, SO_PEERCRED gate). All privileged handles
  (backlights, socket) opened pre-drop. NEVER executes anything; pushed
  state is render-input only; intents originate only from physical touch.
- Helper (`tiny-dfr-helper.service`, user session, Python stdlib asyncio,
  runs from the archdots checkout — ~/.config/tiny-dfr is NOT stowed):
  watches `niri msg --json event-stream` + `pactl subscribe`, pushes
  output-grouped state (2s heartbeat), executes typed intents
  (set-volume via wpctl + un-mute; focus-workspace via focus-monitor-next
  + focus-workspace with live idx, id-validated). Niri socket path changes
  per compositor restart; helper re-globs.
- Contract: `docs/helper-protocol.md` (v1) — the single source of truth.

## Where things are

- tiny-dfr fork: master (all pushed? push status unchecked — origin is
  Ben's fork). Milestone tags `ben-live/*` pair with archdots config
  commits. Installed = `ben-live/phase-c-live-strip`.
- Reviews: `.scratch/touchbar-buildout/reviews/{r1-touch-safety,
  r2-root-writes,r3-ipc}.md` — all SAFE TO INSTALL, all findings fixed.
- Design docs: `.scratch/touchbar-buildout/design-*.md` (note: parts
  superseded by the anchored-overlay/dual-monitor revision — see the
  approved plan of 2026-07-02 and docs/helper-protocol.md for current
  truth).
- Deploy: `rtouchbar` zsh alias = sudo install-ben-service.sh (snapshots
  last-good binary+config; `scripts/rollback-ben-last-good.sh` reverts).
  Helper: `bash ~/archdots/scripts/install-tiny-dfr-helper.sh` (no sudo).
  Config validation: `scripts/check-config.sh`.
- Tests: 88 daemon (`cargo test`, pre-commit hook runs full gates) + 10
  helper (`python3 -m unittest discover -s ~/archdots/.config/tiny-dfr/helper`).

## Known items / next

- **Phase D: pet Claude critters — PARKED (2026-07-03).** Built and complete,
  but disabled by default behind the `EnableCritters` config flag: the
  animation's `dirty()` traffic wedges the appletbdrm USB display, and the
  10fps/merged-damage fix was not enough. All code stays in the tree
  (`src/critters.rs`, the `claude` protocol types, gated wiring in
  `src/main.rs`, the sprite, the helper /proc scan). Full context, why it's
  parked, how to re-enable, and the display-throughput problem to solve first:
  `.scratch/claude-critter/parked-state.md` (design: `idea.md`).
- Fn layer is dead on this machine (keyd grabs the keyboard and cannot
  emit KEY_FN) — logged in `.scratch/fn-layer-keyd/issue.md`, Ben chose to
  ignore.
- >2 monitors: focus-monitor-next single-hop is best-effort (documented).
- Old design-phasing M6 polish items (square sizing taste, docs sweep)
  fold into whatever session comes next.

## Recovery / escape hatch (Ben-runnable, no agent needed)

The critter animation can wedge the appletbdrm USB display channel
(kernel spams `appletbdrm ... Failed to send message (-110)`, daemon stuck
in D-state, bar frozen). Recovery ladder, mildest first:

1. `unwedge-touchbar` — resets the 05ac:8302 USB device (software
   replug; NOT the forbidden config force-switch). Detects the wedge
   itself; no-ops when healthy. Script: `scripts/unwedge-touchbar.sh`.
2. `rtouchbar` — normal install of the current build; now auto-unwedges
   first if the daemon is D-state.
3. `rtouchbar-safe` — installs `dist/tiny-dfr-ben-safe`, a prebuilt
   binary from tag `ben-live/phase-c-live-strip`: the last
   hardware-validated bar WITHOUT critters (live strip, anchored
   overlays, sliders). Script: `scripts/install-ben-safe.sh` (has
   rebuild instructions in its header).
4. Stock upstream: `sudo systemctl disable --now tiny-dfr-ben.service &&
   sudo systemctl enable --now tiny-dfr.service`.
5. Reboot (only if the USB reset fails to re-enumerate).

## Safety rules (unchanged)

No sudo/service restarts without Ben's explicit approval per action; stock
tiny-dfr untouched and recoverable; never force-switch USB 05ac:8302;
ProtectHome=true stays; no shell exec in the daemon; archdots has unrelated
local state — stage only tiny-dfr files, never `git add -A`.
