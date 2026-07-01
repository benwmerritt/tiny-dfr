# tiny-dfr Daemon Design: Workspace Strip + Overlay Controls + Sliders + Control Socket

Verified against `master` @ `e1ecf83` (clean). Bar geometry confirmed live: appletbdrm mode `60x2008` → after the 90-degree rotation in `FunctionLayer::draw` (main.rs:921-922) the drawing space is **width=2008, height=60**. Button band: `bot = 9`, `top = 51` (main.rs:939-940), corner radius 8. The ESC auto-injection (config.rs:129-150) does not fire (2008 < 2170).

---

## 0. Conflicts between locked decisions and code reality

| # | Locked decision | Code reality | Resolution |
|---|---|---|---|
| C1 | "The daemon (runs as root) writes sysfs DIRECTLY" | `real_main` **drops privileges to `nobody:input,video`** at main.rs:1276-1282, before the event loop. Post-drop, opening sysfs files for write fails. | Follow the existing `BacklightManager` precedent (constructed at main.rs:1269, *before* the drop; it holds a pre-opened `File` and rewrites it forever, backlight.rs:64-66,83-86). Open write handles for `intel_backlight/brightness` and `:white:kbd_backlight/brightness` and **bind the unix listener** before `PrivDrop`. Reads of `brightness`/`max_brightness` are world-readable and work post-drop. Verified in the live unit's mount namespace: `/sys` is mounted `rw` despite `ProtectKernelTunables=true` (only `/sys/fs/bpf`, tracing, debugfs, cgroup are ro), so sysfs writes are viable. |
| C2 | "One narrow unix socket… /run/user/1000/…" (scout suggested helper-side socket) | Daemon runs as `nobody` with `ProtectHome=true`; it cannot connect into `/run/user/1000` (mode 0700, owned by ben). | **Daemon is the listener** at `/run/tiny-dfr-ben/control.sock`; the user-session helper connects as a client. Matches decision 8 ("one socket, state in / intents out") and the handoff doc's "narrow state-only control socket". |
| C3 | Socket in `/run` | `ProtectSystem=strict` mounts everything ro except `/dev /proc /sys`; current unit and `scripts/install-ben-service.sh` heredoc (lines 87-112) have **no `RuntimeDirectory=`**, so `/run/tiny-dfr-ben` neither exists nor is writable. | Add `RuntimeDirectory=tiny-dfr-ben` + `RuntimeDirectoryMode=0755` to the *ben* service heredoc only (stock unit untouched). `RuntimeDirectory` dirs are exempt from `ProtectSystem=strict`. Socket file chmod 0666 after bind (pre-privdrop), plus an `SO_PEERCRED` uid check on accept. Requires an installer rerun + service restart → **Ben approval per CLAUDE.md**. |
| C4 | "Overlays NEST (sound → volume → slider)" as a *stack* | Decision 4 simultaneously says tapping vol "**swaps** overlay content". Current code already swap-replaces (`open_overlay` main.rs:873-884). There is no user-visible "go back one level" (no × buttons; outside-tap closes everything). | Implement a small `Vec<String>` stack anyway (cheap, requested, future-proof): `OpenOverlay` pushes (dedupe if same as top), outside-tap/timeout **clears the whole stack**, the legacy `CloseOverlay` action pops one (config back-compat). Visible set = stack top. Functionally identical to swap for the locked UX. |
| C5 | Live strip stays pinned while overlay is open | `hit_in_set` returns `None` for any non-visible set (main.rs:1045-1046); only **one** set is visible at a time; overlay open/close does a full-bar redraw. | Introduce a per-layer *Regions* mode with two simultaneously visible sets (strip + controls). Keep full-bar redraw on overlay transitions — at 2008x60 it's one atomic buffer copy; the strip is redrawn pixel-identical in the same flip, so it "never disappears". |
| C6 | — (latent) | `hit_index` computes the initial virtual index from absolute `x` ignoring `x_offset` (layout.rs:62-69). | Regions hit-test in **region-local coordinates**: pass `x - region_origin` and `x_offset: 0.0`. `layout.rs` needs no changes; `button_spans`' `x_offset` is used only for drawing (as today, main.rs:936 vs 1056). |
| C7 | Fallback "single static [1] button (unhighlighted)" | — | The fallback button still carries `Keys([LeftAlt, Num1])` so tapping it focuses workspace 1 (harmless, useful on fresh boot). "Static" = not live-updated, not action-less. |
| C8 | Config reload as today | `update_config` rebuilds `[FunctionLayer; 2]` wholesale (config.rs:223-225), which would wipe a live strip. | After a successful reload (main.rs:1343-1346), re-apply the strip from `RuntimeState` before drawing. Also: socket path / sysfs paths are fixed at process start (opened pre-privdrop); log-and-ignore if a reload changes them. |
| C9 | Touch semantics | `TouchEvent::Down` presses keys immediately (main.rs:1450-1453). | Strip rebuilds mid-touch must first release in-flight strip touches (walk `touches`, `set_active(false)` against the *old* generation, remove entries) before swapping buttons. Sliders bypass `set_active` key emission entirely (their `ButtonAction::Slider` has no keys; drag is handled separately). |
| C10 | Timeout plumbing | `epoll.wait(…, next_timeout_ms as u16)` (main.rs:1393-1396) caps at 65535 ms and the loop already re-polls everything non-blockingly each wakeup, ignoring which fd fired. | No timerfd needed. Overlay auto-timeout and slider-throttle flush just clamp `next_timeout_ms` (like `pixel_shift.update()` does, main.rs:1352-1358) and are checked at the top of each loop iteration. |

Verified sysfs facts: `intel_backlight/max_brightness = 17777`; `:white:kbd_backlight/max_brightness = 14660` (device is HID `05AC:8102` under apple-bce — *not* the forbidden `05ac:8302` display device; no USB config touching involved).

---

## 1. State model

### New module `src/state.rs`

```rust
pub struct WorkspaceState {          // wire-agnostic; protocol.rs converts
    pub idx: u8,                     // per-output idx on the focused output (1-based)
    pub label: String,               // usually idx.to_string(); helper may send names
    pub active: bool,                // this is the focused workspace -> highlighted
}

pub struct RuntimeState {
    pub connected: bool,             // helper socket liveness
    pub workspaces: Vec<WorkspaceState>,
    pub volume: Option<f64>,         // 0.0..=1.0, last pushed by helper
}
```

- `RuntimeState::default()` → `connected: false, workspaces: vec![], volume: None`.
- `fn apply(&mut self, u: StateUpdate)` — folds `protocol::StateUpdate` in.
- `fn set_connected(&mut self, c: bool)` — on `false`, keeps `volume` (last known) but the strip rule below ignores stale workspaces.
- **Fallback rule lives here** (single source of truth):

```rust
pub fn strip_buttons(&self) -> Vec<WorkspaceState> {
    if !self.connected || self.workspaces.is_empty() {
        vec![WorkspaceState { idx: 1, label: "1".into(), active: false }]  // decision 7
    } else {
        self.workspaces.clone()   // helper already filtered to focused output, capped at MaxButtons
    }
}
```

### Feeding rendering

`RuntimeState` is owned by `real_main` next to `layers`. A free function in main.rs:

```rust
fn apply_strip_state(
    layers: &mut [FunctionLayer; 2],
    state: &RuntimeState,
    cfg: &Config,
    touches: &mut HashMap<Slot, (usize, ButtonSetKey, usize)>,
    uinput: &mut UInputHandle<File>,
) -> bool /* needs_complete_redraw */
```

For each layer with `kind == LayerKind::Regions`:
1. Release + remove any `touches` entries whose key is `ButtonSetKey::Strip(_)` (C9).
2. `layer.rebuild_strip(state.strip_buttons(), &cfg.workspaces)` → `StripChange::{None, Highlight, Rebuilt}`.
3. `Highlight` → per-button `changed` flags only (partial redraw). `Rebuilt` (count/label change) → generation bump + `needs_complete_redraw = true` (geometry reflow).

Called: after every batch of socket updates, after `update_config` reload, and once at startup (renders the fallback `[1]` immediately — decision 7 satisfied on fresh boot before the helper connects).

Volume feeds rendering via `FunctionLayer::refresh_volume_slider(v: f64)`: finds any visible `ButtonImage::Slider` whose action target is `Volume` and `!dragging`, updates value, marks changed.

---

## 2. Layout model: regions

### Geometry constants (main.rs, near `BUTTON_SPACING_PX`)

Bar: 2008 x 60; drawable band 42 px tall (+8 px radii). "Compact square" strip buttons ≈ band height:

```rust
const STRIP_BUTTON_WIDTH_PX: i32 = 60;   // ~square vs 42px band + radii
const STRIP_SPACING_PX: i32     = 10;    // tighter than the 16px control spacing
const STRIP_LEFT_MARGIN_PX: f64 = 12.0;
const CONTROL_UNIT_PX: i32      = 110;   // one virtual slot in the controls region
const CONTROL_RIGHT_MARGIN_PX: f64 = 12.0;
// controls keep BUTTON_SPACING_PX (16)
```

Worst case strip (9 workspaces): `9*60 + 8*10 = 620 px`. Two launchers: `2*110 + 16 = 236 px`. Middle free space ≥ 1100 px — the critter's future home. A `Stretch = 5` slider overlay: `5*110 + 4*16 = 614 px`, right-anchored.

### Pure helpers in `src/layout.rs` (additive; existing fns untouched)

```rust
pub(crate) struct RegionGeometry { pub origin: f64, pub width: i32 }

pub(crate) fn strip_region(n_buttons: usize) -> RegionGeometry;      // left-anchored
pub(crate) fn controls_region(virtual_count: usize, bar_width: i32) -> RegionGeometry; // right-anchored:
// width = count*CONTROL_UNIT_PX + (count-1)*BUTTON_SPACING_PX
// origin = bar_width - CONTROL_RIGHT_MARGIN_PX - width
```

Per-region rendering reuses `button_spans` with `total_width = region.width`, region spacing, and `x_offset = region.origin (+ pixel_shift_x)`. Per-region hit-testing reuses `hit_index` with `x_rel = x - region.origin`, `x_offset: 0.0` (C6). **`layout.rs` core stays byte-identical.**

### Data model changes in main.rs

```rust
enum LayerKind { Classic, Regions }

enum ButtonSetKey {
    Base,                 // Classic: whole bar. Regions: right controls launchers
    Overlay(String),
    Strip(u64),           // generation-tagged — the mid-touch safety extension
}

pub struct FunctionLayer {
    kind: LayerKind,
    base: ButtonSet,
    overlays: HashMap<String, ButtonSet>,
    overlay_stack: Vec<String>,          // replaces open_overlay: Option<String>
    strip: ButtonSet,                    // Regions only; starts as fallback [1]
    strip_generation: u64,
    overlay_opened_or_touched: Option<Instant>,  // auto-timeout anchor
}
```

- `FunctionLayer::with_config(cfg, control_groups, kind)` — the media layer gets `Regions` iff `config.workspaces.is_some()`; the Fn/primary layer stays `Classic` (unchanged full-width F-keys). `kind` travels with the layer through the double-Fn `layers.swap(0,1)` (main.rs:1422).
- `controls_key()` (renamed `visible_key`, main.rs:802-808): `overlay_stack.last()` → `Overlay(name)` else `Base`.
- `button_set(&ButtonSetKey)` (main.rs:810-815) gains `Strip(g) => (g == self.strip_generation && matches!(self.kind, Regions)).then_some(&self.strip)`.
- `is_set_visible` (main.rs:833-835): `Strip(g)` → generation match; others → `== controls_key()`.
- `rebuild_strip(states: Vec<WorkspaceState>, ws_cfg: &WorkspacesCfg) -> StripChange`:
  - Builds `Button::new_text(label, ButtonAction::Keys(ws_cfg.action_for(idx)))` + `set_highlighted(active)` per entry (highlight reuses `BUTTON_COLOR_ACTIVE` fill via `is_visually_active`, main.rs:959-965 — no new render code for the active-workspace look).
  - Diffs against current strip: identical → `None`; same idx/label sequence, different actives → toggle highlights in place → `Highlight`; else replace buttons, `strip_generation += 1` → `Rebuilt`.

### Drawing (main.rs `FunctionLayer::draw`, 906-1034)

Factor the per-button body (lines 949-1030) into
`fn draw_button_set(c, set: &mut ButtonSet, spans: &[ButtonSpan], cfg, height, complete_redraw, pixel_shift_y, clips: &mut Vec<ClipRect>)`.

- `Classic`: exactly today's single call (spans from full-width `LayoutSpec`, main.rs:931-937).
- `Regions`: two calls —
  1. strip: spans from `strip_region(n)` (spacing `STRIP_SPACING_PX`),
  2. controls: `controls_key()` set, spans from `controls_region(count, width)`.
- Damage model unchanged: per-button `ClipRect`s (main.rs:1023-1029) work because spans now carry absolute `left_edge`s. Overlay open/close and strip `Rebuilt` keep setting `needs_complete_redraw` (C5). `Highlight` and slider drags ride the existing partial-redraw path (`any_button_changed()` must OR in the strip: `self.visible_sets().any(...)`).
- The middle region is simply never painted (black). Future critter: one more region + a `changed` flag + a `next_timeout_ms` clamp exactly like `pixel_shift.update()` — nothing to build now, nothing here precludes it.

### Hit-testing (main.rs 1036-1074)

```rust
enum HitOutcome {
    Button(ButtonSetKey, usize),
    OutsideControls,   // Regions only: no button hit while overlay_stack is non-empty
    Miss,
}
fn hit_target(&self, width: u16, height: u16, x: f64, y: f64) -> HitOutcome
```

Regions order: strip region first (`Strip(current_gen)`), then controls region (`controls_key()`), else `OutsideControls` if stack non-empty, else `Miss`. `hit_in_set` keeps its signature but routes `Strip(g)` through strip geometry and returns `None` for stale generations — this is the existing `hidden_button_set_does_not_retarget_active_touch` guarantee (main.rs:1157-1182) extended to vanishing workspace buttons: a `Motion`/`Up` constrained re-hit on a stale generation silently no-ops, exactly like today's hidden overlay.

---

## 3. Overlay stack, tap-outside, auto-timeout, pinned-strip semantics

### Stack (main.rs:873-904 rework)

```rust
fn open_overlay(&mut self, name: &str) -> bool   // push; no-op if top == name; marks visible set changed+unpressed (existing body)
fn close_top_overlay(&mut self) -> bool          // pop (legacy CloseOverlay action)
fn close_all_overlays(&mut self) -> bool         // clear; marks base controls changed+unpressed
```

`activate_button_action` (main.rs:898-904): `OpenOverlay → open_overlay`, `CloseOverlay → close_top_overlay`, `Slider(_)/Keys/None → false`.

### Touch flow changes (main.rs:1436-1506)

**Down** — match `hit_target`:
- `Button(Strip(g), i)` **while stack non-empty**: `close_all_overlays()` first (drain other touches + release, reuse the drain block at main.rs:1490-1498, `needs_complete_redraw = true`), *then* record the touch and `set_active(true)` as normal → the workspace switch fires (keys pressed on Down already) — decision 3's "closes the overlay AND performs the switch".
- `Button(key, i)` where the button is a slider → record touch, start drag (Section 4); **skip `set_active`**.
- `Button(...)` otherwise → exactly today's path (main.rs:1444-1454).
- `OutsideControls` → `close_all_overlays()`, drain/release all touches, `needs_complete_redraw = true`, do **not** record the touch. (Close-on-Down mirrors the iPhone folder feel.)
- `Miss` → nothing.

**Motion / Up** — unchanged for normal buttons (constrained re-hit in the origin set, main.rs:1457-1502); slider touches branch into drag handling. `Up` on a strip button of a stale generation: `is_set_visible` is false → action suppressed (existing main.rs:1480 guard).

### Auto-timeout

- Config: `OverlayTimeoutMs` (default **8000**; `0` disables).
- `overlay_opened_or_touched` set on `open_overlay` and refreshed on **every** `TouchEvent` handled while the stack is non-empty (Down/Motion/Up — "no touches for ~8 s").
- Loop-top check (next to the time-redraw check, main.rs:1360-1371): if stack non-empty and anchor elapsed ≥ timeout → `close_all_overlays()` + drain/release touches + `needs_complete_redraw = true`.
- Timeout scheduling: clamp `next_timeout_ms = min(next_timeout_ms, remaining_ms)` alongside the pixel-shift clamp (main.rs:1352-1358). Values always ≤ `TIMEOUT_MS` = 10 000 so the `as u16` cast (C10) stays safe.
- Testability: extract `fn overlay_timeout_due(anchor: Option<Instant>, now: Instant, timeout: Duration) -> bool` + `fn overlay_timeout_remaining_ms(...) -> Option<i32>` as pure functions.

---

## 4. Slider widget

### Types

```rust
// config.rs
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Debug)]
pub enum SliderTarget { DisplayBrightness, KeyboardBrightness, Volume }
// ButtonConfig += pub slider: Option<SliderTarget>,

// main.rs
enum ButtonAction { Keys(Vec<Key>), OpenOverlay(String), CloseOverlay, Slider(SliderTarget), None }
// from_config precedence (main.rs:111-121): slider > open_overlay > close_overlay > keys

struct SliderState { value: f64 /* 0..=1 */, dragging: bool }
enum ButtonImage { ..., Slider(SliderState) }
```

### Rendering (`Button::render` match arm, main.rs:478-576; bar h=60)

- No rounded-rect button fill: extend the `!matches!(button.image, ButtonImage::Spacer)` guard (main.rs:976) to also exclude `Slider` — the slider paints itself.
- Track: rounded rect, `y = 27..33` (6 px), inset `SLIDER_TRACK_INSET_PX = 30` from both span edges, gray `0.25`.
- Fill: same rect clipped to `track_left + value * track_len`, white `0.85`.
- Knob: filled circle r = 14 at `(track_left + value * track_len, 30)`, white; 42 px band gives comfortable clearance.
- No text label (icons in the parent overlay already say what it is); value is self-evident from fill.

### Touch semantics (absolute mapping)

Helper on `FunctionLayer`:
`fn button_span_abs(&self, key: &ButtonSetKey, index: usize, bar_width: i32) -> Option<(f64 /*left*/, f64 /*width*/)>` — recomputes region + spans (same math as draw, sans pixel shift, matching hit behavior).

Pure mapping (unit-testable):
```rust
fn slider_value_from_x(span_left: f64, span_width: f64, x: f64) -> f64 // clamp 0..=1 over track extent (inset applied)
```

- **Down** on slider: `dragging = true`; value = mapped x (absolute jump, no grab-offset); `changed = true`; emit (throttled).
- **Motion**: remap x → value (y ignored — drags may wander vertically); `changed = true`; throttled emit. Constrained re-hit is *not* required to stay inside the span; the touch stays bound to the slider until Up (nicer drag feel; the `touches` entry already pins set+index).
- **Up**: final unconditional emit; `dragging = false`; remove touch. No `activate_button_action` effect (`Slider` returns `false`).
- Overlay force-close / strip rebuild drains: clear `dragging` when releasing (extend the drain block to reset slider state instead of `set_active`).

### Throttling

```rust
struct EmitThrottle { last: Option<Instant>, pending: Option<f64> }  // MIN interval 50 ms
```
One instance per in-flight slider drag (keyed by touch slot, or a single `Option` — only one drag matters in practice). `Motion` stores `pending`; an emit fires if `last` is ≥ 50 ms old; loop-top flushes `pending` (clamping `next_timeout_ms` to ≤ 50 when pending). `Up` always flushes. Sysfs writes and socket intents both go through this.

### Three backends — new module `src/sliders.rs`

```rust
pub struct SysfsSlider { write_handle: File, attr_path: PathBuf, pub max: u32, min_fraction: f64 }
impl SysfsSlider {
    pub fn open(dir: &Path, min_fraction: f64) -> Result<Self>;  // reads max_brightness (backlight.rs:22-28 pattern),
                                                                 // opens brightness for write (backlight.rs:83-86 pattern)
    pub fn read_value(&self) -> f64;                             // fs::read_to_string(attr_path) / max — works post-privdrop
    pub fn write_value(&mut self, v: f64);                       // raw = round(clamp(v, min_fraction, 1.0) * max); write_all (backlight.rs:64-66)
}

pub struct SliderBackends {
    pub display: Option<SysfsSlider>,   // /sys/class/backlight/intel_backlight, max 17777, min_fraction 0.01 (never fully black panel)
    pub keyboard: Option<SysfsSlider>,  // /sys/class/leds/:white:kbd_backlight, max 14660, min_fraction 0.0 (0 = off is desired)
}
impl SliderBackends { pub fn new(cfg: &Config) -> Self }  // paths from config; failures logged -> None (slider inert, drags dropped)
```

- Constructed in `real_main` **before** `PrivDrop` (insert after main.rs:1269, alongside `BacklightManager::new()`) — resolves C1.
- Refactor `backlight.rs` `read_attr`/`set_backlight` to `pub(crate)` and reuse; do not disturb `BacklightManager` (it only *reads* intel_backlight for adaptive brightness, main.rs:127-131 in backlight.rs — no interaction with our write handle).
- **Volume** backend = `ControlSocket::send_intent(Intent::SetVolume(v))` (Section 5). Disconnected → intent dropped silently; the slider still moves visually and re-syncs on the next pushed volume state.
- **Value initialization**: when an overlay opens, `FunctionLayer::sync_sliders(backends, runtime_state)` walks the newly visible set: `DisplayBrightness/KeyboardBrightness → backend.read_value()`; `Volume → state.volume.unwrap_or(0.0)`. Also re-synced on `Volume` state updates while visible and `!dragging` (Section 1).

---

## 5. Unix socket integration

### New modules

`src/protocol.rs` — **the seam; wire schema owned by the protocol designer.** Line-delimited JSON via a new `serde_json = "1"` dependency. Public surface the daemon codes against:

```rust
pub enum StateUpdate { Workspaces(Vec<WorkspaceState>), Volume(f64) }
pub enum Intent { SetVolume(f64) }        // later: claude-presence rides the same StateUpdate enum
pub fn parse_state_line(line: &str) -> Result<StateUpdate>;  // unknown/malformed -> Err, caller logs & ignores (forward-compat)
pub fn encode_intent(intent: &Intent) -> String;             // newline-terminated
```

`src/socket.rs`:

```rust
pub struct ControlSocket {
    listener: UnixListener,          // nonblocking
    client: Option<UnixStream>,      // nonblocking; newest-connection-wins
    line_buf: LineBuffer,            // pure, testable byte->lines splitter, 64 KiB cap
}
impl ControlSocket {
    pub fn bind(path: &Path) -> Result<Self>;      // unlink stale, bind, chmod 0666, set_nonblocking(true)
    pub fn register(&self, epoll: &Epoll);         // listener EpollEvent data=4; client data=5 on accept
    pub fn pump(&mut self, epoll: &Epoll) -> (Vec<StateUpdate>, bool /*liveness changed*/);
    pub fn send_intent(&mut self, intent: &Intent); // best-effort; write error -> drop client
    pub fn connected(&self) -> bool;
}
```

- `bind()` runs pre-privdrop (root creates the socket inside `RuntimeDirectory` and chmods it; C1/C3).
- `pump()` each loop iteration: accept pending (drop old client — epoll del/add), read until `WouldBlock`, split lines, parse; EOF/error → disconnect. **Peer check**: `getsockopt(PeerCredentials)` via nix (add `"socket"`+`"net"` features to the nix dep, Cargo.toml:21) — accept only uid 1000 (configurable `AllowedUid`, default = first non-root uid... simplest: hardcode-from-config `SocketAllowedUid = 1000`) or root; others dropped immediately.
- The daemon **never executes anything**: sysfs writes + typed intents only (hard rule preserved).

### Event-loop integration (main.rs:1265-1512)

- Construct gated on `cfg.control_socket` (default **false** → binary behaves exactly as today): `let mut socket = cfg.control_socket.then(|| ControlSocket::bind(&cfg.socket_path)).transpose().unwrap_or_else(...log...)` before `PrivDrop` (after main.rs:1271); `register` after the epoll setup block (main.rs:1299-1311), data slots 4/5.
- The existing loop wakes on any registered fd and then services everything non-blockingly with a 1-slot event buffer (main.rs:1393-1399) — we keep that style: right after `epoll.wait`, run:

```rust
if let Some(sock) = socket.as_mut() {
    let (updates, liveness_changed) = sock.pump(&epoll);
    if liveness_changed { state.set_connected(sock.connected()); }
    for u in updates { state.apply(u); }
    if liveness_changed || !updates_was_empty {
        needs_complete_redraw |= apply_strip_state(&mut layers, &state, &cfg, &mut touches, &mut uinput);
        for layer in layers.iter_mut() { layer.refresh_volume_slider(&state); }
    }
}
```

- **Disconnect → fallback**: `set_connected(false)` makes `strip_buttons()` return the static `[1]`, `apply_strip_state` swaps it in (generation bump cancels in-flight strip touches). Nothing else changes (decision 7): overlays, launchers, sysfs sliders all keep working; volume intents drop.
- Startup: `apply_strip_state` once before the loop → `[1]` renders on first frame.

---

## 6. Config schema additions (`src/config.rs`)

All new keys optional → **fully backwards compatible**; existing `ControlGroups`/`OpenOverlay`/`CloseOverlay` untouched.

```toml
# --- new top-level keys ---
ControlSocket = true                    # default false
ControlSocketPath = "/run/tiny-dfr-ben/control.sock"   # default shown
SocketAllowedUid = 1000                 # default 1000
OverlayTimeoutMs = 8000                 # default 8000; 0 disables
DisplayBacklightPath = "/sys/class/backlight/intel_backlight"   # default shown
KbdBacklightPath = "/sys/class/leds/:white:kbd_backlight"       # default shown

[Workspaces]                            # presence => media layer renders in Regions mode
MaxButtons = 9                          # helper contract cap; daemon truncates defensively
# Actions[i] = keys emitted by workspace idx i+1; default LeftAlt+Num1..Num9
Actions = [ ["LeftAlt","Num1"], ["LeftAlt","Num2"], ["LeftAlt","Num3"], ["LeftAlt","Num4"],
            ["LeftAlt","Num5"], ["LeftAlt","Num6"], ["LeftAlt","Num7"], ["LeftAlt","Num8"], ["LeftAlt","Num9"] ]

# --- target archdots layout (right controls region; decision 1) ---
MediaLayerKeys = [
  { Icon = "brightness_high", OpenOverlay = "brightness" },
  { Icon = "volume_up",       OpenOverlay = "sound" },
]

[ControlGroups]
brightness = [                          # decision 5: [screen][keyboard]
  { Icon = "brightness_high", OpenOverlay = "slider-screen" },
  { Icon = "backlight_high",  OpenOverlay = "slider-kbd" },
]
"slider-screen" = [ { Slider = "DisplayBrightness",  Stretch = 5 } ]
"slider-kbd"    = [ { Slider = "KeyboardBrightness", Stretch = 5 } ]
sound = [                               # decision 4: [vol][play/pause]; no skip, no mute
  { Icon = "volume_up",  OpenOverlay = "slider-volume" },
  { Icon = "play_pause", Action = "PlayPause" },
]
"slider-volume" = [ { Slider = "Volume", Stretch = 5 } ]
```

Code changes:
- `ConfigProxy` (config.rs:28-41) += `workspaces`, `control_socket`, `control_socket_path`, `socket_allowed_uid`, `overlay_timeout_ms`, `display_backlight_path`, `kbd_backlight_path`; merge with the existing `.or()` chains (config.rs:113-124).
- `Config` (config.rs:19-26) gains resolved, defaulted fields (`workspaces: Option<WorkspacesCfg>`, `overlay_timeout: Duration`, `control_socket: bool`, `socket_path: PathBuf`, `socket_allowed_uid: u32`, backlight `PathBuf`s).
- `ButtonConfig` (config.rs:70-88) += `slider: Option<SliderTarget>`; **derive `Default`** and convert the ESC-injection literal (config.rs:132-148) and both test fixtures (main.rs:621-637, 1088-1103) to `..Default::default()` — kills field-add churn permanently.
- `load_config` (config.rs:151-152): `FunctionLayer::with_config(media_layer_keys, groups, if workspaces.is_some() { Regions } else { Classic })`; primary layer always `Classic`.
- All existing icons referenced above already ship in `share/tiny-dfr/` (verified: `brightness_high`, `backlight_high`, `volume_up`, `play_pause`).

---

## 7. Ordered implementation steps (reviewable commits)

Every commit ends green on `cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build`. No hardware needed for any test.

**Commit 1 — Overlay stack + ButtonConfig Default**
`main.rs` (FunctionLayer stack rework, 778-904), `config.rs` (Default derive, ESC literal), test fixtures.
Tests: `nested_open_shows_innermost_overlay` (sound→volume visibility), `close_all_returns_to_base_from_depth_two`, `close_overlay_action_pops_one_level`, `reopening_top_overlay_is_a_noop`; the 4 existing `function_layer_tests` must pass unmodified (depth-1 equivalence).

**Commit 2 — Region geometry + LayerKind + strip skeleton (static fallback)**
`layout.rs` (`RegionGeometry`, `strip_region`, `controls_region` + constants), `main.rs` (`LayerKind`, `ButtonSetKey::Strip(u64)`, strip `ButtonSet`, `draw_button_set` extraction, `HitOutcome`, region hit routing), `config.rs` (`[Workspaces]`, default actions).
Tests: `controls_region_is_right_anchored`, `strip_region_width_scales_with_button_count`, `strip_hits_use_region_local_coordinates`, `stale_strip_generation_is_not_visible`, `fallback_strip_has_single_unhighlighted_workspace_one`, `classic_layer_hit_behavior_unchanged`.

**Commit 3 — RuntimeState + live strip rebuild plumbing (no socket yet)**
`state.rs` (new), `main.rs` (`rebuild_strip` diffing, `apply_strip_state`, reapply after `update_config`, drain-strip-touches-on-rebuild).
Tests: `strip_buttons_falls_back_when_disconnected_or_empty`, `rebuild_with_same_workspaces_reports_no_change`, `focus_move_only_toggles_highlights_without_generation_bump`, `count_change_bumps_generation`, `rebuild_invalidates_inflight_strip_touch` (via `hit_in_set` on stale key).

**Commit 4 — Tap-outside close + overlay auto-timeout**
`main.rs` (Down `HitOutcome` handling, strip-tap-closes-and-acts, `overlay_opened_or_touched`, pure timeout fns, loop clamp), `config.rs` (`OverlayTimeoutMs`).
Tests: `miss_inside_regions_is_outside_when_stack_open_else_miss`, `strip_hit_while_stack_open_still_targets_button`, `timeout_due_after_configured_idle`, `touch_refreshes_timeout_anchor`, `zero_timeout_never_fires`.

**Commit 5 — Slider widget (UI + drag + throttle, mock emit)**
`main.rs` (`ButtonAction::Slider`, `ButtonImage::Slider`, render arm, background-fill exclusion, `button_span_abs`, `slider_value_from_x`, `EmitThrottle`, touch branches), `config.rs` (`Slider` field, `SliderTarget`).
Tests: `slider_config_takes_precedence_over_keys_and_overlay`, `slider_value_mapping_clamps_and_respects_track_inset`, `throttle_emits_at_most_every_interval_and_flushes_final`, `slider_action_never_changes_overlay_state`, `volume_state_ignored_while_dragging`.

**Commit 6 — Sysfs slider backends**
`sliders.rs` (new), `backlight.rs` (`pub(crate)` read/write helpers), `main.rs` (pre-privdrop `SliderBackends::new`, wiring, `sync_sliders` on overlay open), `config.rs` (path keys).
Tests: `raw_value_round_trips_with_rounding_and_clamps` (0.0/0.5/1.0 × max 17777/14660), `display_backend_enforces_min_fraction`, `keyboard_backend_allows_zero`, `max_brightness_parse_tolerates_trailing_newline`.

**Commit 7 — Control socket + protocol seam**
`protocol.rs`, `socket.rs` (incl. `LineBuffer`), `Cargo.toml` (`serde_json`, nix `socket`/`net` features), `main.rs` (pre-privdrop bind, epoll data 4/5, pump→state→strip/volume, volume intents from slider emits, disconnect fallback), `config.rs` (`ControlSocket*`, `SocketAllowedUid`).
Tests: `protocol_roundtrip_and_unknown_lines_are_nonfatal`, `line_buffer_reassembles_partial_reads_and_caps_length`, `disconnect_flips_state_to_fallback`, `intents_are_dropped_when_disconnected`, `runtime_state_apply_folds_updates`.

**Commit 8 — Packaging + ship config (no live system changes)**
`scripts/install-ben-service.sh` heredoc: add `RuntimeDirectory=tiny-dfr-ben`, `RuntimeDirectoryMode=0755` (keep `ProtectHome=true`, all hardening intact); handoff doc update. Separately (archdots repo, own commit, unrelated local state untouched): new `MediaLayerKeys`/`ControlGroups`/`[Workspaces]`/`ControlSocket` config per Section 6.
Deployment (installer rerun + `systemctl restart tiny-dfr-ben`) happens **only with Ben's explicit approval**, with the documented rollback path.

Dependency order is strict: 1→2→3→4 (layer/state core), 5→6 (sliders) can start after 2, 7 needs 3+5, 8 last.

---

### Critical Files for Implementation
- /home/ben/dev/projects/tiny-dfr/src/main.rs
- /home/ben/dev/projects/tiny-dfr/src/config.rs
- /home/ben/dev/projects/tiny-dfr/src/layout.rs
- /home/ben/dev/projects/tiny-dfr/src/backlight.rs
- /home/ben/dev/projects/tiny-dfr/scripts/install-ben-service.sh
