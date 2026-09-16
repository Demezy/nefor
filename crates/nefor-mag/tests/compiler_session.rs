use nefor_mag::{
    compile_file_with_inputs_and_module_roots_and_options, compile_profiled_with_options,
    compile_with_inputs_and_module_roots_and_options, CompileRequest, CompilerLimits,
    CompilerOptions, CompilerSession, CompilerSessionStats, FileCompileRequest,
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

fn file_compile_request<'a>(
    source_dir: &'a Path,
    module_roots: &'a [PathBuf],
) -> FileCompileRequest<'a> {
    FileCompileRequest {
        source_dir,
        entry: "main.mag",
        inputs: json!({"answer": 41}),
        module_roots,
        options: options(),
    }
}

#[test]
fn session_and_free_apis_have_cold_memory_and_file_compile_parity() {
    let root = workspace("parity");
    std::fs::write(
        root.join("support.mag"),
        "let expose: fn(Int) -> Int = |value| => value",
    )
    .unwrap();
    let source = r#"
        import support.{}
        type Answer {answer: Int}
        let answer = support.expose(host_input("answer", type_tag<Int>()))
        artifact(Answer {answer: answer})
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

    let free_program = compile_file_with_inputs_and_module_roots_and_options(
        &root,
        "main.mag",
        json!({"answer": 41}),
        &roots,
        options(),
    )
    .unwrap();
    let session_program = session
        .compile_file(file_compile_request(&root, &roots))
        .unwrap();
    assert_eq!(session_program, free_program);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_preserves_structured_syntax_errors() {
    let root = workspace("errors");
    let source = "artifact(";
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
            source: "artifact(1)",
            source_dir: &root,
            inputs: json!({}),
            module_roots: &roots,
            options: CompilerOptions::default(),
        })
        .unwrap();
    let error = session
        .compile_file(FileCompileRequest {
            source_dir: &root,
            entry: "missing.mag",
            inputs: json!({}),
            module_roots: &roots,
            options: CompilerOptions::default(),
        })
        .unwrap_err();
    assert!(matches!(error, nefor_mag::error::MagError::Eval(_)));
    std::fs::write(root.join("main.mag"), "artifact(2)").unwrap();
    session
        .compile_file(file_compile_request(&root, &roots))
        .unwrap();

    assert_eq!(
        session.stats(),
        CompilerSessionStats {
            memory_compile_requests: 1,
            file_compile_requests: 2,
            cold_compilations: 3,
            successful_compilations: 2,
            failed_compilations: 1,
        }
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_stats_accept_incomplete_wire_shapes() {
    let stats: CompilerSessionStats = serde_json::from_value(json!({
        "memory_compile_requests": 1,
        "file_compile_requests": 2,
        "cold_compilations": 3
    }))
    .unwrap();
    assert_eq!(
        stats,
        CompilerSessionStats {
            memory_compile_requests: 1,
            file_compile_requests: 2,
            cold_compilations: 3,
            successful_compilations: 0,
            failed_compilations: 0,
        }
    );
}
