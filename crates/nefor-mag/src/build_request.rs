use std::path::{Path, PathBuf};

use nefor_mag::{CompilerOptions, FileCompileRequest};
use serde_json::Value;

use crate::{load_inputs, project_config, require_directory, Diagnostic};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachePolicy {
    Use,
    Bypass,
}

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
    require_directory(&project_root, "project")?;
    let config = project_config::load(&project_root)?;
    let module_roots: Vec<_> = std::iter::once(project_root.clone())
        .chain(
            config
                .module_roots
                .iter()
                .chain(extra_roots)
                .map(|path| project_root.join(path)),
        )
        .collect();
    for root in &module_roots {
        require_directory(root, "module_root")?;
    }
    let inputs = load_inputs(input_specs, Some(&project_root))?;
    Ok(ProjectBuildRequest {
        project_root,
        entry,
        module_roots,
        inputs,
        options,
        config_version: config.version,
    })
}
