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
fn fixture_fingerprint_separates_workload_and_implementation_roots() {
    let scratch = scratch("fingerprint-roots");
    let workload_root = scratch.join("workload-modules");
    let implementation_root = scratch.join("implementation-modules");
    fs::create_dir_all(&workload_root).unwrap();
    fs::create_dir_all(&implementation_root).unwrap();
    fs::write(workload_root.join("library.mag"), "let value = 1").unwrap();
    fs::write(implementation_root.join("library.mag"), "let value = 1").unwrap();
    let mut case = fixture(
        &scratch,
        "case",
        "oracle",
        "oracle",
        None,
        "artifact {}",
        vec![
            ModuleRoot::workload("workload", workload_root.clone()),
            ModuleRoot::implementation("implementation", implementation_root.clone()),
        ],
        json!({}),
        None,
        "static",
        Some(json!({})),
    );
    let initial = fixture_fingerprint(&case);

    fs::write(workload_root.join("library.mag"), "let value = 2").unwrap();
    assert_ne!(initial, fixture_fingerprint(&case));
    fs::write(workload_root.join("library.mag"), "let value = 1").unwrap();

    fs::write(implementation_root.join("library.mag"), "let value = 2").unwrap();
    assert_eq!(initial, fixture_fingerprint(&case));

    case.module_roots[2].role = ModuleRootRole::Workload;
    assert_ne!(initial, fixture_fingerprint(&case));
    case.module_roots[2].role = ModuleRootRole::Implementation;

    case.module_roots.swap(0, 1);
    assert_ne!(initial, fixture_fingerprint(&case));

    fs::remove_dir_all(scratch).ok();
}

#[test]
fn fixture_fingerprint_covers_non_module_workload_inputs() {
    let scratch = scratch("fingerprint-inputs");
    let mut case = fixture(
        &scratch,
        "case",
        "oracle",
        "oracle",
        None,
        "artifact {}",
        vec![],
        json!({"nested":{"value":1}}),
        None,
        "static",
        Some(json!({})),
    );
    write_fixture_file(&mut case, "data.txt", b"first");
    let initial = fixture_fingerprint(&case);

    case.inputs = json!({"nested":{"value":2}});
    assert_ne!(initial, fixture_fingerprint(&case));
    case.inputs = json!({"nested":{"value":1}});

    fs::write(case.source_dir.join("main.mag"), "artifact {changed: true}").unwrap();
    assert_ne!(initial, fixture_fingerprint(&case));
    fs::write(case.source_dir.join("main.mag"), "artifact {}").unwrap();

    write_fixture_file(&mut case, "data.txt", b"second");
    assert_ne!(initial, fixture_fingerprint(&case));
    write_fixture_file(&mut case, "data.txt", b"first");

    case.expected_artifact = Some(json!({"changed":true}));
    assert_ne!(initial, fixture_fingerprint(&case));
    case.expected_artifact = Some(json!({}));

    fs::remove_dir_all(scratch).ok();
}

#[test]
fn marginal_comparison_reports_performance_semantics_and_workload_independently() {
    let baseline = report(100, 100, "artifact", "workload");

    let slow = report(100, 100, "artifact", "workload");
    let comparison = compare_reports(&baseline, &slow, false);
    assert!(comparison.semantic.passed);
    assert!(!comparison.target_median.passed);
    assert!(!comparison.target_logical_counter.passed);
    assert!(!comparison.overall.passed);

    let semantic_mismatch = report(80, 50, "different", "workload");
    let comparison = compare_reports(&baseline, &semantic_mismatch, false);
    assert!(!comparison.semantic.passed);
    assert!(!comparison.overall.passed);

    let workload_mismatch = report(80, 50, "artifact", "changed-workload");
    let comparison = compare_reports(&baseline, &workload_mismatch, false);
    assert!(!comparison.compatibility.passed);
    assert!(!comparison.overall.passed);
}

#[test]
fn marginal_comparison_reports_p90_regression_independently_of_target_median() {
    let baseline = report_with_p90(100, 100, 100, "artifact", "workload");
    let candidate = report_with_p90(80, 111, 50, "artifact", "workload");

    let comparison = compare_reports(&baseline, &candidate, false);
    assert!(comparison.target_median.passed);
    assert!(comparison.target_logical_counter.passed);
    assert!(!comparison.all_case_p90.passed);
    assert!(comparison
        .all_case_p90
        .detail
        .contains("nefor-linear-validate-12"));
    assert!(!comparison.overall.passed);
}

fn report(median: u64, equality_visits: u64, artifact_hash: &str, workload: &str) -> Report {
    report_with_p90(median, median, equality_visits, artifact_hash, workload)
}

fn report_with_p90(
    median: u64,
    p90: u64,
    equality_visits: u64,
    artifact_hash: &str,
    workload: &str,
) -> Report {
    let counters = OperationCounters {
        value_equality_visits: equality_visits,
        ..OperationCounters::default()
    };
    Report {
        schema_version: SCHEMA_VERSION,
        identity: Some(ReportIdentity {
            report_schema_version: SCHEMA_VERSION,
            workload_catalog_version: PHASE0_WORKLOAD_CATALOG_VERSION.into(),
            workload_fingerprint: workload.into(),
            parent_workload_catalog_version: CURRENT_MAIN_A0_WORKLOAD_CATALOG_VERSION.into(),
            parent_workload_fingerprint: CURRENT_MAIN_A0_WORKLOAD_FINGERPRINT.into(),
            oracle_catalog_version: ORACLE_CATALOG_VERSION.into(),
            oracle_fingerprint: CURRENT_MAIN_A0_ORACLE_FINGERPRINT.into(),
            profiler_schema_version: PROFILER_SCHEMA_VERSION.into(),
            statistics_policy_version: STATISTICS_POLICY_VERSION.into(),
            source_ref: "source".into(),
            executable_digest: "binary".into(),
        }),
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
        statistics_policy: statistics_policy(),
        cases: vec![CaseReport {
            name: "nefor-linear-validate-12".into(),
            family: "nefor-linear".into(),
            stage: "validate".into(),
            size: Some(12),
            fixture_fingerprint: "fixture".into(),
            topology_fingerprint: None,
            outcome: "success".into(),
            expected_error: None,
            artifact_hash: Some(artifact_hash.into()),
            wall_ns: vec![median],
            distribution_ns: Distribution {
                min: median,
                median,
                mean: median,
                p90,
                p95: median,
                max: median,
            },
            counters: Some(counters),
            derived: None,
            profiled_phase_median_ns: None,
            profiled_invocations_are_separate: true,
            self_comparison: None,
        }],
        oracles: vec![],
        exclusive_sections: vec![],
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
