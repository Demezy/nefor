use super::*;
use crate::profile::{CompileProfile, CompileProfiler};
use crate::CompilerOptions;
use serde_json::{json, Value};

struct Fixture {
    root: PathBuf,
    roots: Vec<PathBuf>,
    inputs: Value,
    options: CompilerOptions,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mag-project-cache-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("extra")).unwrap();
        let this = Self {
            roots: vec![root.clone(), root.join("extra")],
            root,
            inputs: json!({}),
            options: CompilerOptions::default(),
        };
        this.write(
            "main.mag",
            "(require \"a\")\n(artifact [a.value (read \"note.txt\") (read-json \"data.json\")])",
        );
        this.write("a.mag", "(require \"b\")\n(let value b.value)");
        this.write("b.mag", "(let value 1)");
        this.write("note.txt", "first");
        this.write("data.json", "{\"a\":1}");
        this
    }
    fn write(&self, path: &str, text: &str) {
        fs::write(self.root.join(path), text).unwrap();
    }
    fn request(&self) -> FileCompileRequest<'_> {
        FileCompileRequest {
            source_dir: &self.root,
            entry: "main.mag",
            module_roots: &self.roots,
            inputs: self.inputs.clone(),
            options: self.options,
        }
    }
    fn identity(&self, compiler: &[u8]) -> Identity {
        Identity::new(
            &self.request(),
            1,
            CompilerBuildId::from_executable_bytes(compiler),
        )
        .unwrap()
    }
    fn run(&self, compiler: &[u8]) -> BuildOutput {
        build_with_identity(self.request(), 1, CachePolicy::Use, None, || {
            Ok(CompilerBuildId::from_executable_bytes(compiler))
        })
        .unwrap()
    }
    fn bucket(&self) -> PathBuf {
        self.identity(b"A").bucket().unwrap()
    }
    fn records(&self) -> Vec<PathBuf> {
        fs::read_dir(self.bucket())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_str().is_some_and(record_name))
            .map(|e| e.path())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn status(out: &BuildOutput, expected: &str) {
    assert_eq!(serde_json::to_value(&out.cache.status).unwrap(), expected);
}

#[test]
fn seeded_hit_is_exact_bytes_and_zero_compiler_work_after_unrelated_edits() {
    let f = Fixture::new();
    let population = f.run(b"A");
    status(&population, "miss");
    f.write("unrelated.mag", "bad syntax @");
    f.write("mag.toml", "# unrelated formatting\n");
    let profiler = CompileProfiler::new();
    let hit = build_with_identity(f.request(), 1, CachePolicy::Use, Some(&profiler), || {
        Ok(CompilerBuildId::from_executable_bytes(b"A"))
    })
    .unwrap();
    status(&hit, "hit");
    assert_eq!(hit.bytes, population.bytes);
    assert_eq!(profiler.snapshot(), CompileProfile::default());
    assert!(hit.cache.lookup_duration_ns > 0);
    assert_eq!(hit.cache.publication_duration_ns, 0);
    let cold = crate::CompilerSession::new()
        .compile_file(f.request())
        .unwrap();
    assert_eq!(hit.bytes, serialize_artifact(&cold).unwrap());
}

#[test]
fn every_observed_input_invalidates_and_source_a_b_a_survives() {
    for (path, changed) in [
        ("main.mag", "(artifact 2)"),
        ("b.mag", "(let value 2)"),
        ("note.txt", "second"),
        ("data.json", "{\"a\":2}"),
    ] {
        let f = Fixture::new();
        let original = fs::read_to_string(f.root.join(path)).unwrap();
        let a = f.run(b"A");
        f.write(path, changed);
        let b = f.run(b"A");
        status(&b, "miss");
        assert_ne!(a.bytes, b.bytes);
        f.write(path, &original);
        let restored = f.run(b"A");
        status(&restored, "hit");
        assert_eq!(restored.bytes, a.bytes);
        assert_eq!(f.records().len(), 2);
    }
}

#[test]
fn stable_roots_recheck_new_module_and_json_ambiguity_and_cold_precedence() {
    for (path, value) in [("extra/a.mag", "(let value 2)"), ("extra/data.json", "{}")] {
        let f = Fixture::new();
        f.run(b"A");
        f.write(path, value);
        let cached = build_with_identity(f.request(), 1, CachePolicy::Use, None, || {
            Ok(CompilerBuildId::from_executable_bytes(b"A"))
        })
        .unwrap_err();
        let cold = crate::CompilerSession::new()
            .compile_file(f.request())
            .unwrap_err();
        assert_eq!(cached.to_string(), cold.to_string());
        assert!(cached.to_string().contains("ambiguous"));
        f.write("main.mag", "(artifact @)");
        let cached = build_with_identity(f.request(), 1, CachePolicy::Use, None, || {
            Ok(CompilerBuildId::from_executable_bytes(b"A"))
        })
        .unwrap_err();
        assert!(matches!(cached, MagError::Syntax(_)));
    }
}

#[cfg(unix)]
#[test]
fn canonical_duplicate_module_is_a_hit_but_retargeted_symlink_is_not() {
    let f = Fixture::new();
    f.run(b"A");
    std::os::unix::fs::symlink(f.root.join("a.mag"), f.root.join("extra/a.mag")).unwrap();
    status(&f.run(b"A"), "hit");
    fs::remove_file(f.root.join("extra/a.mag")).unwrap();
    std::os::unix::fs::symlink(f.root.join("b.mag"), f.root.join("extra/a.mag")).unwrap();
    assert!(
        build_with_identity(f.request(), 1, CachePolicy::Use, None, || Ok(
            CompilerBuildId::from_executable_bytes(b"A")
        ))
        .is_err()
    );
}

#[test]
fn compiler_and_complete_contexts_coexist_without_replacing_prior_records() {
    let mut f = Fixture::new();
    let a = f.run(b"stable-executable");
    status(&f.run(b"edge-executable"), "miss");
    status(&f.run(b"stable-executable"), "hit");
    f.inputs = json!({"unused": {"full": [1, 2]}});
    status(&f.run(b"stable-executable"), "miss");
    f.inputs = json!({});
    assert_eq!(f.run(b"stable-executable").bytes, a.bytes);
    status(&f.run(b"stable-executable"), "hit");
    let original = f.identity(b"A");
    for field in 0..4 {
        let defaults = CompilerOptions::default();
        f.options = defaults;
        match field {
            0 => f.options.limits.evaluation_steps += 1,
            1 => f.options.limits.call_depth += 1,
            2 => f.options.limits.expression_depth += 1,
            _ => f.options.limits.memoized_calls += 1,
        }
        assert_ne!(f.identity(b"A"), original);
        status(&f.run(b"stable-executable"), "miss");
    }
    f.options = CompilerOptions::default();
    status(&f.run(b"stable-executable"), "hit");
    f.roots.reverse();
    status(&f.run(b"stable-executable"), "miss");
    f.roots.reverse();
    status(&f.run(b"stable-executable"), "hit");
    assert_ne!(
        Identity::new(
            &f.request(),
            2,
            CompilerBuildId::from_executable_bytes(b"A")
        )
        .unwrap(),
        original
    );
}

#[test]
fn bypass_and_unavailable_identity_never_touch_storage_or_observe() {
    let f = Fixture::new();
    let run = || {
        build_with_identity(f.request(), 1, CachePolicy::Bypass, None, || {
            panic!("bypass requested identity")
        })
        .unwrap()
    };
    status(&run(), "bypass");
    assert!(!f.root.join(".mag").exists());
    let unavailable = build_with_identity(f.request(), 1, CachePolicy::Use, None, || {
        Err(io::Error::other("no executable"))
    })
    .unwrap();
    status(&unavailable, "unavailable");
    assert!(!f.root.join(".mag").exists());
    f.run(b"A");
    let record = f.records().pop().unwrap();
    let old_provenance = fs::read(record.join("provenance.json")).unwrap();
    f.write("b.mag", "(let value 5)");
    let bypass = run();
    assert_ne!(
        bypass.bytes,
        fs::read(record.join("artifact.json")).unwrap()
    );
    assert_eq!(
        fs::read(record.join("provenance.json")).unwrap(),
        old_provenance
    );
    assert_eq!(f.records().len(), 1);
}

#[test]
fn corruption_and_interrupted_records_are_silent_misses() {
    for damage in 0..11 {
        let f = Fixture::new();
        let expected = f.run(b"A").bytes;
        let record = f.records().pop().unwrap();
        match damage {
            0 => fs::write(record.join("artifact.json"), b"broken").unwrap(),
            1 => fs::write(record.join("provenance.json"), b"{}").unwrap(),
            2 => fs::remove_file(record.join("artifact.json")).unwrap(),
            3 => fs::remove_file(record.join("provenance.json")).unwrap(),
            4 => {
                fs::rename(&record, f.bucket().join(".tmp-interrupted")).unwrap();
            }
            _ => {
                let mut provenance: Value =
                    serde_json::from_slice(&fs::read(record.join("provenance.json")).unwrap())
                        .unwrap();
                match damage {
                    5 => provenance["format"] = json!("future"),
                    6 => provenance["output"] = json!("future"),
                    7 => provenance["observations"]["version"] = json!(2),
                    8 => provenance["request"]["inputs"] = json!({"changed":true}),
                    9 => provenance["artifact_length"] = json!(0),
                    _ => {
                        // Even internally consistent non-line output cannot be replayed.
                        let bytes = b"[\n1]\n";
                        fs::write(record.join("artifact.json"), bytes).unwrap();
                        provenance["artifact_length"] = json!(bytes.len());
                        provenance["artifact_sha256"] = json!(digest(bytes));
                    }
                }
                let bytes = serde_json::to_vec(&provenance).unwrap();
                fs::write(record.join("provenance.json"), &bytes).unwrap();
                fs::rename(&record, f.bucket().join(digest(&bytes))).unwrap();
            }
        }
        let actual = f.run(b"A");
        status(&actual, "miss");
        assert_eq!(actual.bytes, expected);
    }
}

#[test]
fn publication_failure_or_changed_consumption_does_not_change_success() {
    let f = Fixture::new();
    fs::write(f.root.join(".mag"), "not a directory").unwrap();
    status(&f.run(b"A"), "miss");
    fs::remove_file(f.root.join(".mag")).unwrap();
    let compiled = compile_file_observed(f.request(), None).unwrap();
    let bytes = serialize_artifact(&compiled.artifact).unwrap();
    let provenance = Provenance {
        format: FORMAT.into(),
        request: f.identity(b"A"),
        output: OUTPUT.into(),
        observations: compiled.observations,
        artifact_length: bytes.len() as u64,
        artifact_sha256: digest(&bytes),
    };
    f.write("b.mag", "(let value 9)");
    publish(&f.bucket(), &provenance, &bytes).unwrap();
    assert!(f.records().is_empty());
    assert_eq!(fs::read_dir(f.bucket()).unwrap().count(), 0);
}

#[test]
fn concurrent_identical_publications_leave_one_complete_record() {
    let f = Fixture::new();
    std::thread::scope(|scope| {
        let handles = (0..4)
            .map(|_| scope.spawn(|| f.run(b"A").bytes))
            .collect::<Vec<_>>();
        for h in handles {
            assert_eq!(h.join().unwrap(), b"[1,\"first\",{\"a\":1}]\n");
        }
    });
    assert_eq!(f.records().len(), 1);
    status(&f.run(b"A"), "hit");
    assert_eq!(fs::read_dir(f.bucket()).unwrap().count(), 1);
}

#[test]
fn deleted_entry_and_failures_are_recompiled_without_publishing() {
    let f = Fixture::new();
    f.run(b"A");
    for source in [Some("(artifact"), Some("(artifact missing)"), None] {
        match source {
            Some(source) => f.write("main.mag", source),
            None => fs::remove_file(f.root.join("main.mag")).unwrap(),
        }
        for _ in 0..2 {
            let profiler = CompileProfiler::new();
            let cached =
                build_with_identity(f.request(), 1, CachePolicy::Use, Some(&profiler), || {
                    Ok(CompilerBuildId::from_executable_bytes(b"A"))
                })
                .unwrap_err();
            let cold = crate::CompilerSession::new()
                .compile_file(f.request())
                .unwrap_err();
            assert_eq!(cached.to_string(), cold.to_string());
            assert!(profiler.snapshot().total_duration_ns > 0);
            assert_eq!(f.records().len(), 1);
        }
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_request_paths_bypass_without_lossy_identity() {
    use std::os::unix::ffi::OsStringExt;
    let mut f = Fixture::new();
    let root = f.root.join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
    // Some filesystems reject non-UTF-8 names. An unused module root still
    // exercises lossless identity rejection without requiring such a filesystem.
    f.write("main.mag", "(artifact 1)");
    f.roots = vec![root.clone()];
    let request = f.request();
    let output = build_with_identity(request, 1, CachePolicy::Use, None, || {
        Ok(CompilerBuildId::from_executable_bytes(b"A"))
    })
    .unwrap();
    status(&output, "unavailable");
    assert_eq!(output.bytes, b"1\n");
    assert!(!f.root.join(".mag").exists());
}
