# tiny-dfr Touch Bar Rebuild — Phasing, Risk & Rollout Plan

Scope: sequencing, validation, rollback, and config migration for the locked design (workspace strip + nested overlays + real sliders + helper socket). Companion architects own the daemon internals and helper internals; this plan pins *when* each piece lands, *how* it is proven, and *how* every step is reversible on Ben's daily-driver hardware.

Ground truths this plan relies on (verified in-repo today):

- `master` = `e1ecf83`; overlay support (`ebf49e6`) is committed but has **never run on hardware** — the running `tiny-dfr-ben` process (PID 321063, started 04:30) predates the 05:06 binary install, and the live config has no `ControlGroups`, so the overlay code paths are dormant even after a restart until a config exercises them.
- `src/config.rs` has **no `deny_unknown_fields`** — stock tiny-dfr and older fork binaries silently ignore fork-only keys (`ControlGroups`, `OpenOverlay`, future `OverlayTimeoutMs`). Rollback with a "too new" config degrades gracefully rather than crashing, but we still restore a matching config on rollback.
- `ProtectHome=true` hides `/home`, `/root` **and `/run/user`** from the daemon. Therefore the socket direction is fixed: **the root daemon listens** at `/run/tiny-dfr-ben/tiny-dfr.sock` (via `RuntimeDirectory=tiny-dfr-ben`), and the user helper connects/reconnects. The helper can never host the socket in `/run/user/1000` for this daemon.
- `scripts/install-ben-service.sh` already: preflights the appletbdrm DRM card, refuses to touch services without it, backs up `/etc/tiny-dfr/config.toml`, and auto-rolls-back to stock on failed start. `archdots/scripts/install-tiny-dfr-system-links.sh` deploys config and defers to `tiny-dfr-ben.service` when it is enabled.
- archdots working tree has unrelated local state (`.config/niri/config.kdl` modified, branch ahead 1). Commit hygiene must never sweep it up.

---

## 1. Milestones

Ordering rationale (summary):

1. **Prove the committed overlay code live before building on it** (M0). It is the foundation of everything and has zero hardware minutes.
2. **Region split before sliders or dynamic content** (M1). The pinned-left/overlay-right model is the highest-rework-risk rendering change; every later feature draws inside it.
3. **Overlay stack + close semantics before sliders** (M2). Sliders live at overlay depth 2; the navigation shell must exist and be reviewed for touch safety first.
4. **Sliders via brightness before volume** (M3). Brightness needs no helper/socket — the root daemon writes sysfs directly — so the entire drag-widget interaction is proven end-to-end with instantly visible feedback (screen dims) before any IPC exists.
5. **Socket before dynamic workspaces** (M4 before M5). Volume is the simplest live state to prove the channel; the workspace strip then reuses a channel already known-good.
6. **Config end-state last** (M6), once every key it references is implemented.

Every milestone ends with the same five-step ritual:

1. **Gates**: `cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build --release`, plus `python3 -c "import tomllib,sys; tomllib.load(open(sys.argv[1],'rb'))" <config>` (formalized as `scripts/check-config.sh` in M0).
2. **Commits**: small, reviewable, listed per milestone.
3. **Install + restart**: `sudo scripts/install-ben-service.sh` — **each run individually approved by Ben in-chat** (hard rule). Installer preflight already refuses if the appletbdrm card is absent.
4. **2-minute hardware script**: exact taps below; any FAIL → immediate rollback (Section 2), fix, re-run.
5. **Tag last-good**: on PASS, `git tag ben-live/mN-<slug>` on the exact installed commit (tiny-dfr repo) and commit the paired config in archdots.

### M0 — Live canary of overlay code + rollback tooling

**Scope (small on purpose):**
- tiny-dfr repo: modify `scripts/install-ben-service.sh` to snapshot last-good artifacts *before* overwriting (see Section 2). Add `scripts/check-config.sh` (tomllib parse + sanity checks: referenced overlay names exist in `ControlGroups`, icons exist in `share/tiny-dfr/`).
- archdots: add a **temporary canary overlay** to the existing config — one launcher on the right (`OpenOverlay = "canary"`) and a `[ControlGroups] canary = [...]` with 2 key buttons + an explicit `CloseOverlay` button (explicit close is temporary; tap-outside replaces it in M2).
- No daemon code changes beyond the installer script.

**Commits:** tiny-dfr: `installer: snapshot last-good binary/config`, `scripts: add check-config.sh`. archdots: `tiny-dfr: add canary overlay group`.

**Hardware test (2 min):**
1. Bar renders as before + one new canary launcher. PASS = no flicker/artifacts.
2. Tap canary launcher → right side swaps to canary buttons; workspace buttons `1–4` still visible. (Note: in M0 the whole bar redraws on overlay open — full pinning lands in M1; PASS = strip *contents* unchanged.)
3. Tap a canary key button → its key action fires (e.g. PlayPause toggles media).
4. Hold one finger on a canary button, tap `CloseOverlay` with another → overlay closes, **no stuck key** (verify: type in a terminal; no phantom modifiers; `wev`/keyboard behaves normally).
5. Tap workspace `2` → Niri switches. Fn layer toggle still works.

**Why first:** all later work builds on `ebf49e6`; if OpenOverlay/CloseOverlay/touch-draining misbehaves on real hardware, we learn it in a 30-line change, not under five stacked features.

### M1 — Region split: pinned left strip (static) + right-region overlays

**Scope:**
- Left/Right region model in `layout.rs` + `ButtonSet` (per-region virtual counts and spans; left = compact square buttons ~0.6–0.75× width).
- Overlays replace **right region only**; left strip stays rendered and interactive while an overlay is open. Damage rects confined to right region on overlay transitions.
- Implement locked decision 3's strip interaction now: tapping a workspace button while an overlay is open **closes the overlay and performs the switch** in one tap.
- Left strip is still **static config buttons** (`1–4`, Alt+NumN) — dynamism is M5. Big middle spacer reserved.

**Unit tests:** per-region span math; hit routing left-vs-right with overlay open; overlay-open damage excludes left region; workspace-tap-during-overlay closes+fires keys; existing 17 tests still pass.

**Hardware test (2 min):**
1. Bar: squares `1 2 3 4` left, empty middle, launchers right.
2. Open canary overlay → **left strip does not blink or move** (watch closely — this is the damage-region check).
3. With overlay open, tap workspace `3` → overlay closes AND Niri switches, one tap.
4. With overlay open, press-and-hold workspace `1`, release → switch fires; no stuck Alt (type `1` in a terminal afterwards to confirm no modifier residue).
5. Fn layer flip with overlay open → sane state (overlay dismissed, F-keys shown).

### M2 — Overlay stack, tap-outside close, auto-timeout

**Scope:**
- `open_overlay: Option<String>` → `overlay_stack: Vec<String>`; nested groups.
- Tap-outside: any touch in the right region not on an overlay button closes overlays. **Proposal: pop the entire stack to base** (iPhone-folder metaphor: tapping outside a folder closes everything). One-line change to pop-one if Ben prefers after feeling it.
- Auto-timeout: `OverlayTimeoutMs` config key (default 8000, 0 = off); monotonic deadline, refreshed on every touch down/motion; **never fires while any touch slot is active**; integrated into the epoll timeout computation exactly like `pixel_shift`. CLOCK_MONOTONIC halts across suspend → an overlay left open across suspend simply times out ~8s after resume; acceptable, no special resume hook.
- Config reload / Fn-layer-flip while overlay open: drain touches, release any pressed keys, reset stack (defensive invariant, unit-tested).
- Remove the explicit `CloseOverlay` canary button from config (no x buttons per locked decision 3).

**Unit tests (the meat of this milestone):** stack push/pop/pop-all; tap-outside classification incl. middle spacer area; timeout with injected clock; timeout deferred during held touch; drain-releases-keys invariant; reload-with-open-overlay resets cleanly.

**Review gate R1 (before install):** adversarial touch-safety review — see Section 5.

**Hardware test (2 min):**
1. Open canary/sound overlay → tap the empty middle → closes.
2. Open overlay, open nested overlay → tap outside → back to base (or one level if pop-one chosen).
3. Open overlay, hands off, count to ~8 → auto-closes.
4. Open overlay, keep a finger resting on an overlay button past 8s → does NOT close mid-touch; closes ~8s after release.
5. Open overlay, `touch`-edit `/etc/tiny-dfr/config.toml` (rewrite same content) → reload lands, bar returns to base, no stuck keys.

### M3 — Slider widget + direct sysfs brightness (screen + keyboard)

**Scope:**
- `ButtonImage::Slider` rendering (wide track + fill) and `ButtonAction::Slider { target }` with **typed** targets: `ScreenBacklight`, `KbdBacklight`, `Volume` (Volume dormant until M4).
- Drag → value mapping clamped 0.0–1.0, quantized; **slot ownership** (first touch owns the slider; other slots ignored by it).
- Root daemon writes sysfs directly: `/sys/class/backlight/intel_backlight/brightness` (max 17777) and `/sys/class/leds/:white:kbd_backlight/brightness` (max 14660). Harden the `backlight.rs` pattern: no panics — missing path / EACCES → log once + no-op; lazy open; write rate-limited (on quantized-value change, ≤~30 Hz).
- **Screen slider floor**: clamp screen writes to ≥1% of max. Dragging the panel to raw 0 turns the screen off with no visible way to recover — a self-inflicted lockout we design out. Kbd backlight may go to 0.
- Config: `brightness = [screen, kbd]` group; each opens its slider overlay (nesting from M2).
- On overlay open, seed slider from a sysfs read (current brightness), so the widget always reflects reality.

**Unit tests:** value↔raw mapping incl. floor and both max values; clamping; quantization/rate-limit; two-slot ownership; sysfs writer behind a small trait, tested against tempdir files.

**Hardware test (2 min):**
1. Tap brightness → `[screen][kbd]`. Tap screen → wide slider at current brightness position.
2. Drag left/right slowly → panel visibly dims/brightens *while dragging*; slider fill tracks finger.
3. Drag to far left → panel dims to floor, never black.
4. Back out (tap outside), open kbd slider, drag up → **keyboard backlight lights** (first time on this machine). Drag to 0 → off.
5. Two fingers on slider → only first finger moves it; no jumps.
6. Drag then slide finger off the top edge of the bar → value holds at last position, no stuck state.

**Review gate R2 (before install):** root-writes review — see Section 5.

### M4 — Socket plane: daemon listener + user helper; volume goes real

**Scope:**
- Daemon: non-blocking `UnixListener` at `/run/tiny-dfr-ben/tiny-dfr.sock`, epoll-integrated (new fd data=4). Newline-delimited JSON with a `v` field. **SO_PEERCRED gate: accept uid 0 or 1000 only.** Bounded line buffer (drop + log oversize); state updates coalesce last-wins per type; client disconnect is routine, never fatal. Daemon **never shell-execs** (hard rule) — intents go out the socket only.
- Service unit: add `RuntimeDirectory=tiny-dfr-ben` / `RuntimeDirectoryMode=0755` to the generated `tiny-dfr-ben.service` in the installer. `ProtectHome=true` and `RestrictAddressFamilies=AF_UNIX AF_NETLINK` unchanged.
- Protocol v1: in `{"v":1,"type":"volume","value":0.60}` and `{"v":1,"type":"workspaces",...}` (schema landed now, used in M5); out `{"v":1,"type":"set-volume","value":0.42}`.
- Helper (archdots): Python 3 stdlib only, `.local/bin/tiny-dfr-helper` + `.config/systemd/user/tiny-dfr-helper.service` (`Restart=on-failure`, `After=pipewire.service`). Volume watch via `pactl subscribe` + `wpctl get-volume @DEFAULT_AUDIO_SINK@` parse (`Volume: 0.60 [MUTED]`); applies `set-volume` intents via `wpctl set-volume`. Reconnect loop with backoff to the daemon socket.
- Sound group live: `[vol][play/pause]`; vol → real slider (seeded/updated from pushed state; drags emit set-volume intents). No mute, no skip. Helper down → slider renders last-known value, drags log + no-op (renderer-architect may refine visual).

**Unit tests:** protocol parser vs garbage/partial/oversize/flood input; coalescing; peer-cred decision logic; helper's wpctl output parser (`[MUTED]`, locale oddities) via pytest or `python3 -m unittest` in archdots.

**Bench test without the bar:** run the helper against a throwaway Python mock listener socket to validate reconnect/parse before touching hardware.

**Hardware test (2 min):**
1. `systemctl --user start tiny-dfr-helper` (user-level, no sudo needed).
2. Open sound → vol → slider shows the *real* current volume position.
3. Drag → audio volume changes live (`wpctl get-volume` confirms); play something and hear it.
4. Change volume from a niri keybind/waybar → slider (if open) tracks within ~1s.
5. `systemctl --user stop tiny-dfr-helper` mid-drag → no daemon crash (`systemctl status tiny-dfr-ben` clean); restart helper → slider resyncs.

### M5 — Live workspace strip

**Scope:**
- Helper: `niri msg --json event-stream` subscription; maintains workspace model; on change, pushes the list **filtered to the focused output** (occupied-or-focused, sorted by `idx`), debounced ~50 ms.
- Daemon: left region rebuilt from pushed state; focused workspace highlighted (existing `set_highlighted`); buttons emit `LeftAlt+Num<idx>` (cap render+bind at idx ≤ 9 to match niri's Alt+1..9); mid-touch disappearance uses the existing constrained-hit safety.
- Fallback (locked decision 7): no state ever received, or socket/helper down (mark state stale on disconnect) → single static unhighlighted `[1]` button. Nothing else changes.
- Remove static `1–4` config buttons (strip is now runtime-generated).

**Unit tests:** workspace-state → button-model mapping (occupied/focused filter, output filter, idx→key, cap at 9, highlight); fallback transitions (never-connected, disconnect-stale, reconnect); left-region reflow span math; shrinking-list mid-touch safety.

**Review gate R3 (before install):** IPC/dynamic-content review — see Section 5.

**Hardware test (2 min):**
1. Strip shows exactly the occupied workspaces + focused one; focused is highlighted.
2. Tap another workspace square → Niri switches, highlight moves.
3. Switch workspaces from the keyboard (Alt+N) → strip highlight follows without touching the bar.
4. Open a window on a fresh workspace → new square appears; close it and leave → square disappears.
5. Focus DP-3 (external) → strip switches to DP-3's workspaces; back to eDP-1.
6. `systemctl --user stop tiny-dfr-helper` → strip collapses to static `[1]`; restart → live strip returns.
7. Open sound overlay, then tap a workspace square → overlay closes + switch (M1 behavior, now with live buttons).

### M6 — Config end-state migration + polish

**Scope:** deploy the final config (Section 6 draft): right side reduced to exactly `[brightness][sound]`, canary group deleted, legacy media keys removed; size/spacing tuning of squares and the reserved middle after seeing it live; docs (`CONTEXT.md`/rollback notes) updated; final tags. No new daemon features. The middle stays an inert spacer — animation ticks are already possible via the epoll-timeout mechanism (pixel-shift precedent), which is all "do not preclude" requires.

**Hardware test (2 min):** full walkthrough — strip live-follows workspaces; brightness→screen/kbd sliders work; sound→vol slider + play/pause work; tap-outside and 8s timeout close overlays; helper stop → `[1]` fallback; Fn layer flip normal; 30 seconds of ordinary typing to confirm zero phantom keys.

---

## 2. Rollback

### Tiered rollback (cheapest first)

**Tier 0 — last-good binary (seconds, one sudo):**
Installer (from M0) snapshots before every overwrite:
- `cp -a /usr/local/bin/tiny-dfr-ben /usr/local/bin/tiny-dfr-ben.last-good`
- `cp -a /etc/tiny-dfr/config.toml /etc/tiny-dfr/config.toml.last-good`
- Written *only after* the previous install passed its hardware test in practice, because we only re-run the installer at milestone boundaries; the snapshot is by construction the last bar that worked.

Recovery: `sudo install -m0755 /usr/local/bin/tiny-dfr-ben.last-good /usr/local/bin/tiny-dfr-ben && sudo install -m0644 /etc/tiny-dfr/config.toml.last-good /etc/tiny-dfr/config.toml && sudo systemctl restart tiny-dfr-ben` (propose adding this as `scripts/rollback-ben-last-good.sh` in M0 so it's one approved command). Binary and config roll back **together** — they are a matched pair.

**Tier 1 — rebuild from tag (minutes):** every passed milestone is tagged `ben-live/mN-<slug>` on the exact installed commit; archdots gains a paired `tiny-dfr: mN config` commit. Recovery: `git checkout ben-live/mN-*`, `cargo build --release`, checkout matching archdots config, re-run installer (approved).

**Tier 2 — stock tiny-dfr (always valid):** `sudo systemctl disable --now tiny-dfr-ben.service && sudo systemctl enable --now tiny-dfr.service`, restoring a config backup if desired. This path is untouched by every milestone: nothing in the plan modifies `/usr/bin/tiny-dfr` or `tiny-dfr.service`; the only shared artifact is `/etc/tiny-dfr/config.toml`, and (verified) the config parser ignores unknown keys, so even the end-state fork config leaves stock tiny-dfr functional (fork-only buttons become inert). The installer's existing timestamped `.bak` copies plus `.last-good` cover exact restoration. **One watch-item:** M4 adds `RuntimeDirectory=` to `tiny-dfr-ben.service` only — the stock unit is never edited, and `ProtectHome=true` stays in both.

**Tier 3 — reboot:** the known-safe recovery if the Touch Bar USB display wedges (per installer preflight comments). Never force-switch `05ac:8302`.

### Per-milestone notes
- M0: Tier 0 becomes available *during* this install (installer snapshots the currently-running-known-good binary before overwriting).
- M4/M5: rollback must also `systemctl --user stop tiny-dfr-helper` if the helper is implicated; helper is user-level and needs no sudo. An old daemon + new helper is safe (helper's connect just fails/retries); a new daemon + no helper is the designed fallback path — no ordering hazard.

---

## 3. Edge-case risk catalog

| # | Scenario | Expected behavior | How to test |
|---|----------|-------------------|-------------|
| 1 | Slider drag in progress when overlay timeout expires | Timeout never fires while any touch slot is active; deadline refreshes on touch activity; re-arms on release | **Unit** (injected clock + synthetic touch stream); hardware: hold slider >8s (M3 step 4) |
| 2 | Workspace button disappears mid-touch (window closed / output refocus) | Constrained hit returns None; touch-up performs nothing; no key press ever half-emitted; strip reflows | **Unit** (extend `hidden_button_set_does_not_retarget_active_touch` pattern to a shrinking left region); hardware: hold square while `niri msg action close-window` from a terminal |
| 3 | Two fingers on one slider | First slot owns it; second slot ignored by the slider (may hit other buttons normally); owner-up ends the drag | **Unit** (two synthetic slots); hardware M3 step 5 |
| 4 | Socket flood / garbage / oversize lines while overlay open | Bounded buffer, per-type last-wins coalescing, one redraw per loop iteration; malformed lines logged+dropped; event loop never blocks | **Unit** (parser + coalescer); bench: mock client spamming 10k msgs/s, watch daemon CPU/mem via `systemctl status` |
| 5 | Helper restarts mid-drag | set-volume write fails → logged no-op, no panic; slider keeps local value; snaps to true state on next push after reconnect | **Unit** (write-failure path); hardware M4 step 5 |
| 6 | Suspend/resume with overlay open | Monotonic deadline halts during suspend → overlay times out ~8s after resume if untouched; touches map empty after resume (libinput re-init); no stuck keys | **Hardware only** (open overlay, `systemctl suspend`, resume, observe + type test) |
| 7 | Config reload while overlay open / mid-touch | Before layer rebuild: release all pressed keys via uinput, drain touches, reset overlay stack; new layers start clean | **Unit** (invariant: rebuild path asserts no pressed buttons remain); hardware M2 step 5 |
| 8 | Drag ends outside the bar / finger slides off edge; also `TouchEvent::Cancel` | Out-of-bounds motion keeps last in-bounds value; Up/Cancel outside releases keys and ends drag; slider commits last value. Audit that Cancel is handled everywhere Up is | **Unit** (Up/Cancel with out-of-range coords); hardware M3 step 6 |
| 9 | Stuck-key class (historical bug source): any `set_active(true)` without a paired release — overlay open/close drains, layer flips, reloads, workspace-tap-closes-overlay | Single invariant: every path that invalidates the `touches` map must release keys first. Centralize into one `drain_touches_releasing_keys()` used by all five paths | **Unit** (each path asserts invariant) + **R1 review focus** + hardware "type test" after every overlay hardware step |
| 10 | Workspace tap while overlay open | One tap = close all overlays + emit Alt+NumN; keys emitted exactly once | **Unit**; hardware M1 step 3 |
| 11 | Focused output flips (eDP-1 ↔ DP-3) between helper push and user tap | Strip may lag ≤ debounce; tap acts on the *rendered* button's idx — worst case switches a same-idx workspace on the new output (niri semantics); acceptable; disappearing-button safety covers removals | **Unit** (state race replay); hardware M5 step 5 |
| 12 | kbd backlight sysfs missing/EACCES (device quirk) | Log once, slider renders but writes no-op; **daemon never panics** (current `backlight.rs` panics on read — must be hardened in M3) | **Unit** (mock fs paths); hardware: rename risk not testable — rely on unit |
| 13 | Fresh boot ordering: daemon up before niri/helper | Daemon renders fallback `[1]` + launchers immediately; helper connects later and upgrades the strip; no startup dependency between units | Hardware: reboot once during M5 window |
| 14 | Unauthorized local socket client | SO_PEERCRED uid∉{0,1000} → connection dropped + logged | **Unit** (gate logic); bench with `nobody`-uid test if desired |

---

## 4. Hardware-free vs hardware-required validation

**Fully validated without the bar (cargo test / scripts, run at every gate):**
- All layout math: per-region spans, square sizing, reflow with 1–9 workspaces, middle reserve.
- Hit-testing: left/right routing, constrained hits, tap-outside classification, shrinking sets.
- State machines: overlay stack, timeout (injected clock), touch-drain/key-release invariants, reload reset, fallback transitions.
- Slider logic: value mapping/clamping/floor/quantization, slot ownership, sysfs writer against tempdir files.
- Protocol: parse/serialize, garbage/flood/partial-line handling, coalescing, peer-cred decisions.
- Helper: wpctl output parsing, niri event JSON → model, reconnect logic — against a mock socket listener, plus live `niri msg --json workspaces` parsing (read-only, safe on the running session).
- Config: `scripts/check-config.sh` (tomllib parse + referenced-overlay/icon existence) on every archdots config change — CI-style, no daemon needed.

**Strictly needs the physical bar:**
- Visual truth: rotation, damage-region correctness (left-strip flicker), square button legibility at 60px, slider granularity/feel, highlight visibility.
- Real input: multi-slot behavior of the actual digitizer, drag latency, edge-of-bar behavior, libinput Cancel semantics on this device.
- uinput → Niri end-to-end (Alt+NumN actually switching).
- sysfs effects: panel dimming curve, kbd LED actually lighting (never before verified on this machine).
- Service sandboxing reality: RuntimeDirectory + ProtectHome + socket connect from uid 1000.
- Suspend/resume, fresh-boot ordering, appletbdrm re-enumeration.

---

## 5. Review gates (adversarial passes)

Multi-touch handling produced real stuck-key bugs last time; reviews are placed **before the install** of the milestones that expand input handling, so nothing unreviewed touches hardware:

- **R1 — after M2 code-complete, before M2 install.** Focus: touch/multi-touch safety. Checklist: every `set_active(true)` has a provable release on all exit paths (overlay open/close/pop-all, timeout firing, layer flip, config reload, Cancel events, drain); slot map can never leak entries; timeout cannot fire with active slots; tap-outside cannot double-fire with a button release.
- **R2 — after M3 code-complete, before M3 install.** Focus: root daemon writing hardware. Checklist: no panic paths in sysfs code (this daemon runs as root with `Restart=always` — a panic loop is a bar outage); clamping/floor cannot be bypassed by weird float input (NaN, out-of-range motion coords); write rate-limits hold; no path ever shells out.
- **R3 — after M4+M5 code-complete, before M5 install** (M4 and M5 may be reviewed together if landed close). Focus: untrusted-ish IPC into a root process. Checklist: bounded buffers, allocation caps, malformed JSON never panics (serde into typed structs, no `unwrap` on parse), peer-cred gate, workspace count cap, helper input (niri/wpctl output) treated as untrusted by the helper too.

Mechanics: run the repo's `/code-review` pass plus a targeted second pass seeded with the checklist above; findings fixed before Ben is asked to approve the install. Cheap inline reviews at M0/M1/M6 commits (small diffs) rather than full gates.

---

## 6. archdots config migration

### End-state draft — `~/archdots/.config/tiny-dfr/config.toml` (deployed M6)

Key names for fork-only features (`OpenOverlay`, `Slider`, `OverlayTimeoutMs`, `WorkspaceStrip`) must match the daemon architect's final spelling; layout and semantics below are the locked design.

```toml
# Ben's tiny-dfr Touch Bar config (fork: tiny-dfr-ben).
# Canonical copy lives in ~/archdots/.config/tiny-dfr/ and is copied into
# /etc/tiny-dfr/config.toml by scripts/install-tiny-dfr-system-links.sh.
# Fork-only keys are ignored by stock tiny-dfr (safe for rollback).

MediaLayerDefault = true
ShowButtonOutlines = true
EnablePixelShift = false
FontTemplate = ":bold"
AdaptiveBrightness = true
ActiveBrightness = 160
DoublePressSwitchLayers = 200

# Fork: overlays auto-close after this many ms without touches (0 = never).
OverlayTimeoutMs = 8000
# Fork: left region is the live Niri workspace strip fed by the helper socket;
# falls back to a single static [1] when no helper state is available.
WorkspaceStrip = true

MediaLayerKeys = [
    # Left region: dynamic workspace strip (daemon-rendered; not configured here).
    # Middle: reserved space for the future critter.
    { Stretch = 6 },
    # Right region: exactly two launchers.
    { Id = "brightness", Icon = "brightness_high", OpenOverlay = "brightness" },
    { Id = "sound",      Icon = "volume_up",       OpenOverlay = "sound" },
]

[ControlGroups]
brightness = [
    { Id = "brightness-screen", Icon = "brightness_high", OpenOverlay = "brightness-screen-slider" },
    { Id = "brightness-kbd",    Icon = "backlight_high",  OpenOverlay = "brightness-kbd-slider" },
]
brightness-screen-slider = [
    { Id = "screen-slider", Slider = "ScreenBacklight", Stretch = 4 },
]
brightness-kbd-slider = [
    { Id = "kbd-slider", Slider = "KbdBacklight", Stretch = 4 },
]
sound = [
    { Id = "sound-vol",  Icon = "volume_up",  OpenOverlay = "volume-slider" },
    { Id = "play-pause", Icon = "play_pause", Action = "PlayPause" },
]
volume-slider = [
    { Id = "volume-slider", Slider = "Volume", Stretch = 4 },
]
```

Intermediate configs: M0 adds the canary group to today's file; M1 keeps static `1–4` squares; M2 drops the CloseOverlay button; M3 adds the brightness groups; M4 adds sound groups; M5 removes static workspace buttons; M6 is the file above. Every intermediate config passes `check-config.sh` and is committed in archdots in the same breath as the tiny-dfr tag it pairs with.

### New archdots files (M4)
- `.local/bin/tiny-dfr-helper` (Python 3, stdlib only, executable)
- `.config/systemd/user/tiny-dfr-helper.service` (`Restart=on-failure`, `After=pipewire.service graphical-session.target`)
- Both are new paths — stow `--no-folding` handles them; run `stow` after commit, then `systemctl --user daemon-reload && systemctl --user enable --now tiny-dfr-helper` (user-level; no sudo approval needed, but announce it).
- `scripts/install-tiny-dfr-system-links.sh` needs **no change** for the socket (RuntimeDirectory lives in the tiny-dfr repo's installer); only touch it if config deployment semantics change.

### Commit hygiene (archdots)
- Working tree currently has unrelated state: `.config/niri/config.kdl` modified and branch ahead-by-1. **Never** `git add -A` / `git commit -a`. Stage explicitly and only: `.config/tiny-dfr/config.toml`, `.local/bin/tiny-dfr-helper`, `.config/systemd/user/tiny-dfr-helper.service`, and (only if actually edited) `scripts/install-tiny-dfr-system-links.sh`.
- One archdots commit per milestone, message prefix `tiny-dfr:` (e.g. `tiny-dfr: m3 brightness slider config`), so tiny-dfr tags and config commits pair 1:1 for Tier-1 rollback.
- Do not push archdots or resolve the ahead-by-1 state as part of this work unless Ben asks.

---

### Critical Files for Implementation
- /home/ben/dev/projects/tiny-dfr/src/main.rs
- /home/ben/dev/projects/tiny-dfr/src/layout.rs
- /home/ben/dev/projects/tiny-dfr/scripts/install-ben-service.sh
- /home/ben/dev/projects/tiny-dfr/src/config.rs
- /home/ben/archdots/.config/tiny-dfr/config.toml
