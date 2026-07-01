# Source seams scout report

Subagent run: `/home/ben/.pi/agent/subpi-runs/20260701101353-abe859/output.log`

## Files inspected

- `AGENTS.md`
- `FORK.md`
- `Cargo.toml`
- `share/tiny-dfr/config.toml`
- `src/backlight.rs`
- `src/config.rs`
- `src/display.rs`
- `src/fonts.rs`
- `src/main.rs`
- `src/pixel_shift.rs`
- `etc/systemd/system/tiny-dfr.service`
- `etc/systemd/system/systemd-backlight@backlight:*.service`
- `etc/udev/rules.d/99-touchbar-seat.rules`
- `etc/udev/rules.d/99-touchbar-tiny-dfr.rules`

No source files were changed by the scout.

## Config parsing / reload

File: `src/config.rs`

- `Config`: runtime daemon config for outlines, pixel shift, font, brightness, and double-press layer switch.
- `ConfigProxy`: TOML-facing optional fields with `PascalCase` names.
- `ButtonConfig`: current button TOML fields:
  - `icon`, `text`, `theme`, `time`, `battery`, `locale`
  - `action: Vec<Key>`
  - `stretch`, `icon_width`, `icon_height`
- `array_or_single`: allows `Action = "F1"` or key arrays.
- `load_config(width)`: merges `/usr/share/tiny-dfr/config.toml` with `/etc/tiny-dfr/config.toml`, inserts `esc`, and builds exactly two `FunctionLayer`s.
- `ConfigManager`: inotify-backed reload manager.

Key seam: `ButtonConfig.action: Vec<Key>` blocks non-key actions like overlay toggles. Introduce an internal action enum instead of overloading raw key vectors.

## Button model

File: `src/main.rs`

- `ButtonImage`: `Text`, `Svg`, `Bitmap`, `Time`, `Battery`, `Spacer`.
- `Button`: image, changed flag, active flag, action key vector, icon dimensions.
- `Button::with_config`: converts `ButtonConfig` into runtime `Button`.
- Constructors: spacer, text, icon, battery, time.
- `Button::set_active`: currently both changes visual active state and emits uinput key events.

Key seam: split button state into press/uinput state and persistent visual highlight state. Do not reuse current `Button.active` for Niri workspace highlight.

## Rendering

File: `src/main.rs`

- `FunctionLayer::with_config`: computes stretch positions and virtual button count.
- `FunctionLayer::draw`: central layout/render pass, uses `button.active` to choose active color.
- `Button::render`: draws text/svg/png/time/battery contents.

Overlay groups should be implemented through a derived visible view model or shared layout function so drawing and hit testing cannot drift apart.

## Hit testing

File: `src/main.rs`

- `FunctionLayer::hit`: duplicates virtual-button layout math from `draw`.

Risk: overlays/groups will desync render and touch behavior if draw/hit layout stays duplicated. Prefer extracting shared layout calculation.

## Event handling

File: `src/main.rs`

- `real_main`: main event loop.
- `emit` / `toggle_keys`: uinput emission helpers.
- Touch handling: down, motion, up blocks in the main loop.
- Fn handling: key event branch switches the two layers and handles double-press swapping.

Key seam: add an action dispatcher between touch events and key emission.

## Layer model

Current model is exactly two layers: `[FunctionLayer; 2]` plus local `active_layer`.

Overlay groups probably should not be a third physical layer initially. Prefer a `UiState` / `OverlayState` that derives the visible buttons from base layer plus overlay state.

## Config reload

- Inotify watch path: `/etc/tiny-dfr/config.toml`.
- Watch flags: `IN_MOVED_TO | IN_CLOSE | IN_ONESHOT`.
- Reload reconstructs config/layers and resets `active_layer = 0`.

External active states and overlay state need explicit preservation or reset semantics on reload.

## Unix control socket seam

No socket code exists now.

Recommended new module:

- `src/control.rs`
- nonblocking `UnixListener`
- register fd in existing epoll loop
- socket path under `/run/tiny-dfr-ben/control.sock`

Avoid `/tmp` because `PrivateTmp=true`; avoid `$HOME` because `ProtectHome=true`.

## Validation risks

- Most core types are private in `src/main.rs`, making tests hard without refactor.
- `Button.active` has side effects.
- `FunctionLayer::draw` and `hit` duplicate layout math.
- Config parsing has many panic/unwrap paths.
- Layer model is fixed-size `[FunctionLayer; 2]`.
- Cargo/Rust toolchain is currently unavailable in the parent shell, so code implementation should wait or install toolchain before writing substantial Rust changes.

## Open questions

1. Should overlay groups be declarative TOML or initially hardcoded for Ben's preferred layout?
2. Should group expansion trigger on touch-down, touch-up, or toggle until another group/back is tapped?
3. Should workspace buttons continue to emit key combos, or move to socket/direct actions later?
4. Should socket messages be JSON lines, compact text, or another format?
5. Should config reload preserve overlay and external active states when button IDs still exist?
6. Should spacers be explicitly non-hit-testable?
