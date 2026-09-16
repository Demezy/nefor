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
fn imported_arrow_operator_is_infix_only_in_expression_context() {
    let root = workspace("arrow-term-operator");
    fs::write(
        root.join("operators.mag"),
        "let (->): Int -> Int -> Int = |left| => |right| => left\n",
    )
    .unwrap();
    fs::write(
        root.join("main.mag"),
        "import operators.{`->`}\ninfixr 4 (->)\nlet identity: Int -> Int = |value| => value\nartifact(identity(1 -> 2))\n",
    )
    .unwrap();

    let artifact = compile_file_with_syntax(&root, "main.mag", SyntaxMode::New).unwrap();
    assert_eq!(artifact, serde_json::json!(1));
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

#[test]
fn import_exposure_modes_are_explicit_and_non_transitive() {
    let root = workspace("import-modes");
    fs::write(
        root.join("base.mag"),
        "type Box {value: Int}\nlet value = 42\nlet other = 7\n",
    )
    .unwrap();
    fs::write(
        root.join("middle.mag"),
        "import base.{}\nlet copied = base.value\n",
    )
    .unwrap();

    for (name, source, expected) in [
        ("open", "import base\nartifact(value)\n", 42),
        ("qualified", "import base.{}\nartifact(base.value)\n", 42),
        (
            "selected",
            "import base.{value as selected}\nartifact(selected)\n",
            42,
        ),
        ("aliased", "import base as b\nartifact(b.value)\n", 42),
        (
            "suppressed",
            "import base\nimport base.{value as _}\nartifact(other)\n",
            7,
        ),
    ] {
        let entry = format!("{name}.mag");
        fs::write(root.join(&entry), source).unwrap();
        assert_eq!(
            compile_file_with_syntax(&root, &entry, SyntaxMode::New).unwrap(),
            serde_json::json!(expected)
        );
    }

    fs::write(
        root.join("missing-direct.mag"),
        "import middle.{}\nartifact(base.value)\n",
    )
    .unwrap();
    let missing_direct =
        compile_file_with_syntax(&root, "missing-direct.mag", SyntaxMode::New).unwrap_err();
    let missing_direct = missing_direct.to_string();
    assert!(missing_direct.contains("base.value"), "{missing_direct}");
    assert!(
        missing_direct.contains("import base.{}"),
        "{missing_direct}"
    );
    fs::write(
        root.join("suppressed-missing.mag"),
        "import base\nimport base.{value as _}\nartifact(value)\n",
    )
    .unwrap();
    assert!(compile_file_with_syntax(&root, "suppressed-missing.mag", SyntaxMode::New).is_err());
}

#[test]
fn transparent_aliases_erase_while_newtypes_require_explicit_boundaries() {
    let transparent = compile_with_syntax(
        "type Label = String\nlet label: Label = \"ok\"\nartifact(type_id(type_evidence(type_tag<Label>())))\n",
        Path::new("."),
        SyntaxMode::New,
    ).unwrap();
    let target = compile_with_syntax(
        "artifact(type_id(type_evidence(type_tag<String>())))\n",
        Path::new("."),
        SyntaxMode::New,
    )
    .unwrap();
    assert_eq!(transparent, target);

    let explicit = compile_with_syntax(
        "newtype UserId = String\nlet raw = \"u-1\"\nlet id: UserId = (raw: UserId)\nlet erased: String = (id: String)\nartifact(erased)\n",
        Path::new("."),
        SyntaxMode::New,
    ).unwrap();
    assert_eq!(explicit, serde_json::json!("u-1"));

    let implicit = compile_with_syntax(
        "newtype UserId = String\nlet raw = \"u-1\"\nlet id: UserId = raw\nartifact(id)\n",
        Path::new("."),
        SyntaxMode::New,
    );
    assert!(implicit.is_err());
}

#[test]
fn unresolved_names_get_static_non_evaluating_import_suggestions() {
    let root = workspace("import-suggestions");
    fs::write(
        root.join("candidate.mag"),
        "let desired = fail(\"must not run\")\nlet absent = read(\"missing.txt\")\n",
    )
    .unwrap();
    fs::write(root.join("main.mag"), "artifact(desired)\n").unwrap();

    let error = compile_file_with_syntax(&root, "main.mag", SyntaxMode::New).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("desired"), "{message}");
    assert!(message.contains("import candidate.{desired}"), "{message}");
    assert!(!message.contains("must not run"));
    assert!(!message.contains("missing.txt"));
}

#[test]
fn import_collisions_use_canonical_declarations_and_preserve_overload_sets() {
    let root = workspace("import-declaration-identity");
    fs::write(
        root.join("a.mag"),
        "let value = 1\nlet bomb = fail(\"must not evaluate before collision resolution\")\n",
    )
    .unwrap();
    fs::write(root.join("b.mag"), "let value = 1\n").unwrap();
    fs::write(
        root.join("collision.mag"),
        "import a\nimport b\nartifact(value)\n",
    )
    .unwrap();

    let error = compile_file_with_syntax(&root, "collision.mag", SyntaxMode::New)
        .unwrap_err()
        .to_string();
    assert!(error.contains("a.value"), "{error}");
    assert!(error.contains("b.value"), "{error}");

    fs::write(
        root.join("overloads.mag"),
        "let convert: fn(Int) -> String = |value| => str(value)\nlet convert: fn(String) -> Int = |value| => count([value])\n",
    )
    .unwrap();
    fs::write(
        root.join("overload-main.mag"),
        "import overloads\nartifact([convert(1), str(convert(\"x\"))])\n",
    )
    .unwrap();
    assert_eq!(
        compile_file_with_syntax(&root, "overload-main.mag", SyntaxMode::New).unwrap(),
        serde_json::json!(["1", "1"])
    );
}

#[test]
fn imports_do_not_reexport_dependency_bindings_but_keep_type_closure() {
    let root = workspace("direct-export-and-type-closure");
    fs::write(
        root.join("base.mag"),
        "type Record {value: Int}\nlet hidden = 7\n",
    )
    .unwrap();
    fs::write(
        root.join("facade.mag"),
        "import base\ntype Public = base.Record\nlet make: fn(Int) -> Public = |value| => base.Record {value: value}\n",
    )
    .unwrap();
    fs::write(
        root.join("main.mag"),
        "import facade.{}\nartifact(type_id(type_evidence(type_tag<facade.Public>())))\n",
    )
    .unwrap();
    assert!(compile_file_with_syntax(&root, "main.mag", SyntaxMode::New).is_ok());

    fs::write(root.join("leak.mag"), "import facade\nartifact(hidden)\n").unwrap();
    assert!(compile_file_with_syntax(&root, "leak.mag", SyntaxMode::New).is_err());
}

#[test]
fn newtype_ascriptions_reject_nested_conversion_and_accept_call_results() {
    let call = compile_with_syntax(
        "newtype UserId = String\nlet raw: fn(Unit) -> String = |value| => \"u-1\"\nlet id = (raw(nil): UserId)\nartifact((id: String))\n",
        Path::new("."),
        SyntaxMode::New,
    )
    .unwrap();
    assert_eq!(call, serde_json::json!("u-1"));

    for source in [
        "newtype UserId = String\nlet raw = [\"u-1\"]\nlet ids = (raw: List<UserId>)\nartifact(ids)\n",
        "newtype FirstId = String\nnewtype SecondId = String\nlet first: List<FirstId> = [((\"u-1\": FirstId))]\nlet second = (first: List<SecondId>)\nartifact(second)\n",
    ] {
        assert!(compile_with_syntax(source, Path::new("."), SyntaxMode::New).is_err());
    }
}

#[test]
fn newtype_grammar_never_declares_records_or_sums() {
    for source in [
        "newtype Wrong = Variant(Int)\nartifact(())\n",
        "newtype AlsoWrong = {value: Int}\nartifact(())\n",
        "newtype WrongBar = | Variant(Int)\nartifact(())\n",
        "newtype WrongFields = Variant {value: Int}\nartifact(())\n",
    ] {
        assert!(compile_with_syntax(source, Path::new("."), SyntaxMode::New).is_err());
    }
}

#[cfg(unix)]
#[test]
fn import_suggestions_do_not_follow_cycles_or_escape_module_roots() {
    use std::os::unix::fs::symlink;

    let root = workspace("safe-import-suggestions");
    let outside = workspace("outside-import-suggestions");
    fs::write(outside.join("escaped.mag"), "let desired = 1\n").unwrap();
    symlink(".", root.join("loop")).unwrap();
    symlink(&outside, root.join("escape")).unwrap();
    fs::write(root.join("valid.mag"), "type Desired {value: Int}\n").unwrap();
    fs::write(
        root.join("main.mag"),
        "type Alias = Desired\nartifact(())\n",
    )
    .unwrap();

    let error = compile_file_with_syntax(&root, "main.mag", SyntaxMode::New)
        .unwrap_err()
        .to_string();
    assert!(error.contains("import valid.{Desired}"), "{error}");
    assert!(!error.contains("escape"), "{error}");
}

#[test]
fn namespace_alias_collisions_are_order_independent() {
    let root = workspace("namespace-alias-collision");
    fs::write(root.join("a.mag"), "let value = 1\n").unwrap();
    fs::write(root.join("b.mag"), "let value = 2\n").unwrap();
    for (name, imports) in [
        ("forward.mag", "import a as x\nimport b as x\n"),
        ("reverse.mag", "import b as x\nimport a as x\n"),
    ] {
        fs::write(root.join(name), format!("{imports}artifact(x.value)\n")).unwrap();
        let error = compile_file_with_syntax(&root, name, SyntaxMode::New)
            .unwrap_err()
            .to_string();
        assert!(error.contains("namespace alias x for a"), "{error}");
        assert!(error.contains("namespace alias x for b"), "{error}");
    }
}

#[test]
fn wildcard_import_is_not_language_syntax() {
    let root = workspace("wildcard-removed");
    fs::write(root.join("base.mag"), "let value = 1\n").unwrap();
    fs::write(root.join("main.mag"), "import base.*\nartifact(value)\n").unwrap();
    assert!(compile_file_with_syntax(&root, "main.mag", SyntaxMode::New)
        .unwrap_err()
        .to_string()
        .contains("wildcard imports are unsupported"));
}
