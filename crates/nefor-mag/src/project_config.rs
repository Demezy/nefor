use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{path_diagnostic, Diagnostic};

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

#[allow(clippy::result_large_err)]
pub fn load(root: &Path) -> Result<ProjectConfig, Diagnostic> {
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
