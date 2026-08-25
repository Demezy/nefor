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
    assert_eq!(body["version"], 1);
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["artifact"]["contracts"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(body["hash"].as_str().map(str::len), Some(64));
}

#[test]
fn profile_is_opt_in_and_machine_readable() {
    let fixture = Fixture::new("profile");
    fixture.write("main.mag", "(artifact {})");

    let ordinary = json_stdout(&run(&compile_args(&fixture.root, &[])));
    assert!(ordinary.get("profile").is_none());

    let profiled = json_stdout(&run(&compile_args(&fixture.root, &["--profile"])));
    assert_eq!(profiled["ok"], true);
    assert!(profiled["profile"]["phases"]["entry_evaluate_ns"].is_u64());
    assert_eq!(profiled["profile"]["counters"]["evaluator_steps"], 3);
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
        assert!(!output.stderr.is_empty(), "{name} needs human stderr");
        let body = json_stdout(&output);
        assert_eq!(body["version"], 1);
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"]["code"], code);
        assert_eq!(body["error"]["stage"], stage);
        assert!(body["error"]["message"].is_string());
        if name == "syntax" {
            assert_eq!(
                body["error"]["diagnostic"]["path"],
                fixture.root.join("main.mag").display().to_string()
            );
            assert_eq!(body["error"]["diagnostic"]["source"], source);
            assert_eq!(body["error"]["diagnostic"]["span"]["start"], source.len());
            assert_eq!(body["error"]["diagnostic"]["span"]["end"], source.len());
            assert_eq!(body["error"]["diagnostic"]["related"]["span"]["start"], 0);
        }
    }
}

#[test]
fn required_module_syntax_diagnostic_owns_its_snapshot() {
    let fixture = Fixture::new("module-diagnostic");
    let module = fixture.write("bad.mag", "[λ]");
    fixture.write("main.mag", "(require \"bad\")\n(artifact {})");
    let output = run(&compile_args(&fixture.root, &[]));
    assert!(!output.status.success());
    let body = json_stdout(&output);
    assert_eq!(body["error"]["code"], "syntax_lex");
    assert_eq!(
        body["error"]["diagnostic"]["path"],
        module
            .canonicalize()
            .expect("canonical module")
            .display()
            .to_string()
    );
    assert_eq!(body["error"]["diagnostic"]["source"], "[λ]");
    assert_eq!(body["error"]["diagnostic"]["span"]["start"], 1);
    assert_eq!(body["error"]["diagnostic"]["span"]["end"], 3);
}

#[test]
fn path_and_host_input_failures_are_structured() {
    let fixture = Fixture::new("paths");
    fixture.write("main.mag", "(artifact {})");
    let missing = fixture.root.join("missing.json");
    let input = format!("data={}", missing.display());
    let output = run(&compile_args(&fixture.root, &["--input", &input]));
    assert!(!output.status.success());
    let body = json_stdout(&output);
    assert_eq!(body["error"]["code"], "input_read");
    assert_eq!(
        body["error"]["path"],
        missing.to_str().expect("missing path")
    );
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
