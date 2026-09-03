#[allow(dead_code)]
#[path = "../benches/bench_support.rs"]
mod bench_support;
#[allow(dead_code)]
#[path = "../benches/support/cache_scenarios.rs"]
mod cache_scenarios;

use cache_scenarios::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn load_runtime_contracts(_path: &Path) -> Value {
    json!({})
}

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn cache_catalog_is_ordered_and_has_an_independent_identity() {
    let names = definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "cold-module-chain",
            "cold-shipped-lead-turn",
            "populate-module-chain",
            "identical-repeat-module-chain",
            "entry-bytes-changed",
            "transitive-module-changed",
            "module-ambiguity-introduced",
            "read-target-changed",
            "read-json-target-changed",
            "read-json-ambiguity-introduced",
            "host-input-changed",
            "compiler-options-changed",
            "broken-module-repaired",
            "entry-lex-precedes-module-ambiguity",
            "module-ambiguity-precedes-host-input",
            "required-module-precedes-entry-error",
            "entry-deleted-after-success",
            "alternating-context-a-b-a-b",
        ]
    );
    assert_eq!(
        CACHE_SCENARIO_PROTOCOL_VERSION,
        "mag-cache-scenario-worker-v1"
    );
    assert_eq!(CACHE_SCENARIO_CATALOG_VERSION, "cycle-4-pre-cache-v1");
    assert_ne!(
        CACHE_SCENARIO_PROTOCOL_VERSION,
        bench_support::WORKER_PROTOCOL_VERSION
    );
    assert!(!cache_catalog_fingerprint().is_empty());
}

#[test]
fn cold_transition_scenarios_report_semantics_profile_and_session_stats() {
    let root = source_root();
    for definition in definitions() {
        if definition.name == "cold-shipped-lead-turn" {
            continue;
        }
        let sample = run_sample(&root, &definition.name);
        assert!(
            sample.compile_profile.total_duration_ns > 0,
            "{}",
            definition.name
        );
        assert!(sample.target_duration_ns >= sample.compile_profile.total_duration_ns);
        assert_eq!(sample.session_stats.compile_requests, 0);
        assert_eq!(
            sample.session_stats.successful_compilations + sample.session_stats.failed_compilations,
            sample.session_stats.load_requests
        );
    }
}

#[test]
fn repeat_setup_is_outside_the_single_profiled_target_operation() {
    let (samples, raw_batch_ns) = run_batch(&source_root(), "identical-repeat-module-chain", 2);
    assert_eq!(samples.len(), 2);
    for sample in &samples {
        assert_eq!(sample.session_stats.load_requests, 2);
        assert_eq!(sample.session_stats.cold_compilations, 2);
        assert_eq!(sample.session_stats.successful_compilations, 2);
        assert_eq!(sample.compile_profile.counters.modules_loaded, 2);
    }
    assert!(
        raw_batch_ns
            >= samples
                .iter()
                .map(|sample| sample.target_duration_ns)
                .sum::<u64>(),
        "batch timing contains the target operations but excludes preparation"
    );
}

#[test]
fn worker_compatibility_rejects_protocol_and_catalog_mismatch() {
    let definition = definitions().remove(0);
    let sample = run_sample(&source_root(), &definition.name);
    let manifest = CacheScenarioManifest {
        definition_fingerprint: definition_fingerprint(&definition),
        definition,
        baseline_sample: sample,
    };
    let hello = CacheWorkerHello {
        protocol_version: CACHE_SCENARIO_PROTOCOL_VERSION.into(),
        catalog_version: CACHE_SCENARIO_CATALOG_VERSION.into(),
        catalog_fingerprint: cache_catalog_fingerprint(),
        executable_identity: bench_support::WorkerExecutableIdentity {
            path: "/worker".into(),
            sha256: "binary".into(),
        },
        source_identity: bench_support::WorkerSourceIdentity {
            source_root: "/source".into(),
            source_ref: "ref".into(),
            tree: "tree".into(),
            dirty: false,
        },
        cases: vec![manifest],
    };
    assert!(validate_hello_pair(&hello, &hello).is_ok());
    let mut stale = hello.clone();
    stale.protocol_version = bench_support::WORKER_PROTOCOL_VERSION.into();
    assert_eq!(
        validate_hello_pair(&hello, &stale),
        Err("cache scenario worker protocol mismatch".into())
    );
    stale = hello.clone();
    stale.catalog_fingerprint = "changed".into();
    assert_eq!(
        validate_hello_pair(&hello, &stale),
        Err("cache scenario catalog mismatch".into())
    );
}
