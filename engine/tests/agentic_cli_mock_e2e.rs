//! Headless starter acceptance: the real engine/plugins, deterministic provider,
//! closed stdin, and a plugin directory in which no TUI executable exists.
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

fn binaries() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { root().join(p) })
        .unwrap_or_else(|| root().join("target"))
        .join("debug")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    run_inner(dir, args, false, &[])
}

fn run_with_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    run_inner(dir, args, false, env)
}

fn run_inner(dir: &Path, args: &[&str], interrupt: bool, extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(binaries().join("nefor"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("NEFOR_RUNTIME_ROOT", root())
        .env("NEFOR_EXECUTABLE_ROOT", dir.join("bin"))
        .env("NEFOR_TEST_FAST_MOCK", "1")
        .env("NEFOR_STARTUP_TIMEOUT_MS", "10000")
        .env("OPENAI_PROVIDER_API_KEY", "offline-sentinel")
        .arg("--config")
        .arg(root().join("examples/nefor-agent"))
        .arg("run")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn headless engine");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let drain = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let out = drain(Box::new(stdout));
    let err = drain(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut interrupted = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if interrupt && !interrupted {
            let accepted = std::fs::read_dir(dir.join("data/nefor/sessions"))
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
                .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                .any(|text| {
                    text.contains("SLOW_STREAM_REGRESSION_INTERRUPT")
                        && text.contains("mag.run_started")
                });
            if accepted {
                assert!(Command::new("/bin/kill")
                    .env_clear()
                    .args(["-INT", &child.id().to_string()])
                    .status()
                    .unwrap()
                    .success());
                interrupted = true;
            }
        }
        if Instant::now() >= deadline {
            let _ = Command::new("/bin/kill")
                .env_clear()
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "headless request timed out; retained artifacts: {}",
                dir.display()
            );
        }
        thread::sleep(Duration::from_millis(10));
    };
    if interrupt {
        assert!(interrupted, "the test must interrupt accepted work");
    }
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

fn success(out: &Output) -> String {
    assert!(
        out.status.success(),
        "status={:?}\nstderr={}\nstdout={}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!out.stdout.contains(&0x1b), "no terminal escapes");
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn headless_starter_request_resume_tools_and_failures() {
    std::fs::create_dir_all(root().join("tmp")).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("headless-acceptance-")
        .tempdir_in(root().join("tmp"))
        .unwrap()
        .keep();
    for name in ["home", "config", "data", "cache", "bin"] {
        std::fs::create_dir_all(dir.join(name)).unwrap();
    }
    for name in [
        "mag-plugin",
        "mock-plugin",
        "tool-gate",
        "basic-tools",
        "git-worktree",
    ] {
        let binary = binaries().join(name);
        assert!(
            binary.is_file(),
            "missing {}; run just build-headless",
            binary.display()
        );
        std::os::unix::fs::symlink(binary, dir.join("bin").join(name)).unwrap();
    }
    assert!(!dir.join("bin/nefor-tui").exists());
    std::fs::write(dir.join("README.md"), "headless fixture read result\n").unwrap();

    let first = run(
        &dir,
        &[
            "--frontend",
            "cli",
            "--prompt",
            "Summarise octopuses in one sentence.",
        ],
    );
    let text = success(&first);
    assert!(text.contains("Octopuses") && text.ends_with('\n'));
    let stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = stderr
        .lines()
        .find_map(|line| line.strip_prefix("session_id: "))
        .unwrap()
        .to_owned();
    assert_eq!(stderr.matches("session_id: ").count(), 1);
    let session_file = dir
        .join("data/nefor/sessions")
        .join(format!("{session_id}.jsonl"));
    assert!(
        session_file.is_file(),
        "initial CLI input must open persistence"
    );

    let resumed = run(
        &dir,
        &[
            "--frontend",
            "cli",
            "--resume",
            &session_id,
            "--prompt",
            "read readme",
            "--format",
            "json",
        ],
    );
    let result: serde_json::Value = serde_json::from_str(&success(&resumed)).unwrap();
    assert_eq!(result["session_id"], session_id);
    assert_eq!(result["status"], "success");
    assert!(result["request_id"]
        .as_str()
        .unwrap()
        .starts_with("request-"));
    let persisted = std::fs::read_to_string(&session_file).unwrap();
    let mut submits = 0;
    let mut tools = 0;
    for line in persisted.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let Some(payload) = row["payload"].as_str() else {
            continue;
        };
        let envelope: serde_json::Value = serde_json::from_str(payload).unwrap();
        let body = &envelope["body"];
        if body["kind"] == "chat.input.submit" {
            submits += 1;
        }
        if body["kind"] == "conversation.fact.recorded" {
            if payload.contains("tool_") {
                tools += 1;
            }
            assert!(!payload.contains("\"display\":"));
        }
        assert_ne!(
            body["kind"], "tool.register",
            "no frontend display catalogs persisted"
        );
    }
    assert_eq!(submits, 2, "resume does not re-submit old input");
    assert!(tools > 0, "tool exchange remains canonical");

    let dispatch = run(
        &dir,
        &[
            "--frontend",
            "cli",
            "--mode",
            "yolo",
            "--format",
            "json",
            "--prompt",
            "summarise octopuses and lighthouses in parallel and combine into one paragraph",
        ],
    );
    let answer: serde_json::Value = serde_json::from_str(&success(&dispatch)).unwrap();
    assert!(answer["answer"]
        .as_str()
        .unwrap()
        .contains("steadfast lighthouse"));
    let dispatched_session = answer["session_id"].as_str().unwrap();
    assert!(dir
        .join("data/nefor/sessions")
        .join(dispatched_session)
        .join("mag")
        .is_dir());

    let error = run(
        &dir,
        &["--frontend", "cli", "--prompt", "fail", "--format", "json"],
    );
    assert_eq!(
        error.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&error.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&error.stdout).unwrap()["status"],
        "error"
    );
    let missing = run(
        &dir,
        &[
            "--frontend",
            "cli",
            "--resume",
            "does-not-exist",
            "--prompt",
            "hello",
        ],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    let usage = run(&dir, &["--frontend", "cli"]);
    assert_eq!(usage.status.code(), Some(2));
    assert!(usage.stdout.is_empty());
    let withheld = run_with_env(
        &dir,
        &[
            "--frontend",
            "cli",
            "--resume",
            &session_id,
            "--prompt",
            "This prompt must not submit.",
            "--format",
            "json",
        ],
        &[
            ("NEFOR_DEFAULT_PROVIDER", "unavailable-default"),
            ("NEFOR_TEST_PROVIDER_HELLO", "withhold"),
            ("NEFOR_STARTUP_TIMEOUT_MS", "8000"),
        ],
    );
    assert_eq!(withheld.status.code(), Some(1));
    let withheld_result: serde_json::Value = serde_json::from_slice(&withheld.stdout).unwrap();
    assert_eq!(withheld_result["status"], "error");
    assert!(withheld_result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("mock-plugin"));
    assert_eq!(
        std::fs::read_to_string(&session_file)
            .unwrap()
            .matches("chat.input.submit")
            .count(),
        2,
        "historical readiness must not submit a new prompt"
    );

    let resumed_with_saved_provider = run_with_env(
        &dir,
        &[
            "--frontend",
            "cli",
            "--resume",
            &session_id,
            "--prompt",
            "Summarise octopuses in one sentence.",
            "--format",
            "json",
        ],
        &[
            ("NEFOR_DEFAULT_PROVIDER", "unavailable-default"),
            ("NEFOR_TEST_PROVIDER_HELLO", "delay"),
        ],
    );
    let saved_provider_result: serde_json::Value =
        serde_json::from_str(&success(&resumed_with_saved_provider)).unwrap();
    assert_eq!(saved_provider_result["status"], "success");
    assert_eq!(saved_provider_result["session_id"], session_id);

    let interrupted = run_inner(
        &dir,
        &[
            "--frontend",
            "cli",
            "--format",
            "json",
            "--prompt",
            "SLOW_STREAM_REGRESSION_INTERRUPT",
        ],
        true,
        &[],
    );
    assert_eq!(
        interrupted.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&interrupted.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&interrupted.stdout).unwrap()["status"],
        "interrupted"
    );
    println!("retained headless artifacts: {}", dir.display());
}
