pub mod ast;
mod checker;
pub mod diagnostic;
pub mod env;
pub mod error;
pub mod eval;
pub mod json;
pub mod lexer;
pub mod parser;
pub mod profile;
pub mod schema;
pub mod session;
pub mod types;

use ast::Value;
use env::Env;
use error::MagError;
use profile::{CompileProfile, CompileProfiler, Phase};
use serde::{Deserialize, Serialize};
pub use session::{CompileRequest, CompilerSession, CompilerSessionTelemetry, LoadRequest};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

/// Resource bounds applied to initial compilation and resident function calls.
/// These limits bound cost and failure; they do not alter successful values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerLimits {
    pub evaluation_steps: u64,
    pub call_depth: u16,
    pub expression_depth: u16,
    pub memoized_calls: usize,
}

impl Default for CompilerLimits {
    fn default() -> Self {
        Self {
            evaluation_steps: 1_000_000,
            call_depth: 64,
            expression_depth: 128,
            memoized_calls: 16_384,
        }
    }
}

impl From<u64> for CompilerLimits {
    fn from(evaluation_steps: u64) -> Self {
        Self {
            evaluation_steps,
            ..Self::default()
        }
    }
}

/// Options shared by in-memory and file-backed compilation entry points.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompilerOptions {
    pub limits: CompilerLimits,
}

pub fn compile(source: &str, source_dir: &Path) -> Result<serde_json::Value, MagError> {
    compile_with_options(source, source_dir, CompilerOptions::default())
}

pub fn compile_with_options(
    source: &str,
    source_dir: &Path,
    options: CompilerOptions,
) -> Result<serde_json::Value, MagError> {
    compile_with_inputs_and_options(
        source,
        source_dir,
        serde_json::Value::Object(Default::default()),
        options,
    )
}

pub fn compile_with_inputs(
    source: &str,
    source_dir: &Path,
    inputs: serde_json::Value,
) -> Result<serde_json::Value, MagError> {
    compile_with_inputs_and_options(source, source_dir, inputs, CompilerOptions::default())
}

pub fn compile_with_inputs_and_options(
    source: &str,
    source_dir: &Path,
    inputs: serde_json::Value,
    options: CompilerOptions,
) -> Result<serde_json::Value, MagError> {
    compile_with_inputs_and_module_roots_and_options(
        source,
        source_dir,
        inputs,
        &[source_dir.to_path_buf()],
        options,
    )
}

pub fn compile_with_inputs_and_module_roots(
    source: &str,
    source_dir: &Path,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
) -> Result<serde_json::Value, MagError> {
    compile_with_inputs_and_module_roots_and_options(
        source,
        source_dir,
        inputs,
        module_roots,
        CompilerOptions::default(),
    )
}

pub fn compile_with_inputs_and_module_roots_and_options(
    source: &str,
    source_dir: &Path,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    options: CompilerOptions,
) -> Result<serde_json::Value, MagError> {
    CompilerSession::new().compile(CompileRequest {
        source,
        source_dir,
        inputs,
        module_roots,
        options,
    })
}

pub fn compile_profiled(
    source: &str,
    source_dir: &Path,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
) -> Result<(serde_json::Value, CompileProfile), MagError> {
    compile_profiled_with_options(
        source,
        source_dir,
        inputs,
        module_roots,
        CompilerOptions::default(),
    )
}

pub fn compile_profiled_with_options(
    source: &str,
    source_dir: &Path,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    options: CompilerOptions,
) -> Result<(serde_json::Value, CompileProfile), MagError> {
    CompilerSession::new().compile_profiled(CompileRequest {
        source,
        source_dir,
        inputs,
        module_roots,
        options,
    })
}

pub(crate) fn compile_cold(
    request: CompileRequest<'_>,
    profiler: Option<&CompileProfiler>,
) -> Result<serde_json::Value, MagError> {
    let _total = profiler.map(CompileProfiler::start_total);
    let CompileRequest {
        source,
        source_dir,
        inputs,
        module_roots,
        options,
    } = request;
    let _fuel = eval::fuel::install(options.limits);
    let mut env = Env::new_with_stdlib_source_dir_module_roots_profiler_and_limits(
        source_dir,
        module_roots.to_vec(),
        profiler.cloned(),
        options.limits,
    );
    env.define("inputs", Value::HostInputs(inputs));
    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryLex));
    let source_snapshot = diagnostic::SourceSnapshot::named("<memory>", source);
    let tokens = lexer::tokenize_source(&source_snapshot)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryParse));
    let exprs = parser::parse_source(&tokens, &source_snapshot)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryEvaluate));
    let value = eval::eval_program(&mut env, &exprs)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::ArtifactConversion));
    let artifact = extract_artifact(value, "top-level program")?;
    drop(phase);

    Ok(artifact)
}

#[derive(Debug, Clone)]
pub struct LoadedProgram {
    pub env: Env,
    pub artifact: serde_json::Value,
    pub hash: String,
}

/// Opaque reference to a resolved unary MAG function returning `Artifact`.
/// Applications may associate it with their own artifact data; MAG core does
/// not interpret that data or assign application semantics to the function.
#[derive(Debug, Clone)]
pub struct ArtifactFunction {
    binding: env::BindingHandle,
    name: String,
}

pub fn load(source_dir: &Path, entry: &str) -> Result<LoadedProgram, MagError> {
    load_with_options(source_dir, entry, CompilerOptions::default())
}

pub fn load_with_options(
    source_dir: &Path,
    entry: &str,
    options: CompilerOptions,
) -> Result<LoadedProgram, MagError> {
    load_with_inputs_and_options(
        source_dir,
        entry,
        serde_json::Value::Object(Default::default()),
        options,
    )
}

pub fn load_with_inputs(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
) -> Result<LoadedProgram, MagError> {
    load_with_inputs_and_options(source_dir, entry, inputs, CompilerOptions::default())
}

pub fn load_with_inputs_and_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    options: CompilerOptions,
) -> Result<LoadedProgram, MagError> {
    load_with_inputs_and_module_roots_and_options(
        source_dir,
        entry,
        inputs,
        &[source_dir.to_path_buf()],
        options,
    )
}

pub fn load_with_inputs_and_module_roots(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
) -> Result<LoadedProgram, MagError> {
    load_with_inputs_and_module_roots_and_options(
        source_dir,
        entry,
        inputs,
        module_roots,
        CompilerOptions::default(),
    )
}

pub fn load_with_inputs_and_module_roots_and_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    options: CompilerOptions,
) -> Result<LoadedProgram, MagError> {
    CompilerSession::new().load(LoadRequest {
        source_dir,
        entry,
        inputs,
        module_roots,
        options,
    })
}

pub fn load_profiled(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
) -> Result<(LoadedProgram, CompileProfile), MagError> {
    load_profiled_with_options(
        source_dir,
        entry,
        inputs,
        module_roots,
        CompilerOptions::default(),
    )
}

pub fn load_profiled_with_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    options: CompilerOptions,
) -> Result<(LoadedProgram, CompileProfile), MagError> {
    CompilerSession::new().load_profiled(LoadRequest {
        source_dir,
        entry,
        inputs,
        module_roots,
        options,
    })
}

pub fn load_with_profiler(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    profiler: &CompileProfiler,
) -> Result<LoadedProgram, MagError> {
    load_with_profiler_and_options(
        source_dir,
        entry,
        inputs,
        module_roots,
        profiler,
        CompilerOptions::default(),
    )
}

pub fn load_with_profiler_and_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    profiler: &CompileProfiler,
    options: CompilerOptions,
) -> Result<LoadedProgram, MagError> {
    CompilerSession::new().load_with_profiler(
        LoadRequest {
            source_dir,
            entry,
            inputs,
            module_roots,
            options,
        },
        profiler,
    )
}

pub(crate) fn load_cold(
    request: LoadRequest<'_>,
    profiler: Option<&CompileProfiler>,
) -> Result<LoadedProgram, MagError> {
    let _total = profiler.map(CompileProfiler::start_total);
    let LoadRequest {
        source_dir,
        entry,
        inputs,
        module_roots,
        options,
    } = request;
    let _fuel = eval::fuel::install(options.limits);
    let path = eval::resolve_workspace_path(source_dir, entry)?;
    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryRead));
    let source = std::fs::read_to_string(&path)
        .map_err(|e| MagError::Eval(format!("cannot read program {}: {e}", path.display())))?;
    drop(phase);

    let mut env = Env::new_with_stdlib_source_dir_module_roots_profiler_and_limits(
        source_dir,
        module_roots.to_vec(),
        profiler.cloned(),
        options.limits,
    );
    env.define("inputs", Value::HostInputs(inputs));
    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryLex));
    let source_snapshot = diagnostic::SourceSnapshot::file(&path, &source);
    let tokens = lexer::tokenize_source(&source_snapshot)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryParse));
    let exprs = parser::parse_source(&tokens, &source_snapshot)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryEvaluate));
    let value = eval::eval_program(&mut env, &exprs)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::ArtifactConversion));
    let artifact = extract_artifact(value, "top-level program")?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::ArtifactSerializeHash));
    let encoded = serde_json::to_vec(&artifact)
        .map_err(|e| MagError::Eval(format!("serialize artifact: {e}")))?;
    let hash = format!("{:x}", Sha256::digest(encoded));
    drop(phase);

    Ok(LoadedProgram {
        env,
        artifact,
        hash,
    })
}

pub fn eval_fn(
    program: &LoadedProgram,
    name: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, MagError> {
    let _fuel = eval::fuel::install(program.env.compiler_limits());
    let mut matching = vec![];
    let candidates = artifact_function_candidates(program, name)?;
    if candidates.len() == 1 {
        matching = candidates;
    } else {
        for function in candidates {
            let schema = schema::TypeSchema::reify(&program.env, &function.param_types[0])?;
            let encoded = serde_json::to_string(&input)
                .map_err(|error| MagError::Eval(format!("serialize function input: {error}")))?;
            if schema.validate_json(&encoded).ok {
                matching.push(function);
            }
        }
    }
    let function = match matching.as_slice() {
        [function] => function.clone(),
        [] => {
            return Err(MagError::Type(format!(
                "no overload of function '{name}' accepts the supplied input"
            )))
        }
        _ => {
            return Err(MagError::Type(format!(
                "ambiguous overload of function '{name}' for the supplied input"
            )))
        }
    };
    eval_resolved_artifact_fn(program, name, function, input)
}

/// Resolve a unary named function returning `Artifact` for one concrete input
/// semantic type. The returned handle is meaningful only with `program`.
pub fn resolve_artifact_fn(
    program: &LoadedProgram,
    name: &str,
    expected_input: &serde_json::Value,
) -> Result<ArtifactFunction, MagError> {
    let mut matching = Vec::new();
    for handle in program.env.lookup_handles(name) {
        let Ok(Value::Fn(function)) = Env::ready_handle(&handle) else {
            continue;
        };
        if function.params.len() != 1 || function.return_type != types::MagType::Artifact {
            continue;
        }
        let actual = json::concrete_type_to_json(&types::ConcreteType::resolve(
            &program.env,
            &function.param_types[0],
        )?)?;
        if actual == *expected_input {
            matching.push(handle);
        }
    }
    match matching.as_slice() {
        [binding] => Ok(ArtifactFunction {
            binding: binding.clone(),
            name: name.to_owned(),
        }),
        [] => Err(MagError::Type(format!(
            "function '{name}' input does not match the requested semantic type"
        ))),
        _ => Err(MagError::Type(format!(
            "ambiguous overload of function '{name}' for the requested semantic type"
        ))),
    }
}

/// Evaluate a previously resolved artifact function with JSON input.
pub fn eval_artifact_fn(
    program: &LoadedProgram,
    function: &ArtifactFunction,
    input: serde_json::Value,
) -> Result<serde_json::Value, MagError> {
    let _fuel = eval::fuel::install(program.env.compiler_limits());
    if !program.env.owns_binding_handle(&function.binding) {
        return Err(MagError::Eval(format!(
            "function '{}' belongs to a different loaded program",
            function.name
        )));
    }
    let Value::Fn(value) = Env::ready_handle(&function.binding)? else {
        return Err(MagError::Type(format!(
            "function '{}' no longer resolves to a function",
            function.name
        )));
    };
    eval_resolved_artifact_fn(program, &function.name, value, input)
}

fn eval_resolved_artifact_fn(
    program: &LoadedProgram,
    label: &str,
    function: Arc<ast::FnValue>,
    input: serde_json::Value,
) -> Result<serde_json::Value, MagError> {
    let input_type = function.param_types[0].clone();
    let schema = schema::TypeSchema::reify(&program.env, &input_type)?;
    let encoded = serde_json::to_string(&input)
        .map_err(|error| MagError::Eval(format!("serialize function input: {error}")))?;
    let validation = schema.validate_json(&encoded);
    if !validation.ok {
        let detail = validation
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| {
                validation
                    .violations
                    .iter()
                    .map(|violation| format!("{}: {}", violation.path, violation.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            });
        return Err(MagError::Type(format!(
            "function '{label}' input does not conform to {input_type}: {detail}"
        )));
    }
    let argument = Value::Typed(
        std::sync::Arc::new(json::json_to_typed_value(
            &program.env,
            &input,
            &input_type,
        )?),
        input_type,
    );
    let evaluated = eval::apply_value(&program.env, &Value::Fn(function), &[argument])?;
    let artifact = extract_artifact(evaluated, &format!("function '{label}'"));
    program.env.collect_frames(&[]);
    artifact
}

fn artifact_function_candidates(
    program: &LoadedProgram,
    name: &str,
) -> Result<Vec<std::sync::Arc<ast::FnValue>>, MagError> {
    let functions = named_function_candidates(program, name)?;
    let unary = functions
        .iter()
        .filter(|function| function.params.len() == 1)
        .cloned()
        .collect::<Vec<_>>();
    if unary.is_empty() {
        return Err(MagError::Type(format!(
            "function '{name}' must be unary, got {} parameters",
            functions[0].params.len()
        )));
    }
    let artifact = unary
        .iter()
        .filter(|function| function.return_type == types::MagType::Artifact)
        .cloned()
        .collect::<Vec<_>>();
    if artifact.is_empty() {
        return Err(MagError::Type(format!(
            "function '{name}' must return Artifact, got {}",
            unary[0].return_type
        )));
    }
    Ok(artifact)
}

fn named_function_candidates(
    program: &LoadedProgram,
    name: &str,
) -> Result<Vec<std::sync::Arc<ast::FnValue>>, MagError> {
    let named = program.env.lookup_candidates(name);
    if named.is_empty() {
        return Err(MagError::Unresolved(name.into()));
    }
    let functions = named
        .into_iter()
        .filter_map(|value| match value {
            Value::Fn(function) => Some(function),
            _ => None,
        })
        .collect::<Vec<_>>();
    if functions.is_empty() {
        return Err(MagError::Type(format!(
            "function '{name}' is not a function"
        )));
    }
    Ok(functions)
}

/// Validate a named function's arity and declared return type without knowing
/// how an application stores or invokes that function.
pub fn validate_fn(
    program: &LoadedProgram,
    name: &str,
    parameter_count: usize,
    return_type: &types::MagType,
) -> Result<(), MagError> {
    let candidates = named_function_candidates(program, name)?;
    let matching_arity = candidates
        .iter()
        .filter(|function| function.params.len() == parameter_count)
        .collect::<Vec<_>>();
    if matching_arity.is_empty() {
        return Err(MagError::Type(format!(
            "function '{name}' must have {parameter_count} parameters, got {}",
            candidates[0].params.len()
        )));
    }
    let matching_return = matching_arity
        .iter()
        .filter(|function| function.return_type == *return_type)
        .collect::<Vec<_>>();
    match matching_return.len() {
        1 => Ok(()),
        0 => Err(MagError::Type(format!(
            "function '{name}' must return {return_type}, got {}",
            matching_arity[0].return_type
        ))),
        _ => Err(MagError::Type(format!(
            "function '{name}' has ambiguous overloads with {parameter_count} parameters returning {return_type}"
        ))),
    }
}

pub fn validate_fn_input(
    program: &LoadedProgram,
    name: &str,
    parameter: usize,
    expected_input: &serde_json::Value,
) -> Result<(), MagError> {
    let mut matching = 0;
    for function in named_function_candidates(program, name)? {
        let Some(parameter_type) = function.param_types.get(parameter) else {
            continue;
        };
        let actual = json::concrete_type_to_json(&types::ConcreteType::resolve(
            &program.env,
            parameter_type,
        )?)?;
        if actual == *expected_input {
            matching += 1;
        }
    }
    match matching {
        1 => Ok(()),
        0 => Err(MagError::Type(format!(
            "function '{name}' input does not match its structural source type"
        ))),
        _ => Err(MagError::Type(format!(
            "function '{name}' has ambiguous overloads for its structural source type"
        ))),
    }
}

fn extract_artifact(value: Value, source: &str) -> Result<serde_json::Value, MagError> {
    match value {
        Value::Artifact(artifact) => Ok(artifact),
        Value::Typed(inner, types::MagType::Artifact) => {
            extract_artifact(inner.as_ref().clone(), source)
        }
        other => Err(MagError::Eval(format!(
            "{source} must return Artifact, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod loaded_program_lifetime_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn workspace(name: &str, source: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nefor-mag-{name}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("main.mag"), source).unwrap();
        path
    }

    #[test]
    fn dropping_a_loaded_program_releases_its_binding_arena() {
        let root = workspace("loaded-drop", "(let answer 42)\n(artifact answer)");
        let program = load(&root, "main.mag").unwrap();
        let handle = program.env.lookup_handles("answer").pop().unwrap();
        drop(program);

        assert!(matches!(
            Env::ready_handle(&handle),
            Err(MagError::Eval(message)) if message == "binding program has been dropped"
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_host_calls_reclaim_self_capturing_local_frames() {
        let root = workspace(
            "repeated-host-calls",
            r#"
              (let run (fn [[input Int]] -> Artifact
                (let local (fn [[value Artifact]] -> Artifact value))
                (local (artifact input))))
              (artifact {})
            "#,
        );
        let program = load(&root, "main.mag").unwrap();
        let baseline = program.env.live_frame_count();
        for input in 0..128 {
            let artifact = eval_fn(&program, "run", serde_json::json!(input)).unwrap();
            assert_eq!(artifact, serde_json::json!(input));
            assert_eq!(program.env.live_frame_count(), baseline);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn core_load_preserves_application_data_without_interpreting_its_schema() {
        let root = workspace(
            "opaque-application-artifact",
            r#"(artifact {:rules [{:id 7 :fn false :on "opaque"}] :application "custom"})"#,
        );
        let program = load(&root, "main.mag").expect("core accepts arbitrary artifact data");
        assert_eq!(
            program.artifact,
            serde_json::json!({
                "rules": [{"id": 7, "fn": false, "on": "opaque"}],
                "application": "custom"
            })
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opaque_artifact_function_resolves_by_input_type_and_evaluates() {
        let root = workspace(
            "artifact-function",
            r#"
              (type Task {:task String})
              (let expand (fn [[value Task]] -> Artifact (artifact value)))
              (let expand (fn [[value Int]] -> Artifact (artifact value)))
              (artifact {})
            "#,
        );
        let program = load(&root, "main.mag").unwrap();
        let task_type = serde_json::json!({
            "kind": "named",
            "name": "main.Task",
            "arguments": [],
            "body": {
                "kind": "record",
                "fields": [{
                    "name": "task",
                    "type": {"kind": "primitive", "name": "String"}
                }]
            }
        });
        let function = resolve_artifact_fn(&program, "expand", &task_type).unwrap();
        assert_eq!(
            eval_artifact_fn(
                &program,
                &function,
                serde_json::json!({"task": "write docs"})
            )
            .unwrap(),
            serde_json::json!({"task": "write docs"})
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opaque_artifact_function_rejects_a_different_loaded_program() {
        let first = workspace(
            "artifact-function-owner-a",
            r#"
              (type Task {:a Int})
              (let run (fn [[value Task]] -> Artifact
                (artifact (get value "a"))))
              (artifact {})
            "#,
        );
        let second = workspace(
            "artifact-function-owner-b",
            r#"
              (type Task {:b String})
              (artifact {})
            "#,
        );
        let first_program = load(&first, "main.mag").unwrap();
        let second_program = load(&second, "main.mag").unwrap();
        let task_type = serde_json::json!({
            "kind": "named",
            "name": "main.Task",
            "arguments": [],
            "body": {
                "kind": "record",
                "fields": [{
                    "name": "a",
                    "type": {"kind": "primitive", "name": "Int"}
                }]
            }
        });
        let function = resolve_artifact_fn(&first_program, "run", &task_type).unwrap();
        assert!(matches!(
            eval_artifact_fn(
                &second_program,
                &function,
                serde_json::json!({"b": "wrong program"})
            ),
            Err(MagError::Eval(message)) if message.contains("different loaded program")
        ));
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }
}
