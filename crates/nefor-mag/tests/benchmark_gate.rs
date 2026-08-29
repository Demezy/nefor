#[allow(dead_code)]
#[path = "../benches/bench_support.rs"]
mod bench_support;

use bench_support::*;
use nefor_mag::profile::OperationCounters;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn nearest_rank_tail_is_exact_for_tiny_and_default_samples() {
    assert_eq!(nearest_rank_quantile(&[10, 20, 30], 90), 30);
    let thirty = (1..=30).collect::<Vec<_>>();
    assert_eq!(nearest_rank_quantile(&thirty, 90), 27);
}

#[test]
fn fixture_fingerprint_covers_every_workload_input() {
    let scratch = scratch("fingerprint");
    let module_root = scratch.join("modules");
    fs::create_dir_all(&module_root).unwrap();
    fs::write(module_root.join("library.mag"), "(let value 1)").unwrap();
    let mut case = fixture(
        &scratch,
        "case",
        "oracle",
        "oracle",
        None,
        "(artifact {})",
        vec![module_root.clone()],
        json!({"nested":{"value":1}}),
        None,
        "static",
        Some(json!({})),
        vec![probe_success("run", json!(1), json!(1))],
    );
    write_fixture_file(&mut case, "data.txt", b"first");
    let initial = fixture_fingerprint(&case);

    case.inputs = json!({"nested":{"value":2}});
    let input_changed = fixture_fingerprint(&case);
    assert_ne!(initial, input_changed);
    case.inputs = json!({"nested":{"value":1}});

    fs::write(module_root.join("library.mag"), "(let value 2)").unwrap();
    let module_changed = fixture_fingerprint(&case);
    assert_ne!(initial, module_changed);
    fs::write(module_root.join("library.mag"), "(let value 1)").unwrap();

    let second_root = scratch.join("second-modules");
    fs::create_dir_all(&second_root).unwrap();
    fs::write(second_root.join("other.mag"), "(let other 2)").unwrap();
    case.module_roots.push(second_root);
    let roots_changed = fixture_fingerprint(&case);
    assert_ne!(initial, roots_changed);
    case.module_roots.pop();

    write_fixture_file(&mut case, "data.txt", b"second");
    let file_changed = fixture_fingerprint(&case);
    assert_ne!(initial, file_changed);
    write_fixture_file(&mut case, "data.txt", b"first");

    case.expected_artifact = Some(json!({"changed":true}));
    let outcome_changed = fixture_fingerprint(&case);
    assert_ne!(initial, outcome_changed);
    case.expected_artifact = Some(json!({}));

    case.probes = vec![probe_success("run", json!(1), json!(2))];
    let probe_changed = fixture_fingerprint(&case);
    assert_ne!(initial, probe_changed);

    fs::remove_dir_all(scratch).ok();
}

#[test]
fn resident_probe_expectation_mismatch_aborts_report_creation() {
    let scratch = scratch("probe-mismatch");
    let mut case = fixture(
        &scratch,
        "case",
        "oracle",
        "oracle",
        None,
        "(let run (fn [[value Int]] -> Artifact (artifact value)))\n(artifact {})",
        vec![],
        json!({}),
        None,
        "resident",
        Some(json!({})),
        vec![probe_success("run", json!(1), json!(2))],
    );
    refresh_fixture_fingerprints(std::slice::from_mut(&mut case));
    assert!(std::panic::catch_unwind(|| observe(&case)).is_err());
    fs::remove_dir_all(scratch).ok();
}

#[test]
fn gate_artifact_rejects_performance_semantics_and_workload_independently() {
    let baseline = report(100, 100, "artifact", "workload");

    let slow = report(100, 100, "artifact", "workload");
    let comparison = compare_reports(&baseline, &slow, true);
    assert!(comparison.semantic.passed);
    assert!(!comparison.target_median.passed);
    assert!(!comparison.target_logical_counter.passed);
    assert!(!comparison.overall.passed);

    let semantic_mismatch = report(80, 50, "different", "workload");
    let comparison = compare_reports(&baseline, &semantic_mismatch, true);
    assert!(!comparison.semantic.passed);
    assert!(!comparison.overall.passed);

    let workload_mismatch = report(80, 50, "artifact", "changed-workload");
    let comparison = compare_reports(&baseline, &workload_mismatch, true);
    assert!(!comparison.compatibility.passed);
    assert!(!comparison.overall.passed);
}

fn report(median: u64, equality_visits: u64, artifact_hash: &str, workload: &str) -> Report {
    let mut counters = OperationCounters::default();
    counters.value_equality_visits = equality_visits;
    Report {
        schema_version: SCHEMA_VERSION,
        metadata: Metadata {
            git_commit: "commit".into(),
            git_dirty: false,
            git_tree: "tree".into(),
            git_diff_digest: None,
            package_version: "0.4.0".into(),
            rustc: "rustc".into(),
            target: "target".into(),
            os: "os".into(),
            arch: "arch".into(),
            logical_cpus: 1,
            profile: "bench (optimized)".into(),
            samples_per_case: 30,
            warmup_iterations: 3,
            quantile_policy: "nearest-rank".into(),
            case_definition_hash: workload.into(),
            size_replacements: BTreeMap::new(),
        },
        counter_semantics: BTreeMap::new(),
        cases: vec![CaseReport {
            name: "nefor-linear-validate-12".into(),
            family: "nefor-linear".into(),
            stage: "validate".into(),
            size: Some(12),
            fixture_fingerprint: "fixture".into(),
            outcome: "success".into(),
            expected_error: None,
            artifact_hash: Some(artifact_hash.into()),
            wall_ns: vec![median],
            distribution_ns: Distribution {
                min: median,
                median,
                mean: median,
                p90: median,
                p95: median,
                max: median,
            },
            counters: Some(counters),
            derived: None,
            profiled_phase_median_ns: None,
            profiled_invocations_are_separate: true,
        }],
        oracles: vec![],
        recommendation: Recommendation {
            candidate: Some("Nefor graph validation".into()),
            evidence: "test".into(),
        },
    }
}

fn scratch(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp/mag-optimization-cycle-1/tests")
        .join(format!("{name}-{nonce}"));
    fs::create_dir_all(&path).unwrap();
    path
}
