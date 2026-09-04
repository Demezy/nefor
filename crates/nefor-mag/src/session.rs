use crate::error::MagError;
use crate::profile::{CompileProfile, CompileProfiler};
use crate::CompilerOptions;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// All semantic inputs for compiling an in-memory MAG entry program.
#[derive(Debug, Clone)]
pub struct CompileRequest<'a> {
    pub source: &'a str,
    pub source_dir: &'a Path,
    pub inputs: Value,
    pub module_roots: &'a [PathBuf],
    pub options: CompilerOptions,
}

/// All semantic inputs for compiling a file-backed MAG entry program.
#[derive(Debug, Clone)]
pub struct FileCompileRequest<'a> {
    pub source_dir: &'a Path,
    pub entry: &'a str,
    pub inputs: Value,
    pub module_roots: &'a [PathBuf],
    pub options: CompilerOptions,
}

/// Cache-neutral accounting for work submitted through one compiler session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompilerSessionStats {
    pub memory_compile_requests: u64,
    pub file_compile_requests: u64,
    pub cold_compilations: u64,
    pub successful_compilations: u64,
    pub failed_compilations: u64,
}

/// A generic owner for artifact-only MAG compilation requests.
///
/// The current implementation is deliberately cold-only: it retains accounting,
/// but no source, dependency, compiler, evaluator, or compiled-program state.
/// Every request constructs an independent compilation state and returns only
/// the resulting artifact.
#[derive(Debug, Default)]
pub struct CompilerSession {
    stats: Mutex<CompilerSessionStats>,
}

impl CompilerSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compile(&self, request: CompileRequest<'_>) -> Result<Value, MagError> {
        self.record_memory_compile_request();
        self.record_result(crate::compile_cold(request, None))
    }

    pub fn compile_profiled(
        &self,
        request: CompileRequest<'_>,
    ) -> Result<(Value, CompileProfile), MagError> {
        let profiler = CompileProfiler::new();
        let artifact = self.compile_with_profiler(request, &profiler)?;
        Ok((artifact, profiler.snapshot()))
    }

    pub fn compile_with_profiler(
        &self,
        request: CompileRequest<'_>,
        profiler: &CompileProfiler,
    ) -> Result<Value, MagError> {
        self.record_memory_compile_request();
        self.record_result(crate::compile_cold(request, Some(profiler)))
    }

    pub fn compile_file(&self, request: FileCompileRequest<'_>) -> Result<Value, MagError> {
        self.record_file_compile_request();
        self.record_result(crate::compile_file_cold(request, None))
    }

    pub fn compile_file_profiled(
        &self,
        request: FileCompileRequest<'_>,
    ) -> Result<(Value, CompileProfile), MagError> {
        let profiler = CompileProfiler::new();
        let artifact = self.compile_file_with_profiler(request, &profiler)?;
        Ok((artifact, profiler.snapshot()))
    }

    pub fn compile_file_with_profiler(
        &self,
        request: FileCompileRequest<'_>,
        profiler: &CompileProfiler,
    ) -> Result<Value, MagError> {
        self.record_file_compile_request();
        self.record_result(crate::compile_file_cold(request, Some(profiler)))
    }

    pub fn stats(&self) -> CompilerSessionStats {
        *self.stats.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn record_memory_compile_request(&self) {
        let mut stats = self.stats.lock().unwrap_or_else(|error| error.into_inner());
        stats.memory_compile_requests = stats.memory_compile_requests.saturating_add(1);
        stats.cold_compilations = stats.cold_compilations.saturating_add(1);
    }

    fn record_file_compile_request(&self) {
        let mut stats = self.stats.lock().unwrap_or_else(|error| error.into_inner());
        stats.file_compile_requests = stats.file_compile_requests.saturating_add(1);
        stats.cold_compilations = stats.cold_compilations.saturating_add(1);
    }

    fn record_result<T>(&self, result: Result<T, MagError>) -> Result<T, MagError> {
        let mut stats = self.stats.lock().unwrap_or_else(|error| error.into_inner());
        match &result {
            Ok(_) => {
                stats.successful_compilations = stats.successful_compilations.saturating_add(1);
            }
            Err(_) => {
                stats.failed_compilations = stats.failed_compilations.saturating_add(1);
            }
        }
        result
    }
}
