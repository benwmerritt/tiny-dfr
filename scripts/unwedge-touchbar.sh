#!/usr/bin/env bash
set -euo pipefail

# Recover the Touch Bar display from an appletbdrm USB wedge without a reboot.
#
# Failure mode: sustained small-rect damage traffic can wedge the Touch Bar's
# USB display channel. The kernel then streams
#   appletbdrm ... [drm] *ERROR* Failed to send message (-110)
# and the daemon sits in uninterruptible D-state on the stuck DRM ioctl, so
# systemctl stop/restart hangs.
#
# Fix: deauthorize/reauthorize the 05ac:8302 device — the software equivalent
# of unplugging and replugging it. That cancels the stuck transfer (freeing
# the daemon) and re-enumerates the display. This is NOT the forbidden
# bConfigurationValue force-switch; the config stays untouched.
#
# Safe to run when in doubt: with no wedge detected it exits without touching
# anything (pass --force to reset regardless).

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  echo "Must run as root: sudo $0" >&2
  exit 1
fi

force=0
[[ "${1:-}" == "--force" ]] && force=1

find_touchbar_usb_dev() {
  local dev
  for dev in /sys/bus/usb/devices/*; do
    [[ -f "$dev/idVendor" && -f "$dev/idProduct" ]] || continue
    if [[ "$(cat "$dev/idVendor")" == "05ac" && "$(cat "$dev/idProduct")" == "8302" ]]; then
      printf '%s\n' "$dev"
      return 0
    fi
  done
  return 1
}

daemon_wedged() {
  local pid state
  pid="$(systemctl show -p MainPID --value tiny-dfr-ben.service 2>/dev/null || true)"
  [[ -n "$pid" && "$pid" != 0 ]] || return 1
  state="$(awk '{print $3}' "/proc/$pid/stat" 2>/dev/null || true)"
  [[ "$state" == D* ]]
}

kernel_erroring() {
  journalctl -k --no-pager --since "-2 minutes" 2>/dev/null \
    | grep -q 'appletbdrm.*ERROR' || return 1
}

if ! usb_dev="$(find_touchbar_usb_dev)"; then
  echo "Touch Bar USB device 05ac:8302 not found on the bus; nothing to reset." >&2
  echo "If the bar is dead a reboot is the remaining recovery." >&2
  exit 1
fi

if [[ $force -eq 0 ]] && ! daemon_wedged && ! kernel_erroring; then
  echo "No wedge detected (daemon not in D-state, no recent appletbdrm errors)."
  echo "Nothing done. Use --force to reset anyway."
  exit 0
fi

echo "Resetting Touch Bar display at $usb_dev (deauthorize/reauthorize)..."
echo 0 > "$usb_dev/authorized"
sleep 3
echo 1 > "$usb_dev/authorized"

# Wait for appletbdrm to rebind so a following service (re)start finds the card.
for _ in $(seq 1 10); do
  sleep 1
  for card in /sys/class/drm/card[0-9]*; do
    [[ -e "$card/device/driver" ]] || continue
    if [[ "$(basename "$(readlink -f "$card/device/driver")")" == "appletbdrm" ]]; then
      echo "Touch Bar display re-enumerated as /dev/dri/$(basename "$card")."
      exit 0
    fi
  done
done

echo "Device reset but appletbdrm did not rebind within 10s; a reboot may still be needed." >&2
exit 1
