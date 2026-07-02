# R3 adversarial IPC review — 2026-07-02

Scope: commits 6ea2610..45c8686 (helper protocol types, socket link, grouped
strip layout, live strip + event-loop wiring, RuntimeDirectory) plus the
Python helper's intent execution. Verdict recorded here; fixes landed in the
follow-up hardening commits.

## Verdict

**SAFE TO INSTALL.** 0 BLOCKER, 1 FIX-NOW, 2 NICE-TO-HAVE, 6 LOW. Verified
empirically: no panic path from hostile NDJSON (serde_json rejects ±1e999 and
depth>128 well under the 4096B line cap; u64/idx extremes are typed errors);
memory/CPU bounded (LineBuffer hard cap, per-pump line budget, accept-drain,
no fd leaks); state→action isolation airtight (pushed state can release keys
early via structural drains but can never press keys, write sysfs, or send
intents — all intents trace to physical touches); the drain-before-bump
contract holds including the stale-Down/Up race; installer hardening intact.

## Findings and disposition (fixed same day unless noted)

- **FIX-NOW**: sanitize kept empty groups, so a zero-button-but-non-empty
  model broke classify's fixed point (structural rebuild + full drain on
  every heartbeat, persisting after the sender stops). Unreachable from the
  real helper. → FIXED: sanitize retains only non-empty groups AND
  rebuild_strip's fallback branch stores the model as applied (+ fixed-point
  regression test).
- **NICE**: structural rebuild rate bounded only by the 200 msg/s budget
  (hostile churn = drag DoS + CPU). → FIXED: 100ms minimum interval between
  structural applies, latest pending model wins, via the epoll timeout.
- **NICE**: strip_model stored untruncated, so changes in never-rendered
  trailing workspaces classified structural. → FIXED: models truncated to
  rendered terms (group-boundary-respecting) before classify and rebuild.
- **LOW**: send_intent could write to a pre-hello client. → FIXED: guard.
- **LOW**: cached state survives client replacement as fresh ≤6s. → KEPT by
  design (avoids fallback flash on helper restart); documented on drop_client.
- **LOW**: doc said 40ms volume rate; code is 50ms. → doc aligned to 50ms.
- **LOW**: helper focus_queue unbounded under wedged niri. → FIXED: cap 8,
  drop-oldest.
- **LOW**: helper float() on "1.0.0"-style regex match / bare dict indexing
  on niri events could kill the helper. → FIXED: try/except in read_volume
  and around niri event handling (skip event, never die).
