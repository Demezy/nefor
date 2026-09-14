use nefor_mag::{
    compile as compile_artifact, compile_file_with_inputs,
    compile_file_with_inputs_and_module_roots, compile_with_options, CompilerLimits,
    CompilerOptions,
};
use serde_json::json;
use std::fs;

fn compile(
    source: &str,
    source_dir: &std::path::Path,
) -> Result<serde_json::Value, nefor_mag::error::MagError> {
    compile_artifact(source, source_dir)
}

fn workspace(name: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("nefor-mag-language-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(path.join("core")).unwrap();
    path
}

#[test]
fn nominal_adts_construct_match_and_serialize_by_constructor() {
    let root = workspace("nominal-adts");
    let artifact = compile(
        r#"
        (type Result [E A] (adt [Error E] [Ok A]))
        (let result (construct (Result String Int) Ok 42))
        (artifact
          {:value result
           :equal (= result (construct (Result String Int) Ok 42))
           :different (= result (construct (Result String Int) Error "42"))
           :rendered (match result
             [Error error error]
             [Ok answer (str answer)])})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(
        artifact,
        json!({
            "value": {"constructor": "Ok", "value": 42},
            "equal": true,
            "different": false,
            "rendered": "42",
        })
    );
}

#[test]
fn nominal_adt_descriptors_schemas_and_ids_include_owner_arguments() {
    let root = workspace("nominal-adt-evidence");
    let artifact = compile(
        r#"
        (type Result [E A] (adt [Ok A] [Error E]))
        (artifact
          {:text (type-evidence (type-tag (Result String Int)))
           :bool (type-evidence (type-tag (Result Bool Int)))
           :text_id (type-id (type-evidence (type-tag (Result String Int))))
           :bool_id (type-id (type-evidence (type-tag (Result Bool Int))))
           :schema (type-schema (type-tag (Result String Int)))})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(artifact["text"]["kind"], "adt");
    assert_eq!(artifact["text"]["name"], "main.Result");
    assert_eq!(artifact["text"]["constructors"][0]["name"], "Error");
    assert_eq!(artifact["text"]["constructors"][1]["name"], "Ok");
    assert_ne!(artifact["text_id"], artifact["bool_id"]);
    assert_eq!(artifact["schema"]["version"], 2);
    assert_eq!(artifact["schema"]["root"]["kind"], "adt");
    assert_eq!(artifact["schema"]["root"]["name"], "main.Result");
    assert_eq!(artifact["schema"]["root"]["owner_id"], artifact["text_id"]);
}

#[test]
fn nominal_adt_checker_enforces_ownership_exhaustiveness_and_branch_uniformity() {
    let root = workspace("nominal-adt-errors");
    let declarations = r#"
      (type First (adt [Same Int] [Other String]))
      (type Second (adt [Same Int] [Other String]))
      (let value (construct First Same 1))
    "#;
    for (expression, expected) in [
        (
            "(match value [Same x x])",
            "non-exhaustive match; missing Other",
        ),
        (
            "(match value [Same x x] [Same y y] [Other z 0])",
            "duplicate match arm for Same",
        ),
        (
            "(match value [Same x x] [Foreign z 0])",
            "constructor Foreign is not a member of main.First",
        ),
        (
            "(if true 1 \"no\")",
            "if branches must return one compatible type",
        ),
        ("[1 \"no\"]", "list elements must have one compatible type"),
        ("(as First 1)", "value does not conform to main.First"),
    ] {
        let source = format!("{declarations}\n(artifact {expression})");
        let error = compile(&source, &root).unwrap_err().to_string();
        assert!(error.contains(expected), "{expression}: {error}");
    }
}

#[test]
fn artifact_is_the_only_top_level_output() {
    let root = workspace("artifact");
    let artifact = compile(r#"(artifact {:answer 42})"#, &root).unwrap();
    assert_eq!(artifact, json!({"answer":42}));
    assert!(compile("42", &root)
        .unwrap_err()
        .to_string()
        .contains("must return Artifact"));
}

#[test]
fn lisp_form_diagnostics_survive_authored_lowering() {
    let root = workspace("lisp-form-diagnostics");

    assert!(matches!(
        compile("(require)", &root),
        Err(nefor_mag::error::MagError::Arity {
            expected: 1,
            got: 0
        })
    ));
    assert_eq!(
        compile("(artifact (let x 1))", &root)
            .unwrap_err()
            .to_string(),
        "type error: let is only valid directly in a source or function block"
    );
    assert_eq!(
        compile("(artifact (fn [] Int 1))", &root)
            .unwrap_err()
            .to_string(),
        "type error: typed fn signature required"
    );
    assert_eq!(
        compile("(artifact (match nil [Int value]))", &root)
            .unwrap_err()
            .to_string(),
        "type error: match arm must be [Constructor binding expression]"
    );
    assert_eq!(
        compile("(artifact (type-tag []))", &root)
            .unwrap_err()
            .to_string(),
        "type error: invalid type expression"
    );
}

#[test]
fn packed_values_have_an_explicit_compiler_owned_envelope() {
    let root = workspace("packed-value-envelope");
    let artifact = compile(
        r#"(artifact (pack {:type "sha256:user-authored" :value {:nested true}}))"#,
        &root,
    )
    .unwrap();

    assert_eq!(
        artifact,
        json!({
            "$mag": "packed-value",
            "value": {
                "type": "sha256:user-authored",
                "value": {"nested": true}
            }
        })
    );
}

#[test]
fn raw_multiline_strings_support_scala_style_margins() {
    let root = workspace("raw-multiline-strings");
    let artifact = compile(
        r#"
          (let script
            (strip-margin """|set -e
                              |echo 'export PATH="$HOME/.local/bin:$PATH"'
                              |find . \( -name '*.mag' -o -name '*.md' \)"""))
          (artifact
            {:script script
             :single-line (replace script "\n" " ")})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(
        artifact,
        json!({
            "script": "set -e\necho 'export PATH=\"$HOME/.local/bin:$PATH\"'\nfind . \\( -name '*.mag' -o -name '*.md' \\)",
            "single-line": "set -e echo 'export PATH=\"$HOME/.local/bin:$PATH\"' find . \\( -name '*.mag' -o -name '*.md' \\)"
        })
    );
}

#[test]
fn rust_compilation_returns_the_artifact_directly() {
    let root = workspace("compilation-artifact");
    assert_eq!(
        CompilerLimits::default(),
        CompilerLimits {
            evaluation_steps: 1_000_000,
            call_depth: 64,
            expression_depth: 128,
            memoized_calls: 16_384,
        }
    );
    let artifact = compile_artifact("(artifact {:answer 42})", &root).unwrap();
    assert_eq!(artifact, json!({"answer": 42}));

    let constrained = compile_with_options(
        "(artifact {:answer 42})",
        &root,
        CompilerOptions {
            limits: CompilerLimits {
                evaluation_steps: 0,
                ..CompilerLimits::default()
            },
        },
    );
    assert!(constrained.is_err());
}

#[test]
fn direct_let_bindings_and_mutual_recursion_share_a_lexical_scope() {
    let root = workspace("direct-let-mutual");
    let artifact = compile(
        r#"
          (let finished "done")
          (let first
            (fn [[items (List Int)]] -> String
              (if (= (count items) 0)
                finished
                (second (remove-at items 0)))))
          (let second
            (fn [[items (List Int)]] -> String
              (if (= (count items) 0)
                finished
                (first (remove-at items 0)))))
          (artifact (first [1 2 3]))
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(artifact, json!("done"));
}

#[test]
fn direct_let_schedules_forward_values_and_rejects_strict_cycles() {
    let root = workspace("direct-let-forward");
    let artifact = compile(
        r#"
          (let message (str prefix " world"))
          (let prefix "hello")
          (artifact message)
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!("hello world"));

    let error = compile(
        r#"
          (let x (str y "!"))
          (let y (str x "?"))
          (artifact x)
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("x -> y -> x"), "{error}");

    let call_through = compile(
        r#"
          (let read-value (fn [] -> String value))
          (let value (read-value))
          (artifact value)
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        call_through.contains("value -> read-value -> value"),
        "{call_through}"
    );
}

#[test]
fn strict_binding_reports_unknown_symbol_instead_of_recursive_peers() {
    let root = workspace("direct-let-unknown");
    let error = compile(
        r#"
          (let answer missing)
          (let rendered (str "answer: " answer))
          (artifact rendered)
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("unresolved symbol: missing"), "{error}");
    assert!(!error.contains("recursive strict bindings"), "{error}");
}

#[test]
fn recursive_activations_have_distinct_local_binding_slots() {
    let root = workspace("recursive-local-slots");
    let artifact = compile(
        r#"
          (let countdown (fn [[again Bool]] -> Int
            (let result (if again (countdown false) 0))
            result))
          (artifact (countdown true))
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!(0));
}

#[test]
fn direct_let_supports_typed_value_and_function_overloads() {
    let root = workspace("direct-let-overloads");
    let artifact = compile(
        r#"
          (let render "plain")
          (let render (fn [[value Int]] -> String (str value)))
          (let render (fn [[value Bool]] -> String (str value)))
          (artifact
            [(as String render) (render 7) (render true)])
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!(["plain", "7", "true"]));

    let duplicate = compile(
        r#"
          (let render (fn [T] [[value T]] -> T value))
          (let render (fn [U] [[value U]] -> U value))
          (artifact {})
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        duplicate.contains("duplicate visible overload"),
        "{duplicate}"
    );
}

#[test]
fn builtin_signatures_participate_in_typed_overload_sets() {
    let root = workspace("builtin-overload-collision");
    let error = compile(
        r#"
          (let count (fn [T] [[items (List T)]] -> Int 99))
          (artifact (count [1 2 3]))
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("duplicate visible overload count"),
        "{error}"
    );

    for (name, declaration) in [
        ("str", r#"(let str (fn [[value Int]] -> String "custom"))"#),
        (
            "count",
            "(let count (fn [[value {:value Int}]] -> Int 0))",
        ),
        (
            "keys",
            "(let keys (fn [[value {:value Int}]] -> (List String) []))",
        ),
        (
            "get",
            "(let get (fn [[value {:value Int}] [key String]] -> Int 0))",
        ),
        (
            "assoc",
            "(let assoc (fn [[value {:value Int}] [key String] [field Int]] -> {:value Int} value))",
        ),
    ] {
        let source = format!("{declaration}\n(artifact {{}})");
        let error = match compile(&source, &root) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("{name} collision unexpectedly compiled"),
        };
        assert!(
            error.contains(&format!("duplicate visible overload {name}")),
            "{name}: {error}"
        );
    }

    let artifact = compile(
        r#"
          (let count (fn [[value Int]] -> Int 99))
          (artifact {:custom (count 1) :builtin (count [1 2 3])})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!({"custom":99,"builtin":3}));
}

#[test]
fn generic_binders_do_not_leak_from_peer_signatures() {
    let root = workspace("generic-binder-scope");
    let error = compile(
        r#"
          (let identity (fn [T] [[value T]] -> T value))
          (let leaked (fn [[value T]] -> T value))
          (artifact {})
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unresolved symbol: T"), "{error}");
}

#[test]
fn nested_same_name_generic_binders_are_fresh_per_candidate() {
    let root = workspace("fresh-generic-binders");
    let artifact = compile(
        r#"
          (type Port [T] {:value T})
          (type Continue [T] {:value T})
          (let store-port
            (fn [T] [[port (Port T)]] -> String "stored"))
          (let retry-gate
            (fn [T] [[continued (Port (Continue T))]] -> String
              (store-port continued)))
          (artifact
            (retry-gate
              (as (Port (Continue Int)) {:value {:value 1}})))
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!("stored"));
}

#[test]
fn ambient_generic_variables_remain_rigid_during_candidate_instantiation() {
    let root = workspace("rigid-ambient-generics");
    let error = compile(
        r#"
          (let takes-string
            (fn [[candidate (Fn String String)]] -> String
              (candidate "value")))
          (let outer
            (fn [T] [[x T]] -> String
              (let local
                (fn [U] [[ignored U]] -> T x))
              (takes-string local)))
          (artifact {})
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("no overload local matches (Fn String String)"),
        "{error}"
    );
}

#[test]
fn expected_results_resolve_return_only_overloads_and_generics() {
    let root = workspace("expected-result-overloads");
    let artifact = compile(
        r#"
          (let choose (fn [] -> Int 7))
          (let choose (fn [] -> String "selected"))
          (let produce (fn [T] [] -> T (as T "generic")))
          (let produce-string (fn [] -> String (produce)))
          (let invoke-string
            (fn [[producer (Fn Unit String)]] -> String (producer nil)))
          (let unit-producer
            (fn [T] [[ignored Unit]] -> T (as T "higher-order")))
          (artifact
            {:overload (as String (choose))
             :declared (produce-string)
             :higher-order (invoke-string unit-producer)})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({
            "overload": "selected",
            "declared": "generic",
            "higher-order": "higher-order",
        })
    );
}

#[test]
fn output_only_generic_memoization_is_specialization_aware() {
    let root = workspace("output-generic-memoization");
    let artifact = compile(
        r#"
          (let identify (fn [T] [] -> (TypeTag T) (type-tag T)))
          (artifact
            {:string (as (TypeTag String) (identify))
             :integer (as (TypeTag Int) (identify))})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(artifact["string"]["name"], json!("String"));
    assert_eq!(artifact["integer"]["name"], json!("Int"));
}

#[test]
fn expected_function_types_resolve_value_function_name_overloads() {
    let root = workspace("higher-order-overload");
    let artifact = compile(
        r#"
          (let transform "plain")
          (let transform (fn [[value Int]] -> String (str value)))
          (let apply-one
            (fn [[operation (Fn Int String)]] -> String
              (operation 7)))
          (artifact
            [(as String transform) (apply-one transform)])
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(artifact, json!(["plain", "7"]));
}

#[test]
fn nested_scopes_preserve_all_differently_typed_overloads() {
    let root = workspace("nested-overloads");
    let artifact = compile(
        r#"
          (let render (fn [[value Int]] -> String (str value)))
          (let use-outer
            (fn [[render String]] -> String
              (str (as String render) (render 7))))
          (let run
            (fn [] -> (List String)
              (let show (fn [[value Int]] -> String (str value)))
              (let show (fn [[value Bool]] -> String (str value)))
              [(show 1) (show true)]))
          (artifact
            {:outer (use-outer "value=") :local (run)})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(
        artifact,
        json!({"outer": "value=7", "local": ["1", "true"]})
    );
}

#[test]
fn closures_inside_strict_values_see_the_completed_peer_frame() {
    let root = workspace("closure-record-recursion");
    let artifact = compile(
        r#"
          (let handlers
            {:even (fn [[items (List Int)]] -> Bool
                     (if (= (count items) 0)
                       true
                       ((get handlers "odd") (remove-at items 0))))
             :odd (fn [[items (List Int)]] -> Bool
                    (if (= (count items) 0)
                      false
                      ((get handlers "even") (remove-at items 0))))})
          (artifact
            ((get handlers "even") [1 2]))
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(artifact, json!(true));
}

#[test]
fn direct_let_builds_nested_shared_scopes_and_checks_parameter_collisions() {
    let root = workspace("direct-let-nested");
    let artifact = compile(
        r#"
          (let run
            (fn [[items (List Int)]] -> String
              (let finished "nested")
              (let first
                (fn [[remaining (List Int)]] -> String
                  (if (= (count remaining) 0)
                    finished
                    (second (remove-at remaining 0)))))
              (let second
                (fn [[remaining (List Int)]] -> String
                  (if (= (count remaining) 0)
                    finished
                    (first (remove-at remaining 0)))))
              (first items)))
          (artifact (run [1 2]))
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!("nested"));

    let collision = compile(
        r#"
          (let item 1)
          (let use-item (fn [[item Int]] -> Int item))
          (artifact {})
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        collision.contains("duplicate visible overload item"),
        "{collision}"
    );
}

#[test]
fn typed_library_functions_return_artifacts() {
    let root = workspace("typed-artifact");
    let source = r#"
      (let emit (fn [T] [[value T]] -> Artifact
        (artifact value)))
      (emit {:answer 42})
    "#;
    let artifact = compile(source, &root).unwrap();
    assert_eq!(artifact, json!({"answer":42}));
}

#[test]
fn namespaced_transitive_and_diamond_imports_are_stable() {
    let root = workspace("modules");
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(
        root.join("core/types.mag"),
        "(type Validated [E T] (| E T))\n(let marker 7)",
    )
    .unwrap();
    fs::write(
        root.join("app/left.mag"),
        "(require \"core.types\")\n(let left core.types.marker)",
    )
    .unwrap();
    fs::write(
        root.join("app/right.mag"),
        "(require \"core.types\")\n(let right core.types.marker)",
    )
    .unwrap();
    fs::write(root.join("main.mag"),"(require \"app.left\")\n(require \"app.right\")\n(artifact {:left app.left.left :right app.right.right :type (str core.types.Validated)})").unwrap();
    let loaded = compile_file_with_inputs(&root, "main.mag", json!({})).unwrap();
    assert_eq!(loaded["left"], 7);
    assert_eq!(loaded["right"], 7);
    assert_eq!(loaded["type"], "core.types.Validated");
}

#[test]
fn qualified_nominal_constructors_do_not_duck_type() {
    let root = workspace("qualified-nominals");
    fs::create_dir_all(root.join("left")).unwrap();
    fs::create_dir_all(root.join("right")).unwrap();
    fs::write(root.join("left/types.mag"), "(type Payload {:value Int})").unwrap();
    fs::write(root.join("right/types.mag"), "(type Payload {:value Int})").unwrap();
    fs::write(
        root.join("main.mag"),
        r#"
          (require "left.types")
          (require "right.types")
          (let accept-left
            (fn [[value left.types.Payload]] -> left.types.Payload value))
          (artifact (accept-left (as right.types.Payload {:value 1})))
        "#,
    )
    .unwrap();

    let error = compile_file_with_inputs(&root, "main.mag", json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("left.types.Payload") && error.contains("right.types.Payload"),
        "same-shaped constructors from different modules must remain distinct: {error}"
    );
}

#[test]
fn product_type_evidence_preserves_order_and_grouping() {
    let root = workspace("product-grouping");
    let artifact = compile(
        r#"
          (artifact {:left (type-evidence (type-tag (+ (+ Int String) Bool)))
             :right (type-evidence (type-tag (+ Int (+ String Bool))))
             :flat (type-evidence (type-tag (+ Int String Bool)))})
        "#,
        &root,
    )
    .unwrap();

    let left = json!({"kind":"product","items":[
        {"kind":"product","items":[
            {"kind":"primitive","name":"Int"},
            {"kind":"primitive","name":"String"}
        ]},
        {"kind":"primitive","name":"Bool"}
    ]});
    let right = json!({"kind":"product","items":[
        {"kind":"primitive","name":"Int"},
        {"kind":"product","items":[
            {"kind":"primitive","name":"String"},
            {"kind":"primitive","name":"Bool"}
        ]}
    ]});
    let flat = json!({"kind":"product","items":[
        {"kind":"primitive","name":"Int"},
        {"kind":"primitive","name":"String"},
        {"kind":"primitive","name":"Bool"}
    ]});
    assert_eq!(artifact["left"], left);
    assert_eq!(artifact["right"], right);
    assert_eq!(artifact["flat"], flat);
    assert_ne!(artifact["left"], artifact["right"]);
    assert_ne!(artifact["left"], artifact["flat"]);
    assert_ne!(artifact["right"], artifact["flat"]);
}

#[test]
fn nominal_types_and_factory_descriptors_are_ordinary_data() {
    let root = workspace("types");
    let source = r#"
      (type Payload {:text String})
      (type Outcome [T] (| T Unit))
      (let worker "nefor.factory.worker")
      (artifact {:payload (str Payload)
                 :factory worker
                 :type_arguments [(type-evidence (type-tag Payload))]})
    "#;
    let artifact = compile(source, &root).unwrap();
    assert_eq!(artifact["payload"], "main.Payload");
    assert_eq!(artifact["factory"], "nefor.factory.worker");
    assert_eq!(artifact["type_arguments"][0]["name"], "main.Payload");
}

#[test]
fn immutable_host_inputs_expose_only_typed_projections() {
    let root = workspace("inputs");
    fs::write(
        root.join("core/input.mag"),
        "(type Contract {:identity String :type_scheme {:input_tags (List String) :outputs (List String)}})\n(let contracts (host-input \"factory_contracts\" (type-tag (List Contract))))",
    )
    .unwrap();
    fs::write(
        root.join("main.mag"),
        "(let inputs 1)\n(require \"core.input\")\n(artifact {:local (as Int inputs) :contracts core.input.contracts})",
    )
    .unwrap();
    let loaded = compile_file_with_inputs(
        &root,
        "main.mag",
        json!({"factory_contracts":[{
            "identity":"x",
            "implementation":"private",
            "params":{"heterogeneous": true},
            "type_scheme":{
                "variables":[],
                "inputs":{"wire":{"kind":"private"}},
                "input_tags":["in"],
                "outputs":["out"]
            }
        }]}),
    )
    .unwrap();
    assert_eq!(
        loaded,
        json!({"local":1,"contracts":[{
            "identity":"x",
            "type_scheme":{"input_tags":["in"],"outputs":["out"]}
        }]})
    );
}

#[test]
fn host_input_projection_selects_the_hidden_capability_by_type() {
    let root = workspace("typed-input-capability");
    fs::write(
        root.join("main.mag"),
        r#"
          (let inputs 1)
          (artifact {:local (as Int inputs)
                     :host (host-input "message" (type-tag String))})
        "#,
    )
    .unwrap();

    let loaded = compile_file_with_inputs(&root, "main.mag", json!({"message":"hello"})).unwrap();
    assert_eq!(loaded, json!({"local":1,"host":"hello"}));
}

#[test]
fn host_input_projection_reports_missing_and_mistyped_values() {
    let root = workspace("input-errors");
    fs::write(
        root.join("main.mag"),
        "(artifact (host-input \"count\" (type-tag Int)))",
    )
    .unwrap();

    let missing = compile_file_with_inputs(&root, "main.mag", json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        missing.contains("host input \"count\" is not present"),
        "{missing}"
    );

    let mistyped = compile_file_with_inputs(&root, "main.mag", json!({"count":"many"}))
        .unwrap_err()
        .to_string();
    assert!(
        mistyped.contains("host input \"count\": type error: expected Int"),
        "{mistyped}"
    );

    for (ty, value) in [
        ("Bool", json!("true")),
        ("String", json!(true)),
        ("Unit", json!({})),
    ] {
        fs::write(
            root.join("main.mag"),
            format!("(artifact (host-input \"value\" (type-tag {ty})))"),
        )
        .unwrap();
        let error = compile_file_with_inputs(&root, "main.mag", json!({"value":value}))
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("expected {ty}")), "{error}");
    }

    fs::write(
        root.join("main.mag"),
        "(type Config {:steps (List {:enabled Bool :label String})})\n(artifact (host-input \"config\" (type-tag Config)))",
    )
    .unwrap();
    let nested = compile_file_with_inputs(
        &root,
        "main.mag",
        json!({"config":{"steps":[{"enabled":"yes","label":"build"}]}}),
    )
    .unwrap_err()
    .to_string();
    assert!(nested.contains("expected Bool"), "{nested}");
}

#[test]
fn json_data_files_are_parsed_into_mag_values() {
    let root = workspace("file-read-json");
    let file = root.join("toolsets.json");
    fs::write(&file, r#"{"read_only":["read_file","read_image"]}"#).unwrap();
    fs::write(
        root.join("main.mag"),
        r#"
          (let manifest (read-json "toolsets.json"))
          (artifact (get manifest "read_only"))
        "#,
    )
    .unwrap();

    let loaded = compile_file_with_inputs(&root, "main.mag", json!({})).unwrap();
    assert_eq!(loaded, json!(["read_file", "read_image"]));

    fs::write(&file, "not json").unwrap();
    let error = compile_file_with_inputs(&root, "main.mag", json!({}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot parse JSON toolsets.json"), "{error}");
}

#[test]
fn fail_preserves_library_diagnostics() {
    let root = workspace("failure");
    let error = compile("(fail {:kind \"Invalid\" :errors [\"bad route\"]})", &root)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Invalid"), "{error}");
    assert!(error.contains("bad route"), "{error}");
}

#[test]
fn typed_generic_functions_construct_nominal_records() {
    let root = workspace("typed-functions");
    let source = r#"
      (type Box [T] {:value T})
      (let box (fn [T] [[value T]] -> (Box T) (as (Box T) {:value value})))
      (artifact (box 42))
    "#;
    let artifact = compile(source, &root).unwrap();
    assert_eq!(artifact, json!({"value":42}));
}

#[test]
fn checker_rejects_bad_returns_and_calls() {
    let root = workspace("type-errors");
    let bad_return = compile(
        "(let wrong (fn [[value Int]] -> String value))\n(artifact {})",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        bad_return.contains("returns Int, declared String"),
        "{bad_return}"
    );

    let bad_call = compile(
        "(let only-int (fn [[value Int]] -> Int value))\n(artifact (only-int \"no\"))",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(bad_call.contains("expected Int, got String"), "{bad_call}");
}

#[test]
fn host_inputs_are_opaque_outside_typed_projections() {
    let root = workspace("opaque-inputs");
    fs::write(root.join("main.mag"), "(artifact (get inputs :contracts))").unwrap();
    let error = compile_file_with_inputs(
        &root,
        "main.mag",
        json!({"contracts":[{"identity":"worker"}]}),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("get expects a map"), "{error}");
}

#[test]
fn factory_descriptions_use_ordinary_typed_functions() {
    let root = workspace("factory-data");
    let source = r#"
      (type Params {:seed String})
      (type Input {:prompt String})
      (type Output {:answer String})
      (let worker
        (fn [[params Params] [input (TypeTag Input)] [output (TypeTag Output)]]
          -> {:factory String :type_arguments (List TypeDescriptor) :params Params}
          {:factory "runtime.worker"
           :type_arguments [(type-evidence input) (type-evidence output)]
           :params params}))
      (artifact
        (worker (as Params {:seed "x"}) (type-tag Input) (type-tag Output)))
    "#;
    let artifact = compile(source, &root).unwrap();
    assert_eq!(artifact["factory"], "runtime.worker");

    let bad = source.replace("{:seed \"x\"}", "{:wrong \"x\"}");
    assert!(compile(&bad, &root).is_err());
}

#[test]
fn type_arguments_round_trip_as_ordinary_descriptors() {
    let root = workspace("factory-type-arguments");
    let artifact = compile(
        r#"
          (type Params {:seed String})
          (type Input {:prompt String})
          (type Error {:message String})
          (type Output {:answer String})
          (artifact
            [(type-evidence (type-tag Params))
             (type-evidence (type-tag (List Input)))
             (type-evidence (type-tag (| Output Error)))])
        "#,
        &root,
    )
    .unwrap();

    let primitive = |name: &str| json!({"kind":"primitive","name":name});
    let named = |name: &str, field: &str, ty: serde_json::Value| {
        json!({
            "kind":"named",
            "name":name,
            "arguments":[],
            "body":{"kind":"record","fields":[{"name":field,"type":ty}]}
        })
    };
    let params = named("main.Params", "seed", primitive("String"));
    let input = named("main.Input", "prompt", primitive("String"));
    let error = named("main.Error", "message", primitive("String"));
    let output = named("main.Output", "answer", primitive("String"));
    assert_eq!(
        artifact,
        json!([
                params,
                {"kind":"list","item":input},
                {"kind":"union","items":[error, output]}
        ])
    );
}

#[test]
fn empty_lists_retain_expected_element_types_at_runtime() {
    let root = workspace("empty-list");
    let source = r#"
      (let accept-strings (fn [[items (List String)]] -> Int (count items)))
      (artifact {:count (accept-strings (as (List String) []))})
    "#;
    let artifact = compile(source, &root).unwrap();
    assert_eq!(artifact["count"], 0);
}

#[test]
fn record_literals_satisfy_precise_homogeneous_string_maps() {
    let root = workspace("record-map");
    let source = r#"
      (let accept-string-map (fn [[value (Map String String)]] -> Int (count value)))
      (artifact {:count (accept-string-map
          (as (Map String String) {:kind "task" :prompt "Audit"}))})
    "#;
    let artifact = compile(source, &root).unwrap();
    assert_eq!(artifact["count"], 2);
}

#[test]
fn circular_modules_report_the_cycle() {
    let root = workspace("cycle");
    fs::write(root.join("core/a.mag"), "(require \"core.b\")").unwrap();
    fs::write(root.join("core/b.mag"), "(require \"core.a\")").unwrap();
    fs::write(root.join("main.mag"), "(require \"core.a\")\n(artifact {})").unwrap();
    let error = compile_file_with_inputs(&root, "main.mag", json!({}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("core.a -> core.b -> core.a"), "{error}");
}

#[test]
fn entry_and_module_search_roots_are_independent() {
    let root = workspace("module-roots");
    let entry_root = root.join("entry");
    let library_root = root.join("libraries");
    fs::create_dir_all(&entry_root).unwrap();
    fs::create_dir_all(library_root.join("core")).unwrap();
    fs::write(library_root.join("core/types.mag"), "(let marker 42)").unwrap();
    fs::write(
        entry_root.join("main.mag"),
        "(require \"core.types\")\n(artifact {:marker core.types.marker})",
    )
    .unwrap();
    let loaded = compile_file_with_inputs_and_module_roots(
        &entry_root,
        "main.mag",
        json!({}),
        &[library_root],
    )
    .unwrap();
    assert_eq!(loaded["marker"], 42);
}

#[test]
fn duplicate_canonical_modules_across_roots_are_rejected() {
    let root = workspace("duplicate-modules");
    let entry = root.join("entry");
    let left = root.join("left");
    let right = root.join("right");
    fs::create_dir_all(&entry).unwrap();
    fs::create_dir_all(left.join("core")).unwrap();
    fs::create_dir_all(right.join("core")).unwrap();
    fs::write(left.join("core/types.mag"), "(let side \"left\")").unwrap();
    fs::write(right.join("core/types.mag"), "(let side \"right\")").unwrap();
    fs::write(
        entry.join("main.mag"),
        "(require \"core.types\")\n(artifact {})",
    )
    .unwrap();
    let error =
        compile_file_with_inputs_and_module_roots(&entry, "main.mag", json!({}), &[left, right])
            .unwrap_err()
            .to_string();
    assert!(error.contains("ambiguous across search roots"), "{error}");
}

#[test]
fn nominal_values_require_explicit_refinement() {
    let root = workspace("nominal-opacity");
    let implicit = compile(
        "(type User {:name String})\n(let user (fn [[name String]] -> User {:name name}))\n(artifact {})",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        implicit.contains("use as for explicit refinement"),
        "{implicit}"
    );

    let artifact = compile(
        "(type User {:name String})\n(let user (fn [[name String]] -> User (as User {:name name})))\n(artifact (user \"Ada\"))",
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!({"name":"Ada"}));
}

#[test]
fn sum_refinement_preserves_explicit_leaf_constructor_evidence() {
    let root = workspace("sum-constructor-evidence");
    let artifact = compile(
        r#"
          (type X {:value Int})
          (type Y {:value Int})
          (type XY (| X Y))
          (type Nested (| XY X))
          (let generic (fn [T] [[value T]] -> T value))
          (let x (as X {:value 1}))
          (let selected (as Nested (generic (as XY x))))
          (artifact (as X selected))
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(artifact, json!({"value": 1}));
}

#[test]
fn match_eliminates_named_and_generic_sum_aliases_by_constructor_evidence() {
    let root = workspace("match-sums");
    let artifact = compile(
        r#"
          (type Some [T] {:value T})
          (type None {})
          (type Option [T] (| (Some T) None))
          (type Alias [T] (Option T))
          (let render
            (fn [[choice (Alias Int)]] -> String
              (match choice
                [(Some Int) present (str (get present "value"))]
                [None absent "none"])))
          (artifact
            {:some (render (as (Alias Int) (as (Some Int) {:value 7})))
             :none (render (as (Alias Int) (as None {})))})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(artifact, json!({"some": "7", "none": "none"}));
}

#[test]
fn match_is_exhaustive_unique_nominal_and_result_typed() {
    let root = workspace("match-errors");
    let prelude = r#"
      (type X {:value Int})
      (type Y {:value Int})
      (type Z {:value Int})
      (type XY (| X Y))
      (let selected (as XY (as X {:value 1})))
    "#;
    for (body, expected) in [
        (
            "(artifact (match selected [X x (get x \"value\")]))",
            "non-exhaustive match; missing main.Y",
        ),
        (
            "(artifact (match selected [X x 1] [X again 2] [Y y 3]))",
            "duplicate match arm for main.X",
        ),
        (
            "(artifact (match selected [X x 1] [Z z 2] [Y y 3]))",
            "constructor main.Z is not an arm of main.XY",
        ),
        (
            "(artifact (match selected [X x 1] [Y y \"wrong\"]))",
            "expected Int, got String",
        ),
    ] {
        let error = compile(&format!("{prelude}\n{body}"), &root)
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }

    let error = compile("(artifact (match 1 [Int value value]))", &root)
        .unwrap_err()
        .to_string();
    assert!(error.contains("match expects a sum value"), "{error}");

    let repeated = compile(
        r#"
          (type Some [T] {:value T})
          (type Repeated (| (Some Int) (Some String)))
          (let selected (as Repeated (as (Some Int) {:value 1})))
          (artifact
            (match selected
              [(Some Int) integer 1]
              [(Some String) string 2]))
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        repeated.contains("match cannot distinguish repeated nominal constructor main.Some"),
        "{repeated}"
    );
}

#[test]
fn match_preserves_mag_generated_sum_serialization() {
    let root = workspace("match-serialization");
    let artifact = compile(
        r#"
          (type X {:value Int})
          (type Y {:value Int})
          (type XY (| X Y))
          (type Alias XY)
          (let selected (as Alias (as X {:value 9})))
          (artifact
            (match selected
              [X x (as Alias x)]
              [Y y (as Alias y)]))
        "#,
        &root,
    )
    .unwrap();
    assert!(artifact["type"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(artifact["value"], json!({"value": 9}));
}

#[test]
fn sum_refinement_rejects_lookalikes_without_matching_constructor_evidence() {
    let root = workspace("sum-constructor-rejection");
    let untagged = compile(
        r#"
          (type X {:value Int})
          (type Y {:value Int})
          (artifact (as (| X Y) {:value 1}))
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(untagged.contains("cannot construct sum"), "{untagged}");
    assert!(untagged.contains("source type"), "{untagged}");
    assert!(
        untagged.contains("no explicit nominal constructor evidence"),
        "{untagged}"
    );
    assert!(
        untagged.contains("distinct from graph-edge compatibility"),
        "{untagged}"
    );

    let primitive = compile("(artifact (as (| Unit String) nil))", &root)
        .unwrap_err()
        .to_string();
    assert!(
        primitive.contains("cannot construct sum (Unit | String) from source type Unit"),
        "{primitive}"
    );
    assert!(
        primitive.contains("primitive and structural sum construction is unsupported"),
        "{primitive}"
    );

    let lookalike = compile(
        r#"
          (type X {:value Int})
          (type Y {:value Int})
          (let selected (as (| X Y) (as X {:value 1})))
          (artifact (as Y selected))
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        lookalike.contains("cannot replace constructor evidence"),
        "{lookalike}"
    );

    let invalid = compile(
        r#"
          (type X {:value Int})
          (type Y {:value Int})
          (type Z {:value Int})
          (artifact (as (| X Y) (as Z {:value 1})))
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(invalid.contains("not accepted"), "{invalid}");
}

#[test]
fn product_values_are_exact_ordered_tuples_with_authored_grouping() {
    let root = workspace("product-values");
    let artifact = compile(
        r#"
          (artifact {:flat (as (+ Int String Int) [1 "middle" 2])
             :left (as (+ (+ Int String) Bool)
                       [(as (+ Int String) [3 "left"]) true])
             :right (as (+ Int (+ String Bool))
                        [4 (as (+ String Bool) ["right" false])])})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({
            "flat": [1, "middle", 2],
            "left": [[3, "left"], true],
            "right": [4, ["right", false]],
        })
    );

    for (name, source) in [
        ("short", r#"(artifact (as (+ Int String) [1]))"#),
        ("long", r#"(artifact (as (+ Int String) [1 "x" 2]))"#),
        ("positional", r#"(artifact (as (+ Int String) ["x" 1]))"#),
        ("old-intersection", r#"(artifact (as (+ Int Int) 1))"#),
    ] {
        let error = compile(source, &workspace(name)).unwrap_err().to_string();
        assert!(error.contains("does not conform"), "{name}: {error}");
    }
}

#[test]
fn product_positions_preserve_selected_constructor_evidence() {
    let root = workspace("product-constructor-evidence");
    let artifact = compile(
        r#"
          (type X {:value Int})
          (type Y {:value Int})
          (let tuple
            (as (+ (| X Y) String)
                [(as (| X Y) (as X {:value 1})) "kept"]))
          (let accept (fn [[value (+ (| X Y) String)]] -> (+ (| X Y) String) value))
          (artifact (accept tuple))
        "#,
        &root,
    )
    .unwrap();
    let envelope = artifact[0].as_object().unwrap();
    assert_eq!(envelope.len(), 2);
    assert!(envelope["type"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(envelope["value"], json!({"value": 1}));
    assert_eq!(artifact[1], "kept");
}

#[test]
fn explicit_record_refinement_reports_missing_and_unexpected_fields() {
    let root = workspace("exact-refinement");
    let error = compile(
        "(type ProcessOptions {:timeout_ms Int})\n(artifact (as ProcessOptions {:timeout-ms 30000}))",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("does not conform"), "{error}");
    assert!(error.contains("missing fields: timeout_ms"), "{error}");
    assert!(error.contains("unexpected fields: timeout-ms"), "{error}");
}

#[test]
fn explicit_record_refinement_reports_field_diffs_for_generic_nominals() {
    let root = workspace("generic-exact-refinement");
    let error = compile(
        "(type Pair [T] {:first T :second T})\n(artifact (as (Pair Int) {:first 1 :other 2}))",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("missing fields: second"), "{error}");
    assert!(error.contains("unexpected fields: other"), "{error}");
}

#[test]
fn recursive_evaluation_is_bounded() {
    let root = workspace("fuel");
    let error = compile(
        "(let loop (fn [[n Int]] -> Int (loop n)))\n(artifact (loop 0))",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("evaluation budget exceeded"), "{error}");
}

#[test]
fn repeated_pure_calls_fit_the_budget_by_reusing_results() {
    let root = workspace("memoized-budget");
    let calls = std::iter::repeat_n("(identity value)", 28_000)
        .collect::<Vec<_>>()
        .join(" ");
    let source = format!(
        "(let value 7)\n(let identity (fn [[item Int]] -> Int item))\n(artifact [{calls}])"
    );

    let artifact = compile(&source, &root).unwrap();
    assert_eq!(artifact.as_array().unwrap().len(), 28_000);
}

#[test]
fn duplicate_visible_function_signatures_are_rejected() {
    let root = workspace("lexical-recursion");
    let error = compile(
        r#"
          (let walk (fn [[items (List Int)]] -> Int
            (if (= (count items) 0)
              0
              (walk (remove-at items 0)))))
          (let saved walk)
          (let before (saved [1 2 3]))
          (let walk (fn [[items (List Int)]] -> Int 99))
          (artifact {:before before :after (saved [1 2 3])})
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("duplicate visible overload walk"), "{error}");
}

#[test]
fn deeply_nested_non_function_expressions_are_bounded() {
    let root = workspace("expression-depth");
    let mut expression = String::from("\"value\"");
    for _ in 0..180 {
        expression = format!("(str {expression})");
    }
    let source = format!("(artifact {expression})");
    let error = compile(&source, &root).unwrap_err().to_string();
    assert!(
        error.contains("expression nesting limit reached"),
        "{error}"
    );
}

#[test]
fn builtin_type_rules_are_total_and_assoc_checks_values() {
    let root = workspace("builtin-rules");
    let arity = compile("(artifact (map))", &root).unwrap_err().to_string();
    assert!(arity.contains("expected 2, got 0"), "{arity}");

    let mismatch = compile(
        "(let update (fn [[value {:count Int}]] -> {:count Int} (assoc value :count \"many\")))\n(artifact {})",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(mismatch.contains("expected Int, got String"), "{mismatch}");

    for source in [
        "(artifact (get {:count 1} 0))",
        "(artifact (assoc {:count 1} 0 2))",
    ] {
        let error = compile(source, &root).unwrap_err().to_string();
        assert!(error.contains("expected String, got Int"), "{error}");
    }

    let keys = compile(
        r#"
          (let original {:count 1})
          (artifact {:string (get original "count")
                     :keyword (get original :count)
                     :assoc-string (get (assoc original "count" 2) :count)
                     :assoc-keyword (get (assoc original :count 3) "count")})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        keys,
        json!({"string":1,"keyword":1,"assoc-string":2,"assoc-keyword":3})
    );

    let distinct = compile(
        r#"
          (let get (fn [[value {:count Int}] [key Int]] -> Int 99))
          (let assoc (fn [[value {:count Int}] [key Int] [field Int]] -> {:count Int}
            {:count field}))
          (artifact {:get (get {:count 1} 0)
                     :assoc (get (assoc {:count 1} 0 7) 0)})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(distinct, json!({"get":99,"assoc":99}));
}

#[test]
fn group_by_orders_keys_and_preserves_bucket_source_order() {
    let root = workspace("group-by-order");
    let artifact = compile(
        r#"
          (let first-character
            (fn [[value String]] -> String
              (if (= value "a1") "a" "b")))
          (let grouped (group-by first-character ["b1" "a1" "b2"]))
          (let empty
            (group-by
              (fn [[value String]] -> String value)
              (as (List String) [])))
          (let skewed
            (group-by
              (fn [[value Int]] -> String "all")
              [1 2 3 4 5 6 7 8]))
          (let interleaved
            (group-by
              (fn [[value String]] -> String
                (if (= value "a1") "a"
                  (if (= value "a2") "a" "b")))
              ["a1" "b1" "a2" "b2"]))
          (artifact
            {:keys (keys grouped)
             :a (get grouped "a")
             :b (get grouped "b")
             :b-concatenated (concat (get grouped "b") ["b3"])
             :b-equals-list (= (get grouped "b") ["b1" "b2"])
             :empty empty
             :skewed (get skewed "all")
             :interleaved-a (get interleaved "a")
             :interleaved-b (get interleaved "b")})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(
        artifact,
        json!({
            "keys": ["a", "b"],
            "a": ["a1"],
            "b": ["b1", "b2"],
            "b-concatenated": ["b1", "b2", "b3"],
            "b-equals-list": true,
            "empty": {},
            "skewed": [1, 2, 3, 4, 5, 6, 7, 8],
            "interleaved-a": ["a1", "a2"],
            "interleaved-b": ["b1", "b2"]
        })
    );
}

#[test]
fn group_by_visits_left_to_right_and_stops_at_the_first_callback_error() {
    let root = workspace("group-by-callback-error");
    let error = compile(
        r#"
          (let key
            (fn [[value String]] -> String
              (if (= value "first")
                (fail (str "visited:" value))
                (fail (str "visited:" value)))))
          (artifact (group-by key ["first" "second"]))
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("visited:first"), "{error}");
    assert!(!error.contains("visited:second"), "{error}");
}

#[test]
fn group_by_rejects_invalid_static_calls() {
    let root = workspace("group-by-invalid-calls");
    for (label, source, expected) in [
        (
            "callback result",
            "(artifact (group-by (fn [[value Int]] -> Int value) [1]))",
            "group-by callback must return String",
        ),
        (
            "collection",
            "(artifact (group-by (fn [[value Int]] -> String (str value)) 1))",
            "group-by expects List",
        ),
        (
            "arity",
            "(artifact (group-by (fn [[value Int]] -> String (str value))))",
            "arity: expected 2, got 1",
        ),
    ] {
        let error = compile(source, &root).unwrap_err().to_string();
        assert!(error.contains(expected), "{label}: {error}");
    }
}

#[test]
fn group_by_builtin_and_non_colliding_user_overload_are_distinguishable() {
    let root = workspace("group-by-overload");
    let artifact = compile(
        r#"
          (let group-by (fn [[value Int]] -> String "custom"))
          (let grouped
            (group-by
              (fn [[value String]] -> String value)
              ["builtin" "builtin"]))
          (artifact {:custom (group-by 1) :builtin (get grouped "builtin")})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(
        artifact,
        json!({"custom": "custom", "builtin": ["builtin", "builtin"]})
    );
}

#[test]
fn canonical_and_sort_by_are_typed_deterministic_builtins() {
    let root = workspace("canonical-sort-by");
    let artifact = compile(
        r#"
          (type Item {:id String :rank Int})
          (let items (as (List Item)
            [(as Item {:id "third" :rank 30})
             (as Item {:id "first" :rank 10})
             (as Item {:id "second" :rank 20})]))
          (let ordered
            (sort-by
              (fn [[item Item]] -> String (get item "id"))
              items))
          (artifact {:canonical (canonical {:nodes ["a" "b"]
                                    :meta {:z 2 :a 1}
                                    :kind "edge"})
             :removed (remove-at ["a" "b" "c"] 1)
             :string-ok (conforms? "value" (type-evidence (type-tag String)))
             :string-bad (conforms? 42 (type-evidence (type-tag String)))
             :item-ok (conforms? {:id "item" :rank 1}
                        (type-evidence (type-tag Item)))
             :ids (map (fn [[item Item]] -> String (get item "id")) ordered)})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({
            "canonical": "{\"kind\":\"edge\",\"meta\":{\"a\":1,\"z\":2},\"nodes\":[\"a\",\"b\"]}",
            "removed": ["a", "c"],
            "string-ok": true,
            "string-bad": false,
            "item-ok": true,
            "ids": ["first", "second", "third"]
        })
    );

    let bad_key = compile(
        "(artifact (sort-by (fn [[value Int]] -> Int value) [2 1]))",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(
        bad_key.contains("sort-by callback must return String"),
        "{bad_key}"
    );

    let bad_arity = compile("(artifact (canonical 1 2))", &root)
        .unwrap_err()
        .to_string();
    assert!(bad_arity.contains("expected 1, got 2"), "{bad_arity}");
}

#[test]
fn fallible_nodes_compose_with_kleisli_semantics() {
    let root = workspace("node-kleisli-composition");
    let mag_lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    fs::write(
        root.join("main.mag"),
        r#"
          (require "nefor.graph")
          (require "nefor.node")

          (type Success {:value Int})
          (type Failure {:message String})

          (let success
            (nefor.graph.identity "success" (type-tag Success)))
          (let failure
            (nefor.graph.identity "failure" (type-tag Failure)))
          (let fallible
            (nefor.node.choose "fallible" success failure))
          (let continuation
            (nefor.graph.identity "continuation" (type-tag Success)))
          (let composed (nefor.node.>=> fallible continuation))

          (artifact composed)
        "#,
    )
    .unwrap();

    let program = compile_file_with_inputs_and_module_roots(
        &root,
        "main.mag",
        json!({}),
        std::slice::from_ref(&mag_lib),
    )
    .unwrap();

    assert_eq!(program["id"], "fallible>=>continuation");
    assert!(program["actors"].as_array().is_some_and(|actors| actors
        .iter()
        .any(|actor| actor["id"] == "fallible>=>continuation.error")));
}

#[test]
fn graph_product_input_accepts_repeated_typed_fan_in() {
    let root = workspace("product-fan-in");
    let mag_lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    fs::write(
        root.join("main.mag"),
        r#"
          (require "nefor.graph")
          (require "nefor.mag")
          (type CoveredLeft {:value Int})
          (type CoveredRight {:value String})
          (type CoveredChoice (| CoveredLeft CoveredRight))
          (let left (nefor.graph.source
                       "left" (type-tag nefor.contracts.Text)
                       (as nefor.contracts.Text {:content "left"})))
          (let right (nefor.graph.source
                        "right" (type-tag nefor.contracts.Text)
                        (as nefor.contracts.Text {:content "right"})))
          (let join-input (nefor.graph.port
                             "join"
                             (type-tag (+ nefor.contracts.Text nefor.contracts.Text))
                             "test.Value"))
          (let join-output (nefor.graph.port
                              "join" (type-tag nefor.contracts.Text) "test.Value"))
          (let join-actor (nefor.graph.actor
                             "join" "test.product"
                             [(type-evidence (type-tag nefor.contracts.Text))] nil
                             (nefor.graph.store-port join-input)
                             [(nefor.graph.store-port join-output)]))
          (let join (nefor.graph.node
                       "join" "ordinary" [join-actor]
                       (as (List nefor.graph.StoredRoute) [])
                       (as (List nefor.graph.Message) [])
                       join-input join-output))
          (let result (nefor.graph.output
                         "result" (type-tag nefor.contracts.Text)))
          (let topology (nefor.graph.graph
                           [(nefor.graph.edge left join)
                            (nefor.graph.edge right join)
                            (nefor.graph.edge join result)]))
          (let product-result (nefor.graph.output
                                 "product-result"
                                 (type-tag
                                   (+ nefor.contracts.Text
                                      nefor.contracts.Text))))
          (let output-topology (nefor.graph.graph
                                  [(nefor.graph.edge left product-result)
                                   (nefor.graph.edge right product-result)]))
          (let reversed-output-topology (nefor.graph.graph
                                           [(nefor.graph.edge right product-result)
                                            (nefor.graph.edge left product-result)]))
          (let choice-start (nefor.graph.source
                    "choice-start"
                    (type-tag CoveredChoice)
                    (as CoveredChoice (as CoveredLeft {:value 1}))))
          (let choice-result (nefor.graph.output "choice-result" (type-tag CoveredLeft)))
          (let choice-topology (nefor.graph.graph
                    [(nefor.graph.edge choice-start choice-result)]))
          (let choice-operation
            (nefor.graph.instantiate-delta-template "observe-choice"
              (get choice-start "output")
              (as (Map String nefor.mag.TypedCapture) {})
              (as (List nefor.mag.Expression) [])
              (as nefor.mag.DeltaTemplate
                {:types (as (Map String TypeDescriptor) {})
                 :actors [] :routes [] :messages [] :nodes []
                 :actor_reference_relocations []})))
          (let contracts (host-input "factory_contracts"
                            (type-tag (List nefor.graph.FactoryContract))))
          (let checked (nefor.graph.validate topology contracts))
          (let output-checked (nefor.graph.validate output-topology contracts))
          (let choice-without-rule (nefor.graph.validate choice-topology contracts))
          (let choice-with-operation (nefor.graph.validate-with-operations
                    choice-topology [choice-operation] contracts))
          (let lowered (nefor.graph.lower topology))
          (let lowered-left (first (filter
                    (fn [[candidate nefor.graph.LowerActor]] -> Bool
                      (= (get candidate "id") "left"))
                    (get lowered "actors"))))
          (let lowered-right (first (filter
                    (fn [[candidate nefor.graph.LowerActor]] -> Bool
                      (= (get candidate "id") "right"))
                    (get lowered "actors"))))
          (let lowered-join (first (filter
                    (fn [[candidate nefor.graph.LowerActor]] -> Bool
                      (= (get candidate "id") "join"))
                    (get lowered "actors"))))
          (let destinations (concat
                    (get (get lowered-left "routes") "nefor.graph.Value")
                    (get (get lowered-right "routes") "nefor.graph.Value")))
          (artifact {:valid (core.validated.valid? checked)
               :output-valid (core.validated.valid? output-checked)
               :choice-without-rule-valid (core.validated.valid? choice-without-rule)
               :choice-with-operation-valid (core.validated.valid? choice-with-operation)
               :type-count (count (get lowered "types"))
               :left-output-id
                 (get (first (get lowered-left "outputs")) "type_id")
               :join-output-id
                 (get (first (get lowered-join "outputs")) "type_id")
               :join-input-id (get (get lowered-join "input") "type_id")
               :left-route-source-id
                 (get (first destinations) "source_type_id")
               :left-route-destination-id
                 (get (first destinations) "destination_type_id")
               :permutation-stable
                 (= (canonical (nefor.graph.lower output-topology))
                    (canonical (nefor.graph.lower reversed-output-topology)))
               :positions
                 (map (fn [[destination nefor.graph.LowerDestination]] -> Int
                        (get destination "product_position"))
                      destinations)
               :edge-ids
                 (map (fn [[destination nefor.graph.LowerDestination]] -> String
                        (get destination "edge_id"))
                      destinations)})
        "#,
    )
    .unwrap();
    let inputs = json!({
        "factory_contracts": [
            {
                "identity": "nefor.factory.source",
                "type_scheme": {
                    "input_tags": ["mag.Unit"],
                    "outputs": ["nefor.graph.Value"]
                }
            },
            {
                "identity": "nefor.factory.output",
                "type_scheme": {
                    "input_tags": ["nefor.graph.Value"],
                    "outputs": ["nefor.graph.Value"]
                }
            },
            {
                "identity": "test.product",
                "type_scheme": {
                    "input_tags": ["test.Value"],
                    "outputs": ["test.Value"]
                }
            }
        ]
    });
    let artifact = compile_file_with_inputs_and_module_roots(
        &root,
        "main.mag",
        inputs,
        &[root.clone(), mag_lib],
    )
    .unwrap();
    assert_eq!(artifact["valid"], true, "{:?}", artifact);
    assert_eq!(artifact["output-valid"], true);
    assert_eq!(artifact["choice-without-rule-valid"], false);
    assert_eq!(artifact["choice-with-operation-valid"], true);
    assert!(artifact["type-count"].as_u64().unwrap() >= 4);
    assert_eq!(artifact["left-output-id"], artifact["left-route-source-id"]);
    assert_eq!(
        artifact["left-output-id"], artifact["join-output-id"],
        "wire names must not participate in semantic type identity"
    );
    assert_eq!(
        artifact["join-input-id"],
        artifact["left-route-destination-id"]
    );
    assert_eq!(artifact["permutation-stable"], true);
    assert_eq!(artifact["positions"], json!([0, 1]));
    let edge_ids = artifact["edge-ids"].as_array().unwrap();
    assert_eq!(edge_ids.len(), 2);
    assert_ne!(edge_ids[0], edge_ids[1]);
}

#[test]
fn graph_descriptor_operations_are_compiler_owned() {
    let root = workspace("graph-descriptors");
    let artifact = compile(
        r#"
          (type Left {:value Int})
          (type Right {:value String})
          (type Choice (| Left Right))
          (artifact {:arm (descriptor-accepts?
                    (type-evidence (type-tag Choice))
                    (type-evidence (type-tag Left)))
             :wrong-arm (descriptor-accepts?
                          (type-evidence (type-tag Left))
                          (type-evidence (type-tag Right)))
             :product (descriptor-input-covered-by?
                        (type-evidence (type-tag (+ Left Left)))
                        [(type-evidence (type-tag Left))
                         (type-evidence (type-tag Left))])
             :assignments (descriptor-input-assignments
                            (type-evidence (type-tag (+ Left Left)))
                            [(type-evidence (type-tag Left))
                             (type-evidence (type-tag (+ Left Left)))
                             (type-evidence (type-tag Left))])
             :covered-output (descriptor-output-covered-by?
                               (type-evidence (type-tag Choice))
                               [(type-evidence (type-tag Left))
                                (type-evidence (type-tag Right))])
             :uncovered-output (descriptor-output-covered-by?
                                 (type-evidence (type-tag Choice))
                                 [(type-evidence (type-tag Left))])})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({
            "arm": true,
            "wrong-arm": false,
            "product": true,
            "assignments": [0, -1, 1],
            "covered-output": true,
            "uncovered-output": false
        })
    );

    let forged = compile(
        r#"(artifact (as TypeDescriptor {:kind "primitive" :name "String"}))"#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(forged.contains("does not conform"), "{forged}");
}

#[test]
fn whole_value_compatibility_does_not_supply_product_components() {
    let root = workspace("whole-value-compatibility");
    let artifact = compile(
        r#"
        (type Text {:content String})
        (let unit (type-evidence (type-tag Unit)))
        (let product (type-evidence (type-tag (+ Unit Unit))))
        (artifact
          {:unit (descriptor-accepts-value? unit unit)
           :sum (descriptor-accepts-value?
             (type-evidence (type-tag (| Unit Text))) unit)
           :product (descriptor-accepts-value? product unit)
           :singleton-product (descriptor-accepts-value?
             (type-evidence (type-tag (+ Unit))) unit)
           :whole-product (descriptor-accepts-value? product product)
           :component-edge (descriptor-accepts? product unit)
           :non-unit (descriptor-accepts-value?
             (type-evidence (type-tag Text)) unit)})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({
            "unit": true, "sum": true, "product": false,
            "singleton-product": false, "whole-product": true,
            "component-edge": true, "non-unit": false
        })
    );
}

#[test]
fn data_is_not_a_source_type_or_cast_target() {
    let root = workspace("removed-data");
    let declaration = compile("(type Payload {:value Data})\n(artifact {})", &root)
        .unwrap_err()
        .to_string();
    assert!(
        declaration.contains("unresolved symbol: Data"),
        "{declaration}"
    );

    let cast = compile("(artifact (as Data {:value 1}))", &root)
        .unwrap_err()
        .to_string();
    assert!(cast.contains("unresolved symbol: Data"), "{cast}");
}

#[test]
fn union_unification_commits_substitutions_and_rejects_ambiguity() {
    let root = workspace("union-substitution");
    let artifact = compile(
        "(let select (fn [T] [[x (| T String)]] -> T (as T x)))\n(artifact (select 42))",
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!(42));

    let ambiguous = compile(
        "(let select (fn [T U] [[x (| T U)]] -> T (as T x)))\n(artifact (select 42))",
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(ambiguous.contains("ambiguous union match"), "{ambiguous}");
}

#[test]
fn type_tags_are_typed_canonical_witnesses() {
    let root = workspace("type-tags");
    let artifact = compile(
        "(type Payload {:value Int})\n(let tag-of (fn [T] [[value T]] -> (TypeTag T) (type-tag T)))\n(artifact (tag-of (as Payload {:value 1})))",
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({
            "kind":"named",
            "name":"main.Payload",
            "arguments":[],
            "body":{
                "kind":"record",
                "fields":[{
                    "name":"value",
                    "type":{"kind":"primitive","name":"Int"}
                }]
            }
        })
    );

    let unknown = compile("(artifact (type-tag Missing))", &root)
        .unwrap_err()
        .to_string();
    assert!(unknown.contains("Missing"), "{unknown}");
}

#[test]
fn repeated_factory_identity_strings_are_ordinary_data() {
    let root = workspace("duplicate-factory-data");
    let artifact = compile("(artifact [\"runtime.worker\" \"runtime.worker\"])", &root).unwrap();
    assert_eq!(artifact, json!(["runtime.worker", "runtime.worker"]));
}

#[test]
fn nested_callbacks_capture_enclosing_generic_binders() {
    let root = workspace("nested-generic-binders");
    let artifact = compile(
        "(let contains? (fn [T] [[values (List T)] [value T]] -> Bool (= (count (filter (fn [[candidate T]] -> Bool (= candidate value)) values)) 1)))\n(artifact (contains? [1 2] 1))",
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!(true));
}

#[test]
fn generic_calls_unify_arguments_inside_the_same_nominal_type() {
    let root = workspace("nominal-generic-unification");
    let artifact = compile(
        "(type Port [T] {:actor String})\n(let identity-port (fn [O] [[port (Port O)]] -> (Port O) port))\n(let forward-port (fn [T] [[port (Port T)]] -> (Port T) (identity-port port)))\n(artifact (forward-port (as (Port Int) {:actor \"worker\"})))",
        &root,
    )
    .unwrap();
    assert_eq!(artifact, json!({"actor":"worker"}));
}

#[test]
fn type_schema_preserves_qualified_nominals_and_substitutes_generics() {
    let root = workspace("type-schema");
    fs::write(
        root.join("core/types.mag"),
        "(type Box [T] {:value T})\n(let schema (type-schema (type-tag (Box (List String)))))",
    )
    .unwrap();
    fs::write(
        root.join("main.mag"),
        "(require \"core.types\")\n(artifact core.types.schema)",
    )
    .unwrap();
    let artifact = compile_file_with_inputs(&root, "main.mag", json!({})).unwrap();
    assert_eq!(artifact["version"], 1);
    assert_eq!(artifact["root"]["kind"], "named");
    assert_eq!(artifact["root"]["name"], "core.types.Box");
    assert_eq!(
        artifact["root"]["body"]["fields"][0]["schema"]["kind"],
        "list"
    );
    assert_eq!(
        artifact["root"]["body"]["fields"][0]["schema"]["item"]["kind"],
        "string"
    );
}

#[test]
fn type_schema_rejects_non_data_and_non_string_map_keys() {
    let root = workspace("type-schema-errors");
    for (ty, expected) in [
        (
            "(Fn String String)",
            "Fn cannot enter a concrete semantic descriptor",
        ),
        (
            "Artifact",
            "Artifact cannot enter a concrete semantic descriptor",
        ),
        (
            "(TypeTag String)",
            "TypeTag cannot enter a concrete semantic descriptor",
        ),
        ("(Map Int String)", "JSON object keys must be String"),
    ] {
        let source = format!("(artifact (type-schema (type-tag {ty})))");
        let error = compile(&source, &root).unwrap_err().to_string();
        assert!(error.contains(expected), "{ty}: {error}");
    }
}

#[test]
fn generic_list_inference_preserves_element_evidence() {
    let root = workspace("generic-list-inference");
    for values in ["[1 2]", "values", "(as (List Int) [1 2])"] {
        let source = format!(
            r#"
          (let values [1 2])
          (let element-tag (fn [T] [[values (List T)]] -> (TypeTag T) (type-tag T)))
          (artifact (= (type-evidence (element-tag {values})) (type-evidence (type-tag Int))))
        "#
        );
        assert_eq!(compile(&source, &root).unwrap(), json!(true), "{values}");
    }
}

#[test]
fn generic_list_inference_keeps_constraints_and_empty_list_behavior() {
    let root = workspace("generic-list-constraints");
    for source in [
        r#"(let tag (fn [T] [[xs (List T)]] -> (TypeTag T) (type-tag T)))
           (artifact (tag [1 "wrong"]))"#,
        r#"(let tag (fn [T] [[xs (List T)]] -> (TypeTag T) (type-tag T)))
           (artifact (tag []))"#,
        r#"(let tag (fn [T] [[xs (List (List T))]] -> (TypeTag T) (type-tag T)))
           (artifact (tag [[1] ["wrong"]]))"#,
    ] {
        assert!(compile(source, &root).is_err(), "{source}");
    }
    for (name, source) in [
        (
            "nested",
            r#"(let tag (fn [T] [[xs (List (List T))]] -> (TypeTag T) (type-tag T)))
          (artifact (= (tag [[1] [2]]) (type-tag Int)))"#,
        ),
        (
            "empty-without-evidence",
            r#"(let size (fn [T] [[xs (List T)]] -> Int (count xs)))
          (artifact (= (size []) 0))"#,
        ),
        (
            "empty-annotated",
            r#"(let tag (fn [T] [[xs (List T)]] -> (TypeTag T) (type-tag T)))
          (artifact (= (tag (as (List Int) [])) (type-tag Int)))"#,
        ),
        (
            "sum",
            r#"(type A {:value Int}) (type B {:value String})
          (let tag (fn [T] [[xs (List T)]] -> (TypeTag T) (type-tag T)))
          (artifact (= (tag [(as (| A B) (as A {:value 1}))
                              (as (| A B) (as B {:value "two"}))]) (type-tag (| A B))))"#,
        ),
        (
            "overload",
            r#"(let tag (fn [T] [[xs (List T)]] -> (TypeTag T) (type-tag T)))
          (let tag (fn [[value String]] -> (TypeTag String) (type-tag String)))
          (artifact (= (tag [1 2]) (type-tag Int)))"#,
        ),
    ] {
        assert_eq!(
            compile(source, &root).unwrap_or_else(|error| panic!("{name}: {error}")),
            json!(true)
        );
    }
}
