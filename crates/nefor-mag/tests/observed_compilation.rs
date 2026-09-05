use nefor_mag::observation::{compile_file_observed, ObservationSet};
use nefor_mag::{CompilerOptions, CompilerSession, FileCompileRequest};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tmp/observed-compilation")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn request<'a>(root: &'a Path, roots: &'a [PathBuf]) -> FileCompileRequest<'a> {
    FileCompileRequest {
        source_dir: root,
        entry: "main.mag",
        module_roots: roots,
        inputs: json!({}),
        options: CompilerOptions::default(),
    }
}

#[test]
fn transitive_inputs_preserve_source_root_and_validate_only_consumed_files() {
    let w = Workspace::new();
    w.write("main.mag", "(require \"a\") (artifact a.value)");
    w.write("lib/a.mag", "(require \"b\") (let value b.value)");
    w.write(
        "lib/b.mag",
        "(let value {:text (read \"text.txt\") :json (read-json \"data.json\")})",
    );
    w.write("text.txt", "source root");
    w.write("lib/text.txt", "not the source root");
    w.write("lib/data.json", "42");
    let roots = [w.0.join("lib")];
    let observed = compile_file_observed(request(&w.0, &roots), None).unwrap();
    assert_eq!(
        observed.artifact,
        json!({"text": "source root", "json": 42})
    );
    assert_eq!(
        observed.artifact,
        CompilerSession::new()
            .compile_file(request(&w.0, &roots))
            .unwrap()
    );
    let observations: ObservationSet =
        serde_json::from_slice(&serde_json::to_vec(&observed.observations).unwrap()).unwrap();
    assert!(observations.validate());
    w.write("lib/unrelated.mag", "invalid source");
    w.write("lib/text.txt", "unrelated edit");
    assert!(observations.validate());
    for (path, original, changed) in [
        (
            "main.mag",
            "(require \"a\") (artifact a.value)",
            "(artifact 0)",
        ),
        (
            "lib/a.mag",
            "(require \"b\") (let value b.value)",
            "(let value 0)",
        ),
        (
            "lib/b.mag",
            "(let value {:text (read \"text.txt\") :json (read-json \"data.json\")})",
            "(let value 0)",
        ),
        ("text.txt", "source root", "SOURCE ROOT"),
        ("lib/data.json", "42", "43"),
    ] {
        let metadata = std::fs::metadata(w.0.join(path)).unwrap();
        w.write(path, changed);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(w.0.join(path))
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
            .unwrap();
        assert!(!observations.validate(), "{path}");
        w.write(path, original);
        assert!(observations.validate(), "source A/B/A: {path}");
    }
    w.write("data.json", "42");
    assert!(
        !observations.validate(),
        "new JSON ambiguity even with identical bytes"
    );
}

#[test]
fn observed_and_cold_failures_have_identical_diagnostics() {
    let w = Workspace::new();
    let roots = [w.0.clone()];
    for source in [
        "(artifact \"\\q\")",
        "(artifact",
        "(artifact (+ 1 true))",
        "(artifact (first (as (List Int) [])))",
        "(require \"absent\") (artifact 1)",
        "(artifact (read \"../escape\"))",
        "(artifact (read-json \"missing\"))",
        "(artifact (read \"missing\"))",
    ] {
        w.write("main.mag", source);
        let cold = CompilerSession::new()
            .compile_file(request(&w.0, &roots))
            .unwrap_err();
        let observed = compile_file_observed(request(&w.0, &roots), None)
            .err()
            .unwrap();
        assert_eq!(cold.to_string(), observed.to_string(), "{source}");
    }
}

#[test]
fn distinct_read_queries_keep_existing_memoization_keys() {
    let w = Workspace::new();
    w.write(
        "main.mag",
        "(artifact [(read \"text\") (read \"./text\") (read-json \"text\")])",
    );
    w.write("text", "42");
    let roots = [w.0.clone()];
    let cold_profile = nefor_mag::profile::CompileProfiler::new();
    let observed_profile = nefor_mag::profile::CompileProfiler::new();
    let cold = CompilerSession::new()
        .compile_file_with_profiler(request(&w.0, &roots), &cold_profile)
        .unwrap();
    let observed = compile_file_observed(request(&w.0, &roots), Some(&observed_profile)).unwrap();
    assert_eq!(cold, observed.artifact);
    let cold = serde_json::to_value(cold_profile.snapshot()).unwrap();
    let observed_counters = serde_json::to_value(observed_profile.snapshot()).unwrap();
    assert_eq!(cold["counters"], observed_counters["counters"]);
    let records = serde_json::to_value(&observed.observations).unwrap();
    assert_eq!(records["records"].as_array().unwrap().len(), 4);
    assert!(observed.observations.validate());
}

#[cfg(unix)]
#[test]
fn canonical_module_dedup_and_leaf_symlinks_preserve_cold_semantics() {
    use std::os::unix::fs::symlink;
    let w = Workspace::new();
    w.write(
        "main.mag",
        "(require \"a\") (artifact {:module a.value :text (read \"link\")})",
    );
    w.write("one/a.mag", "(let value 42)");
    w.write("elsewhere/text", "allowed leaf symlink");
    std::fs::create_dir_all(w.0.join("two")).unwrap();
    symlink(w.0.join("one/a.mag"), w.0.join("two/a.mag")).unwrap();
    symlink(w.0.join("elsewhere/text"), w.0.join("link")).unwrap();
    let roots = [w.0.join("one"), w.0.join("two")];
    let observed = compile_file_observed(request(&w.0, &roots), None).unwrap();
    assert!(observed.observations.validate());
    assert_eq!(
        observed.artifact,
        CompilerSession::new()
            .compile_file(request(&w.0, &roots))
            .unwrap()
    );
    std::fs::remove_file(w.0.join("two/a.mag")).unwrap();
    w.write("two/a.mag", "(let value 42)");
    assert!(!observed.observations.validate());
    let cold = CompilerSession::new()
        .compile_file(request(&w.0, &roots))
        .unwrap_err();
    assert!(cold.to_string().contains("ambiguous"));
    std::fs::remove_file(w.0.join("two/a.mag")).unwrap();
    assert!(observed.observations.validate());
    w.write("elsewhere/other", "allowed leaf symlink");
    std::fs::remove_file(w.0.join("link")).unwrap();
    symlink(w.0.join("elsewhere/other"), w.0.join("link")).unwrap();
    assert!(
        !observed.observations.validate(),
        "target identity changed despite same bytes"
    );
}

#[test]
fn unsupported_and_structurally_invalid_observations_are_misses() {
    let w = Workspace::new();
    w.write("main.mag", "(artifact 42)");
    let observed = compile_file_observed(request(&w.0, &[]), None).unwrap();
    for (key, value) in [
        ("version", json!(100)),
        ("records", json!([])),
        ("consistent", json!(false)),
    ] {
        let mut record = serde_json::to_value(&observed.observations).unwrap();
        record[key] = value;
        let invalid: ObservationSet = serde_json::from_value(record).unwrap();
        assert!(!invalid.validate());
    }
    std::fs::remove_file(w.0.join("main.mag")).unwrap();
    assert!(!observed.observations.validate());
}
