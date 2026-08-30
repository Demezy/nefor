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

pub const SCHEMA_VERSION: u8 = 3;
pub const COMPARISON_SCHEMA_VERSION: u8 = 2;
pub const MAX_TARGET_MEDIAN_RATIO: f64 = 0.90;
pub const MIN_TARGET_COUNTER_REDUCTION: f64 = 0.40;
pub const MAX_CASE_P90_RATIO: f64 = 1.10;

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u8,
    pub metadata: Metadata,
    pub counter_semantics: BTreeMap<String, String>,
    pub cases: Vec<CaseReport>,
    pub oracles: Vec<OracleObservation>,
    pub recommendation: Recommendation,
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
    pub outcome: String,
    pub expected_error: Option<String>,
    pub artifact_hash: Option<String>,
    pub wall_ns: Vec<u64>,
    pub distribution_ns: Distribution,
    pub counters: Option<OperationCounters>,
    pub derived: Option<DerivedCounters>,
    pub profiled_phase_median_ns: Option<PhaseDurations>,
    pub profiled_invocations_are_separate: bool,
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
        resident_probe_results: Vec<ProbeResult>,
    },
    Error {
        class: String,
        message: String,
        policy_stage: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeResult {
    pub function: String,
    pub input: Value,
    pub expected: ProbeExpectation,
    pub result: Result<Value, ErrorObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProbeExpectation {
    Success { value: Value },
    Error { class: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorObservation {
    pub class: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Clone)]
pub struct Probe {
    function: &'static str,
    input: Value,
    expected: ProbeExpectation,
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
    pub fixture_files: Vec<PathBuf>,
    pub expected_error: Option<String>,
    pub policy: String,
    pub expected_artifact: Option<Value>,
    pub probes: Vec<Probe>,
}

pub fn run_case(case: &Fixture, samples: usize, warmups: usize) -> CaseReport {
    for _ in 0..warmups {
        assert_timed_outcome(case, load(case, None));
    }
    let mut wall_ns = Vec::with_capacity(samples);
    let mut hashes = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        let result = load(case, None);
        wall_ns.push(nanos(started.elapsed()));
        if let Some(program) = assert_timed_outcome(case, result) {
            hashes.push(program.hash.clone());
            black_box(program.artifact);
        }
    }
    assert!(
        hashes.windows(2).all(|pair| pair[0] == pair[1]),
        "{} artifact hash changed",
        case.name
    );

    let mut profiles = Vec::with_capacity(samples);
    for _ in 0..samples {
        let profiler = nefor_mag::profile::CompileProfiler::new();
        let result = load(case, Some(&profiler));
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
    }
}

pub fn observe(case: &Fixture) -> OracleObservation {
    let observation = match load(case, None) {
        Ok(program) => {
            let resident_probe_results = case
                .probes
                .iter()
                .map(|probe| {
                    let result = nefor_mag::eval_fn(&program, probe.function, probe.input.clone())
                        .map_err(|error| error_observation(&error));
                    ProbeResult {
                        function: probe.function.into(),
                        input: probe.input.clone(),
                        expected: probe.expected.clone(),
                        result,
                    }
                })
                .collect();
            for (probe, result) in case.probes.iter().zip(&resident_probe_results) {
                validate_probe(case, probe, result);
            }
            SemanticOutcome::Success {
                artifact_json: program.artifact,
                artifact_hash: program.hash,
                resident_probe_results,
            }
        }
        Err(error) => SemanticOutcome::Error {
            class: error_class(&error).into(),
            message: error.to_string(),
            policy_stage: case.policy.clone(),
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

fn load(
    case: &Fixture,
    profiler: Option<&nefor_mag::profile::CompileProfiler>,
) -> Result<nefor_mag::LoadedProgram, MagError> {
    let module_roots = case
        .module_roots
        .iter()
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();
    match profiler {
        Some(profiler) => nefor_mag::load_with_profiler(
            &case.source_dir,
            &case.entry,
            case.inputs.clone(),
            &module_roots,
            profiler,
        ),
        None => nefor_mag::load_with_inputs_and_module_roots(
            &case.source_dir,
            &case.entry,
            case.inputs.clone(),
            &module_roots,
        ),
    }
}

fn assert_timed_outcome(
    case: &Fixture,
    result: Result<nefor_mag::LoadedProgram, MagError>,
) -> Option<nefor_mag::LoadedProgram> {
    match (&case.expected_error, result) {
        (None, Ok(program)) => Some(program),
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
    probes: Vec<Probe>,
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
        fixture_files: vec![PathBuf::from(entry)],
        expected_error: expected_error.map(str::to_owned),
        policy: policy.into(),
        expected_artifact,
        probes,
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

pub fn probe_success(function: &'static str, input: Value, expected: Value) -> Probe {
    Probe {
        function,
        input,
        expected: ProbeExpectation::Success { value: expected },
    }
}

pub fn probe_error(function: &'static str, input: Value, class: &str) -> Probe {
    Probe {
        function,
        input,
        expected: ProbeExpectation::Error {
            class: class.into(),
        },
    }
}

fn validate_probe(case: &Fixture, probe: &Probe, result: &ProbeResult) {
    let accepted = match (&probe.expected, &result.result) {
        (ProbeExpectation::Success { value }, Ok(actual)) => value == actual,
        (ProbeExpectation::Error { class }, Err(actual)) => class == &actual.class,
        _ => false,
    };
    assert!(
        accepted,
        "{} probe {} expectation mismatch: expected {:?}, got {:?}",
        case.name, probe.function, probe.expected, result.result
    );
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
    hash_part(
        &mut digest,
        "expected_artifact",
        &canonical_json_bytes(
            &serde_json::to_value(&case.expected_artifact).expect("expected artifact"),
        ),
    );
    hash_part(
        &mut digest,
        "probes",
        &canonical_json_bytes(
            &serde_json::to_value(
                case.probes
                    .iter()
                    .map(|probe| (&probe.function, &probe.input, &probe.expected))
                    .collect::<Vec<_>>(),
            )
            .expect("probe definitions"),
        ),
    );

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

fn error_observation(error: &MagError) -> ErrorObservation {
    ErrorObservation {
        class: error_class(error).into(),
        message: error.to_string(),
    }
}

pub fn assert_counter_invariants(counters: &OperationCounters, name: &str) {
    assert_eq!(
        counters.function_calls,
        counters.user_function_calls + counters.builtin_calls,
        "{name} function partition"
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
            == counters.user_function_calls + counters.builtin_calls,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    let compatibility_failures = compatibility_failures(baseline, candidate);
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

fn compatibility_failures(baseline: &Report, candidate: &Report) -> Vec<String> {
    let b = &baseline.metadata;
    let c = &candidate.metadata;
    let mut failures = Vec::new();
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
