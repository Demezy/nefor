use crate::bench_support::{
    fingerprint, SemanticOutcome, WorkerExecutableIdentity, WorkerSourceIdentity,
};
use nefor_mag::error::MagError;
use nefor_mag::profile::{CompileProfile, CompileProfiler};
use nefor_mag::{CompilerOptions, CompilerSession, CompilerSessionStats, LoadRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const CACHE_SCENARIO_PROTOCOL_VERSION: &str = "mag-cache-scenario-worker-v1";
pub const CACHE_SCENARIO_CATALOG_VERSION: &str = "cycle-4-pre-cache-v2";

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheScenarioDefinition {
    pub name: String,
    pub family: String,
    pub transition: String,
    pub timed_operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheScenarioSample {
    pub semantic_observation: SemanticOutcome,
    pub compile_profile: CompileProfile,
    pub session_stats: CompilerSessionStats,
    pub target_duration_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheScenarioManifest {
    pub definition: CacheScenarioDefinition,
    pub definition_fingerprint: String,
    pub baseline_sample: CacheScenarioSample,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheWorkerHello {
    pub protocol_version: String,
    pub catalog_version: String,
    pub catalog_fingerprint: String,
    pub executable_identity: WorkerExecutableIdentity,
    pub source_identity: WorkerSourceIdentity,
    pub cases: Vec<CacheScenarioManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheWorkerRequest {
    pub sequence: usize,
    pub scenario_fingerprint: String,
    pub batch_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheWorkerResponse {
    pub sequence: usize,
    pub scenario_name: String,
    pub scenario_fingerprint: String,
    pub executable_identity: WorkerExecutableIdentity,
    pub source_identity: WorkerSourceIdentity,
    pub samples: Vec<CacheScenarioSample>,
    pub raw_batch_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachePairedCaseReport {
    pub name: String,
    pub baseline_samples: Vec<CacheScenarioSample>,
    pub candidate_samples: Vec<CacheScenarioSample>,
    pub baseline_batch_ns: Vec<u64>,
    pub candidate_batch_ns: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachePairedReport {
    pub schema_version: u8,
    pub report_kind: String,
    pub authoritative: bool,
    pub protocol_version: String,
    pub catalog_version: String,
    pub catalog_fingerprint: String,
    pub samples_per_case: usize,
    pub baseline: CacheWorkerHello,
    pub candidate: CacheWorkerHello,
    pub cases: Vec<CachePairedCaseReport>,
}

pub fn definitions() -> Vec<CacheScenarioDefinition> {
    [
        ("cold-module-chain", "cold", "none"),
        ("cold-shipped-lead-turn", "cold", "none"),
        ("populate-module-chain", "population", "empty-session"),
        ("identical-repeat-module-chain", "repeat", "successful-load"),
        (
            "identical-repeat-broken-module",
            "failure",
            "identical-failure",
        ),
        ("entry-bytes-changed", "invalidation", "entry-v1-to-v2"),
        (
            "transitive-module-changed",
            "invalidation",
            "module-b-v1-to-v2",
        ),
        (
            "module-ambiguity-introduced",
            "invalidation",
            "unique-to-ambiguous",
        ),
        ("read-target-changed", "invalidation", "text-v1-to-v2"),
        ("read-json-target-changed", "invalidation", "json-v1-to-v2"),
        (
            "read-json-ambiguity-introduced",
            "invalidation",
            "unique-to-ambiguous",
        ),
        ("host-input-changed", "context", "input-a-to-b"),
        (
            "compiler-options-changed",
            "context",
            "permissive-to-restrictive",
        ),
        (
            "broken-module-repaired",
            "failure-repair",
            "failure-to-success",
        ),
        (
            "entry-lex-precedes-module-ambiguity",
            "precedence",
            "success-to-two-errors",
        ),
        (
            "module-ambiguity-precedes-host-input",
            "precedence",
            "success-to-two-errors",
        ),
        (
            "required-module-precedes-entry-error",
            "precedence",
            "success-to-two-errors",
        ),
        (
            "entry-deleted-after-success",
            "precedence",
            "success-to-entry-read-error",
        ),
        ("alternating-context-a-b-a-b", "context", "a-b-a-to-b"),
    ]
    .into_iter()
    .map(|(name, family, transition)| CacheScenarioDefinition {
        name: name.into(),
        family: family.into(),
        transition: transition.into(),
        timed_operation: "CompilerSession::load_with_profiler exactly once".into(),
    })
    .collect()
}

pub fn definition_fingerprint(definition: &CacheScenarioDefinition) -> String {
    fingerprint(&serde_json::to_vec(definition).expect("serialize cache scenario definition"))
}

pub fn cache_catalog_fingerprint() -> String {
    fingerprint(&serde_json::to_vec(&definitions()).expect("serialize cache scenario catalog"))
}

pub fn run_sample(source_root: &Path, name: &str) -> CacheScenarioSample {
    let mut prepared = PreparedScenario::new(source_root, name);
    prepared.setup(name);
    execute_prepared(&prepared, name)
}

pub fn run_batch(
    source_root: &Path,
    name: &str,
    batch_count: u64,
) -> (Vec<CacheScenarioSample>, u64) {
    let prepared = (0..batch_count)
        .map(|_| {
            let mut sample = PreparedScenario::new(source_root, name);
            sample.setup(name);
            sample
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    let samples = prepared
        .iter()
        .map(|sample| execute_prepared(sample, name))
        .collect::<Vec<_>>();
    let raw_batch_ns = nanos(started.elapsed());
    (samples, raw_batch_ns)
}

fn execute_prepared(prepared: &PreparedScenario, name: &str) -> CacheScenarioSample {
    let profiler = CompileProfiler::new();
    let started = Instant::now();
    let result = prepared
        .session
        .load_with_profiler(prepared.request(), &profiler);
    let target_duration_ns = nanos(started.elapsed());
    let semantic_observation = observe(result, &prepared.root);
    let sample = CacheScenarioSample {
        semantic_observation,
        compile_profile: profiler.snapshot(),
        session_stats: prepared.session.stats(),
        target_duration_ns,
    };
    assert_expected(name, &sample);
    black_box(&sample.semantic_observation);
    sample
}

pub fn manifests(source_root: &Path) -> Vec<CacheScenarioManifest> {
    definitions()
        .into_iter()
        .map(|definition| {
            eprintln!("cache scenario prepare: {}", definition.name);
            let baseline_sample = run_sample(source_root, &definition.name);
            CacheScenarioManifest {
                definition_fingerprint: definition_fingerprint(&definition),
                definition,
                baseline_sample,
            }
        })
        .collect()
}

pub fn validate_hello_pair(
    left: &CacheWorkerHello,
    right: &CacheWorkerHello,
) -> Result<(), String> {
    if left.protocol_version != CACHE_SCENARIO_PROTOCOL_VERSION
        || right.protocol_version != CACHE_SCENARIO_PROTOCOL_VERSION
        || left.protocol_version != right.protocol_version
    {
        return Err("cache scenario worker protocol mismatch".into());
    }
    if left.catalog_version != CACHE_SCENARIO_CATALOG_VERSION
        || right.catalog_version != CACHE_SCENARIO_CATALOG_VERSION
        || left.catalog_fingerprint != right.catalog_fingerprint
        || left.cases.len() != right.cases.len()
        || left.cases.iter().zip(&right.cases).any(|(a, b)| {
            a.definition != b.definition || a.definition_fingerprint != b.definition_fingerprint
        })
    {
        return Err("cache scenario catalog mismatch".into());
    }
    Ok(())
}

pub fn validate_response(
    hello: &CacheWorkerHello,
    manifest: &CacheScenarioManifest,
    request: &CacheWorkerRequest,
    response: &CacheWorkerResponse,
) -> Result<(), String> {
    if response.sequence != request.sequence {
        return Err("cache scenario response order mismatch".into());
    }
    if response.scenario_name != manifest.definition.name
        || response.scenario_fingerprint != request.scenario_fingerprint
    {
        return Err("cache scenario response identity mismatch".into());
    }
    if response.executable_identity != hello.executable_identity
        || response.source_identity != hello.source_identity
    {
        return Err("cache scenario worker identity changed".into());
    }
    if response.samples.len() != request.batch_count as usize {
        return Err("cache scenario response sample count mismatch".into());
    }
    for sample in &response.samples {
        assert_expected(&manifest.definition.name, sample);
    }
    Ok(())
}

struct PreparedScenario {
    root: PathBuf,
    source_dir: PathBuf,
    entry: String,
    module_roots: Vec<PathBuf>,
    inputs: Value,
    options: CompilerOptions,
    session: CompilerSession,
}

impl PreparedScenario {
    fn new(source_root: &Path, name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "nefor-mag-cache-scenario-{name}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let source_dir = root.join("source");
        fs::create_dir_all(&source_dir).expect("create isolated cache scenario fixture");
        let mut this = Self {
            root,
            source_dir: source_dir.clone(),
            entry: "main.mag".into(),
            module_roots: vec![source_dir],
            inputs: json!({}),
            options: CompilerOptions::default(),
            session: CompilerSession::new(),
        };
        this.seed(source_root, name);
        this
    }

    fn seed(&mut self, source_root: &Path, name: &str) {
        match name {
            "cold-shipped-lead-turn" => {
                fs::copy(
                    source_root.join("examples/nefor-agent/agentic-loop/lead-turn.mag"),
                    self.source_dir.join("main.mag"),
                )
                .expect("copy shipped lead entry");
                self.module_roots = vec![
                    source_root.join("mag/lib"),
                    source_root.join("examples/nefor-agent/mag/lib"),
                ];
                let contracts = crate::load_runtime_contracts(
                    &source_root.join("plugins/mag/lua/mag-kernel/init.lua"),
                );
                self.inputs = json!({"factory_contracts": contracts});
            }
            "read-target-changed" => {
                self.write("main.mag", "(artifact (read \"note.txt\"))");
                self.write("note.txt", "v1");
            }
            "read-json-target-changed" | "read-json-ambiguity-introduced" => {
                self.write("main.mag", "(artifact (read-json \"data/value.json\"))");
                self.write("data/value.json", "{\"version\":1}");
            }
            "host-input-changed" | "alternating-context-a-b-a-b" => {
                self.write(
                    "main.mag",
                    "(artifact (host-input \"value\" (type-tag Int)))",
                );
                self.inputs = json!({"value": 1});
            }
            "compiler-options-changed" => {
                self.write("main.mag", "(artifact (concat [1] [2]))");
            }
            "entry-lex-precedes-module-ambiguity" => {
                self.module_chain();
            }
            "module-ambiguity-precedes-host-input" => {
                self.write(
                    "main.mag",
                    "(require \"a\")\n(artifact (host-input \"missing\" (type-tag Int)))",
                );
                self.write("a.mag", "(let value 1)");
                self.inputs = json!({"missing": 1});
            }
            "required-module-precedes-entry-error" => {
                self.write("main.mag", "(require \"a\")\n(artifact a.value)");
                self.write("a.mag", "(let value 1)");
            }
            "entry-deleted-after-success" => self.write("main.mag", "(artifact 1)"),
            _ => self.module_chain(),
        }
    }

    fn module_chain(&self) {
        self.write("main.mag", "(require \"a\")\n(artifact a.value)");
        self.write("a.mag", "(require \"b\")\n(let value b.value)");
        self.write("b.mag", "(let value 1)");
    }

    fn setup(&mut self, name: &str) {
        match name {
            "identical-repeat-module-chain" => self.load_setup_success(),
            "identical-repeat-broken-module" => {
                self.write("b.mag", "(let value missing)");
                self.load_setup_unresolved("missing");
            }
            "entry-bytes-changed" => {
                self.load_setup_success();
                self.write("main.mag", "(require \"a\")\n(artifact 2)");
            }
            "transitive-module-changed" => {
                self.load_setup_success();
                self.write("b.mag", "(let value 2)");
            }
            "module-ambiguity-introduced" => {
                self.load_setup_success();
                self.add_ambiguous_module("a.mag", "(let value 2)");
            }
            "read-target-changed" => {
                self.load_setup_success();
                self.write("note.txt", "v2");
            }
            "read-json-target-changed" => {
                self.load_setup_success();
                self.write("data/value.json", "{\"version\":2}");
            }
            "read-json-ambiguity-introduced" => {
                self.load_setup_success();
                self.add_ambiguous_module("data/value.json", "{\"version\":2}");
            }
            "host-input-changed" => {
                self.load_setup_success();
                self.inputs = json!({"value": 2});
            }
            "compiler-options-changed" => {
                self.load_setup_success();
                self.options.limits.evaluation_steps = 1;
            }
            "broken-module-repaired" => {
                self.write("b.mag", "(let value missing)");
                self.load_setup_unresolved("missing");
                self.write("b.mag", "(let value 2)");
            }
            "entry-lex-precedes-module-ambiguity" => {
                self.load_setup_success();
                self.add_ambiguous_module("a.mag", "(let value 2)");
                self.write("main.mag", "(require \"a\")\n(artifact @)");
            }
            "module-ambiguity-precedes-host-input" => {
                self.load_setup_success();
                self.inputs = json!({});
                self.add_ambiguous_module("a.mag", "(let value 2)");
            }
            "required-module-precedes-entry-error" => {
                self.load_setup_success();
                self.write("a.mag", "(let value missing)");
                self.write("main.mag", "(require \"a\")\n(artifact later_missing)");
            }
            "entry-deleted-after-success" => {
                self.load_setup_success();
                fs::remove_file(self.source_dir.join("main.mag")).expect("delete entry");
            }
            "alternating-context-a-b-a-b" => {
                self.load_setup_success();
                self.inputs = json!({"value": 2});
                self.load_setup_success();
                self.inputs = json!({"value": 1});
                self.load_setup_success();
                self.inputs = json!({"value": 2});
            }
            _ => {}
        }
    }

    fn request(&self) -> LoadRequest<'_> {
        LoadRequest {
            source_dir: &self.source_dir,
            entry: &self.entry,
            inputs: self.inputs.clone(),
            module_roots: &self.module_roots,
            options: self.options,
        }
    }

    fn load_setup_success(&self) {
        self.session
            .load(self.request())
            .expect("scenario setup succeeds");
    }

    fn load_setup_unresolved(&self, expected_symbol: &str) {
        let error = self
            .session
            .load(self.request())
            .expect_err("scenario setup fails");
        assert!(
            matches!(error, MagError::Unresolved(symbol) if symbol == expected_symbol),
            "scenario setup must fail on unresolved symbol {expected_symbol:?}"
        );
    }

    fn add_ambiguous_module(&mut self, relative: &str, contents: &str) {
        let root = self
            .root
            .join(format!("ambiguity-{}", self.module_roots.len()));
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("ambiguous parent"))
            .expect("create ambiguous root");
        fs::write(path, contents).expect("write ambiguous candidate");
        self.module_roots.push(root);
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.source_dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, contents).expect("write cache scenario fixture");
    }
}

impl Drop for PreparedScenario {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).ok();
    }
}

fn observe(
    result: Result<nefor_mag::LoadedProgram, MagError>,
    fixture_root: &Path,
) -> SemanticOutcome {
    match result {
        Ok(program) => SemanticOutcome::Success {
            artifact_json: program.artifact,
            artifact_hash: program.hash,
            resident_probe_results: vec![],
        },
        Err(error) => {
            let message = normalize_fixture_path(&error.to_string(), fixture_root);
            SemanticOutcome::Error {
                class: error_class(&error).into(),
                message: message.clone(),
                policy_stage: "cold-session".into(),
                ordered_diagnostics: vec![message],
            }
        }
    }
}

fn normalize_fixture_path(message: &str, fixture_root: &Path) -> String {
    let direct = fixture_root.display().to_string();
    let canonical = fixture_root
        .canonicalize()
        .ok()
        .map(|path| path.display().to_string());
    let normalized = message.replace(&direct, "<fixture>");
    canonical
        .filter(|path| path != &direct)
        .map_or(normalized.clone(), |path| {
            normalized.replace(&path, "<fixture>")
        })
}

fn assert_expected(name: &str, sample: &CacheScenarioSample) {
    let success_value = match &sample.semantic_observation {
        SemanticOutcome::Success { artifact_json, .. } => Some(artifact_json),
        SemanticOutcome::Error { .. } => None,
    };
    match name {
        "cold-module-chain" | "populate-module-chain" | "identical-repeat-module-chain" => {
            assert_eq!(success_value, Some(&json!(1)))
        }
        "identical-repeat-broken-module" => {
            assert_error_class(sample, "unresolved");
            assert_error_message(sample, "unresolved symbol: missing");
            assert_eq!(
                sample.session_stats.cold_compilations, sample.session_stats.load_requests,
                "identical failures must perform cold work rather than be cached"
            );
            assert!(sample.compile_profile.phases.module_read_ns > 0);
        }
        "cold-shipped-lead-turn" => assert!(success_value.is_some()),
        "entry-bytes-changed" | "transitive-module-changed" | "broken-module-repaired" => {
            assert_eq!(success_value, Some(&json!(2)))
        }
        "read-target-changed" => assert_eq!(success_value, Some(&json!("v2"))),
        "read-json-target-changed" => assert_eq!(success_value, Some(&json!({"version":2}))),
        "host-input-changed" | "alternating-context-a-b-a-b" => {
            assert_eq!(success_value, Some(&json!(2)))
        }
        "module-ambiguity-introduced"
        | "read-json-ambiguity-introduced"
        | "module-ambiguity-precedes-host-input" => assert_error_contains(sample, "ambiguous"),
        "compiler-options-changed" => assert_error_contains(sample, "budget exceeded"),
        "entry-lex-precedes-module-ambiguity" => assert_error_class(sample, "syntax"),
        "required-module-precedes-entry-error" => {
            assert_error_class(sample, "unresolved");
            assert_error_message(sample, "unresolved symbol: missing");
        }
        "entry-deleted-after-success" => assert_error_contains(sample, "cannot read program"),
        other => panic!("unknown cache scenario {other}"),
    }
    assert!(sample.compile_profile.total_duration_ns > 0);
    assert_session_accounting(sample);
}

fn assert_session_accounting(sample: &CacheScenarioSample) {
    let stats = sample.session_stats;
    let requests = stats.compile_requests + stats.load_requests;
    assert_eq!(
        stats.successful_compilations + stats.failed_compilations,
        requests
    );
    assert!(stats.cold_compilations <= requests);
}

fn assert_error_class(sample: &CacheScenarioSample, expected: &str) {
    let SemanticOutcome::Error { class, .. } = &sample.semantic_observation else {
        panic!("expected {expected} error, got success")
    };
    assert_eq!(class, expected);
}

fn assert_error_message(sample: &CacheScenarioSample, expected: &str) {
    let SemanticOutcome::Error { message, .. } = &sample.semantic_observation else {
        panic!("expected error {expected:?}, got success")
    };
    assert_eq!(message, expected);
}

fn assert_error_contains(sample: &CacheScenarioSample, expected: &str) {
    let SemanticOutcome::Error { message, .. } = &sample.semantic_observation else {
        panic!("expected error containing {expected}, got success")
    };
    assert!(message.contains(expected), "{message:?} lacks {expected:?}");
}

fn error_class(error: &MagError) -> &'static str {
    match error {
        MagError::Syntax(_) => "syntax",
        MagError::Lex(_) => "lex",
        MagError::Parse(_) => "parse",
        MagError::Eval(_) => "eval",
        MagError::Budget(_) => "budget",
        MagError::Type(_) => "type",
        MagError::Unresolved(_) => "unresolved",
        MagError::Arity { .. } => "arity",
    }
}

fn nanos(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
