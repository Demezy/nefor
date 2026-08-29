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

pub const SCHEMA_VERSION: u8 = 2;

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
    pub package_version: String,
    pub rustc: String,
    pub target: String,
    pub os: String,
    pub arch: String,
    pub logical_cpus: usize,
    pub profile: String,
    pub samples_per_case: usize,
    pub warmup_iterations: usize,
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
    pub result: Result<Value, ErrorObservation>,
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
}

pub struct Fixture {
    pub name: String,
    pub family: String,
    pub stage: String,
    pub size: Option<usize>,
    pub source_dir: PathBuf,
    pub entry: String,
    pub module_roots: Vec<PathBuf>,
    pub inputs: Value,
    pub source_fingerprint: String,
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
        fixture_fingerprint: case.source_fingerprint.clone(),
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
                        result,
                    }
                })
                .collect();
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
    match profiler {
        Some(profiler) => nefor_mag::load_with_profiler(
            &case.source_dir,
            &case.entry,
            case.inputs.clone(),
            &case.module_roots,
            profiler,
        ),
        None => nefor_mag::load_with_inputs_and_module_roots(
            &case.source_dir,
            &case.entry,
            case.inputs.clone(),
            &case.module_roots,
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
    roots: Vec<PathBuf>,
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
    let mut module_roots = vec![dir.clone()];
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
        source_fingerprint: fingerprint(source.as_bytes()),
        expected_error: expected_error.map(str::to_owned),
        policy: policy.into(),
        expected_artifact,
        probes,
    }
}

pub fn write_module(case: &Fixture, relative: &str, source: &str) {
    let path = case.source_dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create module parent");
    }
    fs::write(path, source).expect("write fixture module");
}

pub fn probe(function: &'static str, input: Value) -> Probe {
    Probe { function, input }
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
        median: percentile(&sorted, 50),
        mean: (sorted.iter().map(|&v| u128::from(v)).sum::<u128>() / sorted.len().max(1) as u128)
            as u64,
        p90: percentile(&sorted, 90),
        p95: percentile(&sorted, 95),
        max: sorted.last().copied().unwrap_or(0),
    }
}
fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    sorted
        .get((sorted.len().saturating_sub(1) * percentile) / 100)
        .copied()
        .unwrap_or(0)
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
        result.insert(key, percentile(&values, 50));
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
            "source collection length for map/indexed-map/filter/flat-map/fold/sort-by/remove-at"
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
