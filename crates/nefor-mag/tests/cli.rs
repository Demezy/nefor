use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("nefor-mag-cli-{name}-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).expect("create fixture");
        Self { root }
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(&path, source).expect("write fixture");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mag"))
        .args(args)
        .output()
        .expect("run mag")
}

fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON document")
}

fn json_stderr(output: &Output) -> Value {
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON diagnostic")
}

fn compile_args<'a>(root: &'a Path, extra: &'a [&'a str]) -> Vec<&'a str> {
    let mut args = vec!["compile", "main.mag", "--source-dir"];
    args.push(root.to_str().expect("utf8 fixture path"));
    args.extend_from_slice(extra);
    args
}

#[test]
fn compiles_with_caller_supplied_module_root_and_host_input() {
    let fixture = Fixture::new("success");
    let modules = fixture.root.join("modules");
    fs::create_dir_all(&modules).expect("modules");
    fixture.write(
        "modules/contracts.mag",
        "(type Scheme {:input_tags (List String) :outputs (List String)})\n(type Contract {:identity String :type_scheme Scheme})\n(let contracts (host-input \"factory_contracts\" (type-tag (List Contract))))",
    );
    fixture.write(
        "main.mag",
        r#"(require "contracts")
(artifact {:contracts contracts.contracts})"#,
    );
    let contracts = fixture.write(
        "contracts.json",
        r#"[{
  "identity": "example.factory.echo",
  "implementation": "echo",
  "params": {},
  "type_scheme": {"variables": [], "inputs": {}, "input_tags": [], "outputs": []},
  "signals": []
}]"#,
    );
    let input = format!("factory_contracts={}", contracts.display());
    let output = run(&compile_args(
        &fixture.root,
        &[
            "--module-root",
            modules.to_str().expect("modules path"),
            "--input",
            &input,
        ],
    ));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = json_stdout(&output);
    assert_eq!(body["contracts"].as_array().map(Vec::len), Some(1));
    assert!(output.stderr.is_empty());
}

#[test]
fn profile_is_opt_in_machine_readable_and_separate_from_the_artifact() {
    let fixture = Fixture::new("profile");
    fixture.write("main.mag", "(artifact {:metadata \"application data\"})");

    let ordinary = run(&compile_args(&fixture.root, &[]));
    assert_eq!(
        json_stdout(&ordinary),
        serde_json::json!({"metadata": "application data"})
    );
    assert!(ordinary.stderr.is_empty());

    let profiled = run(&compile_args(&fixture.root, &["--profile"]));
    assert!(profiled.status.success());
    assert_eq!(
        json_stdout(&profiled),
        serde_json::json!({"metadata": "application data"})
    );
    let profile = json_stderr(&profiled);
    assert!(profile["phases"]["entry_evaluate_ns"].is_u64());
    assert!(profile["counters"]["evaluator_steps"].is_u64());
}

#[test]
fn syntax_type_and_evaluation_failures_are_structured() {
    for (name, source, code, stage) in [
        ("syntax", "(artifact", "syntax_parse", "parse"),
        (
            "type",
            "(artifact {:bad (+ 1 \"x\")})",
            "type_error",
            "typecheck",
        ),
        (
            "evaluation",
            "(fail {:kind \"application\" :message \"requested failure\"})",
            "evaluation_error",
            "evaluate",
        ),
    ] {
        let fixture = Fixture::new(name);
        fixture.write("main.mag", source);
        let output = run(&compile_args(&fixture.root, &[]));
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        assert!(output.stdout.is_empty(), "{name} produced a result value");
        assert!(!output.stderr.is_empty(), "{name} needs human stderr");
        let body = json_stderr(&output);
        assert_eq!(body["code"], code);
        assert_eq!(body["stage"], stage);
        assert!(body["message"].is_string());
        if name == "syntax" {
            assert_eq!(
                body["diagnostic"]["path"],
                fixture.root.join("main.mag").display().to_string()
            );
            assert_eq!(body["diagnostic"]["source"], source);
            assert_eq!(body["diagnostic"]["span"]["start"], source.len());
            assert_eq!(body["diagnostic"]["span"]["end"], source.len());
            assert_eq!(body["diagnostic"]["related"]["span"]["start"], 0);
        }
    }
}

#[test]
fn profiling_does_not_change_failure_stdout_or_diagnostic() {
    let fixture = Fixture::new("profile-failure");
    fixture.write("main.mag", "(artifact {:bad (+ 1 \"x\")})");

    let ordinary = run(&compile_args(&fixture.root, &[]));
    let profiled = run(&compile_args(&fixture.root, &["--profile"]));

    assert!(!ordinary.status.success());
    assert!(!profiled.status.success());
    assert!(ordinary.stdout.is_empty());
    assert_eq!(profiled.stdout, ordinary.stdout);
    let ordinary_diagnostic = json_stderr(&ordinary);
    let profiled_diagnostic = json_stderr(&profiled);
    for field in ["code", "stage", "message", "path", "diagnostic"] {
        assert_eq!(profiled_diagnostic[field], ordinary_diagnostic[field]);
    }
    assert!(ordinary_diagnostic.get("profile").is_none());
    assert!(profiled_diagnostic["profile"]["total_duration_ns"].is_u64());
    assert!(profiled_diagnostic["profile"]["phases"]["checking_ns"].is_u64());
}

#[test]
fn required_module_syntax_diagnostic_owns_its_snapshot() {
    let fixture = Fixture::new("module-diagnostic");
    let module = fixture.write("bad.mag", "[λ]");
    fixture.write("main.mag", "(require \"bad\")\n(artifact {})");
    let output = run(&compile_args(&fixture.root, &[]));
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let body = json_stderr(&output);
    assert_eq!(body["code"], "syntax_lex");
    assert_eq!(
        body["diagnostic"]["path"],
        module
            .canonicalize()
            .expect("canonical module")
            .display()
            .to_string()
    );
    assert_eq!(body["diagnostic"]["source"], "[λ]");
    assert_eq!(body["diagnostic"]["span"]["start"], 1);
    assert_eq!(body["diagnostic"]["span"]["end"], 3);
}

#[test]
fn path_and_host_input_failures_are_structured() {
    let fixture = Fixture::new("paths");
    fixture.write("main.mag", "(artifact {})");
    let missing = fixture.root.join("missing.json");
    let input = format!("data={}", missing.display());
    let output = run(&compile_args(&fixture.root, &["--input", &input]));
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let body = json_stderr(&output);
    assert_eq!(body["code"], "input_read");
    assert_eq!(body["path"], missing.to_str().expect("missing path"));
}

#[test]
fn compiler_limits_are_overridable_from_the_cli() {
    let fixture = Fixture::new("limits");
    fixture.write(
        "main.mag",
        r#"(let identity (fn [[value Int]] -> Int value))
(let first-value (identity 1))
(let second-value (identity 1))
(artifact [first-value second-value])"#,
    );

    for (flag, value) in [
        ("--evaluation-step-limit", "1"),
        ("--call-depth-limit", "0"),
        ("--expression-depth-limit", "1"),
    ] {
        let output = run(&compile_args(&fixture.root, &[flag, value]));
        assert!(
            !output.status.success(),
            "{flag} did not constrain evaluation"
        );
        assert!(output.stdout.is_empty());
        assert_eq!(json_stderr(&output)["code"], "evaluation_budget");
    }

    let output = run(&compile_args(
        &fixture.root,
        &["--memoized-call-limit", "0", "--profile"],
    ));
    assert!(output.status.success());
    assert_eq!(json_stdout(&output), serde_json::json!([1, 1]));
    let body = json_stderr(&output);
    let counters = &body["counters"];
    assert_eq!(counters["memoized_call_hits"], 0);
    assert_eq!(counters["memoized_call_stores"], 0);
}

#[test]
fn command_surface_has_no_execute_or_run_operation() {
    for forbidden in ["execute", "run"] {
        let output = run(&[forbidden]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
    }

    let help = run(&["--help"]);
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains("compile"));
    assert!(!stdout
        .lines()
        .any(|line| line.trim_start().starts_with("execute")));
    assert!(!stdout
        .lines()
        .any(|line| line.trim_start().starts_with("run")));

    let compile_help = run(&["compile", "--help"]);
    assert!(compile_help.status.success());
    let stdout = String::from_utf8_lossy(&compile_help.stdout);
    assert!(stdout.contains("artifact"), "{stdout}");
    assert!(!stdout.contains("resulting graph"), "{stdout}");
}

fn build_at(cwd: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mag"))
        .current_dir(cwd)
        .args(["build", "main.mag"])
        .args(extra)
        .output()
        .expect("run project build")
}

#[test]
fn project_is_explicit_or_cwd_and_never_discovered_upwards() {
    let fixture = Fixture::new("project-selection");
    fixture.write("mag.toml", "");
    fixture.write("main.mag", "(artifact {:a 1})");
    fs::create_dir(fixture.root.join("child")).unwrap();
    let output = build_at(&fixture.root, &["--no-cache"]);
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(output.stdout, b"{\"a\":1}\n");
    assert!(!fixture.root.join(".mag").exists());
    let output = build_at(&fixture.root.join("child"), &[]);
    assert_eq!(json_stderr(&output)["code"], "project_read");
    assert!(output.stdout.is_empty());
    let output = build_at(&fixture.root.join("child"), &["--project", ".."]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\"a\":1}\n");
}

#[test]
fn project_manifest_is_strict_and_input_stage() {
    let fixture = Fixture::new("project-manifest");
    fixture.write("main.mag", "(artifact 1)");
    for (manifest, code) in [
        ("version = 2", "project_version"),
        ("targets = []", "project_config"),
        ("module-roots = 3", "project_config"),
        ("version = ", "project_config"),
    ] {
        fixture.write("mag.toml", manifest);
        let output = build_at(&fixture.root, &[]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(json_stderr(&output)["code"], code);
        assert_eq!(json_stderr(&output)["stage"], "input");
    }
}

#[test]
fn project_roots_and_host_inputs_are_anchored_to_project() {
    let fixture = Fixture::new("project-roots");
    let installed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    fixture.write(
        "mag.toml",
        &format!("version = 1\nmodule-roots = [\"lib\", {:?}]", installed),
    );
    fixture.write("lib/value.mag", "(let number 7)");
    fixture.write("extra/other.mag", "(let number 8)");
    fixture.write("local.mag", "(let number 9)");
    fixture.write(
        "main.mag",
        r#"(require "value")
(require "other")
(require "local")
(require "core.types")
(require "nefor.contracts")
(artifact [value.number other.number local.number (host-input "data" (type-tag Int))])"#,
    );
    fixture.write("data.json", "42");
    fs::create_dir(fixture.root.join("child")).unwrap();
    let output = build_at(
        &fixture.root.join("child"),
        &[
            "--project",
            "..",
            "--module-root",
            "extra",
            "--input",
            "data=data.json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"[7,8,9,42]\n");
    let output = build_at(
        &fixture.root,
        &[
            "--module-root",
            "extra",
            "--input",
            "data=data.json",
            "--input",
            "data=data.json",
        ],
    );
    assert_eq!(json_stderr(&output)["code"], "input_duplicate");
    let output = build_at(
        &fixture.root,
        &["--module-root", "absent", "--input", "data=absent.json"],
    );
    assert_eq!(json_stderr(&output)["code"], "path_unavailable");
    let output = build_at(&fixture.root, &["--input", "data=absent.json"]);
    assert_eq!(json_stderr(&output)["code"], "input_read");
}

#[test]
fn project_limits_and_profile_match_cold_compile() {
    let fixture = Fixture::new("project-limits");
    fixture.write("mag.toml", "");
    fixture.write(
        "main.mag",
        "(let id (fn [[x Int]] -> Int x))\n(artifact [(id 1) (id 1)])",
    );
    for flags in [
        vec!["--evaluation-step-limit", "1"],
        vec!["--call-depth-limit", "0"],
        vec!["--expression-depth-limit", "1"],
        vec!["--memoized-call-limit", "0", "--profile"],
    ] {
        let build = build_at(&fixture.root, &flags);
        let cold = run(&compile_args(&fixture.root, &flags));
        assert_eq!(build.status.success(), cold.status.success());
        assert_eq!(build.stdout, cold.stdout);
        if !build.status.success() {
            assert_eq!(json_stderr(&build), json_stderr(&cold));
        } else {
            assert_eq!(
                json_stderr(&build)["counters"],
                json_stderr(&cold)["counters"]
            );
        }
    }
}

#[test]
fn project_build_replays_exact_bytes_with_real_executable_identity_and_zero_work() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new("project-cache");
    fixture.write("mag.toml", "");
    fixture.write(
        "main.mag",
        "(require \"value\")\n(artifact {:value value.number :text (read \"note.txt\")})",
    );
    fixture.write("value.mag", "(let number 7)");
    fixture.write("note.txt", "hello\nworld");
    let cold = run(&compile_args(&fixture.root, &[]));
    let population = build_at(&fixture.root, &["--profile"]);
    assert!(population.status.success());
    assert_eq!(population.stdout, cold.stdout);
    assert_eq!(json_stderr(&population)["cache"]["status"], "miss");
    assert!(
        json_stderr(&population)["counters"]["evaluator_steps"]
            .as_u64()
            .unwrap()
            > 0
    );
    let executable_hash = format!(
        "{:x}",
        Sha256::digest(fs::read(env!("CARGO_BIN_EXE_mag")).unwrap())
    );
    assert!(fixture
        .root
        .join(".mag/cache/v1")
        .join(executable_hash)
        .is_dir());
    fixture.write(
        "mag.toml",
        "# Same effective configuration\nversion = 1\nmodule-roots = []\n",
    );
    fixture.write("unrelated.txt", "changed");
    let hit = build_at(&fixture.root, &["--profile"]);
    assert!(hit.status.success());
    assert_eq!(hit.stdout, cold.stdout);
    assert_eq!(json_stderr(&hit)["cache"]["status"], "hit");
    assert_eq!(
        json_stderr(&hit)["counters"],
        serde_json::to_value(nefor_mag::profile::OperationCounters::default()).unwrap()
    );
    assert_eq!(
        json_stderr(&hit)["phases"],
        serde_json::to_value(nefor_mag::profile::PhaseDurations::default()).unwrap()
    );
    let bypass = build_at(&fixture.root, &["--no-cache", "--profile"]);
    assert_eq!(bypass.stdout, hit.stdout);
    assert_eq!(json_stderr(&bypass)["cache"]["status"], "bypass");
    assert!(
        json_stderr(&bypass)["counters"]["evaluator_steps"]
            .as_u64()
            .unwrap()
            > 0
    );
    fixture.write("note.txt", "different");
    let changed = build_at(&fixture.root, &["--profile"]);
    assert_eq!(json_stderr(&changed)["cache"]["status"], "miss");
    assert_ne!(changed.stdout, hit.stdout);
    fixture.write("note.txt", "hello\nworld");
    assert_eq!(
        json_stderr(&build_at(&fixture.root, &["--profile"]))["cache"]["status"],
        "hit"
    );
}

#[test]
fn project_preparation_errors_still_precede_a_seeded_cache_hit() {
    let fixture = Fixture::new("project-cache-inputs");
    fixture.write("mag.toml", "");
    fixture.write("main.mag", "(artifact 1)");
    fixture.write("data.json", "{\"unused\":1}");
    let flags = ["--input", "data=data.json", "--profile"];
    assert_eq!(
        json_stderr(&build_at(&fixture.root, &flags))["cache"]["status"],
        "miss"
    );
    fixture.write("data.json", "{\"unused\":2}");
    assert_eq!(
        json_stderr(&build_at(&fixture.root, &flags))["cache"]["status"],
        "miss"
    );
    fixture.write("data.json", "{\"unused\":1}");
    assert_eq!(
        json_stderr(&build_at(&fixture.root, &flags))["cache"]["status"],
        "hit"
    );
    fixture.write("data.json", "not json");
    let failed = build_at(&fixture.root, &flags);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert_eq!(json_stderr(&failed)["code"], "input_json");
    fixture.write("mag.toml", "version = 99");
    assert_eq!(
        json_stderr(&build_at(&fixture.root, &flags))["code"],
        "project_version"
    );
}
