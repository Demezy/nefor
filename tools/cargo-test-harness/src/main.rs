use nefor_cargo_test_harness::{
    build_prepared_manifest, execute_prepared_manifest, load_full_execution_plan,
    prepare_manifest_for_platform, read_manifest, run_cargo_and_prepare, run_cargo_metadata,
    run_cargo_producer, run_doctests, write_immutable_manifest, TestLane,
};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{exit, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Invocation {
    lane: TestLane,
    timeout: Duration,
    prepare_only: bool,
    prepare_mag_e2e: bool,
    test_args: Vec<OsString>,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("prepared test harness error: {error}");
            ExitCode::from(125)
        }
    }
}

fn run() -> io::Result<i32> {
    let invocation = parse_invocation()?;
    let root = repository_root()?;
    let artifact_dir = unique_artifact_dir(&root.join("tmp/prepared-tests"))?;
    if invocation.prepare_mag_e2e {
        return prepare_mag_e2e_helpers(&root, &artifact_dir);
    }

    let plan = load_full_execution_plan(&root)?;
    let inventory = run_cargo_metadata(&root, &artifact_dir)?;
    let producer_json =
        run_cargo_producer(&root, &plan.producer_args(invocation.lane), &artifact_dir)?;
    let manifest = build_prepared_manifest(
        &root,
        &inventory,
        &plan,
        invocation.lane,
        BufReader::new(File::open(producer_json)?),
    )?;
    let manifest_path = artifact_dir.join("prepared-test-manifest.json");
    write_immutable_manifest(&manifest, &manifest_path)?;
    eprintln!(
        "prepared manifest: {} (tests={} helpers={} doctest_targets={})",
        manifest_path.display(),
        manifest.tests().count(),
        manifest.artifacts.len() - manifest.tests().count(),
        manifest.doctest_targets.len()
    );

    let doctest_status = run_doctests(
        &root,
        &plan.doctest_args(invocation.lane, &invocation.test_args),
        &artifact_dir,
    )?;
    if !doctest_status.success() {
        return Ok(101);
    }

    let manifest = read_manifest(&manifest_path)?;
    prepare_manifest_for_platform(&manifest, &artifact_dir)?;
    if invocation.prepare_only {
        eprintln!(
            "=== PREPARED TEST PREPARE ONLY COMPLETE: tests={} signed_artifacts={} ===",
            manifest.tests().count(),
            manifest.signed_paths().len()
        );
        return Ok(0);
    }

    let summary = execute_prepared_manifest(
        &manifest,
        invocation.timeout,
        &invocation.test_args,
        &artifact_dir,
    )?;
    Ok(summary.exit_code())
}

fn parse_invocation() -> io::Result<Invocation> {
    let mut args = env::args_os().skip(1).peekable();
    let mut lane = TestLane::Full;
    let mut timeout = Duration::from_hours(2);
    let mut prepare_only = false;
    let mut prepare_mag_e2e = false;
    let mut test_args = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--" {
            test_args.extend(args);
            break;
        }
        match arg.to_str() {
            Some("--lane") => {
                lane = match required_os(&mut args, "--lane")?.to_str() {
                    Some("default") => TestLane::Default,
                    Some("full") => TestLane::Full,
                    _ => return Err(invalid_input("--lane requires default or full")),
                };
            }
            Some("--timeout-seconds") => {
                timeout = parse_timeout(&required_os(&mut args, "--timeout-seconds")?)?;
            }
            Some("--prepare-only") => prepare_only = true,
            Some("--prepare-mag-e2e") => prepare_mag_e2e = true,
            Some("--help" | "-h") => {
                println!("{}", usage());
                exit(0);
            }
            Some(value) if !value.starts_with('-') => timeout = parse_timeout(&arg)?,
            _ => return Err(invalid_input(usage())),
        }
    }
    if prepare_mag_e2e && (prepare_only || !test_args.is_empty()) {
        return Err(invalid_input(
            "--prepare-mag-e2e cannot be combined with lane execution options",
        ));
    }
    Ok(Invocation {
        lane,
        timeout,
        prepare_only,
        prepare_mag_e2e,
        test_args,
    })
}

fn required_os(args: &mut impl Iterator<Item = OsString>, option: &str) -> io::Result<OsString> {
    args.next()
        .ok_or_else(|| invalid_input(format!("{option} requires a value")))
}

fn parse_timeout(raw: &OsStr) -> io::Result<Duration> {
    let raw = raw
        .to_str()
        .ok_or_else(|| invalid_input("timeout requires UTF-8"))?;
    let seconds: f64 = raw
        .parse()
        .map_err(|_| invalid_input(format!("invalid timeout seconds: {raw}")))?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(invalid_input("timeout must be a positive finite number"));
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn usage() -> &'static str {
    "usage: nefor-cargo-test-harness [--lane default|full] [--timeout-seconds N | N] [--prepare-only] [-- TEST_ARG ...]\n       nefor-cargo-test-harness --prepare-mag-e2e"
}

fn prepare_mag_e2e_helpers(root: &Path, artifact_dir: &Path) -> io::Result<i32> {
    let prepared = run_cargo_and_prepare(
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
        "could not allocate prepared-test artifact directory",
    ))
}
