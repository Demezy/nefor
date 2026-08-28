use std::path::PathBuf;

use nefor_mag::{load_with_inputs, load_with_inputs_and_module_roots};
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

    let actors = program.artifact["actors"].as_array().expect("guide actors");
    for factory in [
        "nefor.factory.worktree-create",
        "nefor.factory.shell-script",
        "nefor.factory.discard",
        "nefor.factory.dynamic-input",
        "nefor.factory.dynamic-output",
        "nefor.factory.dynamic-context",
    ] {
        assert!(
            actors.iter().any(|actor| actor["factory"] == factory),
            "guide must exercise {factory}"
        );
    }
    let rules = program.artifact["rules"].as_array().expect("guide rules");
    assert!(rules.iter().any(|rule| rule["fn"] == "expand-followup"));

    let build_params = actor_params(&program.artifact, "development.build");
    assert_eq!(build_params["script"], "cargo build");
    assert_eq!(build_params["cwd"], "tmp/mag-book-worktree");

    let builder_routes = routes_to(&program.artifact, "development.input.output");
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
    let context_route = routes_to(&program.artifact, "development.builder.entry");
    assert_eq!(context_route.len(), 1, "one complete context feeds builder");
    assert_eq!(context_route[0]["product_position"], -1);

    std::fs::remove_dir_all(workspace).expect("remove guide test workspace");
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
