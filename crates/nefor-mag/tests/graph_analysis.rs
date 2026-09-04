use nefor_mag::compile_file_with_inputs_and_module_roots;
use serde_json::{json, Value};
use std::fs;

fn workspace(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "nefor-mag-graph-analysis-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(path.join("core")).unwrap();
    path
}

fn contracts(extra: Value) -> Value {
    let mut values = vec![
        json!({
            "identity": "nefor.factory.source",
            "type_scheme": {
                "input_tags": ["mag.Unit"],
                "outputs": ["nefor.graph.Value"]
            }
        }),
        json!({
            "identity": "nefor.factory.output",
            "type_scheme": {
                "input_tags": ["nefor.graph.Value"],
                "outputs": ["nefor.graph.Value"]
            }
        }),
    ];
    values.extend(extra.as_array().unwrap().iter().cloned());
    json!({"factory_contracts": values})
}

fn run(name: &str, source: &str, inputs: Value) -> Value {
    let root = workspace(name);
    fs::write(root.join("main.mag"), source).unwrap();
    let mag_lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    compile_file_with_inputs_and_module_roots(&root, "main.mag", inputs, &[root.clone(), mag_lib])
        .unwrap()
}

fn run_error(name: &str, source: &str) -> String {
    let root = workspace(name);
    fs::write(root.join("main.mag"), source).unwrap();
    let mag_lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    compile_file_with_inputs_and_module_roots(
        &root,
        "main.mag",
        json!({}),
        &[root.clone(), mag_lib],
    )
    .unwrap_err()
    .to_string()
}

#[test]
fn analysis_preserves_normalized_first_occurrence_and_flattening_order() {
    let artifact = run(
        "ordered-analysis",
        r#"
          (require "core.validated")
          (require "nefor.graph")
          (require "nefor.mag")

          (let test-operation
            (fn [T] [[id String] [on (nefor.graph.Port T)]] -> nefor.mag.ProgramOperation
              (nefor.graph.instantiate-delta-template id on
                (as (Map String nefor.mag.TypedCapture) {})
                (as (List nefor.mag.Expression) [])
                (as nefor.mag.DeltaTemplate
                  {:types (as (Map String TypeDescriptor) {})
                   :actors [] :routes [] :messages [] :nodes []
                   :actor_reference_relocations []}))))

          (let validation-message
            (fn [[checked (core.validated.Validated String nefor.graph.Graph)]] -> String
              (match checked
                [(core.validated.Valid nefor.graph.Graph) accepted "valid"]
                [(core.validated.Invalid String) rejected
                  (first (get rejected "errors"))])))
          (let start-base
            (nefor.graph.source "z-start" (type-tag nefor.contracts.Text)
              (as nefor.contracts.Text {:content "start"})))
          (let middle-base
            (nefor.graph.identity "m-middle" (type-tag nefor.contracts.Text)))
          (let result
            (nefor.graph.output "a-result" (type-tag nefor.contracts.Text)))
          (let middle
            (nefor.graph.with-operation
              (nefor.graph.with-operation middle-base
                (test-operation "middle-1" (get middle-base "output")))
              (test-operation "middle-2" (get middle-base "output"))))
          (let start
            (nefor.graph.with-operation start-base
              (test-operation "start-1" (get start-base "output"))))
          (let first-edge (nefor.graph.edge start middle))
          (let second-edge (nefor.graph.edge middle result))
          (let topology (nefor.graph.graph [first-edge second-edge first-edge]))
          (let analysis (nefor.graph.analyze-graph topology))
          (let changed-middle
            (nefor.graph.with-operation middle
              (test-operation "changed" (get middle "output"))))
          (let conflict
            (nefor.graph.graph
              [first-edge (nefor.graph.edge start changed-middle) second-edge]))
          (let plain (nefor.graph.graph [first-edge second-edge]))
          (artifact
            {:edge-count (count (get analysis "edges"))
             :node-ids
               (map (fn [[candidate nefor.graph.StoredNode]] -> String
                      (get candidate "id"))
                    (get analysis "nodes"))
             :middle-occurrences
               (count (get (get analysis "nodes_by_id") "m-middle"))
             :actor-ids
               (map (fn [[candidate nefor.graph.Actor]] -> String
                      (get candidate "id"))
                    (get analysis "actors"))
             :message-targets
               (map (fn [[candidate nefor.graph.Message]] -> String
                      (get (get candidate "to") "actor"))
                    (get analysis "messages"))
             :rule-ids
               (map (fn [[candidate nefor.mag.ProgramOperation]] -> String
                      (get candidate "id"))
                    (get analysis "graph_operations"))
             :duplicate-lowers-identically
               (= (canonical (nefor.graph.lower topology))
                  (canonical (nefor.graph.lower plain)))
             :conflict-error
               (validation-message
                 (nefor.graph.validate conflict
                   (host-input "factory_contracts"
                     (type-tag (List nefor.graph.FactoryContract)))))} )
        "#,
        contracts(json!([])),
    );

    assert_eq!(artifact["edge-count"], 2);
    assert_eq!(
        artifact["node-ids"],
        json!(["m-middle", "a-result", "z-start"])
    );
    assert_eq!(artifact["middle-occurrences"], 2);
    assert_eq!(
        artifact["actor-ids"],
        json!(["m-middle", "a-result", "z-start"])
    );
    assert_eq!(artifact["message-targets"], json!(["z-start"]));
    assert_eq!(
        artifact["rule-ids"],
        json!(["middle-1", "middle-2", "start-1"])
    );
    assert_eq!(artifact["duplicate-lowers-identically"], true);
    assert_eq!(
        artifact["conflict-error"],
        "one node id must denote one immutable node definition"
    );
}

#[test]
fn route_assignment_sorts_product_buckets_but_lowers_original_route_order() {
    let artifact = run(
        "route-order",
        r#"
          (require "nefor.graph")

          (let start
            (nefor.graph.source "start" (type-tag nefor.contracts.Text)
              (as nefor.contracts.Text {:content "start"})))
          (let emitter-input
            (nefor.graph.port "emitter" (type-tag nefor.contracts.Text) "test.Value"))
          (let emitter-output
            (nefor.graph.port "emitter" (type-tag nefor.contracts.Text) "test.Value"))
          (let join-input
            (nefor.graph.port "join"
              (type-tag (+ nefor.contracts.Text nefor.contracts.Text)) "test.Value"))
          (let join-output
            (nefor.graph.port "join" (type-tag nefor.contracts.Text) "test.Value"))
          (let emitter
            (nefor.graph.actor "emitter" "test.actor"
              [(type-evidence (type-tag nefor.contracts.Text))] nil
              (nefor.graph.store-port emitter-input)
              [(nefor.graph.store-port emitter-output)]))
          (let join
            (nefor.graph.actor "join" "test.actor"
              [(type-evidence (type-tag nefor.contracts.Text))] nil
              (nefor.graph.store-port join-input)
              [(nefor.graph.store-port join-output)]))
          (let route-z
            (as nefor.graph.StoredRoute
              {:id "z-route"
               :from (nefor.graph.store-port emitter-output)
               :to (nefor.graph.store-port join-input)}))
          (let route-a
            (as nefor.graph.StoredRoute
              {:id "a-route"
               :from (nefor.graph.store-port emitter-output)
               :to (nefor.graph.store-port join-input)}))
          (let composite
            (nefor.graph.node "composite" "ordinary" [emitter join]
              [route-z route-a]
              (as (List nefor.graph.Message) [])
              emitter-input join-output))
          (let result
            (nefor.graph.output "result" (type-tag nefor.contracts.Text)))
          (let topology
            (nefor.graph.graph
              [(nefor.graph.edge start composite)
               (nefor.graph.edge composite result)]))
          (let analysis (nefor.graph.analyze-graph topology))
          (let input-key
            (nefor.graph.port-address-key (nefor.graph.store-port join-input)))
          (let sorted-bucket (get (get analysis "routes_by_input") input-key))
          (let lowered (nefor.graph.lower topology))
          (let lowered-emitter
            (first
              (filter (fn [[candidate nefor.graph.LowerActor]] -> Bool
                        (= (get candidate "id") "emitter"))
                      (get lowered "actors"))))
          (let destinations (get (get lowered-emitter "routes") "test.Value"))
          (let duplicate-first
            (as nefor.graph.StoredRoute
              {:id "duplicate"
               :from (nefor.graph.store-port emitter-output)
               :to (nefor.graph.store-port join-input)}))
          (let duplicate-same-input
            (as nefor.graph.StoredRoute
              {:id "duplicate"
               :from (nefor.graph.store-port join-output)
               :to (nefor.graph.store-port join-input)}))
          (let duplicate-other-input
            (as nefor.graph.StoredRoute
              {:id "duplicate"
               :from (nefor.graph.store-port join-output)
               :to (nefor.graph.store-port emitter-input)}))
          (let duplicate-assignments
            (nefor.graph.assigned-routes
              [duplicate-first duplicate-same-input duplicate-other-input]))
          (artifact
            {:bucket-ids
               (map (fn [[candidate nefor.graph.StoredRoute]] -> String
                      (get candidate "id"))
                    sorted-bucket)
             :lowered-edge-ids
               (map (fn [[candidate nefor.graph.LowerDestination]] -> String
                      (get candidate "edge_id"))
                    destinations)
             :positions
               (map (fn [[candidate nefor.graph.LowerDestination]] -> Int
                      (get candidate "product_position"))
                    destinations)
             :duplicate-source-actors
               (map (fn [[candidate nefor.graph.AssignedRoute]] -> String
                      (get (get (get candidate "route") "from") "actor"))
                    duplicate-assignments)
             :duplicate-positions
               (map (fn [[candidate nefor.graph.AssignedRoute]] -> Int
                      (get candidate "product_position"))
                    duplicate-assignments)})
        "#,
        json!({}),
    );

    assert_eq!(artifact["bucket-ids"], json!(["a-route", "z-route"]));
    assert_eq!(artifact["lowered-edge-ids"], json!(["z-route", "a-route"]));
    assert_eq!(artifact["positions"], json!([1, 0]));
    assert_eq!(
        artifact["duplicate-source-actors"],
        json!(["emitter", "emitter", "join"])
    );
    assert_eq!(artifact["duplicate-positions"], json!([0, 0, -1]));
}

#[test]
fn route_assignment_uses_each_same_address_destination_descriptor() {
    for (name, product_id, component_id) in [
        ("product-route-first", "a-product", "z-component"),
        ("component-route-first", "z-product", "a-component"),
    ] {
        let source = format!(
            r#"
              (require "nefor.graph")

              (type A {{:value Int}})
              (type B {{:value String}})

              (let source-a
                (nefor.graph.port "source-a" (type-tag A) "test.Value"))
              (let source-b
                (nefor.graph.port "source-b" (type-tag B) "test.Value"))
              (let product-input
                (nefor.graph.port "join" (type-tag (+ A B)) "test.Value"))
              (let component-input
                (nefor.graph.port "join" (type-tag B) "test.Value"))
              (let product-route
                (as nefor.graph.StoredRoute
                  {{:id "{product_id}"
                   :from (nefor.graph.store-port source-a)
                   :to (nefor.graph.store-port product-input)}}))
              (let component-route
                (as nefor.graph.StoredRoute
                  {{:id "{component_id}"
                   :from (nefor.graph.store-port source-b)
                   :to (nefor.graph.store-port component-input)}}))
              (artifact
                (nefor.graph.lower-delta
                  (nefor.graph.delta
                    (as (List nefor.graph.Actor) [])
                    [product-route component-route]
                    (as (List nefor.graph.Message) [])
                    (as (List String) []))))
            "#
        );

        let error = run_error(name, &source);
        assert!(
            error.contains("incoming edge types do not completely cover the input"),
            "{name}: {error}"
        );
    }
}

#[test]
fn indexed_reachability_handles_cycles_and_preserves_dead_path_diagnostics() {
    let artifact = run(
        "reachability",
        r#"
          (require "core.validated")
          (require "nefor.graph")
          (require "nefor.mag")

          (let test-operation
            (fn [T] [[id String] [on (nefor.graph.Port T)]] -> nefor.mag.ProgramOperation
              (nefor.graph.instantiate-delta-template id on
                (as (Map String nefor.mag.TypedCapture) {})
                (as (List nefor.mag.Expression) [])
                (as nefor.mag.DeltaTemplate
                  {:types (as (Map String TypeDescriptor) {})
                   :actors [] :routes [] :messages [] :nodes []
                   :actor_reference_relocations []}))))

          (let validation-message
            (fn [[checked (core.validated.Validated String nefor.graph.Graph)]] -> String
              (match checked
                [(core.validated.Valid nefor.graph.Graph) accepted "valid"]
                [(core.validated.Invalid String) rejected
                  (first (get rejected "errors"))])))
          (let contracts
            (host-input "factory_contracts"
              (type-tag (List nefor.graph.FactoryContract))))

          (let start
            (nefor.graph.source "start" (type-tag nefor.contracts.Text)
              (as nefor.contracts.Text {:content "start"})))
          (let cycle-input
            (nefor.graph.port "cycle-a"
              (type-tag (+ nefor.contracts.Text nefor.contracts.Text)) "test.Value"))
          (let cycle-output
            (nefor.graph.port "cycle-a" (type-tag nefor.contracts.Text) "test.Value"))
          (let cycle-actor
            (nefor.graph.actor "cycle-a" "test.cycle"
              [(type-evidence (type-tag nefor.contracts.Text))] nil
              (nefor.graph.store-port cycle-input)
              [(nefor.graph.store-port cycle-output)]))
          (let cycle-a
            (nefor.graph.node "cycle-a" "ordinary" [cycle-actor]
              (as (List nefor.graph.StoredRoute) [])
              (as (List nefor.graph.Message) []) cycle-input cycle-output))
          (let cycle-b
            (nefor.graph.identity "cycle-b" (type-tag nefor.contracts.Text)))
          (let result
            (nefor.graph.output "result" (type-tag nefor.contracts.Text)))
          (let reachable-cycle
            (nefor.graph.graph
              [(nefor.graph.edge start cycle-a)
               (nefor.graph.edge cycle-a cycle-b)
               (nefor.graph.edge cycle-b cycle-a)
               (nefor.graph.edge cycle-b result)]))

          (let orphan-a
            (nefor.graph.identity "orphan-a" (type-tag nefor.contracts.Text)))
          (let orphan-b
            (nefor.graph.identity "orphan-b" (type-tag nefor.contracts.Text)))
          (let disconnected-cycle
            (nefor.graph.graph
              [(nefor.graph.edge start result)
               (nefor.graph.edge orphan-a orphan-b)
               (nefor.graph.edge orphan-b orphan-a)]))

          (let branch-base
            (nefor.graph.identity "branch" (type-tag nefor.contracts.Text)))
          (let branch
            (nefor.graph.with-operation branch-base
              (test-operation "observe-branch"
                (get branch-base "output"))))
          (let dead-branch
            (nefor.graph.graph
              [(nefor.graph.edge start result)
               (nefor.graph.edge start branch)]))

          (artifact
            {:reachable-cycle
               (validation-message (nefor.graph.validate reachable-cycle contracts))
             :disconnected-cycle
               (validation-message (nefor.graph.validate disconnected-cycle contracts))
             :dead-branch
               (validation-message (nefor.graph.validate dead-branch contracts))})
        "#,
        contracts(json!([
            {
                "identity": "test.cycle",
                "type_scheme": {
                    "input_tags": ["test.Value"],
                    "outputs": ["test.Value"]
                }
            }
        ])),
    );

    assert_eq!(artifact["reachable-cycle"], "valid");
    assert_eq!(
        artifact["disconnected-cycle"],
        "every node must be reachable from a root node whose input accepts Unit"
    );
    assert_eq!(
        artifact["dead-branch"],
        "every node must have a path to the output node"
    );
}

#[test]
fn duplicate_precedence_contract_selection_and_sequence_order_are_stable() {
    let artifact = run(
        "duplicates-and-sequence",
        r#"
          (require "core.validated")
          (require "nefor.graph")
          (require "nefor.node")

          (let validation-message
            (fn [[checked (core.validated.Validated String nefor.graph.Graph)]] -> String
              (match checked
                [(core.validated.Valid nefor.graph.Graph) accepted "valid"]
                [(core.validated.Invalid String) rejected
                  (first (get rejected "errors"))])))
          (let start
            (nefor.graph.source "start" (type-tag nefor.contracts.Text)
              (as nefor.contracts.Text {:content "start"})))
          (let result
            (nefor.graph.output "result" (type-tag nefor.contracts.Text)))
          (let duplicated-result
            (nefor.graph.node "result" "output"
              (concat (get start "actors") (get result "actors"))
              (get result "routes") (get result "messages")
              (get result "input") (get result "output")))
          (let duplicate-actor-graph
            (nefor.graph.graph [(nefor.graph.edge start duplicated-result)]))
          (let no-output
            (nefor.graph.graph
              [(nefor.graph.edge start
                 (nefor.graph.identity "ordinary" (type-tag nefor.contracts.Text)))]))

          (let first-contract
            (nefor.graph.factory-contract
              {:identity "nefor.factory.source"
               :type_scheme {:input_tags ["wrong"]
                             :outputs ["nefor.graph.Value"]}}))
          (let second-contract
            (nefor.graph.factory-contract
              {:identity "nefor.factory.source"
               :type_scheme {:input_tags ["mag.Unit"]
                             :outputs ["nefor.graph.Value"]}}))
          (let output-contract
            (nefor.graph.factory-contract
              {:identity "nefor.factory.output"
               :type_scheme {:input_tags ["nefor.graph.Value"]
                             :outputs ["nefor.graph.Value"]}}))
          (let valid-graph
            (nefor.graph.graph [(nefor.graph.edge start result)]))

          (let first-child
            (nefor.graph.identity "first-child" (type-tag nefor.contracts.Text)))
          (let second-child
            (nefor.graph.identity "second-child" (type-tag nefor.contracts.Text)))
          (let sequence
            (nefor.node.sequence "ordered-sequence" [first-child second-child]))
          (let collector
            (first
              (filter (fn [[candidate nefor.graph.Actor]] -> Bool
                        (= (get candidate "id") "ordered-sequence.collector"))
                      (get sequence "actors"))))

          (artifact
            {:duplicate-actor
               (validation-message
                 (nefor.graph.validate duplicate-actor-graph
                   (host-input "factory_contracts"
                     (type-tag (List nefor.graph.FactoryContract)))))
             :no-output
               (validation-message
                 (nefor.graph.validate no-output
                   (host-input "factory_contracts"
                     (type-tag (List nefor.graph.FactoryContract)))))
             :first-contract
               (validation-message
                 (nefor.graph.validate valid-graph
                   [first-contract second-contract output-contract]))
             :sequence-actors
               (map (fn [[candidate nefor.graph.Actor]] -> String
                      (get candidate "id"))
                    (get sequence "actors"))
             :collector-params (get collector "params")})
        "#,
        contracts(json!([])),
    );

    assert_eq!(
        artifact["duplicate-actor"],
        "runtime actor ids must be unique across graph nodes"
    );
    assert_eq!(
        artifact["no-output"],
        "graph needs exactly one concrete output node"
    );
    assert!(artifact["first-contract"]
        .as_str()
        .unwrap()
        .contains("rejects input wire"));
    assert_eq!(
        artifact["sequence-actors"],
        json!([
            "ordered-sequence.input",
            "first-child",
            "second-child",
            "ordered-sequence.collector"
        ])
    );
    assert_eq!(
        artifact["collector-params"]["value"]["expected_senders"],
        json!(["first-child", "second-child"])
    );
}

#[test]
fn nefor_artifact_emits_exact_versioned_program_and_delta_envelopes() {
    let program = run(
        "program-envelope",
        r#"
          (require "nefor.artifact")
          (require "nefor.graph")
          (let start (nefor.graph.source "start" (type-tag nefor.contracts.Text)
            (as nefor.contracts.Text {:content "hello"})))
          (let result (nefor.graph.output "result" (type-tag nefor.contracts.Text)))
          (nefor.artifact.compile
            (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
              (nefor.graph.add-edges graph [(nefor.graph.edge start result)])))
        "#,
        contracts(json!([])),
    );
    assert_eq!(program["format"], "nefor.mag");
    assert_eq!(program["version"], 1);
    assert_eq!(program["kind"], "program");
    assert_eq!(program["program"]["operations"], json!([]));
    let initial = &program["program"]["initial"];
    assert!(initial.get("rules").is_none());
    assert!(initial.get("result").is_some());
    assert!(program.get("actors").is_none());

    let delta = run(
        "delta-envelope",
        r#"
          (require "nefor.artifact")
          (require "nefor.graph")
          (nefor.artifact.delta
            (nefor.graph.delta [] [] [] []))
        "#,
        json!({}),
    );
    assert_eq!(delta["format"], "nefor.mag");
    assert_eq!(delta["version"], 1);
    assert_eq!(delta["kind"], "delta");
    assert!(delta["delta"].get("result").is_none());
    assert!(delta["delta"].get("operations").is_none());
    assert!(delta["delta"].get("rules").is_none());
}
