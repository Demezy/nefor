#!/usr/bin/env bash

set -u

if [ "${NEFOR_PREPARED_TESTS:-false}" = "true" ]; then
  repo=$(cd "$(dirname "$0")/../.." && pwd)
  target="${CARGO_TARGET_DIR:-$repo/target}"
  if [[ "$target" != /* ]]; then target="$repo/$target"; fi
  "$target/debug/nefor"
else
  just run
fi
status=$?
printf '\nNEFOR_EXIT_CODE=%s\n' "$status"
exit "$status"
