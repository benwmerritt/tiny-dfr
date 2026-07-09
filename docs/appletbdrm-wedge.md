# The appletbdrm Touch Bar wedge

Root-cause analysis of the failure that freezes the Touch Bar until a USB reset
or power-off (kernel spams `appletbdrm ... Failed to send message (-110)`).
Derived from reading the mainline driver source
(`drivers/gpu/drm/tiny/appletbdrm.c`, author Kerem Karabay) against the symptom
history in this repo. See also the incident post-mortems in `handoff/` and the
parked-critter throughput notes in `.scratch/claude-critter/parked-state.md`.

## What actually happens

The Touch Bar is a USB display (`05ac:8302`) reached over the T2 bridge
(`apple-bce`). Every framebuffer flush is a **blocking request→response USB
handshake**, not a fire-and-forget queue write:

- `DRM_IOCTL_MODE_DIRTYFB` → `drm_atomic_helper_dirtyfb` → a blocking atomic
  commit → `appletbdrm_primary_plane_helper_atomic_update` →
  `appletbdrm_flush_damage`.
- `appletbdrm_flush_damage` sends the damage frames on the OUT bulk endpoint
  (`appletbdrm_send_request`, `usb_bulk_msg`, **1000 ms** timeout) then reads an
  `UPDATE_COMPLETE` ack on the IN bulk endpoint (`appletbdrm_read_response`,
  same **1000 ms** timeout), in lock-step.
- `-110` is `-ETIMEDOUT` straight from `usb_bulk_msg`: the T2 didn't drain the
  write, or didn't ack, within 1 s. A wedged flush therefore burns up to ~2 s of
  uninterruptible D-state per ioctl.
- `Actual size (16) doesn't match expected size (40)` means the request/response
  byte stream has **desynchronized** — the driver is reading the wrong bytes off
  the bulk pipe (the 40-byte response struct read back as a stale 16-byte
  header).

## Why it's permanent (the driver bug)

The trigger is the T2 side occasionally missing the 1 s deadline — transport /
firmware fragility the driver can't prevent. What makes a transient timeout a
**permanent** wedge is that appletbdrm does **nothing to resynchronize** after
an error: no `usb_clear_halt`, no draining/discarding of a late response, no
reset, and no matching of responses against the timestamp/msg id the protocol
already carries. So a response that arrives after its deadline sits in the pipe;
the next flush reads that stale data, desyncs, and the stream never recovers.

Two consequences for userspace:

1. **DIRTYFB is already serialized by the kernel.** The commit is blocking and
   the DRM atomic machinery holds modeset locks, so two flushes from a single
   client cannot overlap. Adding our own "don't flush while one is outstanding"
   gate is redundant. (Rule that *does* matter: never drive DIRTYFB from more
   than one thread/fd at once.)
2. **No backpressure reaches userspace.** `atomic_update` is `void` and discards
   `appletbdrm_flush_damage`'s return value, so **DIRTYFB returns 0 even when the
   flush timed out.** We cannot detect `-110` from the ioctl — the only symptom
   is the call itself blocking for ~1–2 s.

## What tiny-dfr can and can't do

**Can't:** prevent the wedge, or detect it cleanly. It is not a rate ceiling or
an overlap bug we can close; it's a driver/firmware fault. This is why the
earlier 10 fps + merged-damage critter mitigation *still* wedged — a single
unlucky timeout is unrecoverable regardless of cadence.

**Can (probabilistic risk-reduction only):**

- **Cap the flush rate.** Each flush is an independent dice-roll on the 1 s
  timeout, so fewer round-trips = lower odds. `MIN_FLUSH_INTERVAL` in
  `src/main.rs` bounds flushes to ~30 Hz and coalesces everything that changed
  in between into one `dirty()`, collapsing a slider drag's per-libinput-event
  flush storm (100+/s) into ~30/s. Tune lower for more safety, higher for
  smoothness.
- **Keep per-flush damage tight.** Request size scales with damage area, and a
  bigger transfer takes the T2 longer to process — pushing the round-trip toward
  the 1 s deadline. We already emit tight per-button damage rects and avoid
  full-width repaints except on genuine full-redraw events (config reload, layer
  switch, pixel shift).

Both are odds-lowering, not fixes. Documented as such so nobody mistakes the
throttle for a cure.

## The real fix (kernel-side) — implemented 2026-07-09

A durable fix belongs in appletbdrm's flush path: on any `usb_bulk_msg` error,
resynchronize before the next flush (`usb_clear_halt` a stalled endpoint, drain
stale IN data) so a late ack can be discarded instead of desyncing the stream.
That turns the permanent wedge into a recoverable hiccup.

**This is now built and installed**, in a standalone repo at
`~/dev/projects/appletbdrm-fix` (a patched out-of-tree build of the exact
v7.0.12 driver + DKMS). It adds `appletbdrm_recover_endpoints()` — drain the
stale/late IN-endpoint data (and `usb_clear_halt` a stalled endpoint) after any
flush error so the next flush starts clean — plus a fix for a latent
timestamp-mismatch that returned success. Endpoint draining turned out to be the
load-bearing part; propagating the error to userspace is not reachable through
the void `atomic_update` DRM API, and the resync makes it unnecessary. Validated
on hardware 2026-07-09: every injected and one spontaneous `-110` recovered with
the bar staying live. Installed via DKMS so it survives kernel updates. See that
repo's README for the build/test/install and the kernel-update caveat (a future
`linux-t2` DRM-API bump would need the patch refreshed; the failure is graceful,
falling back to the stock module).

## References

- t2linux wiki #635 — appletbdrm `-110` / bind failures (no recovery patch):
  <https://github.com/t2linux/wiki/issues/635>
- omarchy #5862 — kernel ≥7.0.1 Touch Bar "wedged and unrecoverable without
  reboot" on resume; mitigation is module reload + USB re-enumerate (exactly the
  resync the driver lacks): <https://github.com/basecamp/omarchy/discussions/5862>
- dri-devel driver review (protocol structs, error strings):
  <https://www.mail-archive.com/dri-devel@lists.freedesktop.org/msg531440.html>
- AsahiLinux/tiny-dfr #33, #62 — related "stops responding" / "fails to start"
  reports: <https://github.com/AsahiLinux/tiny-dfr/issues/33>,
  <https://github.com/AsahiLinux/tiny-dfr/issues/62>
