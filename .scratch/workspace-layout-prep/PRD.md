# PRD: Workspace Button IDs and Spacer Layout Prep

Status: ready-for-agent
Owner: Ben
Repo of record: `/home/ben/dev/projects/tiny-dfr`
Related config repo: `/home/ben/archdots`

## Summary

Prepare the live Touch Bar layout for later Niri workspace highlighting and expandable control groups by making the current workspace buttons addressable and adding visible breathing room between the workspace buttons and the right-side controls.

This is intentionally a low-risk config-first slice. It should not add new Touch Bar behavior, control socket code, Niri watcher code, or overlay/group logic.

## Background

The Ben fork now supports optional stable button IDs in `ButtonConfig` / runtime `Button`. The live daemon is `tiny-dfr-ben.service`, and the canonical Touch Bar config is still owned by `archdots`:

```text
/home/ben/archdots/.config/tiny-dfr/config.toml
```

That config is copied to:

```text
/etc/tiny-dfr/config.toml
```

by:

```sh
/home/ben/archdots/scripts/install-tiny-dfr-system-links.sh
```

The current visible layout has:

- workspace buttons `1`, `2`, `3`, `4` on the left;
- brightness/backlight/volume/media controls after them;
- battery and time already removed.

The next code features will need stable IDs such as `workspace-1` and `workspace-2` so a future control socket / Niri watcher can set a `Workspace highlight` without relying on button position or text.

## Goals

1. Add stable `Id` fields to the four workspace buttons in the archdots tiny-dfr config.
2. Add a spacer after workspace `4` to visually separate workspace buttons from controls.
3. Preserve the current button actions and live fork service setup.
4. Keep this change safe to apply while `tiny-dfr-ben.service` is running.

## Non-goals

- Do not implement Niri workspace highlighting.
- Do not implement the control socket.
- Do not implement expandable control groups or overlays.
- Do not change the Rust button model unless a validation failure proves it is necessary.
- Do not edit `/etc/tiny-dfr/config.toml` directly except through the archdots installer.
- Do not force-switch USB device `05ac:8302`.
- Do not relax `ProtectHome=true`.
- Do not switch back to stock `tiny-dfr.service`.

## User-facing behavior

After the change is applied live:

- The workspace buttons should still focus Niri workspaces `1` through `4`.
- There should be a blank gap after workspace `4` before the brightness/backlight/volume/media controls.
- Battery and time should remain absent.
- The Touch Bar should continue to be driven by `tiny-dfr-ben.service`.

## Proposed config shape

Update the `MediaLayerKeys` workspace entries in:

```text
/home/ben/archdots/.config/tiny-dfr/config.toml
```

from the current form:

```toml
{ Text = "1", Action = [ "LeftAlt", "Num1" ] },
{ Text = "2", Action = [ "LeftAlt", "Num2" ] },
{ Text = "3", Action = [ "LeftAlt", "Num3" ] },
{ Text = "4", Action = [ "LeftAlt", "Num4" ] },
```

to this shape:

```toml
{ Id = "workspace-1", Text = "1", Action = [ "LeftAlt", "Num1" ] },
{ Id = "workspace-2", Text = "2", Action = [ "LeftAlt", "Num2" ] },
{ Id = "workspace-3", Text = "3", Action = [ "LeftAlt", "Num3" ] },
{ Id = "workspace-4", Text = "4", Action = [ "LeftAlt", "Num4" ] },

{ Stretch = 2 },
```

Notes:

- `Id` must be PascalCase because tiny-dfr config uses `#[serde(rename_all = "PascalCase")]`.
- A button with no `Text`, `Icon`, `Time`, or `Battery` is a `Spacer` in the Ben fork.
- Start with `Stretch = 2`. If Ben says the gap feels too small/large after trying it, adjust in a follow-up.

## Implementation steps

1. In `/home/ben/archdots`, check status first:

   ```sh
   git status --short --branch
   ```

   Preserve unrelated user changes. At the time this PRD was written, Ben had unrelated local changes in `.config/ghostty/config`, `.config/niri/config.kdl`, and `.zshrc`; do not stage them for this task.

2. Edit only:

   ```text
   /home/ben/archdots/.config/tiny-dfr/config.toml
   ```

   Add the four workspace `Id` fields and the spacer entry.

3. Validate the config parses and the intended widgets are absent:

   ```sh
   cd /home/ben/archdots
   python - <<'PY'
   from pathlib import Path
   import tomllib

   path = Path('.config/tiny-dfr/config.toml')
   with path.open('rb') as f:
       data = tomllib.load(f)

   keys = data['MediaLayerKeys']
   ids = [item.get('Id') for item in keys if item.get('Id', '').startswith('workspace-')]
   assert ids == ['workspace-1', 'workspace-2', 'workspace-3', 'workspace-4'], ids
   assert any(not any(k in item for k in ('Text', 'Icon', 'Time', 'Battery')) and item.get('Stretch') == 2 for item in keys)
   assert all('Battery' not in item and 'Time' not in item for item in keys)
   print(f'OK: media buttons={len(keys)} workspace_ids={ids}')
   PY
   git diff --check
   ```

4. Commit only the tiny-dfr config file:

   ```sh
   git add .config/tiny-dfr/config.toml
   git commit -m "config: add tiny-dfr workspace ids and spacer"
   ```

5. Do not apply live unless Ben explicitly approves in that agent session. If approved, apply with:

   ```sh
   sudo /home/ben/archdots/scripts/install-tiny-dfr-system-links.sh
   systemctl --no-pager --lines=20 status tiny-dfr-ben.service
   ```

   The installer should restart `tiny-dfr-ben.service` and leave stock `tiny-dfr.service` disabled.

## Acceptance criteria

- [ ] `/home/ben/archdots/.config/tiny-dfr/config.toml` has `Id = "workspace-1"` through `Id = "workspace-4"` on the four workspace buttons.
- [ ] A blank spacer entry with `Stretch = 2` sits after workspace `4` and before the control buttons.
- [ ] Battery and time remain absent from `MediaLayerKeys`.
- [ ] The workspace button actions remain unchanged:
  - workspace `1`: `LeftAlt` + `Num1`
  - workspace `2`: `LeftAlt` + `Num2`
  - workspace `3`: `LeftAlt` + `Num3`
  - workspace `4`: `LeftAlt` + `Num4`
- [ ] The config parses with Python `tomllib`.
- [ ] `git diff --check` passes in `archdots`.
- [ ] If applied live, `tiny-dfr-ben.service` is active/running after restart and stock `tiny-dfr.service` remains disabled/inactive.
- [ ] Ben visually confirms the spacer feels acceptable, or a follow-up issue records the desired adjustment.

## Rollback

Config-only rollback:

```sh
cd /home/ben/archdots
git revert <commit-that-added-ids-and-spacer>
sudo /home/ben/archdots/scripts/install-tiny-dfr-system-links.sh
```

Service rollback, only if the fork misbehaves:

```sh
sudo systemctl disable --now tiny-dfr-ben.service
sudo systemctl reset-failed tiny-dfr-ben.service tiny-dfr.service
sudo systemctl enable --now tiny-dfr.service
```

## Follow-up after this PRD

Once this slice is live and confirmed, the next PRD should cover the first true `Control group` / `Overlay` implementation in the Ben fork, likely starting with the volume group because it has simple discrete controls: mute, volume down, volume up.
