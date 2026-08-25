use std::collections::BTreeMap;
use std::fs;
#[cfg(target_os = "macos")]
use std::fs::OpenOptions;
#[cfg(target_os = "macos")]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use nefor_cargo_test_harness::{
    execute_prepared_manifest, prepare_paths_for_platform, verify_paths, PreparedArtifact,
    PreparedArtifactRole, PreparedTestManifest, TestLane, MANIFEST_SCHEMA_VERSION,
};

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

static SIGNING_LOCK: Mutex<()> = Mutex::new(());

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn temp_root(name: &str) -> PathBuf {
    let root = repository_root()
        .join("tmp/cargo-test-harness-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn platform_preparation_writes_exact_manifest_and_rejects_missing_artifacts() {
    let _signing = SIGNING_LOCK.lock().unwrap();
    let root = temp_root("platform-preparation");
    #[cfg(not(target_os = "macos"))]
    {
        let paths = vec![std::env::current_exe().unwrap()];
        prepare_paths_for_platform(&paths, &root).unwrap();
        verify_paths(&paths).unwrap();
    }
    #[cfg(target_os = "macos")]
    {
        let binary = std::env::current_exe().unwrap();
        prepare_paths_for_platform(std::slice::from_ref(&binary), &root).unwrap();
        verify_paths(std::slice::from_ref(&binary)).unwrap();
        let missing = root.join("missing executable");
        let error = nefor_cargo_test_harness::sign_and_verify(&[missing]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn post_sign_verification_rejects_a_mutated_executable() {
    let _signing = SIGNING_LOCK.lock().unwrap();
    let root = temp_root("invalid-signature");
    let binary = root.join("mutated-fixture");
    fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
    nefor_cargo_test_harness::sign_and_verify(std::slice::from_ref(&binary)).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&binary)
        .unwrap()
        .write_all(b"signature-invalidating-byte")
        .unwrap();
    let error = verify_paths(&[binary]).unwrap_err();
    assert!(error.to_string().contains("post-test verify failed"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_rejects_a_missing_watchdog_helper() {
    let binary = std::env::current_exe().unwrap().canonicalize().unwrap();
    let root = repository_root().canonicalize().unwrap();
    let manifest = PreparedTestManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        lane: TestLane::Full,
        repository_root: root.clone(),
        target_dir: binary.parent().unwrap().to_path_buf(),
        artifacts: vec![artifact(
            "fixture-test",
            PreparedArtifactRole::TestExecutable,
            &binary,
            &root,
            None,
        )],
        doctest_targets: Vec::new(),
    };
    let error = manifest.validate().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains("nefor-test-watchdog"));
}

#[test]
fn prepared_runner_aggregates_failure_timeout_and_later_success() {
    let _signing = SIGNING_LOCK.lock().unwrap();
    let root = temp_root("controlled-outcomes");
    let repository = repository_root().canonicalize().unwrap();
    let fixture = std::env::current_exe().unwrap().canonicalize().unwrap();
    let debug_dir = fixture.parent().unwrap().parent().unwrap();
    let watchdog = debug_dir.join("nefor-test-watchdog").canonicalize().unwrap_or_else(|_| {
        panic!(
            "missing prepared watchdog {}; run this fixture through the canonical prepared full lane",
            debug_dir.join("nefor-test-watchdog").display()
        )
    });
    prepare_paths_for_platform(&[fixture.clone(), watchdog.clone()], &root).unwrap();
    let descendant_pid = root.join("descendant.pid");
    let manifest = PreparedTestManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        lane: TestLane::Full,
        repository_root: repository.clone(),
        target_dir: debug_dir.parent().unwrap().to_path_buf(),
        artifacts: vec![
            artifact(
                "nefor-test-watchdog",
                PreparedArtifactRole::RuntimeHelper,
                &watchdog,
                &repository,
                None,
            ),
            fixture_artifact(
                "controlled-failure",
                &fixture,
                &repository,
                "fail",
                None,
                None,
            ),
            fixture_artifact(
                "controlled-timeout",
                &fixture,
                &repository,
                "hang",
                Some(200),
                Some(&descendant_pid),
            ),
            fixture_artifact(
                "post-failure-pass",
                &fixture,
                &repository,
                "pass",
                None,
                None,
            ),
        ],
        doctest_targets: Vec::new(),
    };
    let summary = execute_prepared_manifest(&manifest, Duration::from_secs(5), &[], &root).unwrap();
    assert_eq!(summary.passed, 1);
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.timed_out, 1);
    let descendant: i32 = fs::read_to_string(descendant_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(descendant) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !process_exists(descendant),
        "descendant {descendant} survived"
    );
    fs::remove_dir_all(root).unwrap();
}

fn fixture_artifact(
    name: &str,
    fixture: &Path,
    cwd: &Path,
    behavior: &str,
    timeout_millis: Option<u64>,
    pid_file: Option<&Path>,
) -> PreparedArtifact {
    let mut artifact = artifact(
        name,
        PreparedArtifactRole::TestExecutable,
        fixture,
        cwd,
        timeout_millis,
    );
    artifact.test_args = vec!["--exact".into(), "fixture_process_behavior".into()];
    artifact
        .environment
        .insert("NEFOR_PREPARED_FIXTURE".into(), behavior.into());
    if behavior == "pass" {
        artifact
            .environment
            .insert("NEFOR_PREPARED_FIXTURE_CWD".into(), cwd.as_os_str().into());
        artifact.environment.insert(
            "NEFOR_PREPARED_FIXTURE_TARGET".into(),
            fixture
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .unwrap()
                .as_os_str()
                .into(),
        );
    }
    if let Some(pid_file) = pid_file {
        artifact.environment.insert(
            "NEFOR_PREPARED_FIXTURE_PID".into(),
            pid_file.as_os_str().into(),
        );
    }
    artifact
}

fn artifact(
    target: &str,
    role: PreparedArtifactRole,
    executable: &Path,
    cwd: &Path,
    timeout_millis: Option<u64>,
) -> PreparedArtifact {
    PreparedArtifact {
        package: "fixture".to_owned(),
        target: target.to_owned(),
        target_kind: vec!["test".to_owned()],
        executable: executable.to_path_buf(),
        cwd: cwd.to_path_buf(),
        role,
        test_args: Vec::new(),
        environment: BTreeMap::new(),
        timeout_millis,
    }
}

fn process_exists(pid: i32) -> bool {
    #[cfg(unix)]
    // SAFETY: signal zero checks process existence without delivering a signal.
    unsafe {
        kill(pid, 0) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[test]
fn fixture_process_behavior() {
    match std::env::var("NEFOR_PREPARED_FIXTURE").as_deref() {
        Ok("fail") => {
            println!("controlled fixture failure stdout");
            eprintln!("controlled fixture failure stderr");
            std::process::exit(7);
        }
        Ok("hang") => {
            let child = Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap();
            fs::write(
                std::env::var_os("NEFOR_PREPARED_FIXTURE_PID").unwrap(),
                child.id().to_string(),
            )
            .unwrap();
            thread::sleep(Duration::from_secs(30));
        }
        Ok("pass") => {
            assert_eq!(
                std::env::current_dir().unwrap(),
                PathBuf::from(std::env::var_os("NEFOR_PREPARED_FIXTURE_CWD").unwrap())
            );
            assert_eq!(
                std::env::var_os("CARGO_TARGET_DIR").unwrap(),
                std::env::var_os("NEFOR_PREPARED_FIXTURE_TARGET").unwrap()
            );
        }
        Err(_) => {}
        Ok(other) => panic!("unknown controlled fixture behavior: {other}"),
    }
}
