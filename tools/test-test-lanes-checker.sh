#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$repo/tmp"
scratch="$(mktemp -d "$repo/tmp/test-lane-checker.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

cargo metadata --no-deps --format-version 1 --manifest-path "$repo/Cargo.toml" >"$scratch/root.json"
cargo metadata --no-deps --format-version 1 --manifest-path "$repo/tests/live/Cargo.toml" >"$scratch/live.json"

expect_failure() {
  name="$1"
  root="$2"
  live="$3"
  if "$repo/tools/check-test-lanes.sh" --metadata "$root" "$live" >"$scratch/$name.out" 2>&1; then
    echo "negative checker fixture unexpectedly passed: $name" >&2
    exit 1
  fi
}

jq 'del(.packages[0])' "$scratch/root.json" >"$scratch/missing-package.json"
expect_failure missing-package "$scratch/missing-package.json" "$scratch/live.json"

jq '(.packages[].targets[] | select((.["required-features"] // []) == ["full-tests"]) | .["required-features"]) = ["unknown-tests"]' "$scratch/root.json" >"$scratch/unknown-lane.json"
expect_failure unknown-lane "$scratch/unknown-lane.json" "$scratch/live.json"

jq '.workspace_default_members = []' "$scratch/root.json" >"$scratch/default-member-gap.json"
expect_failure default-member-gap "$scratch/default-member-gap.json" "$scratch/live.json"

jq '.packages = []' "$scratch/live.json" >"$scratch/missing-live.json"
expect_failure missing-live "$scratch/root.json" "$scratch/missing-live.json"

jq '(.packages[0].targets[] | select(.test == true) | .["required-features"]) = ["hidden-live"]' "$scratch/live.json" >"$scratch/gated-live.json"
expect_failure gated-live "$scratch/root.json" "$scratch/gated-live.json"

echo "test-lane checker rejected all negative fixtures"
