#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
plugin_prefix="$repo_root/plugins/"

if [ -n "${NEFOR_CARGO_METADATA_FILE:-}" ]; then
  [ -f "$NEFOR_CARGO_METADATA_FILE" ] || {
    echo "prepared Cargo metadata is missing: $NEFOR_CARGO_METADATA_FILE" >&2
    exit 1
  }
  metadata=(cat "$NEFOR_CARGO_METADATA_FILE")
else
  metadata=(cargo metadata --no-deps --format-version 1 --manifest-path "$repo_root/Cargo.toml")
fi

"${metadata[@]}" | jq -r --arg prefix "$plugin_prefix" '
      .packages[]
      | select(.manifest_path | startswith($prefix))
      | .targets[]
      | select(.kind | index("bin"))
      | .name
    ' \
  | LC_ALL=C sort
