use nefor_mag::profile::{CompileProfile, CompileProfiler};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nefor-mag-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn profile(root: &Path, source: &str) -> CompileProfile {
    fs::write(root.join("main.mag"), source).expect("entry");
    let profiler = CompileProfiler::new();
    nefor_mag::load_with_profiler(
        root,
        "main.mag",
        json!({}),
        &[root.to_path_buf()],
        &profiler,
    )
    .expect("profiled load");
    profiler.snapshot()
}

#[test]
fn profiled_load_reports_phases_and_deterministic_work() {
    let root = temp_dir("profile");
    fs::write(
        root.join("library.mag"),
        "(let copy (fn [[value Int]] -> Int value))",
    )
    .expect("library");
    let source = "(require \"library\")\n(artifact {:value (library.copy 7)})";
    let first = profile(&root, source);
    let second = profile(&root, source);

    assert_eq!(first.counters, second.counters);
    assert_eq!(first.counters.module_requests, 1);
    assert_eq!(first.counters.modules_loaded, 1);
    assert_eq!(first.counters.module_cache_hits, 0);
    assert!(first.counters.evaluator_steps > 0);
    assert!(first.counters.checked_bindings > 0);
    assert!(first.counters.checked_expressions > 0);
    assert!(first.counters.environment_snapshots > 0);
    assert!(first.phases.entry_read_ns > 0);
    assert!(first.phases.entry_lex_ns > 0);
    assert!(first.phases.entry_parse_ns > 0);
    assert!(first.phases.entry_evaluate_ns > 0);
    assert!(first.phases.module_resolve_ns > 0);
    assert!(first.phases.module_read_ns > 0);
    assert!(first.phases.module_lex_ns > 0);
    assert!(first.phases.module_parse_ns > 0);
    assert!(first.phases.module_evaluate_ns > 0);
    assert!(first.phases.checking_ns > 0);
    assert!(first.phases.artifact_serialize_hash_ns > 0);
    fs::remove_dir_all(root).ok();
}

#[test]
fn counters_partition_calls_builtins_and_binding_forces() {
    let root = temp_dir("profile-partitions");
    let profile = profile(&root, "(let identity (fn [[value Int]] -> Int value))\n(let answer (identity 7))\n(artifact {:answer answer})");
    let counters = profile.counters;

    assert_eq!(
        counters.function_calls,
        counters.user_function_calls + counters.builtin_calls
    );
    assert_eq!(
        counters.builtin_calls,
        counters
            .builtin_calls_by_name
            .values()
            .copied()
            .sum::<u64>()
    );
    assert_eq!(counters.binding_slots_declared, 2);
    assert_eq!(counters.binding_force_initializations, 2);
    assert!(counters.binding_force_ready_hits >= 2);
    assert_eq!(counters.binding_force_cycles, 0);
    assert_eq!(counters.builtin_calls_by_name.get("artifact"), Some(&1));
    assert_eq!(counters.user_function_calls, 1);
    fs::remove_dir_all(root).ok();
}

#[test]
fn builtin_item_work_and_recursive_validation_are_exact_for_tiny_fixture() {
    let root = temp_dir("profile-items");
    let profile = profile(&root, "(let identity (fn [[value (List Int)]] -> (List Int) value))\n(let values (identity (as (List Int) (concat [1 2] [3]))))\n(artifact {:count (count values)})");
    let counters = profile.counters;

    assert_eq!(counters.builtin_calls_by_name.get("concat"), Some(&1));
    assert_eq!(counters.builtin_input_items_by_name.get("concat"), Some(&3));
    assert_eq!(counters.builtin_calls_by_name.get("count"), Some(&1));
    assert_eq!(counters.builtin_calls_by_name.get("artifact"), Some(&1));
    assert!(counters.runtime_value_validation_visits > 0);
    fs::remove_dir_all(root).ok();
}
