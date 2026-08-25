#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeSet;
#[cfg(target_os = "macos")]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug)]
pub struct PreparedArtifacts {
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FullExecutionPlan {
    pub harness_packages: Vec<String>,
    pub separate_packages: Vec<String>,
}

impl FullExecutionPlan {
    #[must_use]
    pub fn feature_spec(&self) -> String {
        self.harness_packages
            .iter()
            .map(|package| format!("{package}/full-tests"))
            .collect::<Vec<_>>()
            .join(",")
    }

    #[must_use]
    pub fn workspace_cargo_args(&self, command: &str, full: bool, suffix: &[&str]) -> Vec<String> {
        let mut args = vec![command.to_owned(), "--workspace".to_owned()];
        for package in &self.separate_packages {
            args.extend(["--exclude".to_owned(), package.clone()]);
        }
        args.push("--locked".to_owned());
        args.extend(suffix.iter().map(|arg| (*arg).to_owned()));
        if full {
            args.extend(["--features".to_owned(), self.feature_spec()]);
        }
        args
    }
}

pub fn load_full_execution_plan(repository_root: &Path) -> io::Result<FullExecutionPlan> {
    let source = fs::read_to_string(repository_root.join("tools/test-lanes.json"))?;
    parse_full_execution_plan(&source)
}

fn parse_full_execution_plan(source: &str) -> io::Result<FullExecutionPlan> {
    let value: serde_json::Value = serde_json::from_str(source).map_err(io::Error::other)?;
    let cargo_full = value.get("cargo_full").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "test-lanes.json lacks cargo_full",
        )
    })?;
    let harness_packages = string_array(cargo_full, "harness_packages")?;
    let separate = cargo_full
        .get("separate")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "cargo_full.separate must be an array",
            )
        })?;
    let separate_packages = separate
        .iter()
        .map(|entry| {
            entry
                .get("package")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "every cargo_full.separate entry needs a package",
                    )
                })
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(FullExecutionPlan {
        harness_packages,
        separate_packages,
    })
}

fn string_array(value: &serde_json::Value, key: &str) -> io::Result<Vec<String>> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{key} must be an array"),
            )
        })?
        .iter()
        .map(|entry| {
            entry.as_str().map(str::to_owned).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("every {key} entry must be a string"),
                )
            })
        })
        .collect()
}

pub fn parse_executables(input: impl BufRead) -> io::Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for (index, line) in input.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed Cargo JSON on line {}: {error}", index + 1),
            )
        })?;
        if value.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(executable) = value.get("executable") else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Cargo compiler-artifact on line {} has no executable field",
                    index + 1
                ),
            ));
        };
        if executable.is_null() {
            continue;
        }
        let executable = executable.as_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Cargo executable on line {} is not a string", index + 1),
            )
        })?;
        paths.insert(PathBuf::from(executable));
    }
    Ok(paths.into_iter().collect())
}

pub fn run_cargo_and_prepare(
    repository_root: &Path,
    cargo_args: &[&str],
    target_dir: Option<&Path>,
    artifact_dir: &Path,
) -> io::Result<PreparedArtifacts> {
    fs::create_dir_all(artifact_dir)?;
    let json_path = artifact_dir.join("cargo-artifacts.jsonl");
    let stderr_path = artifact_dir.join("cargo-prebuild.stderr.log");
    let stdout = File::create(&json_path)?;
    let stderr = File::create(&stderr_path)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    eprintln!(
        "=== MACOS TEST PREBUILD: cargo {} ===",
        cargo_args.join(" ")
    );
    let mut command = Command::new(cargo);
    command
        .current_dir(repository_root)
        .args(cargo_args)
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(target_dir) = target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let status = command.status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "Cargo test prebuild failed with {status}; diagnostics: {}",
            stderr_path.display()
        )));
    }
    let paths = parse_executables(BufReader::new(File::open(&json_path)?))?;
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Cargo reported no executable artifacts in {}",
                json_path.display()
            ),
        ));
    }
    prepare_paths_for_platform(&paths, artifact_dir)?;
    Ok(PreparedArtifacts { paths })
}

pub fn prepare_paths_for_platform(paths: &[PathBuf], artifact_dir: &Path) -> io::Result<()> {
    let manifest = artifact_dir.join("signed-executables.txt");
    let mut output = File::create(&manifest)?;
    for path in paths {
        writeln!(output, "{}", path.display())?;
    }
    #[cfg(target_os = "macos")]
    {
        sign_and_verify(paths)?;
        eprintln!(
            "=== MACOS TEST SIGNING: signed and verified {} Cargo-reported executables ===",
            paths.len()
        );
        eprintln!("manifest: {}", manifest.display());
    }
    #[cfg(not(target_os = "macos"))]
    eprintln!(
        "=== TEST SIGNING: non-macOS no-op ({} artifacts) ===",
        paths.len()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn sign_and_verify(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        if !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Cargo-reported executable is missing: {}", path.display()),
            ));
        }
        run_codesign(
            [
                OsStr::new("--force"),
                OsStr::new("--sign"),
                OsStr::new("-"),
                path.as_os_str(),
            ],
            path,
            "sign",
        )?;
        run_codesign(
            [
                OsStr::new("--verify"),
                OsStr::new("--strict"),
                path.as_os_str(),
            ],
            path,
            "verify",
        )?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_codesign<const N: usize>(args: [&OsStr; N], path: &Path, operation: &str) -> io::Result<()> {
    let output = Command::new("/usr/bin/codesign").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "codesign {operation} failed for {} with {}: {}",
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(target_os = "macos")]
pub fn verify_paths(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        run_codesign(
            [
                OsStr::new("--verify"),
                OsStr::new("--strict"),
                path.as_os_str(),
            ],
            path,
            "post-test verify",
        )?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn verify_paths(_paths: &[PathBuf]) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_deduplicates_and_preserves_paths_with_spaces() {
        let input = br#"{"reason":"compiler-artifact","executable":"/tmp/a path/test"}
{"reason":"build-finished","success":true}
{"reason":"compiler-artifact","executable":"/tmp/a path/test"}
{"reason":"compiler-artifact","executable":null}
"#;
        assert_eq!(
            parse_executables(&input[..]).unwrap(),
            vec![PathBuf::from("/tmp/a path/test")]
        );
    }

    #[test]
    fn rejects_malformed_and_missing_artifacts() {
        let error = parse_executables(&b"not-json\n"[..]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let missing = br#"{"reason":"compiler-artifact","target":{}}
"#;
        assert!(parse_executables(&missing[..]).is_err());
    }

    #[test]
    fn full_execution_plan_builds_package_qualified_features() {
        let plan = parse_full_execution_plan(
            r#"{
              "cargo_full": {
                "harness_packages": ["alpha", "beta"],
                "separate": [{"package": "terminal", "test_args": ["--test-threads=1"]}]
              }
            }"#,
        )
        .unwrap();
        assert_eq!(
            plan,
            FullExecutionPlan {
                harness_packages: vec!["alpha".into(), "beta".into()],
                separate_packages: vec!["terminal".into()],
            }
        );
        assert_eq!(plan.feature_spec(), "alpha/full-tests,beta/full-tests");
        assert_eq!(
            plan.workspace_cargo_args("test", true, &["--no-run"]),
            vec![
                "test",
                "--workspace",
                "--exclude",
                "terminal",
                "--locked",
                "--no-run",
                "--features",
                "alpha/full-tests,beta/full-tests",
            ]
        );
    }

    #[test]
    fn full_execution_plan_rejects_missing_membership() {
        let error = parse_full_execution_plan(r#"{"cargo_full":{"separate":[]}}"#).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
