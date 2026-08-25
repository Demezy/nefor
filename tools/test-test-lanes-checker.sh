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
  registry="${4:-$repo/tools/test-lanes.json}"
  expected="${5:-test-lane completeness error}"
  if "$repo/tools/check-test-lanes.sh" --metadata "$root" "$live" "$registry" >"$scratch/$name.out" 2>&1; then
    echo "negative checker fixture unexpectedly passed: $name" >&2
    exit 1
  fi
  rg -q "$expected" "$scratch/$name.out" || {
    echo "negative checker fixture had no actionable '$expected' diagnostic: $name" >&2
    cat "$scratch/$name.out" >&2
    exit 1
  }
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

jq 'del(.cargo_full.harness_packages[0])' "$repo/tools/test-lanes.json" >"$scratch/missing-harness-package.json"
expect_failure missing-harness-package "$scratch/root.json" "$scratch/live.json" "$scratch/missing-harness-package.json" "Cargo full execution packages do not exactly match metadata"

jq '.cargo_full.separate |= map(select(.package != "nefor-tui"))' "$repo/tools/test-lanes.json" >"$scratch/missing-tui-execution.json"
expect_failure missing-tui-execution "$scratch/root.json" "$scratch/live.json" "$scratch/missing-tui-execution.json" "Cargo full execution packages do not exactly match metadata"

jq '.non_cargo |= map(select(.recipe != "test-test-lanes-checker"))' "$repo/tools/test-lanes.json" >"$scratch/unregistered-verification.json"
expect_failure unregistered-verification "$scratch/root.json" "$scratch/live.json" "$scratch/unregistered-verification.json" "non-Cargo verification entrypoints are not registered exactly"

NEFOR_TEST_PLAN_DRY_RUN=1 "$repo/tools/run-separate-tests.sh" default >"$scratch/separate-default.out"
NEFOR_TEST_PLAN_DRY_RUN=1 "$repo/tools/run-separate-tests.sh" full >"$scratch/separate-full.out"
rg -q '^separate-test lane=default package=nefor-tui feature=none args=none$' "$scratch/separate-default.out"
rg -q '^separate-test lane=full package=nefor-tui feature=nefor-tui/full-tests args=--test-threads=1$' "$scratch/separate-full.out"

echo "test-lane checker rejected all negative fixtures"
