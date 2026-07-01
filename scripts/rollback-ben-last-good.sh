#!/usr/bin/env bash
set -euo pipefail

# Restore the last-good tiny-dfr-ben binary/config pair (snapshotted by
# install-ben-service.sh before each overwrite) and restart the service.
# Binary and config roll back together — they are a matched pair.
#
# For a full retreat to stock tiny-dfr instead:
#   sudo systemctl disable --now tiny-dfr-ben.service
#   sudo systemctl enable --now tiny-dfr.service

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  echo "Run as root: sudo $0" >&2
  exit 1
fi

if [[ ! -x /usr/local/bin/tiny-dfr-ben.last-good ]]; then
  echo "No last-good binary snapshot at /usr/local/bin/tiny-dfr-ben.last-good" >&2
  echo "Nothing to roll back to; use the stock tiny-dfr path instead." >&2
  exit 1
fi

install -o root -g root -m 0755 /usr/local/bin/tiny-dfr-ben.last-good /usr/local/bin/tiny-dfr-ben
if [[ -f /etc/tiny-dfr/config.toml.last-good ]]; then
  install -o root -g root -m 0644 /etc/tiny-dfr/config.toml.last-good /etc/tiny-dfr/config.toml
else
  echo "Warning: no config snapshot; keeping current /etc/tiny-dfr/config.toml" >&2
fi

systemctl restart tiny-dfr-ben.service
sleep 2
systemctl --no-pager --lines=30 status tiny-dfr-ben.service
