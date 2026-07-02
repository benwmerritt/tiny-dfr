#!/usr/bin/env bash
set -euo pipefail

# Escape hatch: put the Touch Bar back on the last hardware-validated
# pre-critter build (tag ben-live/phase-c-live-strip: live workspace strip,
# anchored overlays, sliders — no Claude critters, no animation traffic).
#
# Usage:  sudo scripts/install-ben-safe.sh        (or the rtouchbar-safe alias)
#
# Un-wedges the USB display first if needed, then installs dist/tiny-dfr-ben-safe
# via the normal installer (same snapshots, same rollback paths). Rebuild the
# safe binary after a git clean with:
#   git worktree add /tmp/tb-safe ben-live/phase-c-live-strip
#   (cd /tmp/tb-safe && cargo build --release)
#   cp /tmp/tb-safe/target/release/tiny-dfr dist/tiny-dfr-ben-safe
#   git worktree remove --force /tmp/tb-safe
#
# The deeper fallback (stock upstream tiny-dfr) is unchanged:
#   sudo systemctl disable --now tiny-dfr-ben.service
#   sudo systemctl enable --now tiny-dfr.service

repo_root="${TINY_DFR_FORK_DIR:-/home/ben/dev/projects/tiny-dfr}"
safe_binary="$repo_root/dist/tiny-dfr-ben-safe"

if [[ ! -x "$safe_binary" ]]; then
  echo "Missing safe binary: $safe_binary" >&2
  echo "Rebuild it from tag ben-live/phase-c-live-strip (see comments in this script)." >&2
  exit 1
fi

TINY_DFR_BINARY="$safe_binary" exec "$repo_root/scripts/install-ben-service.sh"
