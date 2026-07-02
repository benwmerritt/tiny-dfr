# Pet Claude(s) on the Touch Bar

Labels: ready-for-agent (next phase after the live strip)

## Ben's vision (2026-07-02)

Little Claude critters living in the free middle region of the bar —
**one critter per Claude Code session running on this machine**, including
sessions inside tmux that Ben ssh'es into (they are all local processes).
"All the lil claudes running around the computer."

## Design sketch

- **Detection (helper)**: a third watcher task in tiny_dfr_helper.py scans
  for running Claude Code sessions (process scan for the claude CLI;
  distinguish interactive sessions from short-lived invocations — e.g.
  process age > a few seconds; tmux/ssh sessions are just local processes so
  they're covered automatically). Debounced like everything else.
- **Protocol**: rides the existing `state` message via the must-ignore seam,
  as a list so critters can be individually identified/animated:
  `"claude": {"sessions": [{"id": "<pid-or-hash>"}, ...]}`. Today's daemon
  ignores the field entirely — zero coordination needed to ship the helper
  side early.
- **Rendering (daemon)**: sprites in the middle free region (the layout has
  protected this space from day one). Animation tick = epoll timeout clamp,
  same pattern as pixel-shift; tick only while ≥1 critter exists and the bar
  is on. Sprite = small bitmap/vector frames; wander within the free span
  (strip end .. controls origin), never intercept touches (no hit spans).
- **States**: idle wander; a new session spawns a critter (walk in from the
  right?); session ends → critter walks off/vanishes; helper stale → sleep.
  Possible later: activity signal (session busy vs waiting) → run vs sit.

## Constraints

- No process execution in the daemon (detection is helper-side only).
- Render-only: critter state must never trigger keys/sysfs/intents.
- Animation must not hold the epoll loop hot when no critters/off.
