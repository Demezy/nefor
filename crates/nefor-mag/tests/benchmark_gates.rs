#[allow(dead_code)]
#[path = "../benches/bench_support.rs"]
mod bench_support;

use bench_support::*;
use nefor_mag::profile::OperationCounters;

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

#[test]
fn balanced_schedule_is_replayable_and_balanced() {
    let first = balanced_pair_schedule(42, 20);
    let replay = balanced_pair_schedule(42, 20);
    assert_eq!(first, replay);
    assert_eq!(
        first
            .iter()
            .filter(|order| **order == PairOrder::AB)
            .count(),
        10
    );
    assert_eq!(
        first
            .iter()
            .filter(|order| **order == PairOrder::BA)
            .count(),
        10
    );
}

#[test]
fn paired_analysis_retains_raw_batches_and_calculates_ratios() {
    let samples = vec![
        sample(0, PairOrder::AB, 100, 90),
        sample(1, PairOrder::BA, 100, 100),
        sample(2, PairOrder::AB, 100, 110),
    ];
    let analysis = analyze_paired_samples(&samples, 1);
    assert_eq!(samples[0].baseline_batch_ns, 100);
    assert_eq!(samples[0].batch_count, samples[1].batch_count);
    assert!((analysis.paired_median_ratio - 1.0).abs() < f64::EPSILON);
    assert!((analysis.empirical_p90_ratio - 1.1).abs() < f64::EPSILON);
}

#[test]
fn confidence_verdicts_fail_closed() {
    let pass = (0..20)
        .map(|block| sample(block, PairOrder::AB, 100, 100))
        .collect::<Vec<_>>();
    assert_eq!(
        analyze_paired_samples(&pass, 1).verdict,
        ConfidenceVerdict::Pass
    );

    let regression = (0..20)
        .map(|block| sample(block, PairOrder::BA, 100, 125))
        .collect::<Vec<_>>();
    assert_eq!(
        analyze_paired_samples(&regression, 1).verdict,
        ConfidenceVerdict::Regression
    );

    let mixed = (0..20)
        .map(|block| {
            sample(
                block,
                PairOrder::AB,
                100,
                if block % 2 == 0 { 100 } else { 125 },
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        analyze_paired_samples(&mixed, 1).verdict,
        ConfidenceVerdict::Inconclusive
    );
}

#[test]
fn exclusive_subtraction_requires_matching_proven_boundaries() {
    let smaller = OperationCounters {
        evaluator_steps: 2,
        ..OperationCounters::default()
    };
    let larger = OperationCounters {
        evaluator_steps: 5,
        ..OperationCounters::default()
    };
    let exclusive = exclusive_counters("forward", "topology", "topology", true, &smaller, &larger)
        .expect("proven matched boundary");
    assert_eq!(exclusive.counters["evaluator_steps"], 3);
    assert!(exclusive_counters("forward", "a", "b", true, &smaller, &larger).is_err());
    assert!(exclusive_counters("forward", "a", "a", false, &smaller, &larger).is_err());
    assert!(exclusive_counters("forward", "a", "a", true, &larger, &smaller).is_err());
}

#[test]
fn broad_frontier_formulas_hold_at_phase_zero_sizes() {
    assert_eq!((4 * (4 + 1) + 1, 4 * (4 + 1)), (21, 20));
    assert_eq!((8 * (8 + 1) + 1, 8 * (8 + 1)), (73, 72));
    assert_eq!((16 * (8 + 1) + 1, 16 * (8 + 1)), (145, 144));
}

#[test]
fn incompatible_versions_are_never_gate_compatible() {
    let identity = ReportIdentity {
        report_schema_version: SCHEMA_VERSION,
        workload_catalog_version: PHASE0_WORKLOAD_CATALOG_VERSION.into(),
        workload_fingerprint: "workload".into(),
        parent_workload_catalog_version: LEGACY_WORKLOAD_CATALOG_VERSION.into(),
        parent_workload_fingerprint: "legacy".into(),
        oracle_catalog_version: ORACLE_CATALOG_VERSION.into(),
        oracle_fingerprint: "oracle".into(),
        profiler_schema_version: PROFILER_SCHEMA_VERSION.into(),
        statistics_policy_version: STATISTICS_POLICY_VERSION.into(),
        source_ref: "source".into(),
        executable_digest: "binary".into(),
    };
    let mut incompatible = identity.clone();
    incompatible.statistics_policy_version = "different".into();
    assert!(identities_gate_compatible(&identity, &identity));
    assert!(!identities_gate_compatible(&identity, &incompatible));
}

#[test]
fn batch_calibration_uses_baseline_and_freezes_positive_count() {
    assert_eq!(calibrate_batch_count(2_000, 10_000), 5);
    assert_eq!(calibrate_batch_count(20_000, 10_000), 1);
    assert_eq!(calibrate_batch_count(0, 10_000), 1);
}
