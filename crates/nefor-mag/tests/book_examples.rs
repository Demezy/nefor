use std::path::PathBuf;

use nefor_mag::load_with_inputs;

fn book_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/book")
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
    let path = crate_root.join("docs/book/01. core/00. MAG in Five Minutes.md");
    let markdown = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let source = markdown
        .split_once("```lisp\n")
        .and_then(|(_, rest)| rest.split_once("\n```").map(|(source, _)| source))
        .expect("MAG in Five Minutes contains a complete first Lisp fence");

    nefor_mag::compile(source, &crate_root)
        .unwrap_or_else(|error| panic!("MAG in Five Minutes failed: {error}"));
}
