#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
registry="$repo/tools/test-lanes.json"
lane="${1:-}"

case "$lane" in
  default|full) ;;
  *) echo "usage: tools/run-separate-tests.sh default|full" >&2; exit 2 ;;
esac

while IFS= read -r package; do
  cargo_args=(test -p "$package" --locked)
  test_args=()
  if [ "$lane" = "full" ]; then
    cargo_args+=(--features "$package/full-tests")
    while IFS= read -r test_arg; do
      test_args+=("$test_arg")
    done < <(jq -r --arg package "$package" \
      '.cargo_full.separate[] | select(.package == $package) | .test_args[]' "$registry")
  fi
  if [ "${NEFOR_TEST_PLAN_DRY_RUN:-0}" = "1" ]; then
    printf 'separate-test lane=%s package=%s feature=%s args=%s\n' \
      "$lane" "$package" "$([ "$lane" = "full" ] && printf '%s/full-tests' "$package" || printf none)" "${test_args[*]:-none}"
    continue
  fi
  cargo "${cargo_args[@]}" -- "${test_args[@]}"
done < <(jq -r '.cargo_full.separate[].package' "$registry")
