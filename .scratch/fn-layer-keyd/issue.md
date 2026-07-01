# Fn layer unreachable: keyd virtual keyboard cannot emit KEY_FN

Labels: ready-for-agent (deprioritized by Ben, 2026-07-02)

## Symptom

Holding Fn never shows the F-key layer on the Touch Bar. Not a regression —
this has been broken for as long as keyd has grabbed `*`.

## Root cause (verified 2026-07-02)

- `/etc/keyd/default.conf` has `[ids] *`: keyd exclusively grabs the Apple
  internal keyboard and re-emits through its virtual keyboard.
- The keyd virtual keyboard's capability bitmap does **not** include bit 464
  (KEY_FN), verified via `/proc/bus/input/devices`. It physically cannot
  forward Fn presses.
- tiny-dfr hardcodes `Key::Fn` for layer switching (main.rs keyboard event
  handler), so it never sees a layer-switch key. `DoublePressSwitchLayers`
  is equally dead.

## Fix sketch (when wanted)

1. keyd: map `fn = f20` (or another spare key keyd can emit) in
   `[main]` of `/etc/keyd/default.conf` (archdots `etc/keyd/default.conf`),
   then `sudo systemctl restart keyd`.
2. tiny-dfr: add a config key (e.g. `LayerSwitchKey = "F20"`) accepted in
   addition to `Key::Fn` in the keyboard event handler.
3. Retest hold-to-show and double-press-to-swap on hardware.

## Decision

Ben chose "log it, move on" — F-keys are not part of the target bar design.
