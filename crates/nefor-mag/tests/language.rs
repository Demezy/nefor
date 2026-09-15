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
fn nominal_adt_construction_checks_the_selected_payload_type() {
    let root = workspace("nominal-adt-payload");
    let error = compile(
        r#"
        (type Choice (adt [Number Int]))
        (artifact (construct Choice Number "not an integer"))
        "#,
        &root,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("expected Int, got String"), "{error}");
}

#[test]
fn type_declarations_reject_duplicate_generic_parameters() {
    let root = workspace("duplicate-type-generics");
    let error = compile("(type Bad [T T] (adt [Wrap T])) (artifact {})", &root)
        .unwrap_err()
        .to_string();
    assert!(error.contains("duplicate generic parameter T"), "{error}");

    let error = compile("(artifact ((fn [T T] [[value T]] -> T value) 1))", &root)
        .unwrap_err()
        .to_string();
    assert!(error.contains("duplicate generic parameter T"), "{error}");
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
    assert!(error.contains("get expects a record"), "{error}");
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
fn record_literals_do_not_construct_native_maps() {
    let root = workspace("record-map");
    let source = r#"
      (let accept-string-map (fn [[value (Map String String)]] -> Int (count value)))
      (artifact {:count (accept-string-map
          (as (Map String String) {:kind "task" :prompt "Audit"}))})
    "#;
    assert!(compile(source, &root).is_err());
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
fn group_by_produces_native_map_and_preserves_bucket_source_order() {
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
            {:grouped grouped
             :a (__map-get grouped "a")
             :b (__map-get grouped "b")
             :b-concatenated (concat (__map-get grouped "b") ["b3"])
             :b-equals-list (= (__map-get grouped "b") ["b1" "b2"])
             :empty empty
             :skewed (__map-get skewed "all")
             :interleaved-a (__map-get interleaved "a")
             :interleaved-b (__map-get interleaved "b")})
        "#,
        &root,
    )
    .unwrap();

    assert_eq!(
        artifact,
        json!({
            "grouped": {"a": ["a1"], "b": ["b1", "b2"]},
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
          (artifact {:custom (group-by 1) :builtin (__map-get grouped "builtin")})
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
          (require "core.types")
          (require "nefor.graph")
          (require "nefor.node")

          (type Success {:value Int})
          (type Failure {:message String})

          (let fallible
            (nefor.graph.identity "fallible"
              (type-tag (core.types.Result Failure Success))))
          (let continuation
            (nefor.node.lift-result "continuation-result" (type-tag Failure)
              (nefor.graph.identity "continuation" (type-tag Success))))
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

    assert_eq!(program["id"], "fallible>=>continuation-result");
    assert!(program["actors"].as_array().is_some_and(|actors| actors
        .iter()
        .any(|actor| actor["id"] == "fallible>=>continuation-result.error")));
    assert!(program["actors"].as_array().is_some_and(|actors| actors
        .iter()
        .any(|actor| actor["factory"] == "nefor.factory.adt-unpack")));
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
    assert_eq!(artifact["version"], 2);
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
fn type_schema_rejects_non_data() {
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
          (artifact [(type-evidence (element-tag {values})) (type-evidence (type-tag Int))])
        "#
        );
        let evidence = compile(&source, &root).unwrap();
        assert_eq!(evidence[0], evidence[1], "{values}");
    }
}

#[test]
fn concrete_data_equality_is_recursive_and_float_bit_exact() {
    let root = workspace("concrete-equality");
    let artifact = compile(
        r#"
        (let mapped (map (fn [[x Int]] -> Int x) [1 2]))
        (artifact {:list (= mapped [1 2])
                   :record (= {:b [true false] :a mapped} {:a [1 2] :b [true false]})
                   :ordered (not (= [1 2] [2 1]))
                   :close (not (= 1.0 1.0000000000000002))
                   :signed-zero (not (= 0.0 -0.0))
                   :same (= -0.0 -0.0)})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({"list":true,"record":true,"ordered":true,"close":true,"signed-zero":true,"same":true})
    );
}

#[test]
fn native_maps_and_sets_are_unordered_concrete_data() {
    let root = workspace("native-unordered-data");
    let artifact = compile(
        r#"
        (let empty (__map-empty (type-tag {:id Int}) (type-tag String)))
        (let left (__map-insert (__map-insert empty {:id 2} "two") {:id 1} "one"))
        (let right (__map-insert (__map-insert empty {:id 1} "one") {:id 2} "two"))
        (let set-empty (__set-empty (type-tag Int)))
        (let a (__set-insert (__set-insert set-empty 2) 1))
        (let b (__set-insert (__set-insert set-empty 1) 2))
        (let keyed (__map-insert (__map-empty (type-tag (Set Int)) (type-tag Bool)) a true))
        (artifact {:map left :set a :map-equal (= left right) :set-equal (= a b)
                   :lookup (__map-get left {:id 1}) :set-key (__map-get keyed b)
                   :map-count (__map-count left) :set-count (__set-count a)
                   :map-has (__map-contains left {:id 2}) :map-missing (__map-contains left {:id 3})
                   :set-has (__set-contains a 1) :set-missing (__set-contains a 3)})
        "#,
        &root,
    )
    .unwrap();
    assert_eq!(
        artifact,
        json!({
            "map":{"$mag":"map","entries":[[{"id":1},"one"],[{"id":2},"two"]]},
            "set":{"$mag":"set","items":[1,2]},
            "map-equal":true,"set-equal":true,"lookup":"one","set-key":true,
            "map-count":2,"set-count":2,"map-has":true,"map-missing":false,"set-has":true,"set-missing":false
        })
    );
}

#[test]
fn native_collection_duplicates_and_missing_lookup_fail() {
    let root = workspace("native-duplicate-data");
    for (source, message) in [
        (
            r#"(let m (__map-empty (type-tag Int) (type-tag String)))
            (artifact (__map-insert (__map-insert m 1 "first") 1 "second"))"#,
            "duplicate Map key",
        ),
        (
            r#"(let s (__set-empty (type-tag {:x Int})))
            (artifact (__set-insert (__set-insert s {:x 1}) {:x 1}))"#,
            "duplicate Set member",
        ),
        (
            r#"(artifact (__map-get (__map-empty (type-tag Int) (type-tag String)) 1))"#,
            "Map key not found",
        ),
        (r#"(artifact {:same 1 :same 2})"#, "duplicate"),
        (
            r#"(type Bad {:same Int :same String}) (artifact {})"#,
            "duplicate",
        ),
    ] {
        let error = compile(source, &root).unwrap_err().to_string();
        assert!(error.contains(message), "{source}: {error}");
    }
}

#[test]
fn maps_and_sets_have_no_ordered_collection_or_record_surface() {
    let root = workspace("native-no-enumeration");
    for expression in [
        "(keys m)",
        "(get m \"key\")",
        "(assoc m \"key\" 1)",
        "(first m)",
        "(remove-at m 0)",
        "(map (fn [[x Int]] -> Int x) m)",
        "(keys s)",
        "(first s)",
        "(remove-at s 0)",
        "(fold (fn [[a Int] [b Int]] -> Int a) 0 s)",
    ] {
        let source = format!(
            r#"
            (let m (__map-empty (type-tag String) (type-tag Int)))
            (let s (__set-empty (type-tag Int)))
            (artifact {expression})"#
        );
        assert!(compile(&source, &root).is_err(), "{expression}");
    }
    assert_eq!(compile(r#"(artifact {:keys (keys {:b 2 :a 1}) :get (get {:a 1} "a") :assoc (assoc {:a 1} "a" 2)})"#, &root).unwrap(), json!({"keys":["a","b"],"get":1,"assoc":{"a":2}}));
}

#[test]
fn equality_rejects_behavior_in_nested_and_generic_positions() {
    let root = workspace("equality-obligations");
    for source in [
        "(let f (fn [[x Int]] -> Int x)) (artifact (= f f))",
        "(let f (fn [[x Int]] -> Int x)) (artifact (= [f] [f]))",
        "(let f (fn [[x Int]] -> Int x)) (artifact (= {:f f} {:f f}))",
        "(type Box [T] (adt [Box T])) (let f (fn [[x Int]] -> Int x)) (let b (construct (Box (Fn Int Int)) Box f)) (artifact (= b b))",
        "(let eq (fn [T] [[a T] [b T]] -> Bool (= a b))) (let f (fn [[x Int]] -> Int x)) (artifact (eq f f))",
        "(let eq (fn [T] [[a T] [b T]] -> Bool (= a b))) (let alias eq) (let f (fn [[x Int]] -> Int x)) (artifact (alias f f))",
        "(let eq (fn [T] [[a T] [b T]] -> Bool (= a b))) (let twice (fn [T] [[a T]] -> Bool (eq a a))) (let f (fn [[x Int]] -> Int x)) (artifact (twice f))",
        "(let eq (fn [T] [[a T] [b T]] -> Bool (= a b))) (let f (fn [[x Int]] -> Int x)) (artifact (if true true (eq f f)))",
        "(artifact (__set-empty (type-tag (Fn Int Int))))",
        "(artifact (__map-empty (type-tag {:f (Fn Int Int)}) (type-tag String)))",
    ] {
        assert!(compile(source, &root).is_err(), "accepted {source}");
    }
    assert_eq!(
        compile(
            r#"
        (let eq (fn [T] [[a T] [b T]] -> Bool (= a b)))
        (let twice (fn [T] [[a T]] -> Bool (eq a a)))
        (let identity (fn [T] [[a T]] -> T a))
        (let f (identity (fn [[x Int]] -> Int x)))
        (artifact {:equal (twice {:data [1 2]}) :function (f 7)})"#,
            &root
        )
        .unwrap(),
        json!({"equal":true,"function":7})
    );
}

#[test]
fn native_collection_wire_and_schema_agree_recursively() {
    use nefor_mag::{
        env::Env,
        json::{json_to_typed_value, value_to_json},
        schema::TypeSchema,
        types::MagType,
    };
    let env = Env::new();
    let cases = [
        (
            MagType::Map(Box::new(MagType::Int), Box::new(MagType::String)),
            json!({"$mag":"map","entries":[]}),
        ),
        (
            MagType::Map(Box::new(MagType::String), Box::new(MagType::Int)),
            json!({}),
        ),
        (
            MagType::Set(Box::new(MagType::Int)),
            json!({"$mag":"set","items":[]}),
        ),
        (
            MagType::Map(
                Box::new(MagType::Set(Box::new(MagType::Int))),
                Box::new(MagType::Bool),
            ),
            json!({"$mag":"map","entries":[[{"$mag":"set","items":[1,2]},true]]}),
        ),
    ];
    for (ty, wire) in cases {
        let schema = TypeSchema::reify(&env, &ty).unwrap();
        assert!(schema.validate_json(&wire.to_string()).ok, "{ty}: {wire}");
        let value = json_to_typed_value(&env, &wire, &ty).unwrap();
        assert_eq!(value_to_json(&env, &value).unwrap(), wire, "{ty}");
    }
    for (ty, wire) in [
        (
            MagType::Set(Box::new(MagType::Set(Box::new(MagType::Int)))),
            json!({"$mag":"set","items":[{"$mag":"set","items":[1,2]},{"$mag":"set","items":[2,1]}]}),
        ),
        (
            MagType::Map(
                Box::new(MagType::Set(Box::new(MagType::Int))),
                Box::new(MagType::Bool),
            ),
            json!({"$mag":"map","entries":[[{"$mag":"set","items":[1,2]},true],[{"$mag":"set","items":[2,1]},false]]}),
        ),
        (
            MagType::Set(Box::new(MagType::Float)),
            json!({"$mag":"set","items":[1,1.0]}),
        ),
    ] {
        assert!(
            json_to_typed_value(&env, &wire, &ty).is_err(),
            "decoder accepted {wire}"
        );
        assert!(
            !TypeSchema::reify(&env, &ty)
                .unwrap()
                .validate_json(&wire.to_string())
                .ok,
            "schema accepted {wire}"
        );
    }
    let signed_zeros = json!({"$mag":"set","items":[0.0,-0.0]});
    let ty = MagType::Set(Box::new(MagType::Float));
    assert!(json_to_typed_value(&env, &signed_zeros, &ty).is_ok());
    assert!(
        TypeSchema::reify(&env, &ty)
            .unwrap()
            .validate_json(&signed_zeros.to_string())
            .ok
    );
}

#[test]
fn empty_native_collections_keep_their_wire_type() {
    let root = workspace("empty-native-wire");
    assert_eq!(
        compile(
            r#"(artifact {:map (__map-empty (type-tag Int) (type-tag Bool))
        :strings (__map-empty (type-tag String) (type-tag Int))
        :set (__set-empty (type-tag String))})"#,
            &root
        )
        .unwrap(),
        json!({
            "map":{"$mag":"map","entries":[]},"strings":{},"set":{"$mag":"set","items":[]}
        })
    );
}

#[test]
fn nominal_constructor_and_product_keys_remain_distinct_data() {
    let root = workspace("native-key-constructor");
    assert_eq!(compile(r#"
        (type Key (adt [Left Int] [Right Int]))
        (let m (__map-empty (type-tag Key) (type-tag String)))
        (let m1 (__map-insert m (construct Key Left 1) "left"))
        (let m2 (__map-insert m1 (construct Key Right 1) "right"))
        (let tuple (as (+ Int String) [1 "a"]))
        (let tuples (__set-insert (__set-empty (type-tag (+ Int String))) tuple))
        (artifact {:left (__map-get m2 (construct Key Left 1)) :right (__map-get m2 (construct Key Right 1))
                   :tuple (__set-contains tuples (as (+ Int String) [1 "a"]))})"#, &root).unwrap(), json!({"left":"left","right":"right","tuple":true}));
}

#[test]
fn ordinary_core_modules_expose_unordered_maps_and_sets() {
    let root = workspace("core-native-collections");
    fs::write(
        root.join("main.mag"),
        r#"
        (require "core.map")
        (require "core.set")
        (let map
          (core.map.insert
            (as (Map Int String) (core.map.empty (type-tag Int)))
            1 "one"))
        (let set (core.set.insert (core.set.empty (type-tag String)) "ready"))
        (artifact {:map-value (core.map.get map 1)
                   :map-count (core.map.count map)
                   :set-member (core.set.contains? set "ready")
                   :set-count (core.set.count set)})
        "#,
    )
    .unwrap();
    let module_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    assert_eq!(
        compile_file_with_inputs_and_module_roots(
            &root,
            "main.mag",
            json!({}),
            std::slice::from_ref(&module_root),
        )
        .unwrap(),
        json!({"map-value":"one", "map-count":1, "set-member":true, "set-count":1})
    );

    for source in [
        r#"(require "core.map")
        (let map (core.map.insert
          (as (Map Int String) (core.map.empty (type-tag Int))) 1 "one"))
        (artifact (core.map.insert map 1 "again"))"#,
        r#"(require "core.set")
        (let set (core.set.insert (core.set.empty (type-tag String)) "ready"))
        (artifact (core.set.insert set "ready"))"#,
    ] {
        fs::write(root.join("main.mag"), source).unwrap();
        assert!(compile_file_with_inputs_and_module_roots(
            &root,
            "main.mag",
            json!({}),
            std::slice::from_ref(&module_root),
        )
        .is_err());
    }
}

#[test]
fn generic_equality_obligations_are_static_in_higher_order_dead_code() {
    let root = workspace("equality-higher-order-static");
    for invocation in [
        "(apply eq f)",
        "(apply (if true eq eq) f)",
        "(apply (get {:equal eq} \"equal\") f)",
    ] {
        let source = format!(
            r#"
            (let eq (fn [T] [[a T] [b T]] -> Bool (= a b)))
            (let apply (fn [T] [[compare (Fn T T Bool)] [value T]] -> Bool (compare value value)))
            (let f (fn [[x Int]] -> Int x))
            (artifact (if true true {invocation}))"#
        );
        let valid = source.replace(invocation, &invocation.replace(" f)", " 7)"));
        assert_eq!(
            compile(&valid, &root).unwrap_or_else(|error| panic!("{invocation}: {error}")),
            json!(true),
            "valid {invocation}"
        );
        assert!(compile(&source, &root).is_err(), "accepted {invocation}");
    }
}

#[test]
fn native_collections_support_ordinary_generic_wrappers() {
    let root = workspace("native-generic-wrappers");
    let source = r#"
        (let empty (fn [K V] [[key (TypeTag K)] [value (TypeTag V)]] -> (Map K V)
          (__map-empty key value)))
        (let insert (fn [K V] [[map (Map K V)] [key K] [value V]] -> (Map K V)
          (__map-insert map key value)))
        (let lookup (fn [K V] [[map (Map K V)] [key K]] -> V (__map-get map key)))
        (let same (fn [T] [[left T] [right T]] -> Bool (= left right)))
        (let table (insert (empty (type-tag String) (type-tag Int)) :key 1))
        (artifact {:lookup (lookup table ":key") :table table :keyword (= :key ":key")
                   :equal (same table (insert (empty (type-tag String) (type-tag Int)) ":key" 1))})
    "#;
    assert_eq!(
        compile(source, &root).unwrap(),
        json!({"lookup":1,"table":{":key":1},"keyword":true,"equal":true})
    );
}

#[test]
fn equality_obligations_survive_forward_computed_function_bindings() {
    let root = workspace("equality-forward-computed");
    let source = r#"
        (let compare (as (Fn (Fn Int Int) (Fn Int Int) Bool) (if true eq eq)))
        (let eq (fn [T] [[a T] [b T]] -> Bool (= a b)))
        (let f (fn [[x Int]] -> Int x))
        (artifact (if true true (compare f f)))
    "#;
    assert!(compile(source, &root).is_err());
    assert_eq!(
        compile(
            r#"
        (let compare (as (Fn Int Int Bool) (if true eq eq)))
        (let eq (fn [T] [[a T] [b T]] -> Bool (= a b)))
        (artifact (compare 1 1))"#,
            &root
        )
        .unwrap(),
        json!(true)
    );
}
