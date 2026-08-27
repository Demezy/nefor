use std::path::PathBuf;

use nefor_mag::types::MagType;
use nefor_mag::{eval_fn, load_with_inputs, load_with_inputs_and_module_roots, validate_fn};
use serde_json::{json, Value};

fn book_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/examples")
}

fn first_lisp_fence(path: &std::path::Path) -> String {
    let markdown = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    markdown
        .split_once("```lisp\n")
        .and_then(|(_, rest)| {
            rest.split_once("\n```")
                .map(|(source, _)| source.to_owned())
        })
        .unwrap_or_else(|| panic!("{} contains a complete first Lisp fence", path.display()))
}

#[test]
fn mag_book_examples_compile() {
    let root = book_root();
    let examples = [
        "01-values.mag",
        "02-types.mag",
        "03-functions.mag",
        "04-modules-and-files.mag",
    ];

    for entry in examples {
        load_with_inputs(&root, entry, serde_json::json!({}))
            .unwrap_or_else(|error| panic!("MAG Book example {entry} failed: {error}"));
    }
}

#[test]
fn mag_in_five_minutes_program_compiles() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = crate_root.join("../../mag/book/01. core/00. MAG in Five Minutes.md");
    let source = first_lisp_fence(&path);

    nefor_mag::compile(&source, &crate_root)
        .unwrap_or_else(|error| panic!("MAG in Five Minutes failed: {error}"));
}

#[test]
fn nefor_mag_in_five_minutes_program_is_executable() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join("mag/book/02. nefor/00. Nefor MAG in Five Minutes.md");
    let source = first_lisp_fence(&path);
    let config_lib = root.join("examples/nefor-agent/mag/lib");
    let workspace = root
        .join("tmp")
        .join(format!("nefor-mag-book-guide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).expect("create guide test workspace");
    std::fs::write(workspace.join("main.mag"), source).expect("write guide program");

    let program = load_with_inputs_and_module_roots(
        &workspace,
        "main.mag",
        json!({}),
        &[root.join("mag/lib"), config_lib],
    )
    .unwrap_or_else(|error| panic!("Nefor MAG in Five Minutes failed: {error}"));

    for function in [
        "run-build",
        "route-build-result",
        "revise",
        "finish-development",
        "expand-swarm",
    ] {
        validate_fn(&program, function, 1, &MagType::Artifact)
            .unwrap_or_else(|error| panic!("resident function {function} is invalid: {error}"));
    }

    let build_params = actor_params(&program.artifact, "build");
    let exited_type = build_params["exited_type"]
        .as_str()
        .expect("build exited constructor id");
    let signaled_type = build_params["signaled_type"]
        .as_str()
        .expect("build signaled constructor id");

    let passed = eval_fn(
        &program,
        "route-build-result",
        process_exit_result(exited_type, 0, "compiled", ""),
    )
    .expect("evaluate successful build route");
    let passed_value = assert_delta_message(&passed, "reviewer.entry", "main.BuildPassed");
    assert_eq!(passed_value["stdout"], "compiled");

    let failed = eval_fn(
        &program,
        "route-build-result",
        process_exit_result(exited_type, 101, "", "compile error"),
    )
    .expect("evaluate failed build route");
    let failed_value = assert_delta_message(&failed, "builder.entry", "main.BuildFailed");
    assert_eq!(failed_value["stderr"], "compile error");
    assert_eq!(failed_value["termination"]["value"]["code"], 101);

    let signaled = eval_fn(
        &program,
        "route-build-result",
        process_signal_result(signaled_type, 9, "", "terminated"),
    )
    .expect("evaluate signaled build route");
    let signaled_value = assert_delta_message(&signaled, "builder.entry", "main.BuildFailed");
    assert_eq!(signaled_value["termination"]["value"]["signal"], 9);

    eval_fn(&program, "run-build", json!("implementation complete"))
        .expect("evaluate builder-to-build mapper");
    eval_fn(&program, "revise", json!({"feedback": "fix the error"}))
        .expect("evaluate reviewer feedback mapper");
    eval_fn(
        &program,
        "finish-development",
        json!({"summary": "approved"}),
    )
    .expect("evaluate approval mapper");
    eval_fn(&program, "expand-swarm", json!([])).expect("evaluate empty dynamic swarm");
    eval_fn(
        &program,
        "expand-swarm",
        json!([{"title": "inspect", "instructions": "Inspect the implementation."}]),
    )
    .expect("evaluate one-worker dynamic swarm");

    let builder_routes = routes_to(&program.artifact, "builder-context");
    assert_eq!(
        builder_routes.len(),
        2,
        "task and worktree both feed builder"
    );
    let mut positions = builder_routes
        .iter()
        .map(|route| {
            route["product_position"]
                .as_i64()
                .expect("product position")
        })
        .collect::<Vec<_>>();
    positions.sort_unstable();
    assert_eq!(
        positions,
        vec![0, 1],
        "builder product occurrences are distinct"
    );
    let context_route = routes_to(&program.artifact, "builder.entry");
    assert_eq!(context_route.len(), 1, "one complete context feeds builder");
    assert_eq!(context_route[0]["product_position"], -1);

    std::fs::remove_dir_all(workspace).expect("remove guide test workspace");
}

fn process_exit_result(constructor: &str, code: i64, stdout: &str, stderr: &str) -> Value {
    json!({
        "stdout": stdout,
        "stderr": stderr,
        "termination": {
            "type": constructor,
            "value": {"code": code}
        }
    })
}

fn process_signal_result(constructor: &str, signal: i64, stdout: &str, stderr: &str) -> Value {
    json!({
        "stdout": stdout,
        "stderr": stderr,
        "termination": {
            "type": constructor,
            "value": {"signal": signal}
        }
    })
}

fn actor_params<'a>(artifact: &'a Value, actor: &str) -> &'a Value {
    artifact["actors"]
        .as_array()
        .expect("initial actors")
        .iter()
        .find(|candidate| candidate["id"] == actor)
        .unwrap_or_else(|| panic!("actor {actor}"))
        .get("params")
        .expect("actor params")
}

fn assert_delta_message<'a>(artifact: &'a Value, actor: &str, semantic_type: &str) -> &'a Value {
    let messages = artifact["messages"].as_array().expect("delta messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["to"], actor);
    assert_eq!(messages[0]["semantic_type"]["name"], semantic_type);
    assert!(messages[0]["semantic_type_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("sha256:")));
    let value = &messages[0]["content"]["value"];
    if value.get("type").is_some() {
        &value["value"]
    } else {
        value
    }
}

fn routes_to<'a>(artifact: &'a Value, actor: &str) -> Vec<&'a Value> {
    artifact["actors"]
        .as_array()
        .expect("initial actors")
        .iter()
        .flat_map(|source| {
            source["routes"]
                .as_object()
                .expect("actor route map")
                .values()
                .flat_map(|destinations| destinations.as_array().expect("route destinations"))
        })
        .filter(|destination| destination["actor"] == actor)
        .collect()
}
