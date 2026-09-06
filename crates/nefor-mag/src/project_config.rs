use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Input-stage project preparation failure, independent of CLI rendering.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ProjectError {
    pub code: &'static str,
    pub path: PathBuf,
    pub message: String,
}

fn path_diagnostic(code: &'static str, path: &Path, message: String) -> ProjectError {
    ProjectError {
        code,
        path: path.to_owned(),
        message,
    }
}

/// Explicit project root and ordered effective roots. Paths are not canonicalized:
/// their original spelling is part of project cache identity.
#[derive(Debug)]
pub struct PreparedProject {
    pub project_root: PathBuf,
    pub module_roots: Vec<PathBuf>,
    pub config_version: u32,
}

pub fn prepare(
    project_root: &Path,
    extra_roots: &[PathBuf],
) -> Result<PreparedProject, ProjectError> {
    require_directory(project_root, "project")?;
    let config = load(project_root)?;
    let module_roots: Vec<_> = std::iter::once(project_root.to_owned())
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
    Ok(PreparedProject {
        project_root: project_root.to_owned(),
        module_roots,
        config_version: config.version,
    })
}

fn require_directory(path: &Path, kind: &'static str) -> Result<(), ProjectError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(path_diagnostic(
            "path_not_directory",
            path,
            format!("{kind} is not a directory: {}", path.display()),
        )),
        Err(error) => Err(path_diagnostic(
            "path_unavailable",
            path,
            format!("cannot access {kind} {}: {error}", path.display()),
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProjectConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub module_roots: Vec<PathBuf>,
}

fn default_version() -> u32 {
    1
}

pub fn load(root: &Path) -> Result<ProjectConfig, ProjectError> {
    let path = root.join("mag.toml");
    let source = std::fs::read_to_string(&path).map_err(|error| {
        path_diagnostic(
            "project_read",
            &path,
            format!("cannot read project manifest: {error}"),
        )
    })?;
    let config: ProjectConfig = toml::from_str(&source).map_err(|error| {
        path_diagnostic(
            "project_config",
            &path,
            format!("invalid project manifest: {error}"),
        )
    })?;
    if config.version != 1 {
        return Err(path_diagnostic(
            "project_version",
            &path,
            format!("unsupported project manifest version {}", config.version),
        ));
    }
    Ok(config)
}
