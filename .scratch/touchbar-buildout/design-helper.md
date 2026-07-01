All facts verified. Producing the design document.

# Design: tiny-dfr Helper + Socket Protocol

Architect deliverable, 2026-07-02. Covers the user-session helper (archdots) and the daemon<->helper socket protocol (tiny-dfr). Coordinates with the renderer/layout designer's plan via named seams.

---

## 0. Architecture-critical correction discovered during verification

**The daemon does not run as root at steady state.** `real_main()` starts as root, constructs `BacklightManager` (which opens the Touch Bar brightness sysfs file for write *while root*), then **drops privileges to `nobody` with groups `input,video`** at `/home/ben/dev/projects/tiny-dfr/src/main.rs:1275-1282`:

```rust
PrivDrop::default()
    .user("nobody")
    .group_list(&groups)   // ["input", "video"]
    .apply()
```

Verified live: `ps` shows PID 321063 running as `nobody`. The existing pattern (backlight.rs:83-86) is: **open privileged fds before the drop, keep them, write through them after the drop** (Unix checks DAC at `open()`, not `write()`).

Consequences baked into this design:

1. **Brightness sysfs** (`/sys/class/backlight/intel_backlight/brightness`, `/sys/class/leds/:white:kbd_backlight/brightness`): the daemon must open both `File`s **before** the privdrop in `real_main()` (same as `bl_file`), as `Option<File>` so a missing node degrades gracefully. Brightness never crosses the socket — it is daemon-local, exactly like the locked decision intends.
2. **The unix socket must be bound, chowned, and chmodded before the privdrop** (root can chown to Ben's uid; `nobody` cannot). The listener fd survives the drop; `accept()` keeps working.
3. This is *better* for security than a persistent-root daemon: even a fully compromised protocol parser runs as `nobody:input,video`.

---

## 1. Protocol

### 1.1 Framing: versioned line-delimited JSON (NDJSON)

- One JSON object per line, UTF-8, `\n` terminated, **max 4096 bytes per line** (receiver closes the connection on overflow — a legitimate state message is < 600 bytes).
- Every message has a required string field `"t"` (type). Unknown *fields* are silently ignored (must-ignore rule — this is the future-proofing seam). Unknown *types* are dropped and count against an error budget.
- Version is negotiated once in the `hello` message, not per-message.

**Why NDJSON, not length-prefixed binary or varlink/D-Bus:** both endpoints get it from stdlib (`serde_json` in Rust — one small new dependency; `json` in Python); it is debuggable with `socat - UNIX-CONNECT:/run/tiny-dfr-ben/helper.sock`; message rate is tiny (bursts of ~20/s during a slider drag, ~1/s otherwise) so parsing cost is irrelevant; the fixed line cap gives trivially bounded memory. Length-prefixed framing buys nothing at this scale and loses debuggability. D-Bus would drag the root daemon onto the bus and violate the "one narrow socket" decision.

### 1.2 Helper -> daemon (state in)

Exactly two message types. Everything else is rejected.

**`hello` — must be the first line after connect:**

```json
{"t":"hello","v":1,"src":"tiny-dfr-helper"}
```

- `v` (int, required): protocol version. Daemon supports `{1}`; anything else -> daemon closes the connection. Helper treats close-after-hello as "incompatible" and backs off at max interval (prevents a tight reconnect loop across a version skew).
- `src` (string, optional, informational, max 64 chars).
- If the first line is not a valid `hello`, the daemon closes immediately.

**`state` — full snapshot, idempotent, doubles as heartbeat:**

```json
{"t":"state","ws":[{"idx":1,"occ":true,"foc":false},{"idx":3,"occ":false,"foc":true}],"vol":{"level":0.60,"muted":true}}
```

- `ws` (array, required): the workspace strip for the **focused output**, sorted by `idx` ascending.
  - Each entry: `idx` int in `1..=32`, `occ` bool, `foc` bool. Extra fields ignored.
  - Helper sends at most 9 entries (Niri binds Alt+1..9). Daemon accepts up to 16, renders at most 9, uses the **first** `foc:true` if the helper ever sends more than one.
  - Empty array is legal and means "no usable view" -> daemon renders the fallback `[1]`.
- `vol` (object or absent): `level` finite float clamped by receiver to `[0.0, 1.0]` (helper pre-clamps; wpctl can report > 1.0 on boost), `muted` bool. Absent -> daemon keeps last-known volume (or slider is inert if never known).
- **Full snapshot only, no deltas.** Justification: self-healing (any single message fully repairs the daemon's view after drops/reconnects), no sequence numbers, no delta-application bugs. At < 600 bytes there is nothing to save.
- Sent (a) debounced on any derived-state change, (b) unconditionally every **2 s** as heartbeat, (c) immediately after `hello`.

### 1.3 Daemon -> helper (intents out)

Exactly one message type:

```json
{"t":"set-volume","level":0.42}
```

- `level` finite float; helper clamps to `[0.0, 1.0]` and applies `wpctl set-volume @DEFAULT_AUDIO_SINK@ <level> -l 1.0`.
- Because there is no mute button anywhere (locked decision 4), the helper also runs `wpctl set-mute @DEFAULT_AUDIO_SINK@ 0` on any set-volume intent — otherwise dragging the slider while the sink is muted (it is muted right now: `Volume: 0.60 [MUTED]`) would audibly do nothing. The slider is the single volume authority.
- No acks. The resulting `pactl` change event -> helper pushes fresh `state` — the state echo *is* the ack.
- Daemon rate-limits sends to one per **40 ms, latest-wins**, during slider drags.

**Explicit non-goals of the protocol (enumerated narrowness):** no brightness messages (daemon-local sysfs), no key/command execution, no config, no queries/RPC, no niri actions (workspace switching stays uinput Alt+NumN, physical-touch-driven only). Anything not in 1.2/1.3 is invalid.

### 1.4 Malformed input handling (daemon must never crash)

All parsing via `serde_json` into typed structs with `#[serde(deny_unknown_fields)]` **off** (must-ignore) and strict field types. On any of: JSON parse error, missing/`t` mismatch, out-of-range/non-finite numbers, oversize line -> **drop the line, increment an error counter, log at debug**. After **8 consecutive** invalid lines, or any line > 4096 bytes, or > 200 messages/s sustained over a 1 s window: close the connection (helper will reconnect with backoff; a buggy helper cannot spin the daemon). Counter resets on any valid message. All socket I/O is non-blocking; EOF/EPOLLHUP/read errors -> treat as disconnect, never panic. No `unwrap()` anywhere on the socket path.

### 1.5 Volume echo-fight rule (renderer coordination)

While a slider touch is active on the volume slider, the daemon must **ignore incoming `vol` state for rendering** (local finger position wins) and resume tracking helper state on touch-up. Without this, the state echo of intent N fights the finger at position N+3 mid-drag. This is a one-line seam for the renderer designer: gate `vol` application on "no active slider touch".

---

## 2. Socket

### 2.1 Identity

- **Path:** `/run/tiny-dfr-ben/helper.sock`
- **Type:** `SOCK_STREAM` (connection-oriented: gives ordered byte stream for NDJSON, immediate EPOLLHUP on helper death for instant fallback, and per-connection SO_PEERCRED. SOCK_SEQPACKET would also work but stream + line framing is the simpler, more debuggable choice).
- Created by the daemon (listener); helper is the client. The reverse direction is impossible without relaxing security: a helper-owned socket would live in `/run/user/1000` (mode 0700 `ben:ben`), which the post-privdrop `nobody` daemon cannot traverse.

### 2.2 Service unit change (tiny-dfr repo, installer heredoc)

Add to the `[Service]` section of the `tiny-dfr-ben.service` heredoc in `/home/ben/dev/projects/tiny-dfr/scripts/install-ben-service.sh` (lines 93-108):

```ini
RuntimeDirectory=tiny-dfr-ben
RuntimeDirectoryMode=0755
```

- `ProtectHome=true` untouched (hard rule). `RestrictAddressFamilies=AF_UNIX` already present.
- systemd creates `/run/tiny-dfr-ben` fresh (root:root 0755) on every start and removes it on stop — stale-socket cleanup is nearly free; daemon still does a defensive `unlink()` before `bind()`.

### 2.3 Permission model (recommended: uid-owned socket + SO_PEERCRED)

Sequence in `real_main()`, **before** the privdrop at main.rs:1275:

1. `unlink("/run/tiny-dfr-ben/helper.sock")` (ignore ENOENT), `bind()`, `listen(1)`.
2. `chown(sock, HelperUid, -1)`; `chmod(sock, 0o600)`. Directory stays root:root 0755 (traverse-only for others; nobody else can create files there, so **no socket squatting**).
3. After privdrop, on every `accept()`: read `SO_PEERCRED` (nix `getsockopt(..., PeerCredentials)` — add the `socket` feature to the existing nix 0.29 dependency) and **close unless `peer.uid == HelperUid || peer.uid == 0`**.

`HelperUid` is a new config key (`HelperUid = 1000` in config.toml, default 1000 in code). This is layered: filesystem DAC (only uid 1000 can connect at all) plus an in-daemon check (defense in depth if the socket mode ever regresses). Recommended over a group-based model because it needs no group bookkeeping and the machine is single-user; over 0666+peercred-only because DAC is enforced by the kernel even if daemon code regresses.

**Single-client policy:** at most one active helper connection. A new accepted (and peercred-validated) connection **replaces** the old one (old fd closed). This makes helper restarts seamless — the fresh connection wins, no lockout from a half-dead predecessor.

### 2.4 Epoll integration and staleness (daemon side, coordinated seam)

- Listener fd -> epoll `data=4`; active client fd -> epoll `data=5` (added/removed on accept/close), alongside the existing 0-3 at main.rs:1299-1311.
- Daemon-side state object (the seam handed to the renderer designer):

```rust
struct HelperLink {
    listener: UnixListener,
    client: Option<HelperClient>,      // fd + read buffer + error budget
    last_state_at: Option<Instant>,
    state: HelperState,                // workspaces: Vec<WsEntry>, volume: Option<Vol>
}
impl HelperLink {
    fn is_fresh(&self) -> bool         // client connected AND last_state_at within 6s
}
```

- **Staleness rule:** state is stale when (a) no client connected, or (b) no valid `state` message for **6 s** (three missed 2 s heartbeats). Stale -> renderer draws the single static unhighlighted `[1]` (locked decision 7); volume slider becomes inert (drags produce no intents); nothing else changes. Freshness returning marks the workspace region changed for redraw.
- Disconnects wake epoll immediately (EPOLLHUP), so the common failure (helper crash, logout) hits fallback instantly. For the rare silent-hung-helper case, cap the loop's `next_timeout_ms` (main.rs:1348-1358, same mechanism pixel-shift already uses) at time-until-staleness whenever a client is connected, bounding detection latency to ~6 s.
- `set-volume` writes are non-blocking; if the send buffer is full or write fails, drop the intent (never block the render loop) and let the next drag tick retry.

### 2.5 Helper reconnect semantics

- Connect attempt on startup; on any failure/disconnect: exponential backoff **0.5 s -> 1 -> 2 -> 4 -> 8 s cap**, with jitter (+/-20%), retrying forever (covers fresh boot where the daemon starts later, daemon restarts, and version-skew closes).
- On every successful connect: send `hello`, then an immediate full `state`.
- Helper keeps watching niri/PipeWire while disconnected (state stays warm; nothing to replay — snapshots make reconnect stateless).

---

## 3. Helper implementation plan

### 3.1 Language and shape

**Python 3 (3.14 present), stdlib only, single file, asyncio.** Matches the archdots precedent (`.config/niri/scripts/lock-video-control.py` already does AF_UNIX + JSON in stdlib Python). asyncio cleanly multiplexes: two long-lived subprocess stdout readers, one unix-socket client with backoff, debounce timers, and the heartbeat — no threads, no dependencies, no build step. Rust would add a second cargo project to the dotfiles repo for zero benefit at this message rate.

File: `/home/ben/archdots/.config/tiny-dfr/helper/tiny_dfr_helper.py` (stowed to `~/.config/tiny-dfr/helper/`).

### 3.2 Internal structure (asyncio tasks)

```
main()
 ├─ NiriWatcher        subprocess: niri msg --json event-stream (auto-restart, backoff)
 ├─ VolumeWatcher      subprocess: pactl subscribe (auto-restart) + wpctl get-volume polls
 ├─ SocketClient       connect/backoff/hello/send-state/read-intents
 ├─ IntentApplier      coalesced wpctl set-volume + set-mute 0
 └─ Heartbeat          2s tick -> push_state(force=True)
Shared: Model { workspaces_by_id, windows_by_id, volume, last_sent_state }
```

**NiriWatcher** — spawns `niri msg --json event-stream` with `NIRI_SOCKET` resolved from env, else `glob("/run/user/<uid>/niri.*.sock")` (newest). Niri sends a full initial snapshot on connect (verified: `WorkspacesChanged` and `WindowsChanged` arrive first), so restart = re-seed. Handled events, everything else ignored:
- `WorkspacesChanged` -> replace `workspaces_by_id` (full list).
- `WindowsChanged` -> replace `windows_by_id`.
- `WindowOpenedOrChanged` -> upsert window (carries `workspace_id`).
- `WindowClosed` -> remove window.
- `WorkspaceActivated` / `WindowFocusChanged` / `WorkspaceActiveWindowChanged` -> update the relevant records (focus/active-window bits).

**Occupancy — exact definition:** `occupied(ws) := any(w.workspace_id == ws.id for w in windows_by_id.values())`. This is the authoritative rule (window-set tracking), because `WorkspacesChanged` alone does not fire when a lone window moves between existing workspaces. Seed fallback: if no `WindowsChanged` has arrived yet, use `ws.active_window_id is not None`.

**Focused-output view derivation:**
1. `focused = first ws with is_focused == True` (niri guarantees exactly one; if none — startup race — emit `ws: []`, which the daemon renders as the `[1]` fallback).
2. `strip = [ws for ws in workspaces if ws.output == focused.output and (occupied(ws) or ws.is_focused)]`, sorted by `idx`, truncated to 9.
3. Map to `{"idx": ws.idx, "occ": occupied(ws), "foc": ws.is_focused}`. Note `idx` is the **per-output** index, which is exactly what Alt+NumN targets on the focused output — the daemon's workspace buttons emit `LeftAlt+Num<idx>` and Niri's `focus-workspace N` acts on the focused output. The two-output setup (eDP-1 + DP-3) is therefore handled entirely helper-side.

**VolumeWatcher** — spawns `pactl subscribe`; on any line matching `'change' on (sink|server)` schedules a debounced (30 ms) `wpctl get-volume @DEFAULT_AUDIO_SINK@` and parses `^Volume: ([0-9.]+)( \[MUTED\])?$` -> `{"level": min(v, 1.0), "muted": bool}`. `server` events catch default-sink switches. If `pactl` dies, restart with backoff; the 2 s heartbeat re-polls `wpctl get-volume` regardless, so volume can never drift for more than one heartbeat even with pactl down.

**Debouncing (state push):** any model change arms a 40 ms coalescing timer; on fire, derive the snapshot and push **only if different from `last_sent_state`** (niri emits event bursts on window open; this collapses them to one message). Heartbeat pushes unconditionally.

**IntentApplier:** incoming `set-volume` intents overwrite a single pending value; an applier loop applies the latest at most every 50 ms via `wpctl set-volume @DEFAULT_AUDIO_SINK@ <v> -l 1.0` then `wpctl set-mute @DEFAULT_AUDIO_SINK@ 0`. Latest-wins coalescing means a fast drag never queues a backlog of subprocess spawns.

**Robustness rules:** every JSON parse wrapped; unknown message types from the daemon ignored; all subprocesses restarted with capped backoff; helper never crashes on daemon disconnect; `SIGTERM` -> clean exit (systemd handles restart).

**Testability flags:** `--print-state` (derive and print one state snapshot to stdout, no socket — validates niri parsing standalone) and env override `TINY_DFR_HELPER_SOCKET` (point at a scratch socket for a fake-server test without touching the live daemon).

---

## 4. Packaging in archdots

Archdots facts respected: stow with `--no-folding` (`.stowrc`), AGENTS.md forbids sudo/unrelated changes, repo currently has an unrelated dirty file (`.config/niri/config.kdl`, branch ahead 1) — **do not touch or stage it**. There is no `.config/systemd/user/` in archdots yet; `~/.config/systemd/user/` exists with real files (`codex-remote-control.service`) — `--no-folding` creates real directories and symlinks leaves, so existing units are preserved.

New files (all tiny-dfr-scoped):

```
archdots/
├── .config/tiny-dfr/helper/tiny_dfr_helper.py        # the helper (mode 755)
├── .config/tiny-dfr/README.md                        # extend: helper + protocol pointer
├── .config/systemd/user/tiny-dfr-helper.service      # user unit
└── scripts/install-tiny-dfr-helper.sh                # user-level installer (no sudo)
```

**`tiny-dfr-helper.service`:**

```ini
[Unit]
Description=tiny-dfr user-session helper (niri workspaces + PipeWire volume)
# Needs the niri IPC socket and PipeWire; part of the graphical session.
After=niri.service wireplumber.service
Wants=wireplumber.service
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/bin/python3 %h/.config/tiny-dfr/helper/tiny_dfr_helper.py
Restart=always
RestartSec=2
# Belt and braces; helper also has internal backoff for daemon/niri sockets.

[Install]
WantedBy=graphical-session.target
```

(`niri.service` is `BindsTo=graphical-session.target`, verified via `systemctl --user cat niri.service`, so `WantedBy=graphical-session.target` starts the helper with the session and stops it on logout. The helper's internal backoff tolerates starting before the root daemon's socket exists — fresh-boot ordering between user session and system service needs no coordination.)

**`scripts/install-tiny-dfr-helper.sh`** (user-level, no sudo): re-runs `stow` for the repo (or verifies the two symlinks exist), then `systemctl --user daemon-reload && systemctl --user enable --now tiny-dfr-helper.service` and prints `systemctl --user status`. It touches only the two tiny-dfr paths.

**Coexistence with `install-tiny-dfr-system-links.sh`:** unchanged flow. That script keeps copying `config.toml` -> `/etc/tiny-dfr/` (which will gain `HelperUid = 1000` and the new ControlGroups when the renderer work lands) and managing the *system* services. The helper installer is a separate, sudo-free script; the system script's restart of `tiny-dfr-ben.service` remains the only place the socket/RuntimeDirectory change goes live — gated on Ben's per-action approval per CLAUDE.md.

---

## 5. Security review of this design

Threat actor: a malicious/buggy local process. Baseline: anything running as uid 1000 can already run `wpctl`, `niri msg action`, and inject input via the compositor — the socket must not grant anything *beyond* that baseline, and must not let uid 1000 reach the daemon's root-adjacent capabilities (uinput handle, DRM, sysfs fds).

| Attack | Exposure | Mitigation |
|---|---|---|
| Connect as another uid | None | Socket 0600 owned by uid 1000 in root-owned 0755 dir (kernel DAC) + SO_PEERCRED check in daemon. |
| Socket squatting / replacing the socket | None | Only root can create files in `/run/tiny-dfr-ben`; systemd recreates it per-start. |
| Max-volume blast via forged `set-volume`... | N/A | ...doesn't exist: `set-volume` flows daemon->helper only. A uid-1000 attacker impersonating the *helper* can't send it; impersonating the *daemon* to the helper would require owning the listener (root-only). Direct `wpctl` is the same power they already have. |
| Fake workspace state (impersonate helper) | Cosmetic only | Worst case: wrong buttons rendered. Buttons only ever emit `LeftAlt+NumN` **on physical touch** — socket state never triggers key emission, sysfs writes, or command execution. Invariant to preserve in code review: *state is render-input only; intents originate only from touch*. |
| Brightness flash | None via socket | Brightness is not in the protocol; sysfs fds are daemon-internal, driven by touch. |
| DoS with garbage | Bounded | 4096-byte line cap, 8-invalid-line budget, 200 msg/s cap -> disconnect; single-client policy (new valid connection replaces old, so a wedged attacker connection can't lock the real helper out... and an attacker replacing the real helper's connection only degrades to fallback `[1]` + inert slider); non-blocking I/O; parser runs as `nobody`. |
| Connect-flood | Bounded | `listen(1)` backlog + accept-rate cap (e.g. 10/s, excess closed immediately after peercred check). |
| Memory exhaustion | Bounded | Fixed read buffer per client (8 KB), ≤16 workspaces accepted, no unbounded collections. |
| Compromised helper -> daemon RCE | Minimized | Daemon parses with serde into fixed structs, no `unwrap` on socket path, and post-privdrop the process is `nobody:input,video` — the highest-value residual asset is the uinput fd, which the protocol never exposes. |

Cheap mitigations adopted: SO_PEERCRED uid pinning, uid-owned 0600 socket, bounded line/rate/error budgets, single-replaceable-client, latest-wins coalescing, and the render-only-state invariant. Not adopted (overkill for a single-user laptop): authentication tokens, seccomp on the helper, protocol encryption.

---

## 6. Future-proofing: claude presence + animation

- **Presence rides `state` as an optional field** — no version bump, no redesign, because of the must-ignore rule: `{"t":"state","ws":[...],"vol":{...},"claude":{"on":true}}`. Today's daemon ignores `claude` entirely; the future renderer session reads it into `HelperState`. The helper gains a third watcher task (whatever detects Claude sessions) feeding the same `Model` -> same debounced snapshot path. If richer critter data is ever needed (mood, activity), it nests under `claude` without touching `ws`/`vol`.
- **Liveness for the critter is already solved:** the 2 s heartbeat + 6 s staleness rule means "helper gone" can render the critter asleep, for free.
- **Animation ticks are not a protocol concern:** they will be a daemon-local timerfd in epoll (data=6, same pattern as everything else) driving `changed` flags on the middle region. The protocol deliberately carries *state*, never frames — so nothing here precludes or prescribes the animation work. Per the locked decision, none of this is built now; the only accommodation is the reserved optional field and the epoll slot numbering.

---

## 7. Ordered implementation steps (reviewable commits)

Daemon commits (D*) in `/home/ben/dev/projects/tiny-dfr`; helper commits (H*) in `/home/ben/archdots`. D1-D4 are pure additions compiling behind the existing binary; D5 is the renderer-coordination seam. Nothing goes live until the final Ben-approved deploy step.

| # | Commit | Contents | Validation (no live changes) |
|---|--------|----------|------------------------------|
| D1 | `Add helper protocol types and docs` | `docs/helper-protocol.md` (spec from section 1, single source of truth); `src/helper_proto.rs`: serde structs (`Hello`, `StateMsg`, `WsEntry`, `Vol`, `SetVolume`), line-splitting/validation with error budget, unit tests for: valid messages, unknown-field tolerance, unknown-type rejection, oversize line, non-finite floats, >16 ws truncation. Adds `serde_json` to Cargo.toml. | `cargo test && cargo build --release` |
| D2 | `Add helper socket listener (HelperLink)` | `src/helper_link.rs`: bind/unlink/chown/chmod (pre-privdrop constructor), single-client accept with SO_PEERCRED (`nix` +`socket` feature), non-blocking NDJSON read into `HelperState`, staleness clock (`is_fresh()`), `send_set_volume()` with 40 ms latest-wins; `HelperUid` config key in `src/config.rs` + default in `share/tiny-dfr/config.toml`. Unit tests: framing across partial reads, staleness math, client-replacement. | `cargo test`; manual scratch test: run the parser against a socketpair in tests (no root needed) |
| D3 | `Wire HelperLink into the event loop` | `real_main()`: construct pre-privdrop (main.rs:~1269, beside `BacklightManager`), epoll data=4/5, timeout cap while connected, feed `HelperState` + `is_fresh()` to the layer seam agreed with the renderer designer (fallback `[1]` when stale; volume echo-fight rule from 1.5). | `cargo test && cargo build --release`; behavior review with renderer designer's tests |
| D4 | `Open brightness sysfs fds pre-privdrop` | `backlight.rs` or new `sysfs.rs`: `Option<File>` for `intel_backlight/brightness` (max 17777) and `:white:kbd_backlight/brightness` (max 14660), opened before privdrop, clamped writes; consumed by the renderer designer's slider work. | `cargo test`; code review that open precedes `PrivDrop` |
| D5 | `Add RuntimeDirectory to tiny-dfr-ben service` | `scripts/install-ben-service.sh` heredoc: `RuntimeDirectory=tiny-dfr-ben`, `RuntimeDirectoryMode=0755`. No other unit changes; `ProtectHome=true` untouched. | `bash -n scripts/install-ben-service.sh`; diff review. **Deploy (running the installer + service restart) requires Ben's explicit approval.** |
| H1 | `Add tiny-dfr user-session helper` | `.config/tiny-dfr/helper/tiny_dfr_helper.py` (section 3); README update linking `docs/helper-protocol.md` in the tiny-dfr repo. | `python3 -m py_compile .config/tiny-dfr/helper/tiny_dfr_helper.py`; `python3 .config/tiny-dfr/helper/tiny_dfr_helper.py --print-state` (prints one derived snapshot from live niri/wpctl, no socket) |
| H2 | `Add tiny-dfr-helper user service` | `.config/systemd/user/tiny-dfr-helper.service` (section 4). | `systemd-analyze --user verify .config/systemd/user/tiny-dfr-helper.service` |
| H3 | `Add helper installer script` | `scripts/install-tiny-dfr-helper.sh` (stow + `systemctl --user enable --now`); touches only tiny-dfr paths; preserves the dirty `.config/niri/config.kdl`. | `bash -n scripts/install-tiny-dfr-helper.sh`; running it is user-level but still gets Ben's go-ahead |
| E2E | (no commit) | With Ben's approval: run D5 installer (restarts `tiny-dfr-ben`), run H3, then `socat - UNIX-CONNECT:/run/tiny-dfr-ben/helper.sock` sanity check is *not* possible (single-client would displace the helper) — instead verify via `journalctl --user -u tiny-dfr-helper -f` + `journalctl -u tiny-dfr-ben -f` and physically: switch workspaces, drag volume. | Ben-approved live validation only |

Sequencing notes: D1 must land before H1 (protocol doc is the contract). D2-D4 are independent of H1-H3, so daemon and helper tracks can proceed in parallel after D1. D3 depends on the renderer designer's region/fallback seam existing; if theirs lands later, D3 can stub `HelperState` consumption behind `is_fresh()` with the existing static config path as fallback (which is literally decision 7's behavior).

---

### Critical Files for Implementation

- /home/ben/dev/projects/tiny-dfr/src/main.rs (privdrop at 1275-1282, epoll loop at 1299-1311, timeout calc at 1348-1358 — HelperLink construction and wiring)
- /home/ben/dev/projects/tiny-dfr/src/config.rs (new `HelperUid` key, merge chain)
- /home/ben/dev/projects/tiny-dfr/scripts/install-ben-service.sh (service heredoc gains RuntimeDirectory; only sanctioned deploy path)
- /home/ben/archdots/.config/tiny-dfr/helper/tiny_dfr_helper.py (new — the entire user-session helper)
- /home/ben/archdots/.config/systemd/user/tiny-dfr-helper.service (new — session lifecycle for the helper)
