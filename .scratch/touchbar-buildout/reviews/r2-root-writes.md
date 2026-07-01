# R2 adversarial root-writes review — 2026-07-02

Scope: commit `198570c` (slider widget + sysfs backends) plus the slider
paths in main.rs/config.rs. Verdict recorded here; fixes landed in the
follow-up hardening commit.

## Verdict

**SAFE TO INSTALL.** 0 BLOCKER, 1 FIX-NOW, 5 NICE-TO-HAVE. No panic path,
no below-floor display write, no privilege issue, no shell execution
reachable under any constructed hostile input. Notable verified-OK items:
raw_for cannot exceed max or wrap (float→int casts saturate); the 1%
display floor is hardcoded, not config-reachable; NaN cannot reach
state.value or sysfs; epoll timeout contributors all in [1, 60000].

## Findings and disposition (all fixed same day unless noted)

- **FIX-NOW**: drag aborted by drain (tap-outside/Fn-flip/dup-Down) left
  `SliderState.dragging` stranded true, blocking sync_sliders re-seeding
  until the next completed drag. → FIXED in drain_touches + dup-Down path.
- **NICE**: single global throttle pending slot could drop one drag's final
  value across targets; flush bypassed the rate clock. → FIXED: per-target
  pending array; flush counts against the clock.
- **NICE**: NaN min_fraction would become a NaN clamp bound → panic. →
  FIXED: finite-guard at construction.
- **NICE**: sync_sliders only ran on button-driven overlay transitions. →
  FIXED: also seeded at startup and after tap-outside/timeout closes.
- **NICE**: write_error_logged never reset on success. → FIXED.
- **NICE (pre-existing)**: empty control group panicked in
  ButtonSet::with_config — on an inotify reload that kills the bar until
  manual restart. → FIXED: empty groups dropped with a warning at load.
