#[allow(dead_code)]
#[path = "../benches/bench_support.rs"]
mod bench_support;

use bench_support::*;
use nefor_mag::profile::OperationCounters;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn sample(block: usize, order: PairOrder, baseline: u64, candidate: u64) -> PairedSample {
    PairedSample {
        block,
        generated_order: order,
        actual_order: order,
        batch_count: 8,
        baseline_batch_ns: baseline,
        candidate_batch_ns: candidate,
    }
}

fn success(value: u64) -> SemanticOutcome {
    let artifact = json!({"value": value});
    SemanticOutcome::Success {
        artifact_hash: fingerprint(&serde_json::to_vec(&artifact).unwrap()),
        artifact_json: artifact,
    }
}

fn failure(message: &str) -> SemanticOutcome {
    SemanticOutcome::Error {
        class: "type".into(),
        message: message.into(),
        policy_stage: "static".into(),
        ordered_diagnostics: vec![message.into()],
    }
}

fn counters(steps: u64) -> OperationCounters {
    OperationCounters {
        evaluator_steps: steps,
        ..OperationCounters::default()
    }
}

fn manifest(stage: &str, steps: u64) -> WorkerCaseManifest {
    let observation = success(1);
    WorkerCaseManifest {
        name: format!("broad-frontier-{stage}-4x4"),
        family: "broad-frontier".into(),
        stage: stage.into(),
        policy: "timed".into(),
        size: Some(21),
        case_fingerprint: format!("case-{stage}"),
        workload_fingerprint: "workload".into(),
        fixture_source_fingerprint: "fixture".into(),
        topology_fingerprint: Some("topology".into()),
        forced_terminal_binding: forced_terminal_binding(stage).into(),
        forcing_dependency_proof: Some("proof".into()),
        declared_direct_prerequisite: declared_direct_prerequisite(stage).map(str::to_owned),
        expected_semantic_artifact_fingerprint: semantic_fingerprint(&observation),
        semantic_observation: observation,
        logical_counters: Some(counters(steps)),
        baseline_warmup_ns: 1_000,
    }
}

fn executable(digest: &str) -> WorkerExecutableIdentity {
    WorkerExecutableIdentity {
        path: PathBuf::from(format!("/bin/{digest}")),
        sha256: digest.into(),
    }
}

fn source(reference: &str, dirty: bool) -> WorkerSourceIdentity {
    WorkerSourceIdentity {
        source_root: PathBuf::from(format!("/src/{reference}")),
        source_ref: reference.into(),
        tree: format!("tree-{reference}"),
        dirty,
    }
}

fn hello(executable_digest: &str, source_ref: &str) -> WorkerHello {
    WorkerHello {
        protocol_version: WORKER_PROTOCOL_VERSION.into(),
        executable_identity: executable(executable_digest),
        source_identity: source(source_ref, false),
        benchmark_definition_identity: "definition".into(),
        workload_catalog_version: PHASE0_WORKLOAD_CATALOG_VERSION.into(),
        oracle_catalog_version: ORACLE_CATALOG_VERSION.into(),
        statistics_policy_version: STATISTICS_POLICY_VERSION.into(),
        warmup_iterations: 3,
        cases: vec![manifest("analysis", 2)],
        oracles: vec![],
    }
}

fn request(sequence: usize) -> WorkerRequest {
    WorkerRequest {
        sequence,
        case_fingerprint: "case-analysis".into(),
        batch_count: 8,
        measurement_kind: MeasurementKind::TimedBatch,
    }
}

fn response(hello: &WorkerHello, sequence: usize) -> WorkerResponse {
    let case = &hello.cases[0];
    WorkerResponse {
        sequence,
        case_name: case.name.clone(),
        case_fingerprint: case.case_fingerprint.clone(),
        executable_identity: hello.executable_identity.clone(),
        source_identity: hello.source_identity.clone(),
        semantic_observation: case.semantic_observation.clone(),
        logical_counters: case.logical_counters.clone(),
        raw_batch_ns: 10_000,
    }
}

#[test]
fn persistent_worker_request_response_sequence_is_strict() {
    let worker = hello("baseline", "a0");
    assert!(validate_worker_response(
        &worker,
        &worker.cases[0],
        &request(7),
        &response(&worker, 7)
    )
    .is_ok());
    assert_eq!(
        validate_worker_response(
            &worker,
            &worker.cases[0],
            &request(7),
            &response(&worker, 8)
        ),
        Err("worker response order mismatch".into())
    );
}

#[test]
fn balanced_schedule_is_replayable_and_balanced() {
    let first = balanced_pair_schedule(42, 30);
    assert_eq!(first, balanced_pair_schedule(42, 30));
    assert_eq!(
        first
            .iter()
            .filter(|order| **order == PairOrder::AB)
            .count(),
        15
    );
    assert_eq!(
        first
            .iter()
            .filter(|order| **order == PairOrder::BA)
            .count(),
        15
    );
}

#[test]
fn paired_batches_use_one_frozen_count_for_both_workers() {
    let samples = vec![
        sample(0, PairOrder::AB, 100, 90),
        sample(1, PairOrder::BA, 100, 110),
    ];
    let analysis = analyze_paired_samples(&samples, 1);
    assert_eq!(samples[0].batch_count, samples[1].batch_count);
    assert!(analysis.paired_median_ratio > 0.0);
}

#[test]
fn paired_target_requires_both_speedup_and_logical_reduction() {
    let mut case = PairedCaseReport {
        name: "target".into(),
        family: "broad-frontier".into(),
        stage: "lower".into(),
        seed: 1,
        generated_order: vec![],
        actual_order: vec![],
        batch_count: 1,
        raw_paired_batch_samples: vec![],
        analysis: PairedAnalysis {
            paired_median_ratio: 0.80,
            empirical_p90_ratio: 1.0,
            lower_confidence_bound: 1.0,
            upper_confidence_bound: 1.0,
            verdict: ConfidenceVerdict::Pass,
            confidence_method: "test".into(),
            family_wise_error_policy: "test".into(),
        },
        baseline_semantic_observation: success(1),
        candidate_semantic_observation: success(1),
        semantic_match: true,
        baseline_logical_counters: Some(counters(100)),
        candidate_logical_counters: Some(counters(50)),
        baseline_attribution: None,
        candidate_attribution: None,
    };
    let (median, logical) = paired_target_verdicts(Some(&case), Some("evaluator_steps"));
    assert!(median.passed);
    assert!(logical.passed);

    case.analysis.paired_median_ratio = 0.95;
    case.candidate_logical_counters = Some(counters(70));
    let (median, logical) = paired_target_verdicts(Some(&case), Some("evaluator_steps"));
    assert!(!median.passed);
    assert!(!logical.passed);

    let (median, logical) = paired_target_verdicts(None, None);
    assert!(!median.passed);
    assert!(!logical.passed);
}

#[test]
fn worker_startup_and_warmup_are_outside_timed_response() {
    let worker = hello("baseline", "a0");
    let response = response(&worker, 0);
    assert_eq!(worker.warmup_iterations, 3);
    assert_eq!(worker.cases[0].baseline_warmup_ns, 1_000);
    assert_eq!(response.raw_batch_ns, 10_000);
    let encoded = serde_json::to_value(response).unwrap();
    assert!(encoded.get("baseline_warmup_ns").is_none());
}

#[test]
fn source_and_executable_identities_are_explicit() {
    let worker = hello("sha256:binary", "clean-commit");
    assert!(!worker.executable_identity.sha256.is_empty());
    assert_eq!(worker.source_identity.source_ref, "clean-commit");
    assert_eq!(worker.source_identity.tree, "tree-clean-commit");
    assert!(!worker.source_identity.dirty);
}

#[test]
fn identical_endpoints_are_gate_rejected_but_calibration_accepts_them() {
    let baseline = hello("same", "a0");
    let candidate = hello("same", "a0");
    let rejection = validate_worker_pair(&baseline, &candidate, false);
    assert_eq!(
        rejection,
        vec!["identical endpoints are calibration-only and cannot satisfy an optimization gate"]
    );
    assert!(validate_worker_pair(&baseline, &candidate, true).is_empty());
}

#[test]
fn pair_compatibility_rejects_protocol_workload_fixture_and_source_mismatches() {
    let baseline = hello("baseline", "a0");
    let mut candidate = hello("candidate", "candidate");
    candidate.protocol_version = "old".into();
    candidate.benchmark_definition_identity = "different".into();
    candidate.cases[0].fixture_source_fingerprint = "different".into();
    candidate.source_identity.dirty = true;
    let failures = validate_worker_pair(&baseline, &candidate, false).join("; ");
    assert!(failures.contains("worker protocol mismatch"));
    assert!(failures.contains("benchmark workload definition mismatch"));
    assert!(failures.contains("worker workload or fixture mismatch"));
    assert!(failures.contains("worker source root is dirty"));
}

#[test]
fn response_rejects_mismatched_executable_and_source_identity() {
    let worker = hello("baseline", "a0");
    let mut wrong_executable = response(&worker, 0);
    wrong_executable.executable_identity = executable("other");
    assert_eq!(
        validate_worker_response(&worker, &worker.cases[0], &request(0), &wrong_executable),
        Err("worker executable identity changed".into())
    );
    let mut wrong_source = response(&worker, 0);
    wrong_source.source_identity = source("other", false);
    assert_eq!(
        validate_worker_response(&worker, &worker.cases[0], &request(0), &wrong_source),
        Err("worker source identity changed".into())
    );
}

#[test]
fn exact_success_and_failure_observations_are_required() {
    assert!(observations_match(&success(1), &success(1)));
    assert!(!observations_match(&success(1), &success(2)));
    assert!(observations_match(
        &failure("bad type"),
        &failure("bad type")
    ));
    assert!(!observations_match(
        &failure("bad type"),
        &failure("different message")
    ));
    let mut different_stage = failure("bad type");
    if let SemanticOutcome::Error { policy_stage, .. } = &mut different_stage {
        *policy_stage = "runtime".into();
    }
    assert!(!observations_match(&failure("bad type"), &different_stage));
}

#[test]
fn p90_bootstrap_is_tail_sensitive_when_median_is_unchanged() {
    let stable = (0..30)
        .map(|block| sample(block, PairOrder::AB, 100, 100))
        .collect::<Vec<_>>();
    let tailed = (0..30)
        .map(|block| sample(block, PairOrder::AB, 100, if block < 4 { 125 } else { 100 }))
        .collect::<Vec<_>>();
    let stable = analyze_paired_samples(&stable, 50);
    let tailed = analyze_paired_samples(&tailed, 50);
    assert_eq!(stable.paired_median_ratio, tailed.paired_median_ratio);
    assert!(tailed.empirical_p90_ratio > stable.empirical_p90_ratio);
    assert!(tailed.upper_confidence_bound > stable.upper_confidence_bound);
}

#[test]
fn p90_confidence_verdicts_cover_pass_regression_and_inconclusive() {
    let pass = (0..30)
        .map(|block| sample(block, PairOrder::AB, 100, 100))
        .collect::<Vec<_>>();
    let regression = (0..30)
        .map(|block| sample(block, PairOrder::BA, 100, 125))
        .collect::<Vec<_>>();
    let mixed = (0..30)
        .map(|block| sample(block, PairOrder::AB, 100, if block < 4 { 125 } else { 100 }))
        .collect::<Vec<_>>();
    assert_eq!(
        analyze_paired_samples(&pass, 50).verdict,
        ConfidenceVerdict::Pass
    );
    assert_eq!(
        analyze_paired_samples(&regression, 50).verdict,
        ConfidenceVerdict::Regression
    );
    assert_eq!(
        analyze_paired_samples(&mixed, 50).verdict,
        ConfidenceVerdict::Inconclusive
    );
}

#[test]
fn family_wise_error_is_allocated_per_tail_and_fixed_case_count() {
    assert!((family_tail_probability(50) - 0.0005).abs() < f64::EPSILON);
    assert!((family_tail_probability(1) - 0.025).abs() < f64::EPSILON);
}

#[test]
fn escalation_is_preregistered_and_stops_after_120() {
    assert_eq!(next_escalation_sample_count(3, true), Some(30));
    assert_eq!(next_escalation_sample_count(30, true), Some(60));
    assert_eq!(next_escalation_sample_count(60, true), Some(120));
    assert_eq!(next_escalation_sample_count(120, true), None);
    assert_eq!(next_escalation_sample_count(30, false), None);
}

#[test]
fn retained_tail_regressions_cannot_receive_a_median_derived_pass() {
    for _ in 0..8 {
        let samples = (0..30)
            .map(|block| sample(block, PairOrder::AB, 100, if block < 4 { 125 } else { 100 }))
            .collect::<Vec<_>>();
        let analysis = analyze_paired_samples(&samples, 50);
        assert_eq!(analysis.paired_median_ratio, 1.0);
        assert!(analysis.empirical_p90_ratio > 1.10);
        assert_ne!(analysis.verdict, ConfidenceVerdict::Pass);
    }
}

#[test]
fn stale_statistics_and_report_schema_are_incompatible() {
    let identity = ReportIdentity {
        report_schema_version: SCHEMA_VERSION,
        workload_catalog_version: PHASE0_WORKLOAD_CATALOG_VERSION.into(),
        workload_fingerprint: "workload".into(),
        parent_workload_catalog_version: CURRENT_MAIN_A0_WORKLOAD_CATALOG_VERSION.into(),
        parent_workload_fingerprint: "legacy".into(),
        oracle_catalog_version: ORACLE_CATALOG_VERSION.into(),
        oracle_fingerprint: "oracle".into(),
        profiler_schema_version: PROFILER_SCHEMA_VERSION.into(),
        statistics_policy_version: STATISTICS_POLICY_VERSION.into(),
        source_ref: "source".into(),
        executable_digest: "binary".into(),
    };
    let mut stale = identity.clone();
    stale.report_schema_version -= 1;
    stale.statistics_policy_version = "paired-log-ratio-fwer-v1".into();
    assert!(!identities_gate_compatible(&identity, &stale));
}

#[test]
fn stage_boundary_fingerprint_excludes_stage_body_but_keeps_shared_inputs() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp/mag-optimization-cycle-3/tests")
        .join(format!("stage-boundary-{nonce}"));
    fs::create_dir_all(&scratch).unwrap();
    let mut analysis = fixture(
        &scratch,
        "analysis",
        "broad-frontier",
        "analysis",
        Some(21),
        "artifact {stage: \"analysis\"}",
        vec![],
        json!({"shared": 1}),
        None,
        "timed",
        None,
    );
    let mut forward = fixture(
        &scratch,
        "forward",
        "broad-frontier",
        "forward-reachability",
        Some(21),
        "artifact {stage: \"forward\"}",
        vec![],
        json!({"shared": 1}),
        None,
        "timed",
        None,
    );
    analysis.topology_fingerprint = Some("topology".into());
    forward.topology_fingerprint = Some("topology".into());
    assert_eq!(
        stage_fixture_source_fingerprint(&analysis),
        stage_fixture_source_fingerprint(&forward)
    );
    forward.inputs = json!({"shared": 2});
    assert_ne!(
        stage_fixture_source_fingerprint(&analysis),
        stage_fixture_source_fingerprint(&forward)
    );
    fs::remove_dir_all(scratch).ok();
}

#[test]
fn full_fixture_or_forcing_mismatch_prevents_exclusive_attribution() {
    let analysis = manifest("analysis", 2);
    let mut forward = manifest("forward-reachability", 5);
    assert_eq!(
        stage_attribution(&analysis, &forward).kind,
        AttributionKind::Exclusive
    );
    forward.fixture_source_fingerprint = "other-fixture".into();
    assert_eq!(
        stage_attribution(&analysis, &forward).kind,
        AttributionKind::Inclusive
    );
    forward.fixture_source_fingerprint = "fixture".into();
    forward.forced_terminal_binding.clear();
    assert_eq!(
        stage_attribution(&analysis, &forward).kind,
        AttributionKind::Inclusive
    );
    forward.forced_terminal_binding = "forward-proof".into();
    forward.forcing_dependency_proof = None;
    assert_eq!(
        stage_attribution(&analysis, &forward).kind,
        AttributionKind::Inclusive
    );
    forward.forcing_dependency_proof = Some("proof".into());
    forward.declared_direct_prerequisite = Some("wrong".into());
    assert_eq!(
        stage_attribution(&analysis, &forward).kind,
        AttributionKind::Inclusive
    );
}

#[test]
fn negative_counter_delta_prevents_exclusive_attribution() {
    let analysis = manifest("analysis", 5);
    let forward = manifest("forward-reachability", 2);
    assert_eq!(
        stage_attribution(&analysis, &forward).kind,
        AttributionKind::Inclusive
    );
}

#[test]
fn lowering_remains_inclusive_after_proven_analysis_prefix() {
    let analysis = manifest("analysis", 2);
    let lower = manifest("lower", 8);
    let attribution = stage_attribution(&analysis, &lower);
    assert!(attribution.prefix_proven);
    assert_eq!(
        attribution.kind,
        AttributionKind::InclusiveAfterProvenAnalysisPrefix
    );
    assert_eq!(
        attribution.label,
        "lowering inclusive after proven analysis prefix"
    );
    assert!(attribution.counters.is_empty());
}

#[test]
fn malformed_worker_response_and_missing_case_fail_closed() {
    assert!(decode_worker_response("not-json").is_err());
    let worker = hello("baseline", "a0");
    let mut missing = response(&worker, 0);
    missing.case_name = "missing".into();
    assert_eq!(
        validate_worker_response(&worker, &worker.cases[0], &request(0), &missing),
        Err("worker response missing or mismatched case".into())
    );
}

#[test]
fn batch_calibration_uses_baseline_and_freezes_positive_count() {
    assert_eq!(calibrate_batch_count(2_000, 10_000), 5);
    assert_eq!(calibrate_batch_count(20_000, 10_000), 1);
    assert_eq!(calibrate_batch_count(0, 10_000), 1);
}

#[test]
fn forcing_dependency_proof_requires_the_actual_terminal_dependency() {
    let forward = "let forward = nefor.graph.`forward-reachable`(analysis)\nartifact(FrontierProof {summary: summary, forced: forward_proof})";
    assert!(forcing_dependency_proof_from_sources("forward-reachability", forward, None).is_some());
    assert!(forcing_dependency_proof_from_sources(
        "forward-reachability",
        "let forward = nefor.graph.`forward-reachable`(analysis)\nartifact(summary)",
        None
    )
    .is_none());

    let reverse = "let forward = nefor.graph.`forward-reachable`(analysis)\nlet reverse = force_reverse(forward_proof)\nartifact(FrontierProof {summary: summary, forced: reverse_proof})";
    assert!(forcing_dependency_proof_from_sources("both-reachability", reverse, None).is_some());

    let lower = "let lowered = nefor.graph.lower(topology)\nlet forced = canonical(lowered)\nartifact(LowerFrontier {summary: summary, lowered: lowered, forced: forced})";
    let graph = "let `lower-program`: fn(Graph) -> Modification = |topology| => {\n  let analysis = `analyze-for-lowering`(topology)";
    assert!(forcing_dependency_proof_from_sources("lower", lower, Some(graph)).is_some());
    assert!(forcing_dependency_proof_from_sources(
        "lower",
        lower,
        Some("let `lower-program` = nil")
    )
    .is_none());
}
