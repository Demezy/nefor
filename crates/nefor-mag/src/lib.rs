pub mod ast;
pub(crate) mod authored;
mod checker;
pub mod diagnostic;
pub mod env;
pub mod error;
pub mod eval;
pub mod json;
pub mod lexer;
mod lisp;
pub mod observation;
pub mod parser;
pub mod profile;
pub mod project_cache;
pub mod project_config;
mod resolver;
pub mod schema;
pub mod session;
pub mod types;

use ast::Value;
use env::Env;
use error::MagError;
use profile::{CompileProfile, CompileProfiler, Phase};
use serde::{Deserialize, Serialize};
pub use session::{CompileRequest, CompilerSession, CompilerSessionStats, FileCompileRequest};
use std::path::Path;

/// Resource bounds applied during one compilation.
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
    let source_snapshot = diagnostic::SourceSnapshot::named("<memory>", source);
    let module = lisp::compile_source(&source_snapshot, profiler, lisp::SourceRole::Entry)?;

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryEvaluate));
    let value = eval::eval_program(&mut env, &module)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::ArtifactConversion));
    let artifact = extract_artifact(value, "top-level program")?;
    drop(phase);

    Ok(artifact)
}

pub fn compile_file(source_dir: &Path, entry: &str) -> Result<serde_json::Value, MagError> {
    compile_file_with_options(source_dir, entry, CompilerOptions::default())
}

pub fn compile_file_with_options(
    source_dir: &Path,
    entry: &str,
    options: CompilerOptions,
) -> Result<serde_json::Value, MagError> {
    compile_file_with_inputs_and_options(
        source_dir,
        entry,
        serde_json::Value::Object(Default::default()),
        options,
    )
}

pub fn compile_file_with_inputs(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
) -> Result<serde_json::Value, MagError> {
    compile_file_with_inputs_and_options(source_dir, entry, inputs, CompilerOptions::default())
}

pub fn compile_file_with_inputs_and_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    options: CompilerOptions,
) -> Result<serde_json::Value, MagError> {
    compile_file_with_inputs_and_module_roots_and_options(
        source_dir,
        entry,
        inputs,
        &[source_dir.to_path_buf()],
        options,
    )
}

pub fn compile_file_with_inputs_and_module_roots(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
) -> Result<serde_json::Value, MagError> {
    compile_file_with_inputs_and_module_roots_and_options(
        source_dir,
        entry,
        inputs,
        module_roots,
        CompilerOptions::default(),
    )
}

pub fn compile_file_with_inputs_and_module_roots_and_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    options: CompilerOptions,
) -> Result<serde_json::Value, MagError> {
    CompilerSession::new().compile_file(FileCompileRequest {
        source_dir,
        entry,
        inputs,
        module_roots,
        options,
    })
}

pub fn compile_file_profiled(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
) -> Result<(serde_json::Value, CompileProfile), MagError> {
    compile_file_profiled_with_options(
        source_dir,
        entry,
        inputs,
        module_roots,
        CompilerOptions::default(),
    )
}

pub fn compile_file_profiled_with_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    options: CompilerOptions,
) -> Result<(serde_json::Value, CompileProfile), MagError> {
    CompilerSession::new().compile_file_profiled(FileCompileRequest {
        source_dir,
        entry,
        inputs,
        module_roots,
        options,
    })
}

pub fn compile_file_with_profiler(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    profiler: &CompileProfiler,
) -> Result<serde_json::Value, MagError> {
    compile_file_with_profiler_and_options(
        source_dir,
        entry,
        inputs,
        module_roots,
        profiler,
        CompilerOptions::default(),
    )
}

pub fn compile_file_with_profiler_and_options(
    source_dir: &Path,
    entry: &str,
    inputs: serde_json::Value,
    module_roots: &[std::path::PathBuf],
    profiler: &CompileProfiler,
    options: CompilerOptions,
) -> Result<serde_json::Value, MagError> {
    CompilerSession::new().compile_file_with_profiler(
        FileCompileRequest {
            source_dir,
            entry,
            inputs,
            module_roots,
            options,
        },
        profiler,
    )
}

pub(crate) fn compile_file_cold(
    request: FileCompileRequest<'_>,
    profiler: Option<&CompileProfiler>,
) -> Result<serde_json::Value, MagError> {
    compile_file_cold_observing(request, profiler, None)
}

pub(crate) fn compile_file_cold_observing(
    request: FileCompileRequest<'_>,
    profiler: Option<&CompileProfiler>,
    observer: Option<observation::Observer>,
) -> Result<serde_json::Value, MagError> {
    let _total = profiler.map(CompileProfiler::start_total);
    let FileCompileRequest {
        source_dir,
        entry,
        inputs,
        module_roots,
        options,
    } = request;
    let _fuel = eval::fuel::install(options.limits);
    let path = resolver::resolve_workspace_path(source_dir, entry)?;
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
    env.set_observer(observer);
    env.observe(
        || observation::Query::entry(source_dir, entry),
        &path,
        &source,
    );
    env.define("inputs", Value::HostInputs(inputs));
    let source_snapshot = diagnostic::SourceSnapshot::file(&path, &source);
    let module = lisp::compile_source(&source_snapshot, profiler, lisp::SourceRole::Entry)?;

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::EntryEvaluate));
    let value = eval::eval_program(&mut env, &module)?;
    drop(phase);

    let phase = profiler.map(|profiler| profiler.start_phase(Phase::ArtifactConversion));
    let artifact = extract_artifact(value, "top-level program")?;
    drop(phase);

    Ok(artifact)
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
