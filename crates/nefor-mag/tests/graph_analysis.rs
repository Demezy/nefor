use nefor_mag::{
    compile_file_with_inputs_and_module_roots_and_options_and_syntax, CompilerOptions, SyntaxMode,
};
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
    fs::create_dir_all(root.join("nefor")).unwrap();
    fs::write(
        root.join("nefor/toolsets.json"),
        r#"{"read_only":[],"general":[]}"#,
    )
    .unwrap();
    fs::write(root.join("main.mag"), source).unwrap();
    let mag_lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    compile_file_with_inputs_and_module_roots_and_options_and_syntax(
        &root,
        "main.mag",
        inputs,
        &[root.clone(), mag_lib],
        CompilerOptions::default(),
        SyntaxMode::New,
    )
    .unwrap()
}

fn run_error(name: &str, source: &str) -> String {
    let root = workspace(name);
    fs::write(root.join("main.mag"), source).unwrap();
    let mag_lib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
    compile_file_with_inputs_and_module_roots_and_options_and_syntax(
        &root,
        "main.mag",
        json!({}),
        &[root.clone(), mag_lib],
        CompilerOptions::default(),
        SyntaxMode::New,
    )
    .unwrap_err()
    .to_string()
}

#[test]
fn analysis_preserves_normalized_first_occurrence_and_flattening_order() {
    let artifact = run(
        "ordered-analysis",
        r#"
import core.validated.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
import nefor.mag.{}

let test_operation<T>: fn(String, nefor.graph.Port<T>) -> nefor.mag.ProgramOperation = |id, on| =>
  nefor.graph.instantiate_delta_template(
    id,
    on,
    (core.map.empty(type_tag<String>()): Map<String, nefor.mag.TypedCapture>),
    ([]: List<nefor.mag.Expression>),
    nefor.mag.DeltaTemplate {
      types: (core.map.empty(type_tag<String>()): Map<String, TypeDescriptor>),
      actors: [], routes: [], messages: [], nodes: [], actor_reference_relocations: []
    },
  )

let validation_message: fn(core.validated.Validated<String, nefor.graph.Graph>) -> String = |checked| =>
  match checked {
    case Valid(accepted) => "valid",
    case Invalid(rejected) => first(get(rejected, "errors")),
  }
let start_base = nefor.graph.source("z-start", nefor.contracts.Text {content: "start"})
let middle_base = nefor.graph.identity("m-middle", type_tag<nefor.contracts.Text>())
let result = nefor.graph.output("a-result", type_tag<nefor.contracts.Text>())
let middle = nefor.graph.with_operation(
  nefor.graph.with_operation(middle_base, test_operation("middle-1", get(middle_base, "output"))),
  test_operation("middle-2", get(middle_base, "output")),
)
let start = nefor.graph.with_operation(start_base, test_operation("start-1", get(start_base, "output")))
let first_edge = nefor.graph.edge(start, middle)
let second_edge = nefor.graph.edge(middle, result)
let topology = nefor.graph.graph([first_edge, second_edge, first_edge])
let analysis = nefor.graph.analyze_graph(topology)
let changed_middle = nefor.graph.with_operation(middle, test_operation("changed", get(middle, "output")))
let conflict = nefor.graph.graph([first_edge, nefor.graph.edge(start, changed_middle), second_edge])
let plain = nefor.graph.graph([first_edge, second_edge])
let stored_node_id: fn(nefor.graph.StoredNode) -> String = |candidate| => get(candidate, "id")
let actor_id: fn(nefor.graph.Actor) -> String = |candidate| => get(candidate, "id")
let message_target: fn(nefor.graph.Message) -> String = |candidate| => get(get(candidate, "to"), "actor")
let operation_id: fn(nefor.mag.ProgramOperation) -> String = |candidate| => get(candidate, "id")
artifact {
  edge_count: count(get(analysis, "edges")),
  node_ids: map(stored_node_id, get(analysis, "nodes")),
  middle_occurrences: count(core.map.get(get(analysis, "nodes_by_id"), "m-middle")),
  actor_ids: map(actor_id, get(analysis, "actors")),
  message_targets: map(message_target, get(analysis, "messages")),
  rule_ids: map(operation_id, get(analysis, "graph_operations")),
  duplicate_lowers_identically: (=)(canonical(nefor.graph.lower(topology)), canonical(nefor.graph.lower(plain))),
  conflict_error: validation_message(nefor.graph.validate(conflict, host_input("factory_contracts", type_tag<List<nefor.graph.FactoryContract>>())))
}
"#,
        contracts(json!([])),
    );

    assert_eq!(artifact["edge_count"], 2);
    assert_eq!(
        artifact["node_ids"],
        json!(["z-start", "m-middle", "a-result"])
    );
    assert_eq!(artifact["middle_occurrences"], 2);
    assert_eq!(
        artifact["actor_ids"],
        json!(["z-start", "m-middle", "a-result"])
    );
    assert_eq!(artifact["message_targets"], json!(["z-start"]));
    assert_eq!(
        artifact["rule_ids"],
        json!(["start-1", "middle-1", "middle-2"])
    );
    assert_eq!(artifact["duplicate_lowers_identically"], true);
    assert_eq!(
        artifact["conflict_error"],
        "one node id must denote one immutable node definition"
    );
}

#[test]
fn route_assignment_sorts_product_buckets_but_lowers_original_route_order() {
    let artifact = run(
        "route-order",
        r#"
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}

let start = nefor.graph.source("start", nefor.contracts.Text {content: "start"})
let emitter_input = nefor.graph.port("emitter", type_tag<nefor.contracts.Text>(), "test.Value")
let emitter_output = nefor.graph.port("emitter", type_tag<nefor.contracts.Text>(), "test.Value")
let join_input = nefor.graph.port("join", type_tag<(nefor.contracts.Text, nefor.contracts.Text)>(), "test.Value")
let join_output = nefor.graph.port("join", type_tag<nefor.contracts.Text>(), "test.Value")
let emitter = nefor.graph.actor("emitter", "test.actor", [type_evidence(type_tag<nefor.contracts.Text>())], (), nefor.graph.store_port(emitter_input), [nefor.graph.store_port(emitter_output)])
let join = nefor.graph.actor("join", "test.actor", [type_evidence(type_tag<nefor.contracts.Text>())], (), nefor.graph.store_port(join_input), [nefor.graph.store_port(join_output)])
let route_z = nefor.graph.StoredRoute {id: "z-route", from: nefor.graph.store_port(emitter_output), to: nefor.graph.store_port(join_input)}
let route_a = nefor.graph.StoredRoute {id: "a-route", from: nefor.graph.store_port(emitter_output), to: nefor.graph.store_port(join_input)}
let composite = nefor.graph.node("composite", "ordinary", [emitter, join], [route_z, route_a], ([]: List<nefor.graph.Message>), emitter_input, join_output)
let result = nefor.graph.output("result", type_tag<nefor.contracts.Text>())
let topology = nefor.graph.graph([nefor.graph.edge(start, composite), nefor.graph.edge(composite, result)])
let analysis = nefor.graph.analyze_graph(topology)
let input_key = nefor.graph.port_address_key(nefor.graph.store_port(join_input))
let sorted_bucket = core.map.get(get(analysis, "routes_by_input"), input_key)
let lowered = nefor.graph.lower(topology)
let is_emitter: fn(nefor.graph.LowerActor) -> Bool = |candidate| => (=)(get(candidate, "id"), "emitter")
let lowered_emitter = first(filter(is_emitter, get(lowered, "actors")))
let destinations = core.map.get(get(lowered_emitter, "routes"), "test.Value")
let duplicate_first = nefor.graph.StoredRoute {id: "duplicate", from: nefor.graph.store_port(emitter_output), to: nefor.graph.store_port(join_input)}
let duplicate_same_input = nefor.graph.StoredRoute {id: "duplicate", from: nefor.graph.store_port(join_output), to: nefor.graph.store_port(join_input)}
let duplicate_other_input = nefor.graph.StoredRoute {id: "duplicate", from: nefor.graph.store_port(join_output), to: nefor.graph.store_port(emitter_input)}
let duplicate_assignments = nefor.graph.assigned_routes([duplicate_first, duplicate_same_input, duplicate_other_input])
let route_id: fn(nefor.graph.StoredRoute) -> String = |candidate| => get(candidate, "id")
let destination_id: fn(nefor.graph.LowerDestination) -> String = |candidate| => get(candidate, "edge_id")
let destination_position: fn(nefor.graph.LowerDestination) -> Int = |candidate| => get(candidate, "product_position")
let assignment_actor: fn(nefor.graph.AssignedRoute) -> String = |candidate| => get(get(get(candidate, "route"), "from"), "actor")
let assignment_position: fn(nefor.graph.AssignedRoute) -> Int = |candidate| => get(candidate, "product_position")
artifact {
  bucket_ids: map(route_id, sorted_bucket),
  lowered_edge_ids: map(destination_id, destinations),
  positions: map(destination_position, destinations),
  duplicate_source_actors: map(assignment_actor, duplicate_assignments),
  duplicate_positions: map(assignment_position, duplicate_assignments)
}
"#,
        json!({}),
    );

    assert_eq!(artifact["bucket_ids"], json!(["a-route", "z-route"]));
    assert_eq!(artifact["lowered_edge_ids"], json!(["z-route", "a-route"]));
    assert_eq!(artifact["positions"], json!([1, 0]));
    assert_eq!(
        artifact["duplicate_source_actors"],
        json!(["emitter", "emitter", "join"])
    );
    assert_eq!(artifact["duplicate_positions"], json!([0, 0, -1]));
}

#[test]
fn route_assignment_uses_each_same_address_destination_descriptor() {
    for (name, product_id, component_id) in [
        ("product-route-first", "a-product", "z-component"),
        ("component-route-first", "z-product", "a-component"),
    ] {
        let source = format!(
            r#"
import nefor.graph.{{}}

type A {{value: Int}}
type B {{value: String}}

let source_a = nefor.graph.port("source-a", type_tag<A>(), "test.Value")
let source_b = nefor.graph.port("source-b", type_tag<B>(), "test.Value")
let product_input = nefor.graph.port("join", type_tag<(A, B)>(), "test.Value")
let component_input = nefor.graph.port("join", type_tag<B>(), "test.Value")
let product_route = nefor.graph.StoredRoute {{
  id: "{product_id}",
  from: nefor.graph.store_port(source_a),
  to: nefor.graph.store_port(product_input)
}}
let component_route = nefor.graph.StoredRoute {{
  id: "{component_id}",
  from: nefor.graph.store_port(source_b),
  to: nefor.graph.store_port(component_input)
}}
artifact(nefor.graph.lower_delta(nefor.graph.delta(
  ([]: List<nefor.graph.Actor>),
  [product_route, component_route],
  ([]: List<nefor.graph.Message>),
  ([]: List<String>),
)))
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
import core.validated.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
import nefor.mag.{}

let test_operation<T>: fn(String, nefor.graph.Port<T>) -> nefor.mag.ProgramOperation = |id, on| =>
  nefor.graph.instantiate_delta_template(id, on,
    (core.map.empty(type_tag<String>()): Map<String, nefor.mag.TypedCapture>),
    ([]: List<nefor.mag.Expression>),
    nefor.mag.DeltaTemplate {
      types: (core.map.empty(type_tag<String>()): Map<String, TypeDescriptor>),
      actors: [], routes: [], messages: [], nodes: [], actor_reference_relocations: []
    })
let validation_message: fn(core.validated.Validated<String, nefor.graph.Graph>) -> String = |checked| =>
  match checked {
    case Valid(accepted) => "valid",
    case Invalid(rejected) => first(get(rejected, "errors")),
  }
let contracts = host_input("factory_contracts", type_tag<List<nefor.graph.FactoryContract>>())
let start = nefor.graph.source("start", nefor.contracts.Text {content: "start"})
let cycle_input = nefor.graph.port("cycle-a", type_tag<(nefor.contracts.Text, nefor.contracts.Text)>(), "test.Value")
let cycle_output = nefor.graph.port("cycle-a", type_tag<nefor.contracts.Text>(), "test.Value")
let cycle_actor = nefor.graph.actor("cycle-a", "test.cycle", [type_evidence(type_tag<nefor.contracts.Text>())], (), nefor.graph.store_port(cycle_input), [nefor.graph.store_port(cycle_output)])
let cycle_a = nefor.graph.node("cycle-a", "ordinary", [cycle_actor], ([]: List<nefor.graph.StoredRoute>), ([]: List<nefor.graph.Message>), cycle_input, cycle_output)
let cycle_b = nefor.graph.identity("cycle-b", type_tag<nefor.contracts.Text>())
let result = nefor.graph.output("result", type_tag<nefor.contracts.Text>())
let reachable_cycle = nefor.graph.graph([nefor.graph.edge(start, cycle_a), nefor.graph.edge(cycle_a, cycle_b), nefor.graph.edge(cycle_b, cycle_a), nefor.graph.edge(cycle_b, result)])
let orphan_a = nefor.graph.identity("orphan-a", type_tag<nefor.contracts.Text>())
let orphan_b = nefor.graph.identity("orphan-b", type_tag<nefor.contracts.Text>())
let disconnected_cycle = nefor.graph.graph([nefor.graph.edge(start, result), nefor.graph.edge(orphan_a, orphan_b), nefor.graph.edge(orphan_b, orphan_a)])
let branch_base = nefor.graph.identity("branch", type_tag<nefor.contracts.Text>())
let branch = nefor.graph.with_operation(branch_base, test_operation("observe-branch", get(branch_base, "output")))
let dead_branch = nefor.graph.graph([nefor.graph.edge(start, result), nefor.graph.edge(start, branch)])
artifact {
  reachable_cycle: validation_message(nefor.graph.validate(reachable_cycle, contracts)),
  disconnected_cycle: validation_message(nefor.graph.validate(disconnected_cycle, contracts)),
  dead_branch: validation_message(nefor.graph.validate(dead_branch, contracts))
}
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

    assert_eq!(artifact["reachable_cycle"], "valid");
    assert_eq!(
        artifact["disconnected_cycle"],
        "every node must be reachable from a root node whose input accepts Unit"
    );
    assert_eq!(
        artifact["dead_branch"],
        "every node must have a path to the output node"
    );
}

#[test]
fn duplicate_precedence_contract_selection_and_sequence_order_are_stable() {
    let artifact = run(
        "duplicates-and-sequence",
        r#"
import core.validated.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
import nefor.node.{}

let validation_message: fn(core.validated.Validated<String, nefor.graph.Graph>) -> String = |checked| =>
  match checked {
    case Valid(accepted) => "valid",
    case Invalid(rejected) => first(get(rejected, "errors")),
  }
let start = nefor.graph.source("start", nefor.contracts.Text {content: "start"})
let result = nefor.graph.output("result", type_tag<nefor.contracts.Text>())
let duplicated_result = nefor.graph.node("result", "output", concat(get(start, "actors"), get(result, "actors")), get(result, "routes"), get(result, "messages"), get(result, "input"), get(result, "output"))
let duplicate_actor_graph = nefor.graph.graph([nefor.graph.edge(start, duplicated_result)])
let no_output = nefor.graph.graph([nefor.graph.edge(start, nefor.graph.identity("ordinary", type_tag<nefor.contracts.Text>()))])
let first_contract = nefor.graph.FactoryContract {identity: "nefor.factory.source", type_scheme: nefor.graph.FactoryTypeScheme {input_tags: ["wrong"], outputs: ["nefor.graph.Value"]}}
let second_contract = nefor.graph.FactoryContract {identity: "nefor.factory.source", type_scheme: nefor.graph.FactoryTypeScheme {input_tags: ["mag.Unit"], outputs: ["nefor.graph.Value"]}}
let output_contract = nefor.graph.FactoryContract {identity: "nefor.factory.output", type_scheme: nefor.graph.FactoryTypeScheme {input_tags: ["nefor.graph.Value"], outputs: ["nefor.graph.Value"]}}
let valid_graph = nefor.graph.graph([nefor.graph.edge(start, result)])
let first_child = nefor.graph.source("first-child", nefor.contracts.Text {content: "first"})
let second_child = nefor.graph.source("second-child", nefor.contracts.Text {content: "second"})
let sequence = nefor.node.sequence([first_child, second_child])
let is_collector: fn(nefor.graph.Actor) -> Bool = |candidate| => (=)(get(candidate, "id"), str(get(sequence, "id"), ".collector"))
let collector = first(filter(is_collector, get(sequence, "actors")))
let actor_id: fn(nefor.graph.Actor) -> String = |candidate| => get(candidate, "id")
artifact {
  duplicate_actor: validation_message(nefor.graph.validate(duplicate_actor_graph, host_input("factory_contracts", type_tag<List<nefor.graph.FactoryContract>>()))),
  no_output: validation_message(nefor.graph.validate(no_output, host_input("factory_contracts", type_tag<List<nefor.graph.FactoryContract>>()))),
  first_contract: validation_message(nefor.graph.validate(valid_graph, [first_contract, second_contract, output_contract])),
  sequence_id: get(sequence, "id"),
  sequence_actors: map(actor_id, get(sequence, "actors")),
  collector_params: get(collector, "params")
}
"#,
        contracts(json!([])),
    );

    assert_eq!(
        artifact["duplicate_actor"],
        "runtime actor ids must be unique across graph nodes"
    );
    assert_eq!(
        artifact["no_output"],
        "graph needs exactly one concrete output node"
    );
    assert!(artifact["first_contract"]
        .as_str()
        .unwrap()
        .contains("rejects input wire"));
    let sequence_id = artifact["sequence_id"].as_str().unwrap();
    assert_eq!(
        artifact["sequence_actors"],
        json!([
            format!("{sequence_id}.input"),
            "first-child",
            "second-child",
            format!("{sequence_id}.collector")
        ])
    );
    assert_eq!(
        artifact["collector_params"]["value"]["expected_senders"],
        json!(["first-child", "second-child"])
    );
}

#[test]
fn nefor_artifact_emits_exact_versioned_program_and_delta_envelopes() {
    let program = run(
        "program-envelope",
        r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
let start = nefor.graph.source("start", nefor.contracts.Text {content: "hello"})
let result = nefor.graph.output("result", type_tag<nefor.contracts.Text>())
let close: fn(nefor.graph.Graph) -> nefor.graph.Graph = |graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, result)])
nefor.artifact.compile(close)
"#,
        contracts(json!([])),
    );
    assert_eq!(program["format"], "nefor.mag");
    assert_eq!(program["version"], 2);
    assert_eq!(program["kind"], "program");
    assert_eq!(program["program"]["operations"], json!([]));
    let initial = &program["program"]["initial"];
    assert!(initial.get("rules").is_none());
    assert!(initial.get("result").is_some());
    assert!(program.get("actors").is_none());

    let delta = run(
        "delta-envelope",
        r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
nefor.artifact.delta(nefor.graph.delta([], [], [], []))
"#,
        json!({}),
    );
    assert_eq!(delta["format"], "nefor.mag");
    assert_eq!(delta["version"], 2);
    assert_eq!(delta["kind"], "delta");
    assert!(delta["delta"].get("result").is_none());
    assert!(delta["delta"].get("operations").is_none());
    assert!(delta["delta"].get("rules").is_none());
}

#[test]
fn compile_graph_closes_one_terminal_through_existing_graph_values() {
    let explicit = run(
        "compile-graph-explicit",
        r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
let start = nefor.graph.source("start", "hello")
let result = nefor.graph.output_for("result", start)
let close: fn(nefor.graph.Graph) -> nefor.graph.Graph = |graph| =>
  nefor.graph.add_edges(graph, [nefor.graph.edge(start, result)])
nefor.artifact.compile(close)
"#,
        contracts(json!([])),
    );
    let convenient = run(
        "compile-graph-convenient",
        r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
let start = nefor.graph.source("start", "hello")
nefor.artifact.compile_graph(start)
"#,
        contracts(json!([])),
    );

    assert_eq!(convenient, explicit);
    assert_eq!(convenient["format"], "nefor.mag");
    assert_eq!(convenient["version"], 2);
    assert_eq!(convenient["kind"], "program");
    assert_eq!(
        convenient["program"]["initial"]["actors"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        convenient["program"]["initial"]["result"]["from"]["actor"],
        "result"
    );

    let collision = run(
        "compile-graph-collision",
        r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
let result = nefor.graph.source("result", "hello")
nefor.artifact.compile_graph(result)
"#,
        contracts(json!([])),
    );
    assert_eq!(
        collision["program"]["initial"]["result"]["from"]["actor"],
        "result-"
    );
}

#[test]
fn exhaustive_nefor_facade_supports_the_one_import_shell_program() {
    let artifact = run(
        "nefor-facade-shell",
        r#"
import nefor
compile_graph(script("hello", ShellScriptParams {script: "printf 'hello\\n'", cwd: ".", timeout: timeout_ms(30000)}))
"#,
        contracts(json!([
            {"identity": "nefor.factory.shell-script", "type_scheme": {"input_tags": ["nefor.process.Input"], "outputs": ["nefor.process.Result"]}},
            {"identity": "nefor.factory.output", "type_scheme": {"input_tags": ["nefor.graph.Value"], "outputs": ["nefor.graph.Value"]}}
        ])),
    );

    assert_eq!(artifact["program"]["initial"]["actors"][0]["id"], "hello");
    assert_eq!(
        artifact["program"]["initial"]["actors"][0]["params"]["value"]["timeout"]["milliseconds"],
        30000
    );

    let mapped = run(
        "nefor-facade-map-error",
        r#"
import core.types.{}
import nefor
let input = identity("input", type_tag<core.types.Result<String, Int>>())
let mapper = identity("mapper", type_tag<String>())
artifact(get(map_error(input, mapper), "id"))
"#,
        json!({}),
    );
    assert_eq!(
        mapped,
        json!("nefor.node.composite:5:input16:result.map_error6:mapper")
    );
}

#[test]
fn only_exact_unit_ports_are_root_capable() {
    let artifact = run(
        "exact-unit-root",
        r#"
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
type Maybe = Ready(Unit) | Waiting(String)
let exact = nefor.graph.store_port(nefor.graph.port("exact", type_tag<Unit>(), "wire"))
let sum = nefor.graph.store_port(nefor.graph.port("sum", type_tag<Maybe>(), "wire"))
let product = nefor.graph.store_port(nefor.graph.port("product", type_tag<(Unit, String)>(), "wire"))
artifact {exact: nefor.graph.unit_input(exact), sum: nefor.graph.unit_input(sum), product: nefor.graph.unit_input(product)}
"#,
        json!({}),
    );
    assert_eq!(
        artifact,
        json!({"exact": true, "sum": false, "product": false})
    );
}

#[test]
fn sequence_requires_a_nonempty_compile_time_list() {
    let error = run_error(
        "empty-structural-sequence",
        r#"
import nefor.graph.{}
import nefor.node.{}
let empty = nefor.node.sequence(([]: List<nefor.graph.Node<String, Int>>))
artifact(get(empty, "id"))
"#,
    );
    assert!(error.contains("first expects a non-empty List"), "{error}");
}

#[test]
fn exact_unit_root_cache_preserves_actual_activation_evidence() {
    use nefor_mag::project_cache::{build_with_syntax, CachePolicy, CacheStatus};
    let root = workspace("exact-unit-cache");
    let roots = [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib")];
    fs::write(
        root.join("main.mag"),
        r#"
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
import nefor.artifact.{}
type Arm {text: String}
let start = nefor.graph.identity("start", type_tag<Unit>())
let result = nefor.graph.output_for("result", start)
let close: fn(nefor.graph.Graph) -> nefor.graph.Graph = |g| => nefor.graph.add_edges(g, [nefor.graph.edge(start, result)])
nefor.artifact.compile(close)
"#,
    )
    .unwrap();
    let compile = |policy| {
        build_with_syntax(
            nefor_mag::FileCompileRequest {
                source_dir: &root,
                entry: "main.mag",
                inputs: contracts(json!([])),
                module_roots: &roots,
                options: Default::default(),
            },
            1,
            policy,
            None,
            SyntaxMode::New,
        )
        .unwrap()
    };
    let miss = compile(CachePolicy::Use);
    let hit = compile(CachePolicy::Use);
    let bypass = compile(CachePolicy::Bypass);
    assert!(matches!(miss.cache.status, CacheStatus::Miss));
    assert!(matches!(hit.cache.status, CacheStatus::Hit));
    assert!(matches!(bypass.cache.status, CacheStatus::Bypass));
    assert_eq!(miss.bytes, hit.bytes);
    assert_eq!(miss.bytes, bypass.bytes);
    let artifact: Value = serde_json::from_slice(&hit.bytes).unwrap();
    let initial = &artifact["program"]["initial"];
    assert_eq!(initial["messages"].as_array().unwrap().len(), 1);
    let message = &initial["messages"][0];
    assert_eq!(
        message["semantic_type"],
        json!({"kind":"primitive","name":"Unit"})
    );
    assert_eq!(message["content"]["value"]["value"], Value::Null);
    assert_eq!(
        initial["types"][message["semantic_type_id"].as_str().unwrap()],
        message["semantic_type"]
    );
    assert_eq!(initial["actors"][0]["input"]["type"]["kind"], "primitive");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn delta_bootstrap_uses_exact_unit_and_full_input_address() {
    let artifact = run(
        "delta-bootstrap-address",
        r#"
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
type Arm {text: String}
let unit = nefor.graph.identity("unit", type_tag<Unit>())
let sum = nefor.graph.identity("sum", type_tag<Unit>())
let base = nefor.graph.merge_delta(nefor.graph.node_delta(unit), nefor.graph.node_delta(sum))
let actual = get(unit, "input")
let unrelated = nefor.graph.port("unit", type_tag<Unit>(), "other-wire")
let routed = nefor.graph.delta_route(base, nefor.graph.port("external", type_tag<Unit>(), "out"), actual)
artifact {
  base: nefor.graph.lower_delta(base),
  authored: nefor.graph.lower_delta(nefor.graph.delta_message(base, actual, ())),
  unrelated: nefor.graph.lower_delta(nefor.graph.delta_message(base, unrelated, ())),
  routed: nefor.graph.lower_delta(routed)
}
"#,
        json!({}),
    );
    assert_eq!(artifact["base"]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(artifact["base"]["messages"][0]["to"], "unit");
    assert_eq!(
        artifact["authored"]["messages"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        artifact["unrelated"]["messages"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        artifact["unrelated"]["messages"][2]["content"]["value"]["kind"],
        "nefor.graph.Value"
    );
    assert_eq!(artifact["routed"]["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn indexed_output_diagnostics_include_explicit_operations_and_each_port() {
    let artifact = run(
        "operation-diagnostics",
        r#"
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
import nefor.mag.{}
import core.validated.{}
type Arm {text: String}
type Other {number: Int}
type EmptyParams {}
let input = nefor.graph.port("worker", type_tag<Unit>(), "in")
let one = nefor.graph.port("worker", type_tag<Arm>(), "one")
let two = nefor.graph.port("worker", type_tag<Arm>(), "two")
let worker = nefor.graph.actor("worker", "custom", [], EmptyParams {}, nefor.graph.store_port(input), [nefor.graph.store_port(one), nefor.graph.store_port(two)])
let node = nefor.graph.node("worker", "ordinary", [worker], [], [], input, one)
let result = nefor.graph.output("result", type_tag<Arm>())
let graph = nefor.graph.graph([nefor.graph.edge(node, result)])
let on = nefor.graph.port("worker", type_tag<Arm>(), "one")
let operation = nefor.graph.instantiate_delta_template("explicit-arm", on,
  (core.map.empty(type_tag<String>()): Map<String, nefor.mag.TypedCapture>), [],
  nefor.mag.DeltaTemplate {
    types: (core.map.empty(type_tag<String>()): Map<String, TypeDescriptor>),
    actors: [], routes: [], messages: [], nodes: [], actor_reference_relocations: []
  })
let validation = nefor.graph.validate_with_operations(graph, [operation], [])
artifact(match validation {
  case Invalid(invalid) => get(invalid, "errors"),
  case Valid(valid) => [],
})
"#,
        json!({}),
    );
    let errors = artifact.as_array().unwrap();
    assert_eq!(errors.len(), 1, "{artifact}");
    let details: Vec<Value> = errors
        .iter()
        .map(|error| {
            serde_json::from_str(
                error
                    .as_str()
                    .unwrap()
                    .strip_prefix("output coverage failed: ")
                    .unwrap(),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(details[0]["output"], "worker.two");
    assert_eq!(details[0]["available_handlers"], json!([]));
    let handlers = details[0]["available_handlers"].as_array().unwrap();
    assert_eq!(handlers.len(), 0);
}

#[test]
fn input_diagnostics_keep_raw_occurrences_and_actual_message_evidence() {
    let artifact = run(
        "input-diagnostic-order",
        r#"
import nefor.graph.{}
import nefor.contracts.{}
import core.map.{}
let start = nefor.graph.source("start", ())
let target = nefor.graph.identity("target", type_tag<(Unit, Unit)>())
let first_route = (assoc(nefor.graph.stored_route(nefor.graph.port("raw-first", type_tag<Unit>(), "out"), get(target, "input")), "id", "z"): nefor.graph.StoredRoute)
let second = (assoc(nefor.graph.stored_route(nefor.graph.port("raw-second", type_tag<Unit>(), "out"), get(target, "input")), "id", "a"): nefor.graph.StoredRoute)
let third = (assoc(second, "id", "b"): nefor.graph.StoredRoute)
let node = nefor.graph.node_with_operations_and_nodes("wrapper", "ordinary", get(target, "actors"), [first_route, second, third], [], [], get(target, "nodes"), get(target, "input"), get(target, "output"))
let graph = nefor.graph.graph([nefor.graph.edge(start, node)])
let analysis = nefor.graph.analyze_graph(graph)
let actor = first(get(target, "actors"))
let input = get(actor, "input")
let message = (assoc(nefor.graph.stored_message(input, nefor.graph.MessageContent<Unit> {kind: "nefor.graph.Value", value: ()}), "semantic_type", type_evidence(type_tag<Unit>())): nefor.graph.Message)
let with_message = (assoc(analysis, "messages_by_input", core.map.put(get(analysis, "messages_by_input"), nefor.graph.port_address_key(input), [message])): nefor.graph.GraphAnalysis)
let route_id: fn(nefor.graph.StoredRoute) -> String = |r| => get(r, "id")
artifact {
  sources: nefor.graph.actor_input_sources(with_message, actor),
  covered: nefor.graph.actor_input_coverage_valid(with_message, actor),
  assignment_order: map(route_id, core.map.get_or(get(analysis, "routes_by_input"), nefor.graph.port_address_key(input), ([]: List<nefor.graph.StoredRoute>)))
}
"#,
        json!({}),
    );
    let sources = artifact["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 5);
    for (index, expected) in [
        "raw-first.out",
        "raw-second.out",
        "raw-second.out",
        "start.nefor.graph.Value",
    ]
    .iter()
    .enumerate()
    {
        let source: Value = serde_json::from_str(sources[index].as_str().unwrap()).unwrap();
        assert_eq!(source["from"], *expected);
    }
    assert_eq!(
        sources[4],
        "initial message type {\"kind\":\"primitive\",\"name\":\"Unit\"}"
    );
    assert_eq!(artifact["covered"], false);
    let order = artifact["assignment_order"].as_array().unwrap();
    assert_eq!(&order[..3], &[json!("a"), json!("b"), json!("z")]);
}

#[test]
fn core_nefor_api_uses_inferred_evidence_explicit_ids_and_unit_sequences() {
    let artifact = run(
        "core-nefor-api-reshape",
        r#"
import core.map.{}
import core.types.{}
import nefor.actors.{}
import nefor.contracts.{}
import nefor.dynamic.{}
import nefor.graph.{}
import nefor.node.{}
import nefor.result.{}

type Input {value: Int}
type Output {value: String}

let source = nefor.graph.source("source", Input {value: 1})
let captured = nefor.dynamic.capture_ref(Input {value: 2})
let int_identity = nefor.graph.identity("int", type_tag<Int>())
let int_identity_2 = nefor.graph.identity("int-2", type_tag<Int>())
let string_identity = nefor.graph.identity("string", type_tag<String>())
let unit_source = nefor.graph.source("unit-source", nil)
let explicit_compose = nefor.node.compose("compose", int_identity, int_identity_2)
let operator_compose = nefor.node.`>>>`(int_identity, int_identity_2)
let explicit_then = nefor.node.then("then", int_identity, unit_source)
let operator_then = nefor.node.`*>`(int_identity, unit_source)
let operator_before = nefor.node.`<*`(int_identity, unit_source)
let explicit_fanout = nefor.node.fanout("fanout", int_identity, int_identity_2)
let operator_fanout = nefor.node.`&&&`(int_identity, int_identity_2)
let explicit_parallel = nefor.node.parallel("parallel", int_identity, string_identity)
let operator_parallel = nefor.node.`***`(int_identity, string_identity)
let explicit_choose = nefor.node.choose("choose", int_identity, string_identity)
let operator_choose = nefor.node.`+++`(int_identity, string_identity)
let first_source = nefor.graph.source("first", 1)
let second_source = nefor.graph.source("second", 2)
let nonempty_sequence = nefor.node.sequence([first_source, second_source])
let repeated_sequence = nefor.node.sequence([first_source, second_source])
let reordered_sequence = nefor.node.sequence([second_source, first_source])
let collision_sequence_left = nefor.node.sequence([nefor.graph.source("a", 1), nefor.graph.source("bsequencec", 2)])
let collision_sequence_right = nefor.node.sequence([nefor.graph.source("asequenceb", 1), nefor.graph.source("c", 2)])
let empty_sequence = nefor.node.sequence_empty("empty-sequence", type_tag<Unit>(), type_tag<Int>())

let result_input = nefor.graph.identity("result-input", type_tag<core.types.Result<String, Int>>())
let result_value = nefor.graph.identity("result-value", type_tag<Int>())
let lifted = nefor.result.lift("lifted", type_tag<String>(), result_value)
let mapped = nefor.result.map(result_input, result_value)
let error_mapper = nefor.node.discard("error-mapper", type_tag<String>())
let mapped_error = nefor.result.map_error(result_input, error_mapper)
let bound = nefor.result.and_then("bound", result_input, lifted)
let operator_bound = nefor.result.`>=>`(result_input, lifted)
let colliding_left = nefor.node.composite_id("a&&&b", "&&&", "c")
let colliding_right = nefor.node.composite_id("a", "&&&", "b&&&c")

let policy = nefor.contracts.tool_approval_policy(core.map.insert((core.map.empty(type_tag<String>()): Map<String, String>), "bash", "deny"))
let no_policy = nefor.contracts.no_tool_approval_policy()
let resolve_model: fn(String) -> nefor.actors.AuthoredModel = |model| => named(nefor.actors.AuthoredModel, ResolvedModel, nefor.actors.ResolvedModel {provider: "test", model: model, reasoning_effort: nefor.actors.no_reasoning_effort})
let agent = nefor.actors.agent("agent", resolve_model, nefor.actors.AgentConfig<String> {model: "mock", system: "test", tools: [], tool_approval_policy: policy, max_corrections: 1}, type_tag<Input>(), type_tag<Output>())
let dynamic_agent = nefor.actors.dynamic_agent("dynamic-agent", resolve_model, nefor.actors.AgentConfig<String> {model: "mock", system: "test", tools: [], tool_approval_policy: no_policy, max_corrections: 1}, type_tag<Input>(), type_tag<Output>())
let approval = nefor.actors.approval_gate("approval", nefor.actors.ApprovalConfig {prompt: "Approve?"})
let retry = nefor.actors.retry_gate("retry", nefor.actors.RetryGateConfig {max_retries: 2}, type_tag<Input>())
let run_tool_actor = first(filter(((|candidate| => (=)(get(candidate, "id"), "agent.run-tool")): fn(nefor.graph.Actor) -> Bool), get(agent, "actors")))

artifact {
  source_type: type_id(type_evidence(get(get(source, "output"), "type"))),
  capture_type: type_id(type_evidence(get(captured, "type"))),
  compose_ids: [get(explicit_compose, "id"), get(operator_compose, "id")],
  then_ids: [get(explicit_then, "id"), get(operator_then, "id"), get(operator_before, "id")],
  fanout_ids: [get(explicit_fanout, "id"), get(operator_fanout, "id")],
  parallel_ids: [get(explicit_parallel, "id"), get(operator_parallel, "id")],
  choose_ids: [get(explicit_choose, "id"), get(operator_choose, "id")],
  sequence_inputs: [type_id(type_evidence(get(get(nonempty_sequence, "input"), "type"))), type_id(type_evidence(get(get(empty_sequence, "input"), "type")))],
  sequence_ids: [get(nonempty_sequence, "id"), get(repeated_sequence, "id"), get(reordered_sequence, "id"), get(collision_sequence_left, "id"), get(collision_sequence_right, "id")],
  result_ids: [get(lifted, "id"), get(mapped, "id"), get(mapped_error, "id"), get(bound, "id"), get(operator_bound, "id")],
  map_error_output_matches: (=)(canonical(type_id(type_evidence(get(get(mapped_error, "output"), "type")))), canonical(type_id(type_evidence(type_tag<core.types.Result<Unit, Int>>())))),
  composite_collision_check: [colliding_left, colliding_right],
  actor_ids: [get(agent, "id"), get(dynamic_agent, "id"), get(approval, "id"), get(retry, "id")],
  run_tool_params: get(run_tool_actor, "params"),
  tool_approval_policy: policy
}
"#,
        json!({}),
    );

    assert_eq!(artifact["source_type"], artifact["capture_type"]);
    assert_eq!(
        artifact["compose_ids"],
        json!(["compose", "nefor.node.composite:3:int3:>>>5:int-2"])
    );
    assert_eq!(
        artifact["then_ids"],
        json!([
            "then",
            "nefor.node.composite:3:int2:*>11:unit-source",
            "nefor.node.composite:3:int2:<*11:unit-source"
        ])
    );
    assert_eq!(
        artifact["fanout_ids"],
        json!(["fanout", "nefor.node.composite:3:int3:&&&5:int-2"])
    );
    assert_eq!(
        artifact["parallel_ids"],
        json!(["parallel", "nefor.node.composite:3:int3:***6:string"])
    );
    assert_eq!(
        artifact["choose_ids"],
        json!(["choose", "nefor.node.composite:3:int3:+++6:string"])
    );
    assert_eq!(
        artifact["sequence_inputs"][0],
        artifact["sequence_inputs"][1]
    );
    assert_eq!(artifact["sequence_ids"][0], artifact["sequence_ids"][1]);
    assert_ne!(artifact["sequence_ids"][0], artifact["sequence_ids"][2]);
    assert_ne!(artifact["sequence_ids"][3], artifact["sequence_ids"][4]);
    assert_eq!(
        artifact["result_ids"],
        json!([
            "lifted",
            "nefor.node.composite:12:result-input10:result.map12:result-value",
            "nefor.node.composite:12:result-input16:result.map_error12:error-mapper",
            "bound",
            "nefor.node.composite:12:result-input3:>=>6:lifted"
        ])
    );
    assert_eq!(artifact["map_error_output_matches"], json!(true));
    assert_eq!(
        artifact["composite_collision_check"],
        json!([
            "nefor.node.composite:5:a&&&b3:&&&1:c",
            "nefor.node.composite:1:a3:&&&5:b&&&c"
        ])
    );
    assert_ne!(
        artifact["composite_collision_check"][0],
        artifact["composite_collision_check"][1]
    );
    assert_eq!(
        artifact["actor_ids"],
        json!(["agent", "dynamic-agent", "approval", "retry"])
    );
    assert_eq!(
        artifact["tool_approval_policy"],
        json!({"rules": {"bash": "deny"}})
    );
    assert_eq!(
        artifact["run_tool_params"]["value"]["tool_approval_policy"],
        json!({"rules": {"bash": "deny"}})
    );
}

#[test]
fn generic_list_inference_supports_unit_sequence_with_error_union() {
    for (name, nodes) in [
        ("inline", "[runtime, configs]"),
        ("bound", "workers"),
        ("annotated", "([runtime, configs]: List<nefor.graph.Node<Unit, core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>>)"),
    ] {
        let source = format!(r#"
import core.types.{{}}
import nefor.contracts.{{}}
import nefor.graph.{{}}
import nefor.node.{{}}
let agent_shaped: fn(String) -> nefor.graph.Node<Unit, core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>> = |id| =>
  nefor.graph.node(id, "test", [], [], [],
    nefor.graph.port(id, type_tag<Unit>(), "mag.Unit"),
    nefor.graph.port(id, type_tag<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>(), "nefor.graph.Value"))
let runtime = agent_shaped("runtime")
let configs = agent_shaped("configs")
let workers = [runtime, configs]
let task = nefor.graph.source("task", nefor.contracts.Task {{prompt: "Investigate"}})
let work = nefor.node.then("task-traces", task, nefor.node.sequence({nodes}))
let actual_id = str(type_id(type_evidence(get(get(work, "output"), "type"))))
let expected_id = str(type_id(type_evidence(type_tag<List<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>>())))
artifact((=)(actual_id, expected_id))
"#);
        let artifact = run(&format!("generic-list-{name}"), &source, json!({}));
        assert_eq!(artifact, json!(true), "{name}");
    }
}
