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
    nefor_mag::compile_file_with_profiler(
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
fn profiled_file_compile_reports_phases_and_deterministic_work() {
    let root = temp_dir("profile");
    fs::write(
        root.join("library.mag"),
        "let copy: fn(Int) -> Int = |value| => value",
    )
    .expect("library");
    let source =
        "import library.{}\ntype Result {value: Int}\nartifact(Result {value: library.copy(7)})";
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
    assert!(first.total_duration_ns > 0);
    assert!(first.total_duration_ns >= first.phases.entry_evaluate_ns);
    assert!(first.phases.entry_evaluate_ns >= first.phases.module_evaluate_ns);
    fs::remove_dir_all(root).ok();
}

#[test]
fn failed_in_memory_compile_preserves_error_and_profile() {
    let root = temp_dir("compile-failure");
    let roots = [root.clone()];
    let source = "artifact(1 + \"x\")";
    let ordinary_error =
        nefor_mag::compile_with_inputs_and_module_roots(source, &root, json!({}), &roots)
            .expect_err("fixture must fail without profiling");
    let profiler = CompileProfiler::new();
    let error = nefor_mag::CompilerSession::new()
        .compile_with_profiler(
            nefor_mag::CompileRequest {
                source,
                source_dir: &root,
                inputs: json!({}),
                module_roots: &roots,
                options: nefor_mag::CompilerOptions::default(),
            },
            &profiler,
        )
        .expect_err("fixture must fail");
    let profile = profiler.snapshot();

    assert_eq!(
        std::mem::discriminant(&error),
        std::mem::discriminant(&ordinary_error)
    );
    assert_eq!(error.to_string(), ordinary_error.to_string());
    assert!(profile.total_duration_ns > 0);
    assert!(profile.phases.entry_evaluate_ns > 0);
    assert!(profile.phases.checking_ns > 0);
    fs::remove_dir_all(root).ok();
}

#[test]
fn failed_file_compile_records_total_and_every_started_entry_phase() {
    let cases = [
        (
            "missing",
            None,
            "missing.mag",
            nefor_mag::SyntaxMode::New,
            "entry_read_ns",
        ),
        (
            "new-lex",
            Some("[λ]"),
            "main.mag",
            nefor_mag::SyntaxMode::New,
            "entry_lex_ns",
        ),
        (
            "new-parse",
            Some("let = 1"),
            "main.mag",
            nefor_mag::SyntaxMode::New,
            "entry_parse_ns",
        ),
        (
            "lex",
            Some("[λ]"),
            "main.magl",
            nefor_mag::SyntaxMode::Lisp,
            "entry_lex_ns",
        ),
        (
            "parse",
            Some("(artifact"),
            "main.magl",
            nefor_mag::SyntaxMode::Lisp,
            "entry_parse_ns",
        ),
        (
            "checking",
            Some("artifact(1 + \"x\")"),
            "main.mag",
            nefor_mag::SyntaxMode::New,
            "checking_ns",
        ),
        (
            "evaluate",
            Some(
                "type Failure {kind: String, message: String}\nfail(Failure {kind: \"test\", message: \"stop\"})",
            ),
            "main.mag",
            nefor_mag::SyntaxMode::New,
            "entry_evaluate_ns",
        ),
        (
            "artifact",
            Some("let value = 1"),
            "main.mag",
            nefor_mag::SyntaxMode::New,
            "artifact_conversion_ns",
        ),
    ];

    for (label, source, entry, syntax, expected_phase) in cases {
        let root = temp_dir(label);
        if let Some(source) = source {
            fs::write(root.join(entry), source).expect("entry");
        }
        let profiler = CompileProfiler::new();
        let error = nefor_mag::compile_file_with_profiler_and_options_and_syntax(
            &root,
            entry,
            json!({}),
            std::slice::from_ref(&root),
            &profiler,
            nefor_mag::CompilerOptions::default(),
            syntax,
        )
        .expect_err("fixture must fail");
        let profile = profiler.snapshot();
        let phases = serde_json::to_value(&profile.phases).expect("phase json");

        assert!(profile.total_duration_ns > 0, "{label}: {error}");
        assert!(
            phases[expected_phase]
                .as_u64()
                .is_some_and(|value| value > 0),
            "{label}: phase {expected_phase} was not recorded after {error}"
        );
        fs::remove_dir_all(root).ok();
    }
}

#[test]
fn failed_module_work_records_started_nested_phases() {
    let cases = [
        ("resolve", None, "support.mag", "module_resolve_ns"),
        ("new-lex", Some("[λ]"), "support.mag", "module_lex_ns"),
        (
            "new-parse",
            Some("let = 1"),
            "support.mag",
            "module_parse_ns",
        ),
        ("lex", Some("[λ]"), "support.magl", "module_lex_ns"),
        (
            "parse",
            Some("(let broken"),
            "support.magl",
            "module_parse_ns",
        ),
        (
            "evaluate",
            Some("let bad = 1 + \"x\""),
            "support.mag",
            "module_evaluate_ns",
        ),
    ];

    for (label, module_source, module_entry, expected_phase) in cases {
        let root = temp_dir(&format!("module-{label}"));
        fs::write(root.join("main.mag"), "import support.{}\nartifact(())").expect("entry");
        if let Some(source) = module_source {
            fs::write(root.join(module_entry), source).expect("module");
        }
        let profiler = CompileProfiler::new();
        let error = nefor_mag::compile_file_with_profiler(
            &root,
            "main.mag",
            json!({}),
            std::slice::from_ref(&root),
            &profiler,
        )
        .expect_err("fixture must fail");
        let profile = profiler.snapshot();
        let phases = serde_json::to_value(&profile.phases).expect("phase json");

        assert!(profile.total_duration_ns > 0, "{label}: {error}");
        assert!(profile.phases.entry_evaluate_ns > 0, "{label}: {error}");
        assert!(
            phases[expected_phase]
                .as_u64()
                .is_some_and(|value| value > 0),
            "{label}: phase {expected_phase} was not recorded after {error}"
        );
        fs::remove_dir_all(root).ok();
    }
}

#[test]
fn module_cache_hits_mean_reuse_within_one_program() {
    let root = temp_dir("module-cache-scope");
    fs::write(root.join("support.mag"), "let value = 7").expect("module");
    fs::write(
        root.join("left.mag"),
        "import support.{}\nlet value = support.value",
    )
    .expect("left module");
    fs::write(
        root.join("right.mag"),
        "import support.{}\nlet value = support.value",
    )
    .expect("right module");
    let profile = profile(
        &root,
        "import left.{}\nimport right.{}\nartifact([left.value, right.value])",
    );

    assert_eq!(profile.counters.module_requests, 4);
    assert_eq!(profile.counters.modules_loaded, 3);
    assert_eq!(profile.counters.module_cache_hits, 1);
    fs::remove_dir_all(root).ok();
}

#[test]
fn counters_partition_calls_builtins_and_binding_forces() {
    let root = temp_dir("profile-partitions");
    let profile = profile(&root, "type Answer {answer: Int}\nlet identity: fn(Int) -> Int = |value| => value\nlet answer = identity(7)\nartifact(Answer {answer: answer})");
    let counters = profile.counters;

    assert_eq!(
        counters.function_calls,
        counters.builtin_calls
            + counters
                .user_function_executions_by_name
                .values()
                .copied()
                .sum::<u64>()
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
    assert_eq!(counters.user_function_calls, 0);
    assert_eq!(
        counters.user_function_executions_by_name.get("<anonymous>"),
        Some(&1)
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn group_by_profiles_one_builtin_call_and_one_callback_per_item() {
    let root = temp_dir("profile-group_by");
    let profile = profile(
        &root,
        "let key: fn(Int) -> String = |value| => str(value)\nlet grouped = group_by(key, [1, 2, 3])\nartifact(grouped)",
    );
    let counters = profile.counters;

    assert_eq!(counters.builtin_calls_by_name.get("group_by"), Some(&1));
    assert_eq!(
        counters.builtin_input_items_by_name.get("group_by"),
        Some(&3)
    );
    assert_eq!(counters.user_function_calls, 0);
    assert_eq!(
        counters.user_function_executions_by_name.get("<anonymous>"),
        Some(&3)
    );
    assert_eq!(
        counters.function_calls,
        counters.builtin_calls
            + counters
                .user_function_executions_by_name
                .values()
                .copied()
                .sum::<u64>()
    );
    assert_eq!(
        counters.builtin_calls,
        counters
            .builtin_calls_by_name
            .values()
            .copied()
            .sum::<u64>()
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn named_calls_memoization_and_physical_collection_work_are_separate() {
    let root = temp_dir("profile-exclusive-work");
    let profile = profile(
        &root,
        "type Joined {joined: List<Int>}\ntype Result {first: Int, second: Int, encoded: String}\nlet identity: fn(Int) -> Int = |value| => value\nlet first_value = identity(7)\nlet second_value = identity(7)\nlet removed = remove_at([1, 2, 3], 1)\nlet joined = concat(removed, [4])\nlet encoded = canonical(Joined {joined: joined})\nartifact(Result {first: first_value, second: second_value, encoded: encoded})",
    );
    let counters = profile.counters;

    assert_eq!(counters.user_function_calls, 0);
    assert_eq!(
        counters.user_function_executions_by_name.get("<anonymous>"),
        Some(&1)
    );
    assert_eq!(
        counters.memoized_call_hits_by_name.get("<anonymous>"),
        Some(&1)
    );
    assert_eq!(
        counters.memoized_call_misses_by_name.get("<anonymous>"),
        Some(&1)
    );
    assert_eq!(
        counters.builtin_cloned_items_by_name.get("remove_at"),
        Some(&3)
    );
    assert_eq!(
        counters.builtin_shifted_items_by_name.get("remove_at"),
        Some(&1)
    );
    assert_eq!(
        counters.builtin_copied_items_by_name.get("concat"),
        Some(&3)
    );
    assert!(counters.canonicalization_recursive_visits > 0);
    assert!(counters.canonicalization_serialized_bytes > 0);
    fs::remove_dir_all(root).ok();
}

#[test]
fn descriptor_assignment_and_table_work_are_generic_and_deterministic() {
    let root = temp_dir("profile-descriptors");
    let profile = profile(
        &root,
        "type Result {assignments: List<Int>, declarations: Int}\nlet target = type_evidence(type_tag<(Int, Int)>())\nlet sources = [type_evidence(type_tag<Int>()), type_evidence(type_tag<Int>())]\nlet assignments = descriptor_input_assignments(target, sources)\nlet table = descriptor_table([target])\nartifact(Result {assignments: assignments, declarations: __map_count(table)})",
    );
    let counters = profile.counters;

    assert_eq!(counters.descriptor_assignment_sources_examined, 2);
    assert_eq!(counters.descriptor_assignment_target_product_occurrences, 2);
    assert!(counters.descriptor_assignment_compatibility_checks >= 2);
    assert!(counters.descriptor_assignment_search_branches >= 2);
    assert_eq!(counters.descriptor_assignment_assignments_produced, 2);
    assert_eq!(counters.descriptor_table_top_level_descriptors, 1);
    assert_eq!(counters.descriptor_table_recursive_nodes, 3);
    assert_eq!(counters.descriptor_table_stable_id_invocations, 3);
    assert!(counters.descriptor_table_hashed_bytes > 0);
    fs::remove_dir_all(root).ok();
}

#[test]
fn builtin_item_work_and_recursive_validation_are_exact_for_tiny_fixture() {
    let root = temp_dir("profile-items");
    let profile = profile(&root, "type Result {count: Int}\nlet identity: fn(List<Int>) -> List<Int> = |value| => value\nlet values = identity((concat([1, 2], [3]): List<Int>))\nartifact(Result {count: count(values)})");
    let counters = profile.counters;

    assert_eq!(counters.builtin_calls_by_name.get("concat"), Some(&1));
    assert_eq!(counters.builtin_input_items_by_name.get("concat"), Some(&3));
    assert_eq!(counters.builtin_calls_by_name.get("count"), Some(&1));
    assert_eq!(counters.builtin_calls_by_name.get("artifact"), Some(&1));
    assert!(counters.runtime_value_validation_visits > 0);
    fs::remove_dir_all(root).ok();
}
