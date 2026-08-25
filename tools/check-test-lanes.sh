#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
registry="$repo/tools/test-lanes.json"
fixture_mode=0

if [ "$#" -eq 0 ]; then
  mkdir -p "$repo/tmp"
  scratch="$(mktemp -d "$repo/tmp/test-lanes.XXXXXX")"
  trap 'rm -rf "$scratch"' EXIT
  root_metadata="$scratch/root.json"
  live_metadata="$scratch/live.json"
  cargo metadata --no-deps --format-version 1 --manifest-path "$repo/Cargo.toml" >"$root_metadata"
  cargo metadata --no-deps --format-version 1 --manifest-path "$repo/$(jq -r .live_manifest "$registry")" >"$live_metadata"
elif { [ "$#" -eq 3 ] || [ "$#" -eq 4 ]; } && [ "$1" = "--metadata" ]; then
  fixture_mode=1
  root_metadata="$2"
  live_metadata="$3"
  if [ "$#" -eq 4 ]; then registry="$4"; fi
else
  echo "usage: tools/check-test-lanes.sh [--metadata ROOT_JSON LIVE_JSON [REGISTRY_JSON]]" >&2
  exit 2
fi

fail() {
  echo "test-lane completeness error: $*" >&2
  exit 1
}

jq -e '
  ([.packages[].id] | sort) == (.workspace_members | sort)
' "$root_metadata" >/dev/null || fail "workspace member package missing from classification input"

jq -e '
  (.workspace_default_members | sort) == (.workspace_members | sort)
' "$root_metadata" >/dev/null || fail "workspace default members differ from workspace members"

unknown_root="$(jq -r '
  [.packages[] as $package
   | $package.targets[]
   | select(.test or .doctest)
   | select(((.["required-features"] // []) != []) and ((.["required-features"] // []) != ["full-tests"]))
   | "\($package.name)::\(.name) requires \((.["required-features"] // []) | tojson)"]
  | .[]?
' "$root_metadata")"
[ -z "$unknown_root" ] || fail "unknown or compound lane requirement: $unknown_root"

jq -e '
  (.cargo_full.harness_packages | type == "array" and all(type == "string"))
  and ((.cargo_full.harness_packages | length) == (.cargo_full.harness_packages | unique | length))
  and (.cargo_full.separate | type == "array")
  and (.cargo_full.separate | all(
    (.package | type == "string")
    and (.test_args == ["--test-threads=1"])
  ))
  and ((.cargo_full.separate | map(.package) | length) == (.cargo_full.separate | map(.package) | unique | length))
  and (([.cargo_full.harness_packages[], .cargo_full.separate[].package] | length)
    == ([.cargo_full.harness_packages[], .cargo_full.separate[].package] | unique | length))
' "$registry" >/dev/null || fail "invalid, duplicate, or non-serial Cargo full execution membership"

metadata_full_packages="$(jq -c '
  [.packages[]
   | select(any(.targets[]; (.test or .doctest) and ((.["required-features"] // []) == ["full-tests"])))
   | .name] | unique | sort
' "$root_metadata")"
planned_full_packages="$(jq -c '
  [.cargo_full.harness_packages[], .cargo_full.separate[].package] | unique | sort
' "$registry")"
[ "$planned_full_packages" = "$metadata_full_packages" ] || fail "Cargo full execution packages do not exactly match metadata: planned=$planned_full_packages metadata=$metadata_full_packages"

jq -e '
  ([.packages[].id] | sort) == (.workspace_members | sort)
  and ((.workspace_default_members | sort) == (.workspace_members | sort))
  and (.workspace_members | length == 1)
' "$live_metadata" >/dev/null || fail "missing or inconsistent live inventory"

jq -e '
  [.packages[].targets[] | select(.test or .doctest)] as $targets
  | ($targets | length > 0)
  and ([$targets[] | select((.["required-features"] // []) != [])] | length == 0)
' "$live_metadata" >/dev/null || fail "feature-gated or empty live target graph"

jq -e '
  (.live_manifest | type == "string")
  and (.aggregates.default == "test-default")
  and (.aggregates.full == "test-full")
  and (.aggregates.live == "test-live")
  and (.non_cargo | map(.lane) | all(. == "default" or . == "full"))
  and ((.non_cargo | map(.recipe) | length) == (.non_cargo | map(.recipe) | unique | length))
' "$registry" >/dev/null || fail "invalid checked command registry"

discovered_entrypoints="$(cd "$repo" && rg --files tools \
  | rg '^tools/(check-[^/]+\.(sh|ts)|test-[^/]+\.sh|.+\.test\.ts)$' \
  | sort)"
registered_entrypoints="$(jq -r '.non_cargo[].entrypoint // empty' "$registry" | sort)"
[ "$registered_entrypoints" = "$discovered_entrypoints" ] || fail "non-Cargo verification entrypoints are not registered exactly: registered=[$registered_entrypoints] discovered=[$discovered_entrypoints]"

if [ "$fixture_mode" -eq 0 ]; then
  summary="$(cd "$repo" && just --summary)"
  while IFS= read -r recipe; do
    case " $summary " in
      *" $recipe "*) ;;
      *) fail "registered non-Cargo recipe is missing: $recipe" ;;
    esac
  done < <(jq -r '.non_cargo[].recipe, .aggregates[]' "$registry")

  default_recipe="$(jq -r .aggregates.default "$registry")"
  full_recipe="$(jq -r .aggregates.full "$registry")"
  default_definition="$(cd "$repo" && just --show "$default_recipe")"
  full_definition="$(cd "$repo" && just --show "$full_recipe")"
  [[ "$default_definition" == *'cargo run --quiet -p nefor-cargo-test-harness -- --lane default'* ]] || fail "$default_recipe does not reach the metadata-driven default harness"
  [[ "$full_definition" == *'cargo run --quiet -p nefor-cargo-test-harness -- --lane full'* ]] || fail "$full_recipe does not reach the metadata-driven full harness"
  [[ "$default_definition" == *'tools/run-separate-tests.sh default'* ]] || fail "$default_recipe omits separately owned default Cargo packages"
  [[ "$full_definition" == *'tools/run-separate-tests.sh full'* ]] || fail "$full_recipe omits separately serialized full Cargo packages"
  harness_source="$repo/tools/cargo-test-harness/src/main.rs"
  rg -q 'load_full_execution_plan' "$harness_source" || fail "Cargo harness does not consume the authoritative full execution plan"
  if rg -q 'FULL_FEATURES' "$harness_source"; then fail "Cargo harness duplicates full feature membership outside the registry"; fi
  rg -q '\.cargo_full\.separate' "$repo/tools/run-separate-tests.sh" || fail "separate Cargo runner does not consume the authoritative plan"
  while IFS=$'\t' read -r recipe lane; do
    if [ "$lane" = "default" ]; then
      [[ " $default_definition " == *" $recipe"* ]] || fail "$recipe is not reachable from $default_recipe"
    fi
    [[ " $full_definition " == *" $recipe"* ]] || fail "$recipe is not reachable from $full_recipe"
  done < <(jq -r '.non_cargo[] | [.recipe, .lane] | @tsv' "$registry")

  ignored="$(rg -n '^[[:space:]]*#\[ignore' "$repo" --glob '*.rs' --glob '!target/**' --glob '!tmp/**' --glob '!.worktrees/**' || true)"
  [ -z "$ignored" ] || fail "#[ignore] cannot encode lane membership: $ignored"
  hidden="$(rg -n 'cfg\s*\(\s*feature\s*=\s*"[^"]*-tests"' "$repo" --glob '*.rs' --glob '!target/**' --glob '!tmp/**' --glob '!.worktrees/**' || true)"
  [ -z "$hidden" ] || fail "function-level test-lane cfg bypasses Cargo metadata: $hidden"

  ci="$repo/.github/workflows/ci.yml"
  rg -q 'just check-test-lanes' "$ci" || fail "CI does not run the lane completeness checker"
  rg -q 'just test-default' "$ci" || fail "CI does not run the deterministic default lane"
  rg -q 'just test-full' "$ci" || fail "CI does not run the deterministic full lane"
fi

default_count="$(jq '[.packages[].targets[] | select(.test or .doctest) | select((.["required-features"] // []) == [])] | length' "$root_metadata")"
full_count="$(jq '[.packages[].targets[] | select(.test or .doctest) | select((.["required-features"] // []) == ["full-tests"])] | length' "$root_metadata")"
live_count="$(jq '[.packages[].targets[] | select(.test or .doctest)] | length' "$live_metadata")"
echo "test-lane inventory complete: default=$default_count full=$full_count live=$live_count"
