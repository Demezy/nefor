use nefor_mag::{
    compile_profiled_with_options, compile_with_inputs_and_module_roots_and_options,
    eval_artifact_fn, load_with_inputs_and_module_roots_and_options, resolve_artifact_fn,
    CompileRequest, CompilerLimits, CompilerOptions, CompilerSession, CompilerSessionStats,
    LoadRequest,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

fn workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "nefor-mag-session-{name}-{}-{}",
        std::process::id(),
        NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn options() -> CompilerOptions {
    CompilerOptions {
        limits: CompilerLimits {
            evaluation_steps: 50_000,
            call_depth: 48,
            expression_depth: 96,
            memoized_calls: 1_024,
        },
    }
}

fn compile_request<'a>(
    source: &'a str,
    source_dir: &'a Path,
    module_roots: &'a [PathBuf],
) -> CompileRequest<'a> {
    CompileRequest {
        source,
        source_dir,
        inputs: json!({"answer": 41}),
        module_roots,
        options: options(),
    }
}

fn load_request<'a>(source_dir: &'a Path, module_roots: &'a [PathBuf]) -> LoadRequest<'a> {
    LoadRequest {
        source_dir,
        entry: "main.mag",
        inputs: json!({"answer": 41}),
        module_roots,
        options: options(),
    }
}

#[test]
fn session_and_free_apis_have_cold_compile_and_load_parity() {
    let root = workspace("parity");
    std::fs::write(
        root.join("support.mag"),
        "(let expose (fn [[value Int]] -> Int value))",
    )
    .unwrap();
    let source = r#"
        (require "support")
        (let answer (support.expose (host-input "answer" (type-tag Int))))
        (artifact {:answer answer})
    "#;
    std::fs::write(root.join("main.mag"), source).unwrap();
    let roots = [root.clone()];

    let free_artifact = compile_with_inputs_and_module_roots_and_options(
        source,
        &root,
        json!({"answer": 41}),
        &roots,
        options(),
    )
    .unwrap();
    let session = CompilerSession::new();
    let session_artifact = session
        .compile(compile_request(source, &root, &roots))
        .unwrap();
    assert_eq!(session_artifact, free_artifact);

    let (free_profiled_artifact, free_profile) =
        compile_profiled_with_options(source, &root, json!({"answer": 41}), &roots, options())
            .unwrap();
    let (session_profiled_artifact, session_profile) = session
        .compile_profiled(compile_request(source, &root, &roots))
        .unwrap();
    assert_eq!(session_profiled_artifact, free_profiled_artifact);
    assert_eq!(session_profile.counters, free_profile.counters);

    let free_program = load_with_inputs_and_module_roots_and_options(
        &root,
        "main.mag",
        json!({"answer": 41}),
        &roots,
        options(),
    )
    .unwrap();
    let session_program = session.load(load_request(&root, &roots)).unwrap();
    assert_eq!(session_program.artifact, free_program.artifact);
    assert_eq!(session_program.hash, free_program.hash);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_preserves_structured_syntax_errors() {
    let root = workspace("errors");
    let source = "(artifact {:answer 42}";
    let roots = [root.clone()];

    let free_error = compile_with_inputs_and_module_roots_and_options(
        source,
        &root,
        json!({"answer": 41}),
        &roots,
        options(),
    )
    .unwrap_err();
    let session_error = CompilerSession::new()
        .compile(compile_request(source, &root, &roots))
        .unwrap_err();

    let (nefor_mag::error::MagError::Syntax(free), nefor_mag::error::MagError::Syntax(session)) =
        (free_error, session_error)
    else {
        panic!("both entry points must preserve structured syntax diagnostics");
    };
    assert_eq!(session.code, free.code);
    assert_eq!(session.stage, free.stage);
    assert_eq!(session.message, free.message);
    assert_eq!(session.source_name, free.source_name);
    assert_eq!(session.path, free.path);
    assert_eq!(session.span, free.span);
    assert_eq!(session.location, free.location);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_stats_accounts_for_cold_successes_and_failures() {
    let root = workspace("stats");
    let roots = [root.clone()];
    let session = CompilerSession::new();

    session
        .compile_profiled(CompileRequest {
            source: "(artifact 1)",
            source_dir: &root,
            inputs: json!({}),
            module_roots: &roots,
            options: CompilerOptions::default(),
        })
        .unwrap();
    let error = session
        .load(LoadRequest {
            source_dir: &root,
            entry: "missing.mag",
            inputs: json!({}),
            module_roots: &roots,
            options: CompilerOptions::default(),
        })
        .unwrap_err();
    assert!(matches!(error, nefor_mag::error::MagError::Eval(_)));
    std::fs::write(root.join("main.mag"), "(artifact 2)").unwrap();
    session.load(load_request(&root, &roots)).unwrap();

    assert_eq!(
        session.stats(),
        CompilerSessionStats {
            compile_requests: 1,
            load_requests: 2,
            cold_compilations: 3,
            successful_compilations: 2,
            failed_compilations: 1,
        }
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_stats_accept_older_wire_shapes() {
    let stats: CompilerSessionStats = serde_json::from_value(json!({
        "compile_requests": 1,
        "load_requests": 2,
        "cold_compilations": 3
    }))
    .unwrap();
    assert_eq!(
        stats,
        CompilerSessionStats {
            compile_requests: 1,
            load_requests: 2,
            cold_compilations: 3,
            successful_compilations: 0,
            failed_compilations: 0,
        }
    );
}

#[test]
fn repeated_session_loads_keep_resident_owners_isolated() {
    let root = workspace("resident-owner");
    let source = r#"
        (type Input {:value Int})
        (let captured (host-input "owner" (type-tag String)))
        (let run (fn [[input Input]] -> Artifact
          (artifact {:owner captured :value (get input "value")})))
        (artifact {:owner captured})
    "#;
    std::fs::write(root.join("main.mag"), source).unwrap();
    let roots = [root.clone()];
    let session = CompilerSession::new();
    let request = |owner| LoadRequest {
        source_dir: root.as_path(),
        entry: "main.mag",
        inputs: json!({"owner": owner}),
        module_roots: roots.as_slice(),
        options: CompilerOptions::default(),
    };

    let first = session.load(request("first")).unwrap();
    let second = session.load(request("second")).unwrap();
    let input_type = json!({
        "kind": "named",
        "name": "main.Input",
        "arguments": [],
        "body": {
            "kind": "record",
            "fields": [{
                "name": "value",
                "type": {"kind": "primitive", "name": "Int"}
            }]
        }
    });
    let first_run = resolve_artifact_fn(&first, "run", &input_type).unwrap();
    let second_run = resolve_artifact_fn(&second, "run", &input_type).unwrap();

    assert_eq!(
        eval_artifact_fn(&first, &first_run, json!({"value": 1})).unwrap(),
        json!({"owner": "first", "value": 1})
    );
    assert_eq!(
        eval_artifact_fn(&second, &second_run, json!({"value": 2})).unwrap(),
        json!({"owner": "second", "value": 2})
    );
    assert!(matches!(
        eval_artifact_fn(&second, &first_run, json!({"value": 3})),
        Err(nefor_mag::error::MagError::Eval(message))
            if message.contains("different loaded program")
    ));

    assert_eq!(session.stats().cold_compilations, 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_versions_remain_owned_by_the_program_that_loaded_them() {
    let root = workspace("resident-version");
    let roots = [root.clone()];
    let source = |version| {
        format!(
            "(let run (fn [[value Int]] -> Artifact (artifact {{:version {version} :value value}})))\n(artifact {version})"
        )
    };
    std::fs::write(root.join("main.mag"), source(1)).unwrap();
    let session = CompilerSession::new();
    let request = || LoadRequest {
        source_dir: &root,
        entry: "main.mag",
        inputs: json!({}),
        module_roots: &roots,
        options: CompilerOptions::default(),
    };

    let first = session.load(request()).unwrap();
    let first_run =
        resolve_artifact_fn(&first, "run", &json!({"kind":"primitive","name":"Int"})).unwrap();
    std::fs::write(root.join("main.mag"), source(2)).unwrap();
    let second = session.load(request()).unwrap();
    let second_run =
        resolve_artifact_fn(&second, "run", &json!({"kind":"primitive","name":"Int"})).unwrap();

    assert_eq!(
        eval_artifact_fn(&first, &first_run, json!(7)).unwrap(),
        json!({"version":1,"value":7})
    );
    assert_eq!(
        eval_artifact_fn(&second, &second_run, json!(7)).unwrap(),
        json!({"version":2,"value":7})
    );
    assert!(matches!(
        eval_artifact_fn(&second, &first_run, json!(7)),
        Err(nefor_mag::error::MagError::Eval(message))
            if message.contains("different loaded program")
    ));
    drop(first);
    assert_eq!(
        eval_artifact_fn(&second, &second_run, json!(8)).unwrap(),
        json!({"version":2,"value":8})
    );
    std::fs::remove_dir_all(root).unwrap();
}
