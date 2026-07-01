#!/usr/bin/env bash
set -euo pipefail

# Install Ben's local tiny-dfr fork under a distinct binary/service name and
# switch the live Touch Bar daemon to it. Keep stock tiny-dfr installed and
# recoverable with:
#   sudo systemctl disable --now tiny-dfr-ben.service
#   sudo systemctl enable --now tiny-dfr.service

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  cat >&2 <<EOF
This installer must run as root because it installs /usr/local/bin and switches system services.

Run it from an interactive terminal with:
  sudo $0
EOF
  exit 1
fi

repo_root="${TINY_DFR_FORK_DIR:-/home/ben/dev/projects/tiny-dfr}"
archdots_root="${ARCHDOTS_DIR:-/home/ben/archdots}"
binary_src="$repo_root/target/release/tiny-dfr"
config_src="$archdots_root/.config/tiny-dfr/config.toml"

if [[ ! -x "$binary_src" ]]; then
  echo "Missing built binary: $binary_src" >&2
  echo "Run first: cd $repo_root && source \"$HOME/.cargo/env\" && cargo build --release" >&2
  exit 1
fi

if [[ ! -f "$config_src" ]]; then
  echo "Missing config: $config_src" >&2
  exit 1
fi

find_touchbar_drm_card() {
  local card driver
  for card in /sys/class/drm/card[0-9]*; do
    [[ -e "$card" ]] || continue
    driver="$card/device/driver"
    [[ -e "$driver" ]] || continue
    if [[ "$(basename "$(readlink -f "$driver")")" == "appletbdrm" ]]; then
      printf '/dev/dri/%s\n' "$(basename "$card")"
      return 0
    fi
  done
  return 1
}

if ! touchbar_card="$(find_touchbar_drm_card)"; then
  cat >&2 <<'EOF'
No appletbdrm Touch Bar DRM card is present.

Refusing to stop or switch services because tiny-dfr would otherwise scan the
normal i915 display card and fail with "Device or resource busy" while Niri owns it.
This usually means the Touch Bar USB display did not come back in DFR mode after
suspend/resume. Use the safe path: reboot and retry once /dev/dri/card* includes
an appletbdrm card. Do not force-switch USB config from this script.
EOF
  exit 1
fi

echo "Found Touch Bar DRM device: $touchbar_card"

config_backup=""

rollback_to_stock() {
  echo "Rolling back to stock tiny-dfr.service" >&2
  systemctl disable --now tiny-dfr-ben.service || true
  systemctl reset-failed tiny-dfr-ben.service tiny-dfr.service || true
  if [[ -n "$config_backup" && -f "$config_backup" ]]; then
    echo "Restoring previous /etc/tiny-dfr/config.toml from $config_backup" >&2
    install -o root -g root -m 0644 "$config_backup" /etc/tiny-dfr/config.toml
  fi
  systemctl enable --now tiny-dfr.service
}

install -o root -g root -m 0755 "$binary_src" /usr/local/bin/tiny-dfr-ben
install -d -m 0755 /etc/tiny-dfr

if [[ -e /etc/tiny-dfr/config.toml ]] && ! cmp -s "$config_src" /etc/tiny-dfr/config.toml; then
  config_backup="/etc/tiny-dfr/config.toml.bak.$(date +%Y%m%d-%H%M%S)"
  cp -a /etc/tiny-dfr/config.toml "$config_backup"
fi
install -o root -g root -m 0644 "$config_src" /etc/tiny-dfr/config.toml

cat >/etc/systemd/system/tiny-dfr-ben.service <<'EOF'
[Unit]
Description=Tiny Apple T2 Mac Touch Bar daemon (Ben fork)
After=graphical.target systemd-logind.service dev-tiny_dfr_backlight.device dev-tiny_dfr_display_backlight.device
Wants=dev-tiny_dfr_backlight.device dev-tiny_dfr_display_backlight.device

[Service]
ExecStart=/usr/local/bin/tiny-dfr-ben
Restart=always

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateIPC=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=strict
RestrictAddressFamilies=AF_UNIX AF_NETLINK
RestrictNamespaces=true
RestrictSUIDSGID=true

[Install]
WantedBy=graphical.target
EOF

systemctl daemon-reload
systemctl reset-failed tiny-dfr-ben.service tiny-dfr.service || true

# tiny-dfr owns the Touch Bar DRM/input/uinput devices, so only one instance
# should run at a time.
if ! systemctl stop tiny-dfr.service; then
  echo "Failed to stop stock tiny-dfr.service; aborting without starting fork." >&2
  rollback_to_stock
  exit 1
fi

if ! systemctl start tiny-dfr-ben.service; then
  echo "Failed to start tiny-dfr-ben.service." >&2
  rollback_to_stock
  exit 1
fi
sleep 2

if systemctl is-active --quiet tiny-dfr-ben.service; then
  systemctl disable tiny-dfr.service
  systemctl enable tiny-dfr-ben.service
  echo "tiny-dfr-ben is active. Stock tiny-dfr is disabled but still installed for rollback."
else
  echo "tiny-dfr-ben failed to become active." >&2
  systemctl --no-pager --lines=50 status tiny-dfr-ben.service || true
  rollback_to_stock
  if ! systemctl is-active --quiet tiny-dfr.service; then
    echo "Stock tiny-dfr did not become active either; a reboot is the safest recovery." >&2
    systemctl --no-pager --lines=50 status tiny-dfr.service || true
  fi
  exit 1
fi

systemctl --no-pager --lines=30 status tiny-dfr-ben.service
systemctl --no-pager --lines=8 status tiny-dfr.service || true
