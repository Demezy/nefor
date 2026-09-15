use nefor_mag::{compile_file_with_syntax, compile_with_syntax, SyntaxMode};
use std::fs;
use std::path::Path;

fn workspace(name: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("nefor-mag-frontend-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn new_and_lisp_frontends_preserve_nominal_artifact_semantics() {
    let new = compile_with_syntax(
        "type Box { value: Int }\nartifact(Box { value: 42 })\n",
        Path::new("."),
        SyntaxMode::New,
    )
    .unwrap();
    let lisp = compile_with_syntax(
        "(type Box {:value Int})\n(artifact (as Box {:value 42}))\n",
        Path::new("."),
        SyntaxMode::Lisp,
    )
    .unwrap();

    assert_eq!(new, lisp);
    assert_eq!(new, serde_json::json!({"value": 42}));
}

#[test]
fn infix_is_curried_while_parenthesized_calls_remain_nary() {
    let artifact = compile_with_syntax(
        r#"
let choose: String -> String -> String = |left| => |right| => left
let join: fn(String, String) -> String = |left, right| => str(left, right)
artifact(join("nary:", "left" choose "right"))
"#,
        Path::new("."),
        SyntaxMode::New,
    )
    .unwrap();

    assert_eq!(artifact, serde_json::json!("nary:left"));
}

#[test]
fn generic_adt_match_and_expression_block_use_existing_semantics() {
    let artifact = compile_with_syntax(
        r#"
type Score {name: String, accepted: Bool}
type Result<E, T> = Error(E) | Ok(T)

infixr 5 append
let append: String -> String -> String = |left| => |right| => str(left, right)
let score = Score {name: "review", accepted: true}
let outcome: Result<String, Score> = Result<String, Score>.Ok(score)
let label: String = {
  let suffix = match outcome {
    case Error(reason) => reason,
    case Ok(value) => get(value, "name"),
  }
  "result: " append suffix
}
artifact(label)
"#,
        Path::new("."),
        SyntaxMode::New,
    )
    .unwrap();

    assert_eq!(artifact, serde_json::json!("result: review"));
}

#[test]
fn generic_functions_can_construct_generic_nominal_records() {
    let artifact = compile_with_syntax(
        r#"
type Box<T> {value: T}
let boxed<T>: fn(T) -> Box<T> = |value| => Box<T> {value: value}
artifact(boxed("value"))
"#,
        Path::new("."),
        SyntaxMode::New,
    )
    .unwrap();

    assert_eq!(artifact, serde_json::json!({"value": "value"}));
}

#[test]
fn nary_functions_are_not_adapted_when_used_infix() {
    let error = compile_with_syntax(
        r#"
let take_left: fn(Int, Int) -> Int = |left, right| => left
artifact(1 take_left 2)
"#,
        Path::new("."),
        SyntaxMode::New,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        nefor_mag::error::MagError::Type(_) | nefor_mag::error::MagError::Arity { .. }
    ));
}

#[test]
fn new_entry_can_load_an_explicit_lisp_module() {
    let root = workspace("new-loads-lisp");
    fs::write(root.join("support.magl"), "(let value \"mixed\")\n").unwrap();
    fs::write(
        root.join("main.mag"),
        "import support.{}\nartifact(support.value)\n",
    )
    .unwrap();

    let artifact = compile_file_with_syntax(&root, "main.mag", SyntaxMode::New).unwrap();
    assert_eq!(artifact, serde_json::json!("mixed"));
}

#[test]
fn lisp_entry_can_load_a_new_syntax_module() {
    let root = workspace("lisp-loads-new");
    fs::write(root.join("support.mag"), "let value = \"mixed\"\n").unwrap();
    fs::write(
        root.join("main.magl"),
        "(require \"support\")\n(artifact support.value)\n",
    )
    .unwrap();

    let artifact = compile_file_with_syntax(&root, "main.magl", SyntaxMode::Lisp).unwrap();
    assert_eq!(artifact, serde_json::json!("mixed"));
}

#[test]
fn imported_adts_construct_through_qualified_and_selective_owners() {
    let root = workspace("imported-adts");
    fs::write(
        root.join("support.mag"),
        "type Pair<T> = Pair(T, T)\ntype Outcome = Ok(Int) | Error {message: String}\n",
    )
    .unwrap();
    fs::write(
        root.join("main.mag"),
        r#"
import support.{Outcome}
import support.{}
let left = 1
let right = 2
let pair: support.Pair<Int> = support.Pair<Int>.Pair(left, right)
let outcome: support.Outcome = Outcome.Error(message: "failed")
let message = match outcome { case Ok(value) => str(value), case Error(error) => get(error, "message") }
artifact(message)
"#,
    )
    .unwrap();

    let artifact = compile_file_with_syntax(&root, "main.mag", SyntaxMode::New).unwrap();
    assert_eq!(artifact, serde_json::json!("failed"));
}

#[test]
fn selectively_imported_values_keep_dot_field_access() {
    let root = workspace("imported-field-access");
    fs::write(
        root.join("support.mag"),
        "type Box {value: Int}\nlet box = Box {value: 42}\n",
    )
    .unwrap();
    fs::write(
        root.join("main.mag"),
        "import support.{box}\nartifact(box.value)\n",
    )
    .unwrap();

    let artifact = compile_file_with_syntax(&root, "main.mag", SyntaxMode::New).unwrap();
    assert_eq!(artifact, serde_json::json!(42));
}

#[test]
fn both_module_suffixes_are_ambiguous() {
    let root = workspace("ambiguous-suffix");
    fs::write(root.join("support.mag"), "let value = 1\n").unwrap();
    fs::write(root.join("support.magl"), "(let value 1)\n").unwrap();
    fs::write(root.join("main.mag"), "import support.{}\nartifact(())\n").unwrap();

    let error = compile_file_with_syntax(&root, "main.mag", SyntaxMode::New).unwrap_err();
    assert!(error.to_string().contains("ambiguous"));
    assert!(error.to_string().contains("support.mag"));
    assert!(error.to_string().contains("support.magl"));
}
