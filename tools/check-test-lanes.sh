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
elif [ "$#" -eq 3 ] && [ "$1" = "--metadata" ]; then
  fixture_mode=1
  root_metadata="$2"
  live_metadata="$3"
else
  echo "usage: tools/check-test-lanes.sh [--metadata ROOT_JSON LIVE_JSON]" >&2
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
