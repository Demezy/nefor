use crate::error::MagError;
use crate::profile::{CompileProfile, CompileProfiler};
use crate::{CompilerOptions, LoadedProgram};
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

/// All semantic inputs for loading a file-backed MAG entry program.
#[derive(Debug, Clone)]
pub struct LoadRequest<'a> {
    pub source_dir: &'a Path,
    pub entry: &'a str,
    pub inputs: Value,
    pub module_roots: &'a [PathBuf],
    pub options: CompilerOptions,
}

/// Cache-neutral accounting for work submitted through one compiler session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerSessionTelemetry {
    pub compile_requests: u64,
    pub load_requests: u64,
    pub cold_compilations: u64,
    pub successful_compilations: u64,
    pub failed_compilations: u64,
}

/// A generic owner for MAG compilation requests.
///
/// The current implementation is deliberately cold-only: it retains accounting,
/// but no source, dependency, compiler, evaluator, or loaded-program state. Every
/// request constructs an independent compilation state, and every successful
/// load returns a separately owned [`LoadedProgram`].
#[derive(Debug, Default)]
pub struct CompilerSession {
    telemetry: Mutex<CompilerSessionTelemetry>,
}

impl CompilerSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compile(&self, request: CompileRequest<'_>) -> Result<Value, MagError> {
        self.record_compile_request();
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
        self.record_compile_request();
        self.record_result(crate::compile_cold(request, Some(profiler)))
    }

    pub fn load(&self, request: LoadRequest<'_>) -> Result<LoadedProgram, MagError> {
        self.record_load_request();
        self.record_result(crate::load_cold(request, None))
    }

    pub fn load_profiled(
        &self,
        request: LoadRequest<'_>,
    ) -> Result<(LoadedProgram, CompileProfile), MagError> {
        let profiler = CompileProfiler::new();
        let program = self.load_with_profiler(request, &profiler)?;
        Ok((program, profiler.snapshot()))
    }

    pub fn load_with_profiler(
        &self,
        request: LoadRequest<'_>,
        profiler: &CompileProfiler,
    ) -> Result<LoadedProgram, MagError> {
        self.record_load_request();
        self.record_result(crate::load_cold(request, Some(profiler)))
    }

    pub fn telemetry(&self) -> CompilerSessionTelemetry {
        *self
            .telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn record_compile_request(&self) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        telemetry.compile_requests = telemetry.compile_requests.saturating_add(1);
        telemetry.cold_compilations = telemetry.cold_compilations.saturating_add(1);
    }

    fn record_load_request(&self) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        telemetry.load_requests = telemetry.load_requests.saturating_add(1);
        telemetry.cold_compilations = telemetry.cold_compilations.saturating_add(1);
    }

    fn record_result<T>(&self, result: Result<T, MagError>) -> Result<T, MagError> {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match &result {
            Ok(_) => {
                telemetry.successful_compilations =
                    telemetry.successful_compilations.saturating_add(1);
            }
            Err(_) => {
                telemetry.failed_compilations = telemetry.failed_compilations.saturating_add(1);
            }
        }
        result
    }
}
