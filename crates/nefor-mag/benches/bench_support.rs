use nefor_mag::error::MagError;
use nefor_mag::profile::{CompileProfile, OperationCounters, PhaseDurations};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SCHEMA_VERSION: u8 = 6;
pub const COMPARISON_SCHEMA_VERSION: u8 = 5;
pub const CURRENT_MAIN_A0_WORKLOAD_CATALOG_VERSION: &str = "artifact-only-current-main-v5";
pub const PHASE0_WORKLOAD_CATALOG_VERSION: &str = "cycle-3-a0-artifact-only-v5";
pub const ORACLE_CATALOG_VERSION: &str = "mag-oracles-22-artifact-only-v4";
pub const PROFILER_SCHEMA_VERSION: &str = "artifact-only-compiler-profile-v3";
pub const STATISTICS_POLICY_VERSION: &str = "paired-nearest-rank-p90-fwer-v2";
pub const WORKER_PROTOCOL_VERSION: &str = "mag-bench-worker-v2";
pub const BOOTSTRAP_RESAMPLES: usize = 131_072;
pub const LEGACY_COMBINED_FINGERPRINT: &str =
    "sha256:0e2cf14afde86802d519af05e668a615e8f1618733c5fe2777adc3778d9c8431";
pub const LEGACY_WORKLOAD_FINGERPRINT: &str =
    "sha256:59617bf4d8755e91d7b5ea5d13574bca7efc7c79276e850fd5ce479df037deaa";
pub const CURRENT_MAIN_A0_WORKLOAD_FINGERPRINT: &str =
    "sha256:7d29a24174761b19a506ff31a49dd248433c3ba7f9f5308237ed24ac51bf2dd2";
pub const CURRENT_MAIN_A0_ORACLE_FINGERPRINT: &str =
    "sha256:079162ffe8d364945623e43b61bcd4c57b3854c7659722a408e2bf8c794b8769";
pub const PHASE0_WORKLOAD_FINGERPRINT: &str =
    "sha256:658d96fa4e028e4849c60f66b2291a2f094829dbae3b6471c7cef19fa7ac0cde";
pub const MAX_TARGET_MEDIAN_RATIO: f64 = 0.90;
pub const MIN_TARGET_COUNTER_REDUCTION: f64 = 0.40;
pub const MAX_CASE_P90_RATIO: f64 = 1.10;

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u8,
    #[serde(default)]
    pub identity: Option<ReportIdentity>,
    pub metadata: Metadata,
    pub counter_semantics: BTreeMap<String, String>,
    pub statistics_policy: StatisticsPolicy,
    pub cases: Vec<CaseReport>,
    pub oracles: Vec<OracleObservation>,
    pub exclusive_sections: Vec<ExclusiveCounters>,
    pub recommendation: Recommendation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportIdentity {
    pub report_schema_version: u8,
    pub workload_catalog_version: String,
    pub workload_fingerprint: String,
    pub parent_workload_catalog_version: String,
    pub parent_workload_fingerprint: String,
    pub oracle_catalog_version: String,
    pub oracle_fingerprint: String,
    pub profiler_schema_version: String,
    pub statistics_policy_version: String,
    pub source_ref: String,
    pub executable_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatisticsPolicy {
    pub version: String,
    pub confidence_method: String,
    pub family_wise_error_policy: String,
    pub rng: String,
    pub sample_escalation_schedule: Vec<usize>,
    pub regression_ratio: String,
    pub verdicts: String,
    pub normalization_policy: String,
}

pub fn statistics_policy() -> StatisticsPolicy {
    StatisticsPolicy {
        version: STATISTICS_POLICY_VERSION.into(),
        confidence_method: "paired nearest-rank p90 percentile bootstrap; 131072 deterministic resamples".into(),
        family_wise_error_policy: "Bonferroni 0.05 / (2 × fixed case count) per lower/upper tail".into(),
        rng: "xorshift64; balanced adjacent AB/BA schedule uses SHA-256(case name), bootstrap uses fixed constant xor sample count".into(),
        sample_escalation_schedule: vec![30, 60, 120],
        regression_ratio: "candidate_ns / baseline_ns on raw adjacent batches; analyze paired log-ratios and retain empirical ratios".into(),
        verdicts: "pass iff upper <= 1.10; regression iff lower > 1.10; otherwise inconclusive and fail-closed".into(),
        normalization_policy: "control normalization is diagnostic only and never replaces raw gates".into(),
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metadata {
    pub git_commit: String,
    pub git_dirty: bool,
    pub git_tree: String,
    pub git_diff_digest: Option<String>,
    pub package_version: String,
    pub rustc: String,
    pub target: String,
    pub os: String,
    pub arch: String,
    pub logical_cpus: usize,
    pub profile: String,
    pub samples_per_case: usize,
    pub warmup_iterations: usize,
    pub quantile_policy: String,
    pub case_definition_hash: String,
    pub size_replacements: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CaseReport {
    pub name: String,
    pub family: String,
    pub stage: String,
    pub size: Option<usize>,
    pub fixture_fingerprint: String,
    pub topology_fingerprint: Option<String>,
    pub outcome: String,
    pub expected_error: Option<String>,
    pub artifact_hash: Option<String>,
    pub wall_ns: Vec<u64>,
    pub distribution_ns: Distribution,
    pub counters: Option<OperationCounters>,
    pub derived: Option<DerivedCounters>,
    pub profiled_phase_median_ns: Option<PhaseDurations>,
    pub profiled_invocations_are_separate: bool,
    pub self_comparison: Option<PairedSelfComparison>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Distribution {
    pub min: u64,
    pub median: u64,
    pub mean: u64,
    pub p90: u64,
    pub p95: u64,
    pub max: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DerivedCounters {
    pub unforced_at_load: u64,
    pub function_call_partition_holds: bool,
    pub builtin_name_partition_holds: bool,
    pub force_result_attempts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SemanticOutcome {
    Success {
        artifact_json: Value,
        artifact_hash: String,
    },
    Error {
        class: String,
        message: String,
        policy_stage: String,
        ordered_diagnostics: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OracleObservation {
    pub name: String,
    pub policy: String,
    pub expected_artifact: Option<Value>,
    pub observation: SemanticOutcome,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Recommendation {
    pub candidate: Option<String>,
    pub evidence: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleRootRole {
    Workload,
    Implementation,
}

impl ModuleRootRole {
    fn marker(self) -> &'static str {
        match self {
            Self::Workload => "workload",
            Self::Implementation => "implementation",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleRoot {
    pub label: String,
    pub path: PathBuf,
    pub role: ModuleRootRole,
}

impl ModuleRoot {
    pub fn workload(label: impl Into<String>, path: PathBuf) -> Self {
        Self {
            label: label.into(),
            path,
            role: ModuleRootRole::Workload,
        }
    }

    pub fn implementation(label: impl Into<String>, path: PathBuf) -> Self {
        Self {
            label: label.into(),
            path,
            role: ModuleRootRole::Implementation,
        }
    }
}

pub struct Fixture {
    pub name: String,
    pub family: String,
    pub stage: String,
    pub size: Option<usize>,
    pub source_dir: PathBuf,
    pub entry: String,
    pub module_roots: Vec<ModuleRoot>,
    pub inputs: Value,
    pub fixture_fingerprint: String,
    pub topology_fingerprint: Option<String>,
    pub fixture_files: Vec<PathBuf>,
    pub expected_error: Option<String>,
    pub policy: String,
    pub expected_artifact: Option<Value>,
    pub compiler_options: nefor_mag::CompilerOptions,
}

#[derive(Debug, Deserialize)]
struct LegacyManifest {
    legacy_combined_fingerprint: String,
    cases: Vec<LegacyCase>,
    oracles: Vec<LegacyCase>,
}

#[derive(Debug, Deserialize)]
struct LegacyCase {
    name: String,
    #[serde(default)]
    fixture_fingerprint: String,
}

fn manifest_catalog_fingerprint(entries: &[LegacyCase]) -> String {
    fingerprint(
        &entries
            .iter()
            .flat_map(|entry| {
                format!("{}:{}\n", entry.name, entry.fixture_fingerprint).into_bytes()
            })
            .collect::<Vec<_>>(),
    )
}

pub fn assert_catalog_membership(cases: &[CaseReport], oracles: &[OracleObservation]) {
    let historical: LegacyManifest =
        serde_json::from_str(include_str!("legacy_cycle2_manifest.json"))
            .expect("parse immutable cycle-2 manifest");
    let current: LegacyManifest =
        serde_json::from_str(include_str!("current_main_a0_manifest.json"))
            .expect("parse current-main A0 manifest");
    assert_eq!(
        historical.legacy_combined_fingerprint,
        LEGACY_COMBINED_FINGERPRINT
    );
    assert_eq!(
        current.legacy_combined_fingerprint,
        LEGACY_COMBINED_FINGERPRINT
    );
    assert_eq!(
        manifest_catalog_fingerprint(&historical.cases),
        LEGACY_WORKLOAD_FINGERPRINT
    );
    assert_eq!(
        cases.len(),
        historical.cases.len(),
        "inherited timed case count"
    );
    assert_eq!(cases.len(), current.cases.len(), "current A0 case count");
    for ((actual, historical), current) in cases.iter().zip(historical.cases).zip(current.cases) {
        assert_eq!(actual.name, historical.name, "inherited timed case order");
        assert_eq!(actual.name, current.name, "current A0 timed case order");
    }
    assert_eq!(
        historical.oracles.len(),
        25,
        "immutable legacy oracle count"
    );
    let retired_oracles = [
        "artifact-dead-named-resident-function-remains-callable",
        "resident-function-reads-captured-top-level-peer",
        "required-module-export-remains-available",
    ];
    for retired in retired_oracles {
        assert!(
            historical
                .oracles
                .iter()
                .any(|oracle| oracle.name == retired),
            "immutable legacy manifest retains retired oracle {retired}"
        );
        assert!(
            current.oracles.iter().all(|oracle| oracle.name != retired),
            "artifact-only catalog excludes retired oracle {retired}"
        );
    }
    let retained_historical = historical
        .oracles
        .iter()
        .filter(|oracle| !retired_oracles.contains(&oracle.name.as_str()));
    assert_eq!(oracles.len(), 22, "artifact-only oracle count");
    assert_eq!(
        oracles.len(),
        current.oracles.len(),
        "current A0 oracle count"
    );
    for ((actual, historical), current) in oracles
        .iter()
        .zip(retained_historical)
        .zip(current.oracles.iter())
    {
        assert_eq!(actual.name, historical.name, "retained legacy oracle order");
        assert_eq!(actual.name, current.name, "current A0 oracle order");
    }
}

pub fn run_case(case: &Fixture, samples: usize, warmups: usize) -> CaseReport {
    for _ in 0..warmups {
        assert_timed_outcome(case, compile_fixture(case, None));
    }
    let calibration_started = Instant::now();
    assert_timed_outcome(case, compile_fixture(case, None));
    let calibration_ns = nanos(calibration_started.elapsed());
    let batch_count = calibrate_batch_count(calibration_ns, 5_000_000);
    let seed = u64::from_le_bytes(
        Sha256::digest(case.name.as_bytes())[..8]
            .try_into()
            .expect("seed"),
    );
    let generated_order = balanced_pair_schedule(seed, samples);
    let mut wall_ns = Vec::with_capacity(samples);
    let mut hashes = Vec::new();
    let mut raw_samples = Vec::with_capacity(samples);
    for (block, order) in generated_order.iter().copied().enumerate() {
        let (baseline_batch_ns, candidate_batch_ns) = match order {
            PairOrder::AB => (
                measure_case_batch(case, batch_count, &mut hashes),
                measure_case_batch(case, batch_count, &mut hashes),
            ),
            PairOrder::BA => {
                let candidate = measure_case_batch(case, batch_count, &mut hashes);
                let baseline = measure_case_batch(case, batch_count, &mut hashes);
                (baseline, candidate)
            }
        };
        wall_ns.push(candidate_batch_ns / batch_count);
        raw_samples.push(PairedSample {
            block,
            generated_order: order,
            actual_order: order,
            batch_count,
            baseline_batch_ns,
            candidate_batch_ns,
        });
    }
    let self_comparison = PairedSelfComparison {
        seed,
        generated_order,
        batch_calibration_baseline_ns: calibration_ns,
        batch_count,
        analysis: analyze_paired_samples(&raw_samples, 1),
        raw_samples,
    };
    assert!(
        hashes.windows(2).all(|pair| pair[0] == pair[1]),
        "{} artifact hash changed",
        case.name
    );

    let mut profiles = Vec::with_capacity(samples);
    for _ in 0..samples {
        let profiler = nefor_mag::profile::CompileProfiler::new();
        let result = compile_fixture(case, Some(&profiler));
        if assert_timed_outcome(case, result).is_some() {
            profiles.push(profiler.snapshot());
        }
    }
    let counters = profiles.first().map(|profile| profile.counters.clone());
    if let Some(expected) = &counters {
        assert!(
            profiles.iter().all(|profile| &profile.counters == expected),
            "{} counters are nondeterministic",
            case.name
        );
        assert_counter_invariants(expected, &case.name);
    }
    let derived = counters.as_ref().map(derive_counters);
    CaseReport {
        name: case.name.clone(),
        family: case.family.clone(),
        stage: case.stage.clone(),
        size: case.size,
        fixture_fingerprint: case.fixture_fingerprint.clone(),
        topology_fingerprint: case.topology_fingerprint.clone(),
        outcome: if case.expected_error.is_some() {
            "expected_failure"
        } else {
            "success"
        }
        .into(),
        expected_error: case.expected_error.clone(),
        artifact_hash: hashes.first().cloned(),
        distribution_ns: distribution(&wall_ns),
        wall_ns,
        counters,
        derived,
        profiled_phase_median_ns: phase_medians(&profiles),
        profiled_invocations_are_separate: true,
        self_comparison: Some(self_comparison),
    }
}

fn measure_case_batch(case: &Fixture, batch_count: u64, hashes: &mut Vec<String>) -> u64 {
    let started = Instant::now();
    for _ in 0..batch_count {
        if let Some(artifact) = assert_timed_outcome(case, compile_fixture(case, None)) {
            hashes.push(fingerprint(&canonical_json_bytes(&artifact)));
            black_box(artifact);
        }
    }
    nanos(started.elapsed())
}

pub fn observe(case: &Fixture) -> OracleObservation {
    let observation = match compile_fixture(case, None) {
        Ok(artifact) => SemanticOutcome::Success {
            artifact_hash: fingerprint(&canonical_json_bytes(&artifact)),
            artifact_json: artifact,
        },
        Err(error) => SemanticOutcome::Error {
            class: error_class(&error).into(),
            message: error.to_string(),
            policy_stage: case.policy.clone(),
            ordered_diagnostics: vec![error.to_string()],
        },
    };
    validate_oracle(case, &observation);
    OracleObservation {
        name: case.name.clone(),
        policy: case.policy.clone(),
        expected_artifact: case.expected_artifact.clone(),
        observation,
    }
}

fn compile_fixture(
    case: &Fixture,
    profiler: Option<&nefor_mag::profile::CompileProfiler>,
) -> Result<Value, MagError> {
    let module_roots = case
        .module_roots
        .iter()
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();
    match profiler {
        Some(profiler) => nefor_mag::compile_file_with_profiler_and_options(
            &case.source_dir,
            &case.entry,
            case.inputs.clone(),
            &module_roots,
            profiler,
            case.compiler_options,
        ),
        None => nefor_mag::compile_file_with_inputs_and_module_roots_and_options(
            &case.source_dir,
            &case.entry,
            case.inputs.clone(),
            &module_roots,
            case.compiler_options,
        ),
    }
}

fn assert_timed_outcome(case: &Fixture, result: Result<Value, MagError>) -> Option<Value> {
    match (&case.expected_error, result) {
        (None, Ok(artifact)) => {
            if let Some(expected) = &case.expected_artifact {
                assert_eq!(&artifact, expected, "{} timed artifact", case.name);
            }
            Some(artifact)
        }
        (Some(expected), Err(error)) if error_class(&error) == expected => None,
        (None, Err(error)) => panic!("{} failed: {error}", case.name),
        (Some(expected), Err(error)) => panic!(
            "{} failed as {}, expected {expected}: {error}",
            case.name,
            error_class(&error)
        ),
        (Some(expected), Ok(_)) => panic!("{} succeeded, expected {expected}", case.name),
    }
}

fn validate_oracle(case: &Fixture, observation: &SemanticOutcome) {
    match (&case.expected_error, observation) {
        (None, SemanticOutcome::Success { artifact_json, .. }) => {
            if let Some(expected) = &case.expected_artifact {
                assert_eq!(artifact_json, expected, "{} artifact", case.name);
            }
        }
        (Some(expected), SemanticOutcome::Error { class, .. }) if expected == class => {}
        (Some(_), SemanticOutcome::Success { artifact_json, .. })
            if case.policy == "dead_local_may_elide" =>
        {
            assert_eq!(
                Some(artifact_json),
                case.expected_artifact.as_ref(),
                "{} allowed success artifact",
                case.name
            );
        }
        (_, other) => panic!("{} oracle violated: {other:?}", case.name),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn fixture(
    scratch: &Path,
    name: &str,
    family: &str,
    stage: &str,
    size: Option<usize>,
    source: &str,
    roots: Vec<ModuleRoot>,
    inputs: Value,
    expected_error: Option<&str>,
    policy: &str,
    expected_artifact: Option<Value>,
) -> Fixture {
    let dir = scratch.join(name);
    fs::create_dir_all(&dir).expect("create fixture dir");
    let entry = "main.mag";
    fs::write(dir.join(entry), source).expect("write fixture");
    let mut module_roots = vec![ModuleRoot::workload("fixture-source", dir.clone())];
    module_roots.extend(roots);
    Fixture {
        name: name.into(),
        family: family.into(),
        stage: stage.into(),
        size,
        source_dir: dir,
        entry: entry.into(),
        module_roots,
        inputs,
        fixture_fingerprint: String::new(),
        topology_fingerprint: None,
        fixture_files: vec![PathBuf::from(entry)],
        expected_error: expected_error.map(str::to_owned),
        policy: policy.into(),
        expected_artifact,
        compiler_options: nefor_mag::CompilerOptions::default(),
    }
}

pub fn write_module(case: &mut Fixture, relative: &str, source: &str) {
    let path = case.source_dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create module parent");
    }
    fs::write(path, source).expect("write fixture module");
    case.fixture_files.push(PathBuf::from(relative));
}

pub fn write_fixture_file(case: &mut Fixture, relative: &str, bytes: &[u8]) {
    let path = case.source_dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture file parent");
    }
    fs::write(path, bytes).expect("write fixture file");
    case.fixture_files.push(PathBuf::from(relative));
}

pub fn refresh_fixture_fingerprints(fixtures: &mut [Fixture]) {
    for fixture in fixtures {
        fixture.fixture_fingerprint = fixture_fingerprint(fixture);
    }
}

pub fn fixture_fingerprint(case: &Fixture) -> String {
    let mut digest = Sha256::new();
    hash_part(&mut digest, "entry", case.entry.as_bytes());
    hash_part(&mut digest, "inputs", &canonical_json_bytes(&case.inputs));
    hash_part(
        &mut digest,
        "expected_error",
        case.expected_error.as_deref().unwrap_or("").as_bytes(),
    );
    hash_part(&mut digest, "policy", case.policy.as_bytes());
    if case.compiler_options != nefor_mag::CompilerOptions::default() {
        hash_part(
            &mut digest,
            "compiler_limits",
            format!("{:?}", case.compiler_options.limits).as_bytes(),
        );
    }
    hash_part(
        &mut digest,
        "expected_artifact",
        &canonical_json_bytes(
            &serde_json::to_value(&case.expected_artifact).expect("expected artifact"),
        ),
    );
    // Artifact-only fixtures do not carry post-compilation probes.
    hash_part(&mut digest, "probes", b"[]");

    let mut fixture_files = case.fixture_files.clone();
    fixture_files.sort();
    fixture_files.dedup();
    for relative in fixture_files {
        let bytes = fs::read(case.source_dir.join(&relative))
            .unwrap_or_else(|error| panic!("read fixture file {}: {error}", relative.display()));
        hash_part(
            &mut digest,
            &format!("fixture:{}", relative.to_string_lossy()),
            &bytes,
        );
    }

    for (index, root) in case.module_roots.iter().enumerate() {
        hash_part(
            &mut digest,
            &format!("module-root:{index}:label"),
            root.label.as_bytes(),
        );
        hash_part(
            &mut digest,
            &format!("module-root:{index}:role"),
            root.role.marker().as_bytes(),
        );
        if root.role == ModuleRootRole::Implementation {
            continue;
        }
        let mut modules = Vec::new();
        collect_mag_files(&root.path, &root.path, &mut modules);
        modules.sort_by(|left, right| left.0.cmp(&right.0));
        for (relative, path) in modules {
            let bytes = fs::read(&path)
                .unwrap_or_else(|error| panic!("read module {}: {error}", path.display()));
            hash_part(
                &mut digest,
                &format!("module:{index}:{}", relative.to_string_lossy()),
                &bytes,
            );
        }
    }
    format!("sha256:{:x}", digest.finalize())
}

fn collect_mag_files(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, PathBuf)>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_mag_files(root, &path, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some("mag") {
            files.push((path.strip_prefix(root).unwrap_or(&path).to_path_buf(), path));
        }
    }
}

fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    fn canonicalize(value: &Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), canonicalize(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            value => value.clone(),
        }
    }
    serde_json::to_vec(&canonicalize(value)).expect("canonical JSON")
}

fn hash_part(digest: &mut Sha256, label: &str, bytes: &[u8]) {
    digest.update(label.len().to_le_bytes());
    digest.update(label.as_bytes());
    digest.update(bytes.len().to_le_bytes());
    digest.update(bytes);
}

pub fn fingerprint(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub fn lexical_product_positions(actor_ids: &[String]) -> Vec<usize> {
    let mut route_order = actor_ids.to_vec();
    route_order.sort();
    actor_ids
        .iter()
        .map(|actor_id| {
            route_order
                .binary_search(actor_id)
                .expect("actor id came from the route-order set")
        })
        .collect()
}

pub fn catalog_fingerprint(fixtures: &[Fixture]) -> String {
    fingerprint(
        &fixtures
            .iter()
            .flat_map(|fixture| {
                format!("{}:{}\n", fixture.name, fixture.fixture_fingerprint).into_bytes()
            })
            .collect::<Vec<_>>(),
    )
}

pub fn report_identity(
    root: &Path,
    workload_fingerprint: String,
    parent_workload_fingerprint: String,
    oracle_fingerprint: String,
) -> ReportIdentity {
    let executable_digest = std::env::current_exe()
        .ok()
        .and_then(|path| fs::read(path).ok())
        .map(|bytes| fingerprint(&bytes))
        .unwrap_or_else(|| "unavailable".into());
    ReportIdentity {
        report_schema_version: SCHEMA_VERSION,
        workload_catalog_version: PHASE0_WORKLOAD_CATALOG_VERSION.into(),
        workload_fingerprint,
        parent_workload_catalog_version: CURRENT_MAIN_A0_WORKLOAD_CATALOG_VERSION.into(),
        parent_workload_fingerprint,
        oracle_catalog_version: ORACLE_CATALOG_VERSION.into(),
        oracle_fingerprint,
        profiler_schema_version: PROFILER_SCHEMA_VERSION.into(),
        statistics_policy_version: STATISTICS_POLICY_VERSION.into(),
        source_ref: command_identity(root, &["git", "rev-parse", "HEAD"]),
        executable_digest,
    }
}

fn command_identity(root: &Path, args: &[&str]) -> String {
    std::process::Command::new(args[0])
        .args(&args[1..])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}

pub fn error_class(error: &MagError) -> &'static str {
    match error {
        MagError::Syntax(_) => "syntax",
        MagError::Lex(_) => "lex",
        MagError::Parse(_) => "parse",
        MagError::Eval(_) => "evaluation",
        MagError::Budget(_) => "budget",
        MagError::Type(_) => "type",
        MagError::Unresolved(_) => "unresolved",
        MagError::Arity { .. } => "arity",
    }
}

pub fn assert_counter_invariants(counters: &OperationCounters, name: &str) {
    assert!(
        counters.function_calls >= counters.user_function_calls + counters.builtin_calls,
        "{name} categorized more calls than the total function count"
    );
    assert_eq!(
        counters.builtin_calls,
        counters
            .builtin_calls_by_name
            .values()
            .copied()
            .sum::<u64>(),
        "{name} builtin partition"
    );
    assert!(
        counters.binding_force_initializations <= counters.binding_slots_declared,
        "{name} initialized more slots than declared"
    );
}

fn derive_counters(counters: &OperationCounters) -> DerivedCounters {
    DerivedCounters {
        unforced_at_load: counters
            .binding_slots_declared
            .saturating_sub(counters.binding_force_initializations),
        function_call_partition_holds: counters.function_calls
            >= counters.user_function_calls + counters.builtin_calls,
        builtin_name_partition_holds: counters.builtin_calls
            == counters
                .builtin_calls_by_name
                .values()
                .copied()
                .sum::<u64>(),
        force_result_attempts: counters.binding_force_initializations
            + counters.binding_force_ready_hits
            + counters.binding_force_cycles,
    }
}

fn distribution(samples: &[u64]) -> Distribution {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    Distribution {
        min: sorted.first().copied().unwrap_or(0),
        median: nearest_rank_quantile(&sorted, 50),
        mean: (sorted.iter().map(|&v| u128::from(v)).sum::<u128>() / sorted.len().max(1) as u128)
            as u64,
        p90: nearest_rank_quantile(&sorted, 90),
        p95: nearest_rank_quantile(&sorted, 95),
        max: sorted.last().copied().unwrap_or(0),
    }
}
/// Nearest-rank quantile: rank = ceil(p * n), using one-based ranks.
/// This conservative empirical policy makes p90 the maximum for n=3 and the
/// 27th sorted observation for n=30.
pub fn nearest_rank_quantile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() || percentile == 0 || percentile > 100 {
        return 0;
    }
    let rank = (percentile * sorted.len()).div_ceil(100);
    sorted.get(rank - 1).copied().unwrap_or(0)
}
fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
fn phase_medians(profiles: &[CompileProfile]) -> Option<PhaseDurations> {
    let serialized = profiles
        .iter()
        .map(|p| serde_json::to_value(&p.phases).expect("phase json"))
        .collect::<Vec<_>>();
    let keys = serialized
        .first()?
        .as_object()?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut result = BTreeMap::new();
    for key in keys {
        let mut values = serialized
            .iter()
            .map(|p| p[&key].as_u64().unwrap_or(0))
            .collect::<Vec<_>>();
        values.sort_unstable();
        result.insert(key, nearest_rank_quantile(&values, 50));
    }
    serde_json::from_value(serde_json::to_value(result).ok()?).ok()
}

pub fn counter_semantics() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "checked_bindings".into(),
            "successfully checked IR bindings, including recursively checked function bodies"
                .into(),
        ),
        (
            "checked_expressions".into(),
            "successfully checked IR expressions, including recursively checked children".into(),
        ),
        (
            "binding_force_*".into(),
            "exclusive result buckets for binding force attempts".into(),
        ),
        (
            "function_calls".into(),
            "all callable applications; nominal constructor calls are included in the total but not in user_function_calls or builtin_calls"
                .into(),
        ),
        (
            "builtin_input_items_by_name.concat".into(),
            "sum of copied input collection lengths per concat call".into(),
        ),
        (
            "builtin_input_items_by_name.collection".into(),
            "source collection length for map/group-by/indexed-map/filter/flat-map/fold/sort-by/remove-at"
                .into(),
        ),
        (
            "builtin_input_items_by_name.descriptor".into(),
            "descriptor list length for descriptor-input-assignments and descriptor-table".into(),
        ),
        (
            "builtin_copied_shifted_cloned_items_by_name".into(),
            "actual element clones/copies for concat and remove-at, with remove-at tail shifts reported separately".into(),
        ),
        (
            "runtime_value_validation_visits".into(),
            "every recursive validate_value visit".into(),
        ),
        (
            "value_equality_visits".into(),
            "every recursive value equality visit".into(),
        ),
    ])
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComparisonArtifact {
    pub schema_version: u8,
    pub mode: String,
    pub baseline: SourceIdentity,
    pub candidate: SourceIdentity,
    pub thresholds: Thresholds,
    pub quantile_policy: String,
    pub compatibility: GateVerdict,
    pub semantic: GateVerdict,
    pub clean_candidate: GateVerdict,
    pub target_median: GateVerdict,
    pub target_logical_counter: GateVerdict,
    pub all_case_p90: GateVerdict,
    pub cases: Vec<CaseComparison>,
    pub oracles: Vec<OracleComparison>,
    pub overall: GateVerdict,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub commit: String,
    pub tree: String,
    pub dirty: bool,
    pub diff_digest: Option<String>,
    pub workload: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Thresholds {
    pub target_median_ratio_max: f64,
    pub target_logical_counter_reduction_min: f64,
    pub case_p90_ratio_max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateVerdict {
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CaseComparison {
    pub name: String,
    pub median_ratio: f64,
    pub p90_ratio: f64,
    pub logical_counter_deltas: BTreeMap<String, i128>,
    pub baseline_semantic: TimedSemanticResult,
    pub candidate_semantic: TimedSemanticResult,
    pub semantic_match: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TimedSemanticResult {
    pub outcome: String,
    pub expected_error: Option<String>,
    pub artifact_hash: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OracleComparison {
    pub name: String,
    pub baseline_observation: SemanticOutcome,
    pub candidate_observation: SemanticOutcome,
    pub semantic_match: bool,
}

pub fn compare_reports(baseline: &Report, candidate: &Report, gate: bool) -> ComparisonArtifact {
    let mut compatibility_failures = compatibility_failures(baseline, candidate);
    if gate && baseline.identity.is_some() && candidate.identity.is_some() {
        compatibility_failures.push(
            "same-version gate requires an explicit paired/interleaved runner artifact; independent marginal reports are diagnostic only".into(),
        );
    }
    let compatible = compatibility_failures.is_empty();
    let mut cases = Vec::new();
    let mut timed_semantics = compatible;
    if compatible {
        for (before, after) in baseline.cases.iter().zip(&candidate.cases) {
            let semantic_match = before.name == after.name
                && before.fixture_fingerprint == after.fixture_fingerprint
                && before.outcome == after.outcome
                && before.expected_error == after.expected_error
                && before.artifact_hash == after.artifact_hash;
            timed_semantics &= semantic_match;
            cases.push(CaseComparison {
                name: before.name.clone(),
                median_ratio: ratio(after.distribution_ns.median, before.distribution_ns.median),
                p90_ratio: ratio(after.distribution_ns.p90, before.distribution_ns.p90),
                logical_counter_deltas: counter_deltas(
                    before.counters.as_ref(),
                    after.counters.as_ref(),
                ),
                baseline_semantic: timed_semantic(before),
                candidate_semantic: timed_semantic(after),
                semantic_match,
            });
        }
    }
    let mut oracles = Vec::new();
    let mut oracle_semantics = compatible;
    if compatible {
        for (before, after) in baseline.oracles.iter().zip(&candidate.oracles) {
            let semantic_match = before.name == after.name
                && before.policy == after.policy
                && semantic_accepts(before, after);
            oracle_semantics &= semantic_match;
            oracles.push(OracleComparison {
                name: before.name.clone(),
                baseline_observation: before.observation.clone(),
                candidate_observation: after.observation.clone(),
                semantic_match,
            });
        }
    }
    let semantic_passed = timed_semantics && oracle_semantics;

    let (target_case, target_counter) = target_case_and_counter(baseline);
    let target_comparison = target_case.and_then(|target| {
        cases
            .iter()
            .find(|comparison| comparison.name == target.name)
            .map(|comparison| (target, comparison))
    });
    let target_median = match target_comparison {
        Some((target, comparison)) => verdict(
            comparison.median_ratio <= MAX_TARGET_MEDIAN_RATIO,
            format!(
                "{} median ratio {:.5} (required <= {:.2})",
                target.name, comparison.median_ratio, MAX_TARGET_MEDIAN_RATIO
            ),
        ),
        None => verdict(false, "recommended target case is unavailable".into()),
    };
    let target_logical_counter = match (target_comparison, target_counter) {
        (Some((target, comparison)), Some(counter)) => {
            let before = counter_value(target.counters.as_ref(), counter);
            let delta = comparison
                .logical_counter_deltas
                .get(counter)
                .copied()
                .unwrap_or(0);
            let reduction = if before == 0 {
                0.0
            } else {
                -(delta as f64) / before as f64
            };
            verdict(
                reduction >= MIN_TARGET_COUNTER_REDUCTION,
                format!(
                    "{} {} reduction {:.5}% (required >= {:.0}%)",
                    target.name,
                    counter,
                    reduction * 100.0,
                    MIN_TARGET_COUNTER_REDUCTION * 100.0
                ),
            )
        }
        _ => verdict(false, "recommended target counter is unavailable".into()),
    };
    let compatibility = verdict(
        compatible,
        if compatible {
            "workload definitions and benchmark environments match".into()
        } else {
            compatibility_failures.join("; ")
        },
    );
    let semantic = verdict(
        semantic_passed,
        if semantic_passed {
            "all timed artifacts and independent oracle outcomes match".into()
        } else {
            "one or more timed artifacts or oracle outcomes differ".into()
        },
    );
    let clean_candidate = verdict(
        !candidate.metadata.git_dirty,
        if candidate.metadata.git_dirty {
            "candidate tree is dirty; gate mode requires a clean commit/tree identity".into()
        } else {
            "candidate commit and tree are clean".into()
        },
    );
    let p90_violations = cases
        .iter()
        .filter(|comparison| comparison.p90_ratio > MAX_CASE_P90_RATIO)
        .map(|comparison| format!("{} ({:.5})", comparison.name, comparison.p90_ratio))
        .collect::<Vec<_>>();
    let all_case_p90 = verdict(
        p90_violations.is_empty(),
        if p90_violations.is_empty() {
            format!("every timed case p90 ratio is <= {:.2}", MAX_CASE_P90_RATIO)
        } else {
            format!(
                "timed cases exceeding p90 ratio {:.2}: {}",
                MAX_CASE_P90_RATIO,
                p90_violations.join(", ")
            )
        },
    );
    let passed = compatibility.passed
        && semantic.passed
        && clean_candidate.passed
        && target_median.passed
        && target_logical_counter.passed
        && all_case_p90.passed;
    ComparisonArtifact {
        schema_version: COMPARISON_SCHEMA_VERSION,
        mode: if gate { "gate" } else { "comparison" }.into(),
        baseline: source_identity(baseline),
        candidate: source_identity(candidate),
        thresholds: Thresholds {
            target_median_ratio_max: MAX_TARGET_MEDIAN_RATIO,
            target_logical_counter_reduction_min: MIN_TARGET_COUNTER_REDUCTION,
            case_p90_ratio_max: MAX_CASE_P90_RATIO,
        },
        quantile_policy: candidate.metadata.quantile_policy.clone(),
        compatibility,
        semantic,
        clean_candidate,
        target_median,
        target_logical_counter,
        all_case_p90,
        cases,
        oracles,
        overall: verdict(
            passed,
            if passed {
                "candidate satisfies every required gate".into()
            } else {
                "candidate failed one or more required gates".into()
            },
        ),
    }
}

fn timed_semantic(case: &CaseReport) -> TimedSemanticResult {
    TimedSemanticResult {
        outcome: case.outcome.clone(),
        expected_error: case.expected_error.clone(),
        artifact_hash: case.artifact_hash.clone(),
    }
}

fn source_identity(report: &Report) -> SourceIdentity {
    SourceIdentity {
        commit: report.metadata.git_commit.clone(),
        tree: report.metadata.git_tree.clone(),
        dirty: report.metadata.git_dirty,
        diff_digest: report.metadata.git_diff_digest.clone(),
        workload: report.metadata.case_definition_hash.clone(),
    }
}

#[allow(dead_code)]
pub fn identities_gate_compatible(left: &ReportIdentity, right: &ReportIdentity) -> bool {
    left.report_schema_version == right.report_schema_version
        && left.workload_catalog_version == right.workload_catalog_version
        && left.workload_fingerprint == right.workload_fingerprint
        && left.oracle_catalog_version == right.oracle_catalog_version
        && left.oracle_fingerprint == right.oracle_fingerprint
        && left.profiler_schema_version == right.profiler_schema_version
        && left.statistics_policy_version == right.statistics_policy_version
}

#[allow(dead_code)]
pub fn reports_gate_compatible(baseline: &Report, candidate: &Report) -> bool {
    compatibility_failures(baseline, candidate).is_empty()
}

fn compatibility_failures(baseline: &Report, candidate: &Report) -> Vec<String> {
    let b = &baseline.metadata;
    let c = &candidate.metadata;
    let mut failures = Vec::new();
    match (&baseline.identity, &candidate.identity) {
        (Some(left), Some(right)) => {
            for (label, same) in [
                (
                    "workload catalog version",
                    left.workload_catalog_version == right.workload_catalog_version,
                ),
                (
                    "workload fingerprint",
                    left.workload_fingerprint == right.workload_fingerprint,
                ),
                (
                    "oracle catalog version",
                    left.oracle_catalog_version == right.oracle_catalog_version,
                ),
                (
                    "oracle fingerprint",
                    left.oracle_fingerprint == right.oracle_fingerprint,
                ),
                (
                    "profiler schema version",
                    left.profiler_schema_version == right.profiler_schema_version,
                ),
                (
                    "statistics policy version",
                    left.statistics_policy_version == right.statistics_policy_version,
                ),
            ] {
                if !same {
                    failures.push(format!("incompatible {label}"));
                }
            }
        }
        (None, None)
            if b.case_definition_hash == LEGACY_COMBINED_FINGERPRINT
                && c.case_definition_hash == LEGACY_COMBINED_FINGERPRINT => {}
        _ => failures.push("incompatible explicit report identity".into()),
    }
    for (label, same) in [
        (
            "report schema",
            baseline.schema_version == candidate.schema_version,
        ),
        ("package version", b.package_version == c.package_version),
        ("target", b.target == c.target),
        ("OS", b.os == c.os),
        ("architecture", b.arch == c.arch),
        ("profile", b.profile == c.profile),
        ("samples", b.samples_per_case == c.samples_per_case),
        ("warmups", b.warmup_iterations == c.warmup_iterations),
        ("quantile policy", b.quantile_policy == c.quantile_policy),
        (
            "workload definition",
            b.case_definition_hash == c.case_definition_hash,
        ),
        (
            "size replacements",
            b.size_replacements == c.size_replacements,
        ),
        ("case count", baseline.cases.len() == candidate.cases.len()),
        (
            "oracle count",
            baseline.oracles.len() == candidate.oracles.len(),
        ),
    ] {
        if !same {
            failures.push(format!("incompatible {label}"));
        }
    }
    failures
}

fn target_case_and_counter(report: &Report) -> (Option<&CaseReport>, Option<&'static str>) {
    let target = match report.recommendation.candidate.as_deref() {
        Some("function-local demand selection") => (
            "core-dead-locals",
            "compile",
            "binding_force_initializations",
        ),
        Some("generic collection copying") => (
            "core-concat-growth",
            "compile",
            "builtin_input_items_by_name.concat",
        ),
        Some("Nefor graph validation") => ("nefor-linear", "validate", "value_equality_visits"),
        Some("Nefor graph lowering") => (
            "nefor-fan-in",
            "lower",
            "builtin_input_items_by_name.descriptor-input-assignments",
        ),
        _ => return (None, None),
    };
    let case = report
        .cases
        .iter()
        .filter(|case| case.family == target.0 && case.stage == target.1)
        .max_by_key(|case| case.size.unwrap_or(0));
    (case, Some(target.2))
}

fn verdict(passed: bool, detail: String) -> GateVerdict {
    GateVerdict { passed, detail }
}

fn semantic_accepts(before: &OracleObservation, after: &OracleObservation) -> bool {
    if before.observation == after.observation {
        return true;
    }
    matches!((&before.observation, &after.observation),
        (SemanticOutcome::Error { .. }, SemanticOutcome::Success { artifact_json, .. })
        if before.policy == "dead_local_may_elide" && before.expected_artifact.as_ref() == Some(artifact_json))
}

fn ratio(after: u64, before: u64) -> f64 {
    if before == 0 {
        0.0
    } else {
        after as f64 / before as f64
    }
}

fn counter_deltas(
    before: Option<&OperationCounters>,
    after: Option<&OperationCounters>,
) -> BTreeMap<String, i128> {
    let (Some(before), Some(after)) = (before, after) else {
        return BTreeMap::new();
    };
    let before = serde_json::to_value(before).expect("counter json");
    let after = serde_json::to_value(after).expect("counter json");
    let mut deltas = BTreeMap::new();
    collect_counter_deltas("", &before, &after, &mut deltas);
    deltas
}

fn collect_counter_deltas(
    prefix: &str,
    before: &Value,
    after: &Value,
    deltas: &mut BTreeMap<String, i128>,
) {
    match (before, after) {
        (Value::Number(before), Value::Number(after)) => {
            deltas.insert(
                prefix.into(),
                after.as_u64().unwrap_or(0) as i128 - before.as_u64().unwrap_or(0) as i128,
            );
        }
        (Value::Object(before), Value::Object(after)) => {
            let keys = before
                .keys()
                .chain(after.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for key in keys {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_counter_deltas(
                    &path,
                    before.get(key).unwrap_or(&Value::Null),
                    after.get(key).unwrap_or(&Value::Null),
                    deltas,
                );
            }
        }
        (Value::Null, Value::Number(after)) => {
            deltas.insert(prefix.into(), after.as_u64().unwrap_or(0) as i128);
        }
        (Value::Number(before), Value::Null) => {
            deltas.insert(prefix.into(), -(before.as_u64().unwrap_or(0) as i128));
        }
        _ => {}
    }
}

pub fn derived_exclusive_sections(_cases: &[CaseReport]) -> Vec<ExclusiveCounters> {
    // Independent marginal reports do not carry the forcing and complete
    // fixture evidence required for exclusive subtraction. Paired worker
    // reports own stage attribution and retain that evidence explicitly.
    Vec::new()
}

pub fn recommendation(cases: &[CaseReport]) -> Recommendation {
    let candidates = [
        (
            "function-local demand selection",
            "core-dead-locals",
            "compile",
            "binding_force_initializations",
        ),
        (
            "generic collection copying",
            "core-concat-growth",
            "compile",
            "builtin_input_items_by_name.concat",
        ),
        (
            "Nefor graph validation",
            "nefor-linear",
            "validate",
            "value_equality_visits",
        ),
        (
            "Nefor graph lowering",
            "nefor-fan-in",
            "lower",
            "builtin_input_items_by_name.descriptor-input-assignments",
        ),
    ];
    let mut ranked = candidates
        .iter()
        .filter_map(|(candidate, family, stage, counter)| {
            let mut targeted = cases
                .iter()
                .filter(|case| case.family == *family && case.stage == *stage)
                .collect::<Vec<_>>();
            targeted.sort_by_key(|case| case.size.unwrap_or(0));
            let first = *targeted.first()?;
            let last = *targeted.last()?;
            Some((*candidate, *counter, first, last))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(_, _, _, last)| std::cmp::Reverse(last.distribution_ns.median));
    match ranked.first() {
        Some((candidate, counter, first, last)) => {
            let first_counter = counter_value(first.counters.as_ref(), counter);
            let last_counter = counter_value(last.counters.as_ref(), counter);
            Recommendation {
                candidate: Some((*candidate).into()),
                evidence: format!(
                    "targeted median grows from {} ns at size {} to {} ns at size {}; {counter} grows from {first_counter} to {last_counter}. This selects the first experiment only; the candidate must still pass every semantic and performance gate.",
                    first.distribution_ns.median,
                    first.size.unwrap_or(0),
                    last.distribution_ns.median,
                    last.size.unwrap_or(0),
                ),
            }
        }
        None => Recommendation {
            candidate: None,
            evidence: "no complete targeted family was measured".into(),
        },
    }
}

fn counter_value(counters: Option<&OperationCounters>, path: &str) -> u64 {
    let Some(counters) = counters else {
        return 0;
    };
    match path {
        "binding_force_initializations" => counters.binding_force_initializations,
        "value_equality_visits" => counters.value_equality_visits,
        name if name.starts_with("builtin_input_items_by_name.") => counters
            .builtin_input_items_by_name
            .get(name.trim_start_matches("builtin_input_items_by_name."))
            .copied()
            .unwrap_or(0),
        _ => 0,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairedSelfComparison {
    pub seed: u64,
    pub generated_order: Vec<PairOrder>,
    pub batch_calibration_baseline_ns: u64,
    pub batch_count: u64,
    pub raw_samples: Vec<PairedSample>,
    pub analysis: PairedAnalysis,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PairOrder {
    AB,
    BA,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedSample {
    pub block: usize,
    pub generated_order: PairOrder,
    pub actual_order: PairOrder,
    pub batch_count: u64,
    pub baseline_batch_ns: u64,
    pub candidate_batch_ns: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceVerdict {
    Pass,
    Regression,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairedAnalysis {
    pub paired_median_ratio: f64,
    pub empirical_p90_ratio: f64,
    pub lower_confidence_bound: f64,
    pub upper_confidence_bound: f64,
    pub verdict: ConfidenceVerdict,
    pub confidence_method: String,
    pub family_wise_error_policy: String,
}

pub fn balanced_pair_schedule(seed: u64, blocks: usize) -> Vec<PairOrder> {
    let mut state = seed.max(1);
    let mut schedule = Vec::with_capacity(blocks);
    for pair in 0..blocks.div_ceil(2) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let first = if (state ^ pair as u64) & 1 == 0 {
            PairOrder::AB
        } else {
            PairOrder::BA
        };
        schedule.push(first);
        if schedule.len() < blocks {
            schedule.push(match first {
                PairOrder::AB => PairOrder::BA,
                PairOrder::BA => PairOrder::AB,
            });
        }
    }
    schedule
}

pub fn calibrate_batch_count(baseline_warmup_ns: u64, target_batch_ns: u64) -> u64 {
    if baseline_warmup_ns == 0 {
        return 1;
    }
    target_batch_ns.div_ceil(baseline_warmup_ns).max(1)
}

pub fn analyze_paired_samples(
    samples: &[PairedSample],
    family_case_count: usize,
) -> PairedAnalysis {
    assert!(!samples.is_empty(), "paired analysis requires samples");
    assert!(samples
        .iter()
        .all(|sample| sample.batch_count == samples[0].batch_count));
    let mut log_ratios = samples
        .iter()
        .map(|sample| {
            (sample.candidate_batch_ns as f64 / sample.baseline_batch_ns.max(1) as f64).ln()
        })
        .collect::<Vec<_>>();
    log_ratios.sort_by(f64::total_cmp);
    let median = f64_quantile(&log_ratios, 0.5).exp();
    let p90 = f64_quantile(&log_ratios, 0.9).exp();

    // Deterministic percentile bootstrap on paired observations. Bonferroni
    // allocates the family-wise 5% error budget across every timed case and
    // both one-sided bounds without pretending the cases are independent.
    let tails = family_tail_probability(family_case_count).clamp(0.000_001, 0.025);
    let mut state = 0x9e37_79b9_7f4a_7c15_u64 ^ samples.len() as u64;
    let mut p90_statistics = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    for _ in 0..BOOTSTRAP_RESAMPLES {
        let mut draw = Vec::with_capacity(log_ratios.len());
        for _ in 0..log_ratios.len() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            draw.push(log_ratios[(state as usize) % log_ratios.len()]);
        }
        draw.sort_by(f64::total_cmp);
        p90_statistics.push(f64_quantile(&draw, 0.9).exp());
    }
    p90_statistics.sort_by(f64::total_cmp);
    let lower = f64_quantile(&p90_statistics, tails);
    let upper = f64_quantile(&p90_statistics, 1.0 - tails);
    let verdict = if upper <= MAX_CASE_P90_RATIO {
        ConfidenceVerdict::Pass
    } else if lower > MAX_CASE_P90_RATIO {
        ConfidenceVerdict::Regression
    } else {
        ConfidenceVerdict::Inconclusive
    };
    PairedAnalysis {
        paired_median_ratio: median,
        empirical_p90_ratio: p90,
        lower_confidence_bound: lower,
        upper_confidence_bound: upper,
        verdict,
        confidence_method:
            "deterministic paired nearest-rank p90 percentile bootstrap (131072 resamples)".into(),
        family_wise_error_policy:
            "Bonferroni 5% family-wise error across cases and one-sided bounds".into(),
    }
}

fn f64_quantile(sorted: &[f64], probability: f64) -> f64 {
    let rank = ((probability * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExclusiveCounters {
    pub label: String,
    pub topology_fingerprint: String,
    pub counters: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerEndpoint {
    pub executable_path: PathBuf,
    pub clean_source_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSourceIdentity {
    pub source_root: PathBuf,
    pub source_ref: String,
    pub tree: String,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerExecutableIdentity {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    TimedBatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerRequest {
    pub sequence: usize,
    pub case_fingerprint: String,
    pub batch_count: u64,
    pub measurement_kind: MeasurementKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerResponse {
    pub sequence: usize,
    pub case_name: String,
    pub case_fingerprint: String,
    pub executable_identity: WorkerExecutableIdentity,
    pub source_identity: WorkerSourceIdentity,
    pub semantic_observation: SemanticOutcome,
    pub logical_counters: Option<OperationCounters>,
    pub raw_batch_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerCaseManifest {
    pub name: String,
    pub family: String,
    pub stage: String,
    pub policy: String,
    pub size: Option<usize>,
    pub case_fingerprint: String,
    pub workload_fingerprint: String,
    pub fixture_source_fingerprint: String,
    pub topology_fingerprint: Option<String>,
    pub forced_terminal_binding: String,
    pub forcing_dependency_proof: Option<String>,
    pub declared_direct_prerequisite: Option<String>,
    pub expected_semantic_artifact_fingerprint: String,
    pub semantic_observation: SemanticOutcome,
    pub logical_counters: Option<OperationCounters>,
    pub baseline_warmup_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerHello {
    pub protocol_version: String,
    pub executable_identity: WorkerExecutableIdentity,
    pub source_identity: WorkerSourceIdentity,
    pub benchmark_definition_identity: String,
    pub workload_catalog_version: String,
    pub oracle_catalog_version: String,
    pub statistics_policy_version: String,
    pub warmup_iterations: usize,
    pub cases: Vec<WorkerCaseManifest>,
    pub oracles: Vec<OracleObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttributionKind {
    Exclusive,
    Inclusive,
    InclusiveAfterProvenAnalysisPrefix,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageAttribution {
    pub label: String,
    pub kind: AttributionKind,
    pub workload_fingerprint: String,
    pub fixture_source_fingerprint: String,
    pub topology_fingerprint: Option<String>,
    pub forced_terminal_binding: String,
    pub forcing_dependency_proof: Option<String>,
    pub declared_direct_prerequisite: Option<String>,
    pub expected_semantic_artifact_fingerprint: String,
    pub prefix_proven: bool,
    pub counters: BTreeMap<String, u64>,
}

pub fn semantic_fingerprint(observation: &SemanticOutcome) -> String {
    fingerprint(&canonical_json_bytes(
        &serde_json::to_value(observation).expect("semantic observation"),
    ))
}

pub fn forced_terminal_binding(stage: &str) -> &'static str {
    match stage {
        "analysis" => "analysis-summary-artifact",
        "forward-reachability" => "forward-proof",
        "both-reachability" => "reverse-proof",
        "lower" => "canonical-lowered-artifact",
        _ => "entry-artifact",
    }
}

pub fn declared_direct_prerequisite(stage: &str) -> Option<&'static str> {
    match stage {
        "forward-reachability" => Some("analysis"),
        "both-reachability" => Some("forward-reachability"),
        // Lowering invokes analyze-graph internally. Its analysis prefix is
        // observable but the remaining work cannot be isolated by subtraction.
        "lower" => Some("analysis"),
        _ => None,
    }
}

pub fn stage_attribution(
    smaller: &WorkerCaseManifest,
    larger: &WorkerCaseManifest,
) -> StageAttribution {
    let complete_match = smaller.workload_fingerprint == larger.workload_fingerprint
        && smaller.fixture_source_fingerprint == larger.fixture_source_fingerprint
        && smaller.topology_fingerprint == larger.topology_fingerprint;
    let forcing_proven = larger.declared_direct_prerequisite.as_deref() == Some(&smaller.stage)
        && !smaller.forced_terminal_binding.is_empty()
        && !larger.forced_terminal_binding.is_empty()
        && larger
            .forcing_dependency_proof
            .as_deref()
            .is_some_and(|proof| !proof.is_empty());
    let deltas = counter_deltas(
        smaller.logical_counters.as_ref(),
        larger.logical_counters.as_ref(),
    );
    let nonnegative = deltas.values().all(|delta| *delta >= 0);
    let prefix_proven = complete_match && forcing_proven && nonnegative;
    let kind = if larger.stage == "lower" && prefix_proven {
        AttributionKind::InclusiveAfterProvenAnalysisPrefix
    } else if prefix_proven {
        AttributionKind::Exclusive
    } else {
        AttributionKind::Inclusive
    };
    StageAttribution {
        label: match kind {
            AttributionKind::Exclusive => format!("{} exclusive", larger.stage),
            AttributionKind::Inclusive => format!("{} inclusive", larger.stage),
            AttributionKind::InclusiveAfterProvenAnalysisPrefix => {
                "lowering inclusive after proven analysis prefix".into()
            }
        },
        kind,
        workload_fingerprint: larger.workload_fingerprint.clone(),
        fixture_source_fingerprint: larger.fixture_source_fingerprint.clone(),
        topology_fingerprint: larger.topology_fingerprint.clone(),
        forced_terminal_binding: larger.forced_terminal_binding.clone(),
        forcing_dependency_proof: larger.forcing_dependency_proof.clone(),
        declared_direct_prerequisite: larger.declared_direct_prerequisite.clone(),
        expected_semantic_artifact_fingerprint: larger
            .expected_semantic_artifact_fingerprint
            .clone(),
        prefix_proven,
        counters: if prefix_proven && larger.stage != "lower" {
            deltas
                .into_iter()
                .filter_map(|(name, value)| u64::try_from(value).ok().map(|value| (name, value)))
                .collect()
        } else {
            BTreeMap::new()
        },
    }
}

pub fn validate_worker_pair(
    baseline: &WorkerHello,
    candidate: &WorkerHello,
    calibration: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    if baseline.protocol_version != WORKER_PROTOCOL_VERSION
        || candidate.protocol_version != WORKER_PROTOCOL_VERSION
        || baseline.protocol_version != candidate.protocol_version
    {
        failures.push("worker protocol mismatch".into());
    }
    if baseline.statistics_policy_version != STATISTICS_POLICY_VERSION
        || candidate.statistics_policy_version != STATISTICS_POLICY_VERSION
        || baseline.workload_catalog_version != candidate.workload_catalog_version
        || baseline.oracle_catalog_version != candidate.oracle_catalog_version
        || baseline.warmup_iterations != candidate.warmup_iterations
    {
        failures.push("worker benchmark policy mismatch".into());
    }
    if baseline.benchmark_definition_identity != candidate.benchmark_definition_identity {
        failures.push("benchmark workload definition mismatch".into());
    }
    if baseline.cases.len() != candidate.cases.len()
        || baseline
            .cases
            .iter()
            .zip(&candidate.cases)
            .any(|(left, right)| {
                left.name != right.name
                    || left.policy != right.policy
                    || left.case_fingerprint != right.case_fingerprint
                    || left.workload_fingerprint != right.workload_fingerprint
                    || left.fixture_source_fingerprint != right.fixture_source_fingerprint
                    || left.topology_fingerprint != right.topology_fingerprint
            })
    {
        failures.push("worker workload or fixture mismatch".into());
    }
    if baseline.oracles.len() != candidate.oracles.len()
        || baseline
            .oracles
            .iter()
            .zip(&candidate.oracles)
            .any(|(left, right)| left.name != right.name || left.policy != right.policy)
    {
        failures.push("worker oracle policy mismatch".into());
    }
    if baseline.source_identity.dirty || candidate.source_identity.dirty {
        failures.push("worker source root is dirty".into());
    }
    let identical = baseline.executable_identity.sha256 == candidate.executable_identity.sha256
        || (baseline.source_identity.source_ref == candidate.source_identity.source_ref
            && baseline.source_identity.tree == candidate.source_identity.tree);
    if identical && !calibration {
        failures.push(
            "identical endpoints are calibration-only and cannot satisfy an optimization gate"
                .into(),
        );
    }
    failures
}

pub fn observations_match(left: &SemanticOutcome, right: &SemanticOutcome) -> bool {
    left == right
}

#[allow(dead_code)]
pub fn next_escalation_sample_count(current: usize, inconclusive: bool) -> Option<usize> {
    if !inconclusive {
        return None;
    }
    [30, 60, 120].into_iter().find(|count| *count > current)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairedCaseReport {
    pub name: String,
    pub family: String,
    pub stage: String,
    pub seed: u64,
    pub generated_order: Vec<PairOrder>,
    pub actual_order: Vec<PairOrder>,
    pub batch_count: u64,
    pub raw_paired_batch_samples: Vec<PairedSample>,
    pub analysis: PairedAnalysis,
    pub baseline_semantic_observation: SemanticOutcome,
    pub candidate_semantic_observation: SemanticOutcome,
    pub semantic_match: bool,
    pub baseline_logical_counters: Option<OperationCounters>,
    pub candidate_logical_counters: Option<OperationCounters>,
    pub baseline_attribution: Option<StageAttribution>,
    pub candidate_attribution: Option<StageAttribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairedWorkerReport {
    pub schema_version: u8,
    pub report_kind: String,
    pub authoritative: bool,
    pub status: String,
    pub protocol_version: String,
    pub benchmark_definition_identity: String,
    pub statistics_policy: StatisticsPolicy,
    pub baseline: WorkerHello,
    pub candidate: WorkerHello,
    pub compatibility: GateVerdict,
    pub semantic: GateVerdict,
    pub performance: GateVerdict,
    pub target_case: Option<String>,
    pub target_counter: Option<String>,
    pub target_median: GateVerdict,
    pub target_logical_counter: GateVerdict,
    pub cases: Vec<PairedCaseReport>,
    pub baseline_stage_attributions: Vec<StageAttribution>,
    pub candidate_stage_attributions: Vec<StageAttribution>,
    pub overall: GateVerdict,
}

pub fn paired_target_verdicts(
    case: Option<&PairedCaseReport>,
    counter: Option<&str>,
) -> (GateVerdict, GateVerdict) {
    let Some(case) = case else {
        return (
            verdict(
                false,
                "paired optimization gate requires a selected target case".into(),
            ),
            verdict(
                false,
                "paired optimization gate requires a selected target logical counter".into(),
            ),
        );
    };
    let median = verdict(
        case.analysis.paired_median_ratio <= MAX_TARGET_MEDIAN_RATIO,
        format!(
            "{} paired median ratio {:.5} must be <= {:.2}",
            case.name, case.analysis.paired_median_ratio, MAX_TARGET_MEDIAN_RATIO
        ),
    );
    let Some(counter) = counter else {
        return (
            median,
            verdict(
                false,
                "paired optimization gate requires a selected target logical counter".into(),
            ),
        );
    };
    let baseline = serialized_counter_value(case.baseline_logical_counters.as_ref(), counter);
    let candidate = serialized_counter_value(case.candidate_logical_counters.as_ref(), counter);
    let logical = match (baseline, candidate) {
        (Some(baseline), Some(candidate)) if baseline > 0 && candidate <= baseline => {
            let reduction = 1.0 - candidate as f64 / baseline as f64;
            verdict(
                reduction >= MIN_TARGET_COUNTER_REDUCTION,
                format!(
                    "{} {counter} reduction {:.5} ({baseline} -> {candidate}) must be >= {:.2}",
                    case.name, reduction, MIN_TARGET_COUNTER_REDUCTION
                ),
            )
        }
        (Some(0), _) => verdict(false, format!("{} baseline {counter} is zero", case.name)),
        (Some(baseline), Some(candidate)) => verdict(
            false,
            format!(
                "{} {counter} regressed ({baseline} -> {candidate})",
                case.name
            ),
        ),
        _ => verdict(
            false,
            format!("{} does not expose logical counter {counter}", case.name),
        ),
    };
    (median, logical)
}

fn serialized_counter_value(counters: Option<&OperationCounters>, path: &str) -> Option<u64> {
    let value = serde_json::to_value(counters?).ok()?;
    path.split('.')
        .try_fold(&value, |current, segment| current.get(segment))?
        .as_u64()
}

pub fn warm_worker_case(case: &Fixture, iterations: usize) -> u64 {
    let mut elapsed = 0;
    for _ in 0..iterations.max(1) {
        let started = Instant::now();
        assert_timed_outcome(case, compile_fixture(case, None));
        elapsed = nanos(started.elapsed());
    }
    elapsed
}

pub fn measure_worker_batch(case: &Fixture, batch_count: u64) -> u64 {
    let started = Instant::now();
    for _ in 0..batch_count {
        if let Some(artifact) = assert_timed_outcome(case, compile_fixture(case, None)) {
            black_box(artifact);
        }
    }
    nanos(started.elapsed())
}

pub fn worker_case_observation(case: &Fixture) -> SemanticOutcome {
    observe(case).observation
}

pub fn worker_case_counters(case: &Fixture) -> Option<OperationCounters> {
    let profiler = nefor_mag::profile::CompileProfiler::new();
    assert_timed_outcome(case, compile_fixture(case, Some(&profiler)));
    let counters = profiler.snapshot().counters;
    assert_counter_invariants(&counters, &case.name);
    Some(counters)
}

pub fn executable_identity(path: &Path) -> Result<WorkerExecutableIdentity, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("canonicalize executable {}: {error}", path.display()))?;
    let bytes = fs::read(&canonical)
        .map_err(|error| format!("read executable {}: {error}", canonical.display()))?;
    Ok(WorkerExecutableIdentity {
        path: canonical,
        sha256: fingerprint(&bytes),
    })
}

pub fn family_tail_probability(case_count: usize) -> f64 {
    0.05 / (2.0 * case_count.max(1) as f64)
}

pub fn validate_worker_endpoint(
    endpoint: &WorkerEndpoint,
    hello: &WorkerHello,
) -> Result<(), String> {
    let expected_executable = executable_identity(&endpoint.executable_path)?;
    if expected_executable != hello.executable_identity {
        return Err("worker executable identity does not match configured endpoint".into());
    }
    let expected_root = endpoint
        .clean_source_root
        .canonicalize()
        .map_err(|error| format!("canonicalize source root: {error}"))?;
    if expected_root != hello.source_identity.source_root {
        return Err("worker source identity does not match configured source root".into());
    }
    if hello.source_identity.dirty {
        return Err("worker source root is dirty".into());
    }
    Ok(())
}

pub fn validate_worker_response(
    hello: &WorkerHello,
    case: &WorkerCaseManifest,
    request: &WorkerRequest,
    response: &WorkerResponse,
) -> Result<(), String> {
    if response.sequence != request.sequence {
        return Err("worker response order mismatch".into());
    }
    if response.case_name != case.name || response.case_fingerprint != request.case_fingerprint {
        return Err("worker response missing or mismatched case".into());
    }
    if response.executable_identity != hello.executable_identity {
        return Err("worker executable identity changed".into());
    }
    if response.source_identity != hello.source_identity {
        return Err("worker source identity changed".into());
    }
    if response.semantic_observation != case.semantic_observation {
        return Err("worker semantic observation changed during measurement".into());
    }
    if response.logical_counters != case.logical_counters {
        return Err("worker logical counters changed during measurement".into());
    }
    Ok(())
}

pub fn decode_worker_response(line: &str) -> Result<WorkerResponse, String> {
    serde_json::from_str(line).map_err(|error| format!("malformed worker response: {error}"))
}

pub fn stage_fixture_source_fingerprint(case: &Fixture) -> String {
    let Some(topology) = case.topology_fingerprint.as_deref() else {
        return case.fixture_fingerprint.clone();
    };
    let mut digest = Sha256::new();
    hash_part(&mut digest, "topology", topology.as_bytes());
    hash_part(&mut digest, "inputs", &canonical_json_bytes(&case.inputs));
    hash_part(
        &mut digest,
        "compiler_limits",
        format!("{:?}", case.compiler_options.limits).as_bytes(),
    );
    for (index, root) in case.module_roots.iter().enumerate() {
        hash_part(
            &mut digest,
            &format!("module-root:{index}:label"),
            root.label.as_bytes(),
        );
        hash_part(
            &mut digest,
            &format!("module-root:{index}:role"),
            root.role.marker().as_bytes(),
        );
        if root.role == ModuleRootRole::Implementation || root.path == case.source_dir {
            continue;
        }
        let mut modules = Vec::new();
        collect_mag_files(&root.path, &root.path, &mut modules);
        modules.sort_by(|left, right| left.0.cmp(&right.0));
        for (relative, path) in modules {
            let bytes = fs::read(&path)
                .unwrap_or_else(|error| panic!("read stage source {}: {error}", path.display()));
            hash_part(
                &mut digest,
                &format!("module:{index}:{}", relative.to_string_lossy()),
                &bytes,
            );
        }
    }
    format!("sha256:{:x}", digest.finalize())
}

pub fn forcing_dependency_proof(case: &Fixture) -> Option<String> {
    let source = fs::read_to_string(case.source_dir.join(&case.entry)).ok()?;
    let graph_source = case
        .module_roots
        .iter()
        .find(|root| root.role == ModuleRootRole::Implementation)
        .and_then(|root| fs::read_to_string(root.path.join("nefor/graph.mag")).ok());
    forcing_dependency_proof_from_sources(&case.stage, &source, graph_source.as_deref())
}

pub fn forcing_dependency_proof_from_sources(
    stage: &str,
    source: &str,
    graph_source: Option<&str>,
) -> Option<String> {
    let evidence = match stage {
        "forward-reachability"
            if source.contains("nefor.graph.`forward-reachable`(analysis)")
                && source.contains(
                    "artifact(FrontierProof {summary: summary, forced: forward_proof})",
                ) =>
        {
            "analysis -> forward -> forward-proof"
        }
        "both-reachability"
            if source.contains("nefor.graph.`forward-reachable`(analysis)")
                && source.contains("let reverse = force_reverse(forward_proof)")
                && source.contains(
                    "artifact(FrontierProof {summary: summary, forced: reverse_proof})",
                ) =>
        {
            "forward-proof -> reverse -> reverse-proof"
        }
        "lower"
            if source.contains("let lowered = nefor.graph.lower(topology)")
                && source.contains("let forced = canonical(lowered)")
                && source.contains(
                    "artifact(LowerFrontier {summary: summary, lowered: lowered, forced: forced})",
                )
                && graph_source.is_some_and(|graph_source| {
                    graph_source.contains(
                        "let `lower-program`: fn(Graph) -> Modification = |topology| => {\n  let analysis = `analyze-for-lowering`(topology)",
                    )
                }) =>
        {
            "analysis -> lower-program -> canonical-lowered-artifact"
        }
        _ => return None,
    };
    Some(fingerprint(evidence.as_bytes()))
}
