# Final implementation handoff plan

This handoff synthesizes the approved plan and the read-only subagent recon fanout.

## Current state

Completed:

- Replaced upstream no-agent content with Ben fork instructions.
- Added `FORK.md` describing the fork goal and safety model.
- Added tracked native Rust pre-commit hook at `.githooks/pre-commit`.
- Configured local `core.hooksPath = .githooks`.
- Added `upstream` remote pointing at `git@github.com:AsahiLinux/tiny-dfr.git`.
- Ran read-only subagent recon fanout and captured summaries in:
  - `handoff/source-seams.md`
  - `handoff/niri-ipc-context.md`
  - `handoff/risk-review.md`

Important blocker:

- The parent shell currently has no Rust toolchain on `PATH`; `cargo`, `rustc`, `rustfmt`, and `clippy-driver` are unavailable. The hook correctly fails when `cargo` is missing.
- Do not start substantial Rust implementation until a Rust toolchain is available or Ben explicitly accepts unvalidated edits.

## Recommended implementation order

### Phase 1 — Pure model/test seam preparation

Goal: make overlay/action state testable before touching hardware behavior.

1. Extract a small pure layout helper from duplicated `FunctionLayer::draw` / `FunctionLayer::hit` math.
   - New module candidate: `src/layout.rs`.
   - Output should describe virtual button spans and pixel bounds.
   - Tests should cover stretch, spacer, first/last button bounds, and expected hit behavior.
2. Introduce internal button identity and visual state without changing config behavior yet.
   - Add optional `Id` to `ButtonConfig`.
   - Runtime button should store `id: Option<String>`.
   - Split current `active` semantics into:
     - `pressed` for touch-down/uinput behavior;
     - `highlighted` or `externally_active` for persistent visual state.
   - Existing behavior should remain visually identical when no external state is set.
3. Add tests around the pure parts before wiring socket or Niri.

Validation target:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

### Phase 2 — Action dispatcher and overlay state

Goal: support real expandable groups without Niri or socket integration.

1. Replace raw `Vec<Key>` action usage internally with a `ButtonAction` enum:
   - `Keys(Vec<Key>)`
   - `OpenOverlay(String)`
   - `CloseOverlay`
   - `None`
2. Preserve TOML compatibility for existing configs:
   - existing `Action = "F1"` and arrays keep working as key actions;
   - new overlay fields can be additive.
3. Add `UiState` / `OverlayState`:
   - `open_overlay: Option<String>`;
   - external active button/group map;
   - helper to derive the current visible button view.
4. Decide the first overlay config format only after inspecting how much TOML parser churn is needed.

Recommended first UX semantics:

- Tap group button on touch-up to open overlay.
- Tap child button sends its key action and keeps overlay open for repeated taps.
- Add a back/close button or tap outside to close.
- Add timeout later if needed, not in first slice.

### Phase 3 — Control socket

Goal: allow external state updates without config rewrites.

1. Add `src/control.rs`.
2. Use nonblocking Unix listener and register its fd in existing epoll loop.
3. Socket path: `/run/tiny-dfr-ben/control.sock` for the fork.
4. Start with one message:

```json
{"set_active_exclusive":{"group":"workspaces","id":"workspace-5"}}
```

5. Parser rules:
   - size limit;
   - known messages only;
   - fail closed on malformed/unknown input;
   - no shell commands;
   - no file paths;
   - no hardware/service/device operations.

### Phase 4 — Niri watcher

Goal: user-session process maps Niri workspace focus to tiny-dfr state.

1. Keep watcher separate from tiny-dfr hardware process.
2. Read:

```sh
niri msg --json event-stream
```

3. Initial mapping should be index-based, because live config/IPC currently returns unnamed workspaces:
   - `idx == 1` -> `workspace-1`
   - `idx == 2` -> `workspace-2`
   - etc.
4. Implement watcher core as a pure reducer:

```text
event JSON line -> WorkspaceState -> Vec<TinyDfrCommand>
```

5. Use captured fixtures under `tests/fixtures/` for tests.

### Phase 5 — Packaging/live test

Only after phases 1–4 pass local validation:

1. Build separate binary `tiny-dfr-ben`.
2. Create separate service `tiny-dfr-ben.service`.
3. Do not install upstream udev rule unchanged.
4. Do not force-switch `05ac:8302`.
5. Ask Ben for explicit approval before any live service action.

## Code seams to start with

- `src/config.rs`
  - `ButtonConfig`
  - `load_config(width)`
  - `ConfigManager`
- `src/main.rs`
  - `ButtonImage`
  - `Button`
  - `Button::with_config`
  - `Button::set_active`
  - `FunctionLayer::with_config`
  - `FunctionLayer::draw`
  - `FunctionLayer::hit`
  - `real_main`
  - touch down/motion/up handling
  - Fn key handling

## Immediate next command sequence once Rust is available

```sh
cd /home/ben/dev/projects/tiny-dfr
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build
```

If this fails because the upstream baseline has warnings/tests failing, record baseline failures before changing code.

## Stop conditions

Stop and ask Ben before proceeding if:

- a design requires relaxing `ProtectHome=true`;
- a design needs arbitrary shell command execution in tiny-dfr;
- a design needs runtime config rewrites for workspace state;
- a test/install path requires force-switching `05ac:8302`;
- code cannot be validated because Rust toolchain remains unavailable;
- any live service/install/sudo action would be needed.

## Suggested next decision

Install or expose a Rust toolchain for this repo before beginning the first Rust patch. Without `cargo`, the fork can be documented and planned, but implementation cannot be responsibly validated.
