use std::path::{Path, PathBuf};

use nefor_mag::{CompilerOptions, FileCompileRequest};
use serde_json::Value;

use crate::{load_inputs, path_diagnostic, Diagnostic};

pub struct ProjectBuildRequest {
    pub project_root: PathBuf,
    pub entry: String,
    pub module_roots: Vec<PathBuf>,
    pub inputs: Value,
    pub options: CompilerOptions,
    pub config_version: u32,
}

impl ProjectBuildRequest {
    pub fn as_file_request(&self) -> FileCompileRequest<'_> {
        FileCompileRequest {
            source_dir: &self.project_root,
            entry: &self.entry,
            inputs: self.inputs.clone(),
            module_roots: &self.module_roots,
            options: self.options,
        }
    }
}

pub use nefor_mag::project_cache::CachePolicy;

#[allow(clippy::result_large_err)]
pub fn prepare(
    cwd: &Path,
    project: Option<&Path>,
    entry: String,
    extra_roots: &[PathBuf],
    input_specs: &[String],
    options: CompilerOptions,
) -> Result<ProjectBuildRequest, Diagnostic> {
    let project_root = project.map_or_else(|| cwd.to_owned(), |path| cwd.join(path));
    let project = nefor_mag::project_config::prepare(&project_root, extra_roots)
        .map_err(|error| path_diagnostic(error.code, &error.path, error.message))?;
    let inputs = load_inputs(input_specs, Some(&project_root))?;
    Ok(ProjectBuildRequest {
        project_root,
        entry,
        module_roots: project.module_roots,
        inputs,
        options,
        config_version: project.config_version,
    })
}
