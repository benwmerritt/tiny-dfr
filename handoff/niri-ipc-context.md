# Niri IPC and user-daemon context

Subagent run: `/home/ben/.pi/agent/subpi-runs/20260701101353-03b316/output.log`

## Files inspected

- `/home/ben/plans/md/2026-07-01-tiny-dfr-niri-touchbar-fork-plan.md`
- `/home/ben/md-vaults/agent-vault/research/tiny-dfr-niri-touchbar/deep-research/2026-07-01-dynamic-touch-bar-architecture.md`
- `/home/ben/archdots/.config/niri/README.md`
- `/home/ben/archdots/.config/niri/WORKSPACES.md`
- `/home/ben/archdots/.config/niri/config.kdl`
- `/home/ben/archdots/.config/waybar/config.jsonc`
- tiny-dfr config/source files

No files were changed by the context-builder.

## Niri event-stream usage

Recommended watcher input:

```sh
niri msg --json event-stream
```

Observed live shape:

```json
{"WorkspacesChanged":{"workspaces":[{"id":13,"idx":5,"name":null,"output":"eDP-1","is_active":true,"is_focused":true,"active_window_id":42}]}}
```

Other observed events included `WindowsChanged`, `KeyboardLayoutsChanged`, `OverviewOpenedOrClosed`, `ConfigLoaded`, and `CastsChanged`.

For Touch Bar workspace highlighting, keep only:

- latest `workspaces[]`;
- active/focused workspace per output;
- focused workspace where `is_focused == true`.

Use `is_focused` as the preferred highlight signal. Ignore unknown event variants and fields.

## Ben's current workspace mapping

Important finding: `WORKSPACES.md` describes named persistent workspaces, but the current live/config state appears index-based. Current `config.kdl` binds:

```kdl
Alt+1 { focus-workspace 1; }
Alt+2 { focus-workspace 2; }
Alt+3 { focus-workspace 3; }
Alt+4 { focus-workspace 4; }
```

Live Niri IPC returned `name: null`, so initial Touch Bar mapping should be index-based:

| Button | tiny-dfr action | Niri target | active match |
| --- | --- | --- | --- |
| `1` | `["LeftAlt", "1"]` | `focus-workspace 1` | `idx == 1` |
| `2` | `["LeftAlt", "2"]` | `focus-workspace 2` | `idx == 2` |
| `3` | `["LeftAlt", "3"]` | `focus-workspace 3` | `idx == 3` |
| `4` or `N` | `["LeftAlt", "4"]` | `focus-workspace 4` | `idx == 4` |

Do not assume named workspace mapping until config and live IPC agree.

## User watcher shape

```text
Niri user session
  └─ tiny-dfr-niri-watcher --user
       reads:  $NIRI_SOCKET via niri msg --json event-stream
       writes: /run/tiny-dfr-ben/control.sock

System service
  └─ tiny-dfr-ben
       owns DRM/libinput/uinput
       exposes narrow state-only AF_UNIX socket
```

## Integration boundaries

### tiny-dfr-ben owns

- Touch Bar DRM framebuffer
- libinput Touch Bar events
- uinput key emission
- rendering/layout/overlay state
- active button visual state
- `/run/tiny-dfr-ben/control.sock`

### watcher owns

- Niri IPC/event-stream parsing
- workspace-to-button mapping
- reconnect/backoff on Niri or tiny-dfr socket failure
- no hardware access
- no config rewriting
- no service restarts

## Socket messages

Prefer a group-exclusive update for workspace highlight:

```json
{"set_active_exclusive":{"group":"workspaces","id":"workspace-5"}}
```

Also acceptable for generic state:

```json
{"set_active_button":{"id":"workspace-1","active":true}}
```

Reject shell commands, file reads, arbitrary action dispatch, and Niri-specific semantics inside tiny-dfr.

## Fixture strategy

Capture live fixtures later with:

```sh
timeout 10s niri msg --json event-stream > tests/fixtures/niri-event-stream.ndjson
niri msg --json workspaces > tests/fixtures/niri-workspaces.json
```

Include cases for:

- initial `WorkspacesChanged`;
- focus change between indexes;
- workspace create/remove;
- multi-output when available;
- unknown event variant;
- named and unnamed workspaces.

## Watcher tests

Implement watcher core as a pure reducer:

```text
event JSON line -> WorkspaceState -> Vec<TinyDfrCommand>
```

Test without live Niri:

- parse observed live events;
- map focused `idx` to `workspace-{idx}`;
- ignore unknown events;
- emit no command when active workspace is unchanged.

## Open questions

- `WORKSPACES.md` and live Niri config disagree; decide whether to fix docs/config before coding named workspace assumptions.
- Multi-output behavior needs testing with external displays active.
- Socket permissions need a final systemd/group decision.
- Persistent highlight must not reuse current `Button.active` because it emits key down/up events.
