# Overlay Continuation Handoff

Date: 2026-07-02
Repo: `/home/ben/dev/projects/tiny-dfr`
Related config repo: `/home/ben/archdots`

## Executive summary

Ben's `tiny-dfr` fork is now set up for agent work and has the first real code support for named Touch Bar control groups / overlays. The code has been implemented, reviewed by SubPi reviewers, validated, committed, and pushed.

The live machine is still running `tiny-dfr-ben.service`, but the currently running process was started before the new overlay-capable binary was installed. The installed binary at `/usr/local/bin/tiny-dfr-ben` matches the latest release build, and the installer has now been fixed so a future run restarts an already-active `tiny-dfr-ben.service` instead of treating `systemctl start` as a no-op.

No overlay config is live yet. The visible Touch Bar currently shows workspace buttons `1 2 3 4`, a gap, then the flat brightness/backlight/volume/media controls. Battery and time are gone.

## Current repo state

### tiny-dfr fork

Working tree target:

```text
/home/ben/dev/projects/tiny-dfr
```

Important commits, newest first:

- `afae0a6 Fix tiny-dfr-ben installer restart path`
  - Fixes `scripts/install-ben-service.sh` so running it while `tiny-dfr-ben.service` is already active performs `systemctl restart tiny-dfr-ben.service`.
  - This matters because the previous script installed a new binary but did not restart the already-running service.
- `ebf49e6 Add named control group overlays`
  - Adds `ControlGroups`, `OpenOverlay`, and `CloseOverlay` support.
- `7e049c4 docs: add workspace layout prep PRD`
  - Local markdown PRD for workspace IDs and spacer.
- `2606b73 docs: set up agent skills context`
  - Adds `CONTEXT.md` and `docs/agents/` setup.

Expected clean state after pulling latest:

```sh
cd /home/ben/dev/projects/tiny-dfr
git status --short --branch
# ## master...origin/master
```

### archdots

Working tree target:

```text
/home/ben/archdots
```

Relevant committed config changes:

- `70dd4a5 config: remove tiny-dfr battery and time`
- `8eca850 config: add tiny-dfr workspace ids and spacer`

At handoff time, `archdots` also had unrelated local state. Preserve it unless Ben explicitly asks:

- local commit ahead of origin: `626a49a small stuff`
- uncommitted: `.config/niri/config.kdl`

Do not stage unrelated archdots changes when editing the tiny-dfr config.

## Live system state at handoff

The live service is Ben's fork:

```text
tiny-dfr-ben.service: enabled, active/running
stock tiny-dfr.service: disabled, inactive/dead
```

Observed live process before this handoff:

```text
PID 321063 /usr/local/bin/tiny-dfr-ben
started Thu 2026-07-02 04:30:46 ACST
```

Important caveat:

- The overlay-capable release binary was built and installed after that process started.
- The installed binary hash matched `target/release/tiny-dfr`.
- But because the old installer used `systemctl start tiny-dfr-ben.service` while it was already active, systemd did not restart the process.
- Commit `afae0a6` fixes this for the next installer run.

To actually run the overlay-capable binary live, ask Ben before sudo, then run interactively:

```sh
cd /home/ben/dev/projects/tiny-dfr
sudo ./scripts/install-ben-service.sh
systemctl --no-pager --lines=30 status tiny-dfr-ben.service
```

Expected result after the fixed installer:

- `tiny-dfr-ben.service` has a fresh start time after the command.
- stock `tiny-dfr.service` remains disabled/inactive.
- no USB config switching occurs.

## Safety constraints

Keep these constraints intact:

- Do not use sudo, restart services, or touch live Touch Bar state without explicit Ben approval in the current session.
- Keep stock `/usr/bin/tiny-dfr` recoverable.
- Keep the experimental binary/service names: `/usr/local/bin/tiny-dfr-ben`, `tiny-dfr-ben.service`.
- Do not force-switch USB device `05ac:8302`; this can wedge the Touch Bar display until reboot.
- Do not relax `ProtectHome=true`.
- Do not add arbitrary shell command execution inside tiny-dfr.
- Keep Niri logic out of the system daemon; future Niri integration should use a user-session watcher plus a narrow state-only control socket.

Rollback if the fork misbehaves:

```sh
sudo systemctl disable --now tiny-dfr-ben.service
sudo systemctl reset-failed tiny-dfr-ben.service tiny-dfr.service
sudo systemctl enable --now tiny-dfr.service
systemctl --no-pager --lines=50 status tiny-dfr.service
```

## What was implemented in `ebf49e6`

### Config API

New top-level TOML shape:

```toml
[ControlGroups]
volume = [
    { Icon = "volume_off",  Action = "Mute"       },
    { Icon = "volume_down", Action = "VolumeDown" },
    { Icon = "volume_up",   Action = "VolumeUp"   },
    { Text = "×", CloseOverlay = true },
]
```

Launcher button shape:

```toml
{ Icon = "volume_up", OpenOverlay = "volume" }
```

Important TOML placement rule:

- Put `[ControlGroups]` at the end of the config after `MediaLayerKeys`.
- TOML table scope does not automatically close, so if `[ControlGroups]` appears before `MediaLayerKeys`, a later `MediaLayerKeys = [...]` can be interpreted under the `ControlGroups` table.

### Code files changed

- `src/config.rs`
  - Added top-level `control_groups: Option<HashMap<String, Vec<ButtonConfig>>>` to `ConfigProxy`.
  - Added `open_overlay: Option<String>` and `close_overlay: Option<bool>` to `ButtonConfig`.
  - User config `ControlGroups` replaces base `ControlGroups`, matching existing shallow top-level merge behavior.
- `src/main.rs`
  - Added internal `ButtonAction` enum:
    - `Keys(Vec<Key>)`
    - `OpenOverlay(String)`
    - `CloseOverlay`
    - `None`
  - Added `ButtonSet` and overlay-aware `FunctionLayer`.
  - Visible draw/hit target switches to an overlay when open.
  - `OpenOverlay` / `CloseOverlay` do not emit uinput key events.
  - Overlay actions trigger on release only if the touch is still pressed/hit.
  - Active touches track the original button set to avoid retargeting if overlay visibility changes mid-touch.
- `share/tiny-dfr/config.toml`
  - Added commented `ControlGroups` / `OpenOverlay` / `CloseOverlay` example after `MediaLayerKeys`.

## Review and validation history

SubPi was used heavily.

### Design fanout

```text
20260701190433-1cebb5  scout: next implementation slice
20260701190433-b18cc5  scout: config/API shape options
20260701190433-5057d7  reviewer: safety review
```

Outcome:

- Safe next move was code-only overlay/control-group support.
- Ben chose the durable config shape: named `ControlGroups` plus `OpenOverlay`.

### Worker implementation

A worker implemented the code-only slice. Parent then ran full validation.

Validation passed:

```sh
source "$HOME/.cargo/env"
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build
```

Current tests after implementation: `17 passed`.

### Review fanout and fixes

Initial reviewers found a real multi-touch stuck-key risk:

- overlay visibility could change while another touch was active;
- if touch tracking only stored `(layer, button_index)`, release/motion could resolve against the wrong visible set;
- clearing active touches without sending key-up could strand virtual keys down.

Fixes made:

- Introduced `ButtonSetKey` (`Base` or `Overlay(name)`) in touch tracking.
- Motion/release uses the original `ButtonSetKey` and refuses to retarget hidden sets.
- Overlay transitions drain remaining active touches and call `set_active(..., false)` so key-up events are emitted.

Final reviewer pass:

```text
20260701192538-4192da reviewer: no blockers
```

Final status: no blocker/fix-now findings.

## Next recommended work

### 1. Confirm overlay-capable binary is actually running

Because of the installer restart bug, the next agent should first ask Ben before sudo and run the fixed installer:

```sh
cd /home/ben/dev/projects/tiny-dfr
sudo ./scripts/install-ben-service.sh
```

Then verify service start time is fresh:

```sh
systemctl --no-pager --lines=30 status tiny-dfr-ben.service
```

Do not proceed to overlay config until this is confirmed.

### 2. Create a visible volume overlay experiment in archdots

The smallest visible test should be a volume control group.

Edit only:

```text
/home/ben/archdots/.config/tiny-dfr/config.toml
```

Suggested first experiment:

- Keep workspace buttons and spacer as-is.
- Replace the three flat volume buttons:

```toml
{ Icon = "volume_off",      Action = "Mute"           },
{ Icon = "volume_down",     Action = "VolumeDown"     },
{ Icon = "volume_up",       Action = "VolumeUp"       },
```

with one launcher:

```toml
{ Icon = "volume_up", OpenOverlay = "volume" },
```

- Add at the end of the file:

```toml
[ControlGroups]
volume = [
    { Icon = "volume_off",  Action = "Mute"       },
    { Icon = "volume_down", Action = "VolumeDown" },
    { Icon = "volume_up",   Action = "VolumeUp"   },
    { Text = "×", CloseOverlay = true },
]
```

Validation before live apply:

```sh
cd /home/ben/archdots
python - <<'PY'
from pathlib import Path
import tomllib

path = Path('.config/tiny-dfr/config.toml')
with path.open('rb') as f:
    data = tomllib.load(f)

assert 'ControlGroups' in data, data.keys()
assert 'volume' in data['ControlGroups'], data['ControlGroups'].keys()
assert any(item.get('OpenOverlay') == 'volume' for item in data['MediaLayerKeys'])
assert any(item.get('CloseOverlay') is True for item in data['ControlGroups']['volume'])
assert all('Battery' not in item and 'Time' not in item for item in data['MediaLayerKeys'])
print('OK: volume overlay config parses')
PY
git diff --check
```

Commit only the tiny-dfr config file. Preserve unrelated archdots changes.

Live apply only after Ben approves:

```sh
sudo /home/ben/archdots/scripts/install-tiny-dfr-system-links.sh
systemctl --no-pager --lines=30 status tiny-dfr-ben.service
```

Expected manual test:

1. Touch Bar still shows workspace buttons, gap, brightness/backlight/media controls.
2. The volume area is now a single volume launcher.
3. Tapping the launcher opens the volume overlay.
4. Overlay buttons mute/down/up work.
5. Tapping `×` closes overlay and returns to base layout.
6. Drag-cancel should not trigger overlay actions.

### 3. If volume overlay works, repeat for brightness/backlight/media

Do one group at a time. Each group should have a small config commit and live confirmation.

Potential groups:

- `brightness`: low/down + high/up + close
- `backlight`: illum down + illum up + close
- `media`: previous + play/pause + next + close

## Known rough edges / future decisions

- Overlay layout currently replaces the whole visible button set. It does not yet preserve workspace buttons while expanding only the right side.
- Missing group names fail closed: an `OpenOverlay` to a non-existent group does nothing.
- There is no timeout auto-close yet.
- There is no tap-outside-to-close behavior yet.
- There is no Niri `Workspace highlight` yet.
- There is no control socket yet.
- The runtime config merge for `ControlGroups` is shallow and top-level, like existing layer config.
- Live Touch Bar testing is still required for overlays.

## Useful commands

Tiny-dfr fork validation:

```sh
cd /home/ben/dev/projects/tiny-dfr
source "$HOME/.cargo/env"
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build
cargo build --release
```

Check live services:

```sh
systemctl --no-pager --lines=30 status tiny-dfr-ben.service
systemctl --no-pager --lines=20 status tiny-dfr.service
systemctl is-enabled tiny-dfr-ben.service tiny-dfr.service
```

Check Touch Bar DRM card:

```sh
for card in /sys/class/drm/card[0-9]*; do
  printf '%s ' "$card"
  [ -e "$card/device/driver" ] && basename "$(readlink -f "$card/device/driver")" || echo no-driver
done
```

Expected: one `cardN appletbdrm` for the Touch Bar and one `cardN i915` for normal display/Niri.

## Prompt for the next agent

```text
Continue from /home/ben/dev/projects/tiny-dfr/handoff/2026-07-02-overlay-continuation-handoff.md.

First, read CONTEXT.md, CLAUDE.md, docs/agents/*.md, and the handoff.

Goal: get the overlay-capable tiny-dfr-ben binary actually running, then implement the first visible volume ControlGroup overlay in /home/ben/archdots config if Ben approves live actions.

Hard constraints:
- Preserve unrelated archdots changes.
- No sudo/service restart/live Touch Bar action without explicit Ben approval in your current session.
- Do not force-switch USB 05ac:8302.
- Keep stock tiny-dfr recoverable.
- Do not implement Niri/socket work yet.

Use SubPi for read-only review before live application.
```
