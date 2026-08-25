use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

struct Invocation {
    lane: OsString,
    timeout: OsString,
    full: bool,
    prepare_only: bool,
    prepare_mag_e2e: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("macOS test harness error: {error}");
            ExitCode::from(125)
        }
    }
}

fn run() -> io::Result<u8> {
    let invocation = parse_invocation()?;
    let Invocation {
        lane,
        timeout,
        full,
        prepare_only,
        prepare_mag_e2e,
    } = invocation;
    let root = repository_root()?;
    let execution_plan = nefor_cargo_test_harness::load_full_execution_plan(&root)?;
    let artifact_dir = unique_artifact_dir(&root.join("tmp/macos-test-signing"))?;
    if prepare_mag_e2e {
        return prepare_mag_e2e_helpers(&root, &artifact_dir);
    }
    let cargo_args = execution_plan.workspace_cargo_args("test", full, &[]);

    let watchdog = cargo_target_dir(&root).join("debug/nefor-test-watchdog");
    build_watchdog(&root)?;

    #[cfg(target_os = "macos")]
    let prepared = prepare_test_artifacts(&root, &artifact_dir, &execution_plan, full)?;
    #[cfg(not(target_os = "macos"))]
    let prepared = nefor_cargo_test_harness::PreparedArtifacts { paths: Vec::new() };

    if prepare_only {
        eprintln!(
            "=== TEST PREPARE ONLY COMPLETE: {} executables ===",
            prepared.paths.len()
        );
        return Ok(0);
    }

    let status = Command::new(watchdog)
        .current_dir(&root)
        .arg("--phase")
        .arg(format!("cargo-{lane}", lane = lane.to_string_lossy()))
        .arg("--timeout-seconds")
        .arg(timeout)
        .arg("--")
        .arg(env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
        .args(&cargo_args)
        .status()?;
    nefor_cargo_test_harness::verify_paths(&prepared.paths).map_err(|error| {
        io::Error::other(format!(
            "a Cargo test artifact was replaced or lost its signature after prebuild: {error}"
        ))
    })?;
    status
        .code()
        .map_or(Ok(1), |code| Ok(u8::try_from(code).unwrap_or(1)))
}

fn parse_invocation() -> io::Result<Invocation> {
    let mut args = env::args_os().skip(1);
    let first = args.next();
    let prepare_only = first.as_deref() == Some(std::ffi::OsStr::new("--prepare-only"));
    let prepare_mag_e2e = first.as_deref() == Some(std::ffi::OsStr::new("--prepare-mag-e2e"));
    let (lane, timeout) = if first.as_deref() == Some(std::ffi::OsStr::new("--lane")) {
        let lane = args.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "--lane requires default or full",
            )
        })?;
        let timeout = args.next().unwrap_or_else(|| OsString::from("7200"));
        (lane, timeout)
    } else if prepare_only || prepare_mag_e2e {
        (OsString::from("full"), OsString::from("7200"))
    } else {
        (
            OsString::from("full"),
            first.unwrap_or_else(|| OsString::from("7200")),
        )
    };
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: nefor-cargo-test-harness [--lane default|full [TIMEOUT_SECONDS] | TIMEOUT_SECONDS | --prepare-only | --prepare-mag-e2e]",
        ));
    }
    let full = match lane.to_str() {
        Some("default") => false,
        Some("full") => true,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lane must be default or full",
            ));
        }
    };
    Ok(Invocation {
        lane,
        timeout,
        full,
        prepare_only,
        prepare_mag_e2e,
    })
}

fn prepare_mag_e2e_helpers(root: &Path, artifact_dir: &Path) -> io::Result<u8> {
    let prepared = nefor_cargo_test_harness::run_cargo_and_prepare(
        root,
        &[
            "build",
            "--locked",
            "-p",
            "mag-plugin",
            "--bin",
            "mag-plugin",
            "-p",
            "tool-gate-plugin",
            "--bin",
            "tool-gate",
            "-p",
            "basic-tools-plugin",
            "--bin",
            "basic-tools",
            "-p",
            "openai-provider",
            "--bin",
            "openai-provider",
            "-p",
            "chatgpt-provider",
            "--bin",
            "chatgpt-provider",
        ],
        None,
        &artifact_dir.join("mag-e2e-runtime-helpers"),
    )?;
    eprintln!(
        "=== MAG E2E PREPARATION COMPLETE: {} executables ===",
        prepared.paths.len()
    );
    Ok(0)
}

fn build_watchdog(root: &Path) -> io::Result<()> {
    let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
        .current_dir(root)
        .args(["build", "--quiet", "-p", "nefor-test-watchdog"])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "could not build test watchdog: {status}"
        )));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn prepare_test_artifacts(
    root: &Path,
    artifact_dir: &Path,
    execution_plan: &nefor_cargo_test_harness::FullExecutionPlan,
    full: bool,
) -> io::Result<nefor_cargo_test_harness::PreparedArtifacts> {
    let helper_args = execution_plan.workspace_cargo_args("build", false, &["--bins"]);
    let helper_arg_refs = helper_args.iter().map(String::as_str).collect::<Vec<_>>();
    let helpers = nefor_cargo_test_harness::run_cargo_and_prepare(
        root,
        &helper_arg_refs,
        None,
        &artifact_dir.join("runtime-helpers"),
    )?;
    let test_args = execution_plan.workspace_cargo_args("test", full, &["--no-run"]);
    let test_arg_refs = test_args.iter().map(String::as_str).collect::<Vec<_>>();
    let tests = nefor_cargo_test_harness::run_cargo_and_prepare(
        root,
        &test_arg_refs,
        None,
        &artifact_dir.join("test-executables"),
    )?;
    let mut paths = helpers.paths;
    paths.extend(tests.paths);
    paths.sort();
    paths.dedup();
    Ok(nefor_cargo_test_harness::PreparedArtifacts { paths })
}

fn repository_root() -> io::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("harness manifest is not beneath repository root"))
}

fn unique_artifact_dir(root: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    for suffix in 0..100_u8 {
        let path = root.join(format!("{stamp}-{}-{suffix}", std::process::id()));
        if !path.exists() {
            fs::create_dir(&path)?;
            return Ok(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate signing artifact directory",
    ))
}

fn cargo_target_dir(root: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}
