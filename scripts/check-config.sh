#!/usr/bin/env bash
set -euo pipefail

# Validate a tiny-dfr config.toml before it goes anywhere near the live bar.
# Checks: TOML parses; every OpenOverlay target names an existing ControlGroup;
# every Icon has a matching SVG/PNG in the share dir; MediaLayerKeys was not
# accidentally swallowed into the [ControlGroups] table scope.
#
# Usage: scripts/check-config.sh [config.toml] [share-dir]

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config_path="${1:-/home/ben/archdots/.config/tiny-dfr/config.toml}"
share_dir="${2:-$repo_root/share/tiny-dfr}"

exec python3 - "$config_path" "$share_dir" <<'PY'
import sys
import tomllib
from pathlib import Path

config_path = Path(sys.argv[1])
share_dir = Path(sys.argv[2])
errors = []

with config_path.open("rb") as f:
    data = tomllib.load(f)

groups = data.get("ControlGroups", {})
if not isinstance(groups, dict):
    errors.append(f"ControlGroups is {type(groups).__name__}, expected a table")
    groups = {}

# The TOML table-scope trap: keys written after [ControlGroups] land inside it.
for key in ("MediaLayerKeys", "FnLayerKeys", "PrimaryLayerKeys"):
    if key in groups:
        errors.append(
            f"{key} was parsed INSIDE [ControlGroups] — move [ControlGroups] to the end of the file"
        )

def buttons(where, items):
    if not isinstance(items, list):
        errors.append(f"{where} is not an array of buttons")
        return
    for i, b in enumerate(items):
        if not isinstance(b, dict):
            errors.append(f"{where}[{i}] is not a table")
            continue
        yield f"{where}[{i}]", b

def check_button(where, b):
    target = b.get("OpenOverlay")
    if target is not None and target not in groups:
        errors.append(f'{where}: OpenOverlay = "{target}" has no [ControlGroups] entry')
    icon = b.get("Icon")
    if icon is not None:
        if not any((share_dir / f"{icon}.{ext}").is_file() for ext in ("svg", "png")):
            errors.append(f'{where}: Icon = "{icon}" not found in {share_dir}')

for layer_key in ("MediaLayerKeys", "FnLayerKeys", "PrimaryLayerKeys"):
    if layer_key in data:
        for where, b in buttons(layer_key, data[layer_key]):
            check_button(where, b)

for name, items in groups.items():
    if not isinstance(items, list):
        errors.append(f"ControlGroups.{name} is not an array of buttons")
        continue
    for where, b in buttons(f"ControlGroups.{name}", items):
        check_button(where, b)

if errors:
    print(f"FAIL: {config_path}")
    for e in errors:
        print(f"  - {e}")
    sys.exit(1)

n_groups = len(groups)
print(f"OK: {config_path} parses; {n_groups} control group(s); overlay refs and icons resolve")
PY
