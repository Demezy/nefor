pub mod bridge {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bridge.rs"));
}

mod error {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs"));
}

pub mod kernel {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/kernel.rs"));

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;

        fn write_kernel(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
            let path = dir.join("kernel.lua");
            let mut f = std::fs::File::create(&path).expect("create kernel");
            f.write_all(body.as_bytes()).expect("write kernel");
            path
        }

        fn shipped_host() -> LuaHost {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            LuaHost::load_kernel(
                &manifest.join("lua/mag-kernel/init.lua"),
                Some(&manifest.join("../../lua")),
            )
            .expect("load shipped kernel")
        }

        fn compile_mag_source(host: &LuaHost, name: &str, source: &str) -> JsonValue {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let source_dir = std::env::temp_dir()
                .join(format!("mag-kernel-shell-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&source_dir);
            std::fs::create_dir_all(&source_dir).expect("create shell test workspace");
            std::fs::write(source_dir.join("main.mag"), source).expect("write shell test program");
            let contracts = host.registry_contracts().expect("runtime contracts");
            let artifact =
                nefor_mag::compile_file_with_inputs_and_module_roots_and_options_and_syntax(
                    &source_dir,
                    "main.mag",
                    serde_json::json!({"factory_contracts": contracts}),
                    &[
                        manifest.join("../../mag/lib"),
                        manifest.join("../../examples/nefor-agent/mag/lib"),
                    ],
                    nefor_mag::CompilerOptions::default(),
                    nefor_mag::SyntaxMode::New,
                )
                .expect("compile MAG test program");
            let modification =
                crate::artifact_modification(&artifact).expect("normalize shell artifact");
            let _ = std::fs::remove_dir_all(source_dir);
            modification
        }

        fn compile_mag_source_error(host: &LuaHost, source: &str) -> String {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            nefor_mag::compile_with_inputs_and_module_roots_and_options_and_syntax(
                source,
                &manifest,
                serde_json::json!({"factory_contracts": host.registry_contracts().expect("runtime contracts")}),
                &[
                    manifest.join("../../mag/lib"),
                    manifest.join("../../examples/nefor-agent/mag/lib"),
                ],
                nefor_mag::CompilerOptions::default(),
                nefor_mag::SyntaxMode::New,
            )
            .expect_err("MAG source must fail compilation")
            .to_string()
        }

        fn actor_params<'a>(artifact: &'a JsonValue, actor: &str) -> &'a JsonValue {
            artifact["actors"]
                .as_array()
                .expect("initial actors")
                .iter()
                .find(|candidate| candidate["id"] == actor)
                .unwrap_or_else(|| panic!("actor {actor}"))
                .get("params")
                .expect("actor params")
        }

        fn compile_mag_eval_expression(host: &LuaHost, name: &str, expression: &str) -> JsonValue {
            let source = format!(
                r#"import nefor.artifact.{{}}
    import nefor.contracts.{{}}
    import nefor.graph.{{}}
    import nefor.process.{{}}
    import nefor.result.{{}}
    import nefor.shell.{{}}
    let start = nefor.graph.source("start", nil)
    let operation = {expression}
    let result = nefor.graph.output_for("result", operation)
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [
      nefor.graph.edge(start, operation),
      nefor.graph.edge(operation, result),
    ])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
    "#
            );
            compile_mag_source(host, name, &source)
        }

        #[test]
        fn nefor_mag_in_five_minutes_satisfies_runtime_factory_contracts() {
            let host = shipped_host();
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let repository = manifest.join("../..");
            let book = repository.join("mag/book/02. nefor/00. Nefor MAG in Five Minutes.md");
            let markdown = std::fs::read_to_string(&book).expect("read Nefor guide");
            let source = markdown
                .split("```mag\n")
                .nth(2)
                .and_then(|rest| rest.split_once("\n```").map(|(source, _)| source))
                .expect("Nefor guide contains one complete MAG program");
            let workspace = repository
                .join("tmp")
                .join(format!("mag-book-host-contracts-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&workspace);
            std::fs::create_dir_all(&workspace).expect("create guide host workspace");
            std::fs::write(workspace.join("main.mag"), source).expect("write guide program");

            let contracts = host.registry_contracts().expect("runtime contracts");
            let artifact =
                nefor_mag::compile_file_with_inputs_and_module_roots_and_options_and_syntax(
                    &workspace,
                    "main.mag",
                    serde_json::json!({"factory_contracts": contracts}),
                    &[
                        repository.join("mag/lib"),
                        repository.join("examples/nefor-agent/mag/lib"),
                    ],
                    nefor_mag::CompilerOptions::default(),
                    nefor_mag::SyntaxMode::New,
                )
                .expect("compile Nefor guide with runtime contracts");
            let decoded = crate::artifact_program(&artifact).expect("decode guide program");
            let initial = decoded.initial;

            let actors = initial["actors"].as_array().expect("guide actors");
            for factory in [
                "nefor.factory.shell-script",
                "nefor.factory.discard",
                "nefor.factory.dynamic-each",
                "nefor.factory.dynamic-output",
                "nefor.factory.dynamic-all",
            ] {
                assert!(
                    actors.iter().any(|actor| actor["factory"] == factory),
                    "guide must exercise {factory}"
                );
            }
            assert!(
                !decoded.operations.is_empty(),
                "guide declares runtime operations"
            );
            assert!(decoded
                .operations
                .iter()
                .any(|operation| operation["template"]["actors"]
                    .as_array()
                    .is_some_and(|actors| !actors.is_empty())));

            let build_params = actor_params(&initial, "verification.build");
            assert_eq!(build_params["script"], "cargo build");
            assert_eq!(build_params["cwd"], ".");
            let test_params = actor_params(&initial, "verification.test");
            assert_eq!(test_params["script"], "cargo test");
            assert_eq!(test_params["cwd"], ".");

            assert!(
                host.begin_run("mag-book-contracts", "mag-book-contracts", None)
                    .expect("begin guide run")
                    .ok
            );
            let started = host
                .start_program("mag-book-contracts", &initial, &decoded.operations)
                .expect("start guide modification");
            assert!(
                started.ok,
                "guide host validation failed: {:?}",
                started.error
            );

            host.end_run("mag-book-contracts", TeardownReason::RunComplete)
                .expect("end guide host run");
            std::fs::remove_dir_all(workspace).expect("remove guide host workspace");
        }

        #[test]
        fn ordinary_deterministic_workers_execute_as_traversal_templates() {
            let host = shipped_host();
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let source_dir = std::env::temp_dir().join(format!(
                "mag-kernel-traverse-workers-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&source_dir);
            std::fs::create_dir_all(&source_dir).expect("create traversal test workspace");

            for (name, worker, output_type, expected_factory) in [
                (
                    "identity",
                    "nefor.graph.identity<String>(\"worker\")",
                    "String",
                    "nefor.factory.output",
                ),
                (
                    "record-input",
                    "nefor.graph.identity<Payload>(\"worker\")",
                    "Payload",
                    "nefor.factory.output",
                ),
                (
                    "product-input",
                    "nefor.graph.identity<(String, Int)>(\"worker\")",
                    "(String, Int)",
                    "nefor.factory.output",
                ),
                (
                    "sum-input",
                    "nefor.graph.identity<Choice>(\"worker\")",
                    "Choice",
                    "nefor.factory.output",
                ),
                (
                    "unit-input",
                    "nefor.graph.identity<Unit>(\"worker\")",
                    "Unit",
                    "nefor.factory.output",
                ),
                (
                    "composed",
                    "nefor.node.compose(\"worker\", nefor.graph.identity<String>(\"first\"), nefor.graph.identity<String>(\"second\"))",
                    "String",
                    "nefor.factory.output",
                ),
                (
                    "product",
                    "nefor.node.fanout(\"worker\", nefor.graph.identity<String>(\"left\"), nefor.graph.identity<String>(\"right\"))",
                    "(String, String)",
                    "nefor.factory.product-join",
                ),
                (
                    "choose",
                    "nefor.node.choose(\"worker\", nefor.graph.identity<String>(\"left\"), nefor.graph.identity<Int>(\"right\"))",
                    "core.types.Either<String, Int>",
                    "nefor.factory.adt-unpack",
                ),
                (
                    "result-bind",
                    "nefor.result.and_then(\"worker\", nefor.graph.identity<core.types.Result<String, Int>>(\"input\"), nefor.result.lift<Int, String, Int>(\"lift\", nefor.graph.identity<Int>(\"value\")))",
                    "core.types.Result<String, Int>",
                    "nefor.factory.adt-unpack",
                ),
                (
                    "result-map-error",
                    "nefor.result.map_error(nefor.graph.identity<core.types.Result<String, Int>>(\"input\"), nefor.graph.identity<String>(\"error\"))",
                    "core.types.Result<String, Int>",
                    "nefor.factory.adt-unpack",
                ),
                (
                    "sequence",
                    "nefor.node.sequence([nefor.graph.identity<String>(\"first\"), nefor.graph.identity<String>(\"second\")])",
                    "List<String>",
                    "nefor.factory.collector",
                ),
            ] {
                let input_type = match name {
                    "choose" => "core.types.Either<String, Int>",
                    "result-bind" | "result-map-error" => "core.types.Result<String, Int>",
                    "record-input" => "Payload",
                    "product-input" => "(String, Int)",
                    "sum-input" => "Choice",
                    "unit-input" => "Unit",
                    _ => "String",
                };
                let input_values = match name {
                    "choose" => serde_json::json!([{"constructor":"Left","value":"alpha"},{"constructor":"Right","value":9}]),
                    "result-bind" | "result-map-error" => serde_json::json!([{"constructor":"Error","value":"alpha"},{"constructor":"Ok","value":9}]),
                    "record-input" => serde_json::json!([{"text":"alpha","nested":[1,2]}, {"text":"beta","nested":[]}]),
                    "product-input" => serde_json::json!([["alpha",7],["beta",9]]),
                    "sum-input" => serde_json::json!([{"constructor":"Text","value":"alpha"},{"constructor":"Count","value":9}]),
                    "unit-input" => serde_json::json!([null,null]),
                    _ => serde_json::json!(["alpha","beta"]),
                };
                let provider_values = if name == "product-input" {
                    serde_json::json!([{"0":"alpha","1":7},{"0":"beta","1":9}])
                } else { input_values.clone() };
                let source = format!(
                    r#"
import core.types.{{}}
import nefor.actors.{{}}
import nefor.artifact.{{}}
import nefor.contracts.{{}}
import nefor.dynamic.{{}}
import nefor.graph.{{}}
import nefor.node.{{}}
import nefor.result.{{}}
type InvestigationInput {{prompt: String}}
type Payload {{text: String, nested: List<Int>}}
type Choice = Text(String) | Count(Int)
let exact_model: fn(nefor.actors.ResolvedModel) -> nefor.actors.AuthoredModel = |model| => named(nefor.actors.AuthoredModel, ResolvedModel, model)
let configured_model = nefor.actors.ResolvedModel {{provider: "test-provider", model: "test-model", reasoning_effort: nefor.actors.no_reasoning_effort}}
let start = nefor.graph.source("task", InvestigationInput {{prompt: "produce strings"}})
let planner = nefor.actors.agent<nefor.actors.ResolvedModel, InvestigationInput, nefor.dynamic.DynamicList<{input_type}>>("planner", exact_model, nefor.actors.AgentConfig<nefor.actors.ResolvedModel> {{model: configured_model, system: "Return strings.", tools: [], tool_approval_policy: named(nefor.contracts.ToolApprovalPolicy, Default, nil), max_corrections: 0}})
let planned = nefor.node.`>>>`(start, planner)
let worker = {worker}
let traversal = nefor.dynamic.traverse("traversal", worker)
let lifted = nefor.result.lift<nefor.dynamic.DynamicList<{input_type}>, nefor.contracts.AgentError, nefor.dynamic.DynamicList<{output_type}>>("lifted", traversal)
let completed = nefor.result.`>=>`(planned, lifted)
let contextual = nefor.result.map(completed, nefor.dynamic.context<{output_type}>("completed-context"))
nefor.artifact.compile_graph(contextual)
"#
                );
                std::fs::write(source_dir.join("main.mag"), source)
                    .expect("write traversal test program");
                let contracts = host.registry_contracts().expect("runtime contracts");
                let artifact =
                    nefor_mag::compile_file_with_inputs_and_module_roots_and_options_and_syntax(
                        &source_dir,
                        "main.mag",
                        serde_json::json!({"factory_contracts": contracts}),
                        &[
                            manifest.join("../../mag/lib"),
                            manifest.join("../../examples/nefor-agent/mag/lib"),
                        ],
                        nefor_mag::CompilerOptions::default(),
                        nefor_mag::SyntaxMode::New,
                    )
                    .unwrap_or_else(|error| panic!("compile {name} traversal worker: {error}"));
                let decoded = crate::artifact_program(&artifact)
                    .unwrap_or_else(|error| panic!("decode {name} traversal program: {error}"));
                let operation = decoded
                    .operations
                    .iter()
                    .find(|operation| operation["id"] == "traversal.expand")
                    .unwrap_or_else(|| panic!("{name} traversal operation"));
                let template_actors = operation["template"]["actors"]
                    .as_array()
                    .expect("traversal template actors");
                assert!(
                    template_actors
                        .iter()
                        .any(|actor| actor["factory"] == expected_factory),
                    "{name} traversal must lower its ordinary worker, not a hand-authored empty template: {template_actors:?}"
                );
                assert!(
                    !operation["template"]["routes"]
                        .as_array()
                        .expect("traversal template routes")
                        .is_empty(),
                    "{name} traversal must connect the ordinary worker to its indexed result"
                );
                assert!(host.begin_run(name, name, None).unwrap().ok);
                host.drain_emits().unwrap();
                let started = host.start_program(name, &decoded.initial, &decoded.operations).unwrap();
                assert!(started.ok, "{name} start: {:?}", started.error);
                let emitted = host.drain_emits().unwrap();
                let request = tool_invoke(&emitted, "test-provider");
                host.bus_response(request["id"].as_str().unwrap(),
                    Some(&serde_json::json!({"text":serde_json::json!({"value":provider_values}).to_string()})), None, Some("async")).unwrap();
                let completion = host.take_run_complete(name).unwrap();
                let failure = host.take_run_failed(name).unwrap();
                let _events = host.drain_emits().unwrap();
                assert!(completion.is_some(), "{name} did not complete: {failure:?}");
                let result = completion.unwrap().result.unwrap();
                let ordered = &result["value"]["value"]["content"]["value"];
                let expected = if name == "product" || name == "sequence" {
                    serde_json::json!([["alpha", "alpha"], ["beta", "beta"]])
                } else { input_values };
                assert_eq!(ordered, &expected, "{name}: {result}");
                host.end_run(name, TeardownReason::RunComplete).unwrap();
            }

            std::fs::remove_dir_all(source_dir).expect("remove traversal test workspace");
        }

        #[test]
        fn inline_actor_type_arguments_must_be_dense_lists() {
            let host = shipped_host();
            for (run_id, type_arguments) in [
                ("missing-type-arguments", None),
                (
                    "keyed-type-arguments",
                    Some(serde_json::json!({"named": {"kind":"primitive","name":"String"}})),
                ),
            ] {
                assert!(host.begin_run(run_id, run_id, None).expect("begin").ok);
                let mut actor = serde_json::json!({
                    "id": "inline",
                    "factory": "nefor.factory.stub",
                    "params": {},
                    "routes": {}
                });
                if let Some(type_arguments) = type_arguments {
                    actor["type_arguments"] = type_arguments;
                }
                let modification = serde_json::json!({
                    "actors": [actor],
                    "messages": [],
                    "kills": [],
                    "result": {"from": {"actor": "inline", "wire": "stub.Out", "type": "String"}}
                });
                let outcome = host.start(run_id, &modification).expect("start");
                assert!(!outcome.ok, "{run_id} unexpectedly accepted");
                assert!(
                    outcome
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("type_arguments must be a dense list")),
                    "{run_id}: {:?}",
                    outcome.error
                );
                host.end_run(run_id, TeardownReason::RunFailed)
                    .expect("end invalid run");
            }
        }

        #[test]
        fn compile_reuses_shared_nodes_across_a_multi_agent_graph() {
            let host = shipped_host();
            let source = r#"
    import core.types.{}
    import nefor.actors.{}
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    type InvestigationInput {prompt: String}

    let exact_model: fn(nefor.actors.ResolvedModel) -> nefor.actors.AuthoredModel = |model| => named(nefor.actors.AuthoredModel, ResolvedModel, model)
    let resolved = nefor.actors.ResolvedModel {
      provider: "test-provider",
      model: "test-model",
      reasoning_effort: nefor.actors.reasoning_effort("medium"),
    }

    let make_agent<I, O>: fn(String, TypeTag<I>, TypeTag<O>) -> nefor.graph.Node<I, core.types.Result<nefor.contracts.AgentError, O>> = |id, input_type, output_type| =>
      nefor.actors.agent<nefor.actors.ResolvedModel, I, O>(id, exact_model, nefor.actors.AgentConfig<nefor.actors.ResolvedModel> {model: resolved, system: "", tools: [], tool_approval_policy: named(nefor.contracts.ToolApprovalPolicy, Default, nil), max_corrections: 2, })

    let left_task = nefor.graph.source("left-task", InvestigationInput {prompt: "left"})
    let middle_task = nefor.graph.source("middle-task", InvestigationInput {prompt: "middle"})
    let right_task = nefor.graph.source("right-task", InvestigationInput {prompt: "right"})
    let left = make_agent("left", type_tag<InvestigationInput>(), type_tag<nefor.contracts.TextAnswer>())
    let middle = make_agent("middle", type_tag<InvestigationInput>(), type_tag<nefor.contracts.TextAnswer>())
    let right = make_agent("right", type_tag<InvestigationInput>(), type_tag<nefor.contracts.TextAnswer>())
    let synthesis = make_agent("synthesis", type_tag<(core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>, core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>, core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>)>(), type_tag<nefor.contracts.TextAnswer>())
    let result = nefor.graph.output<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>("result")
    let topology: fn(nefor.graph.Graph) -> nefor.graph.Graph = |graph| => nefor.graph.add_edges(graph, [
      nefor.graph.edge(left_task, left),
      nefor.graph.edge(middle_task, middle),
      nefor.graph.edge(right_task, right),
      nefor.graph.edge(left, synthesis),
      nefor.graph.edge(middle, synthesis),
      nefor.graph.edge(right, synthesis),
      nefor.graph.edge(synthesis, result),
    ])
    nefor.artifact.compile(topology)
    "#;

            let modification = compile_mag_source(&host, "shared-multi-agent", source);
            assert_eq!(
                modification["actors"].as_array().map(Vec::len),
                Some(20),
                "each node is lowered once even when it appears at multiple edge boundaries"
            );
        }

        #[test]
        fn shared_source_union_executes_once_and_fans_out_to_distinct_consumers() {
            let host = shipped_host();
            let source = r#"
    import nefor.artifact.{}
    import nefor.graph.{}
    import nefor.node.{}
    let shared = nefor.graph.source("shared", 7)
    let left = nefor.node.compose("left", shared, nefor.graph.identity<Int>("left-value"))
    let right = nefor.node.compose("right", shared, nefor.graph.identity<Int>("right-value"))
    nefor.artifact.compile_graph(nefor.node.fanout("branched", left, right))
    "#;
            let modification = compile_mag_source(&host, "shared-source-union", source);
            assert_eq!(
                modification["actors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|actor| actor["id"] == "shared")
                    .count(),
                1
            );
            assert!(
                host.begin_run("shared-source-union", "shared-source-union", None)
                    .expect("begin shared source run")
                    .ok
            );
            host.drain_emits().expect("drain begin events");
            let started = host
                .start_program("shared-source-union", &modification, &[])
                .expect("start shared source program");
            assert!(started.ok, "shared source start: {:?}", started.error);
            let completion = host
                .take_run_complete("shared-source-union")
                .expect("read shared source completion")
                .expect("shared source completed");
            assert_eq!(
                completion.result.unwrap()["value"],
                serde_json::json!([7, 7])
            );
            host.end_run("shared-source-union", TeardownReason::RunComplete)
                .expect("end shared source run");
        }

        #[test]
        fn run_model_snapshot_overrides_llm_factories_without_mutating_specs() {
            let host = shipped_host();
            let direct = r#"
    import core.types.{}
    import nefor.actors.{}
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    type InvestigationInput {prompt: String}
    let exact_model: fn(nefor.actors.ResolvedModel) -> nefor.actors.AuthoredModel = |model| => named(nefor.actors.AuthoredModel, ResolvedModel, model)
    let resolved = nefor.actors.ResolvedModel {provider: "authored-provider", model: "authored-model", reasoning_effort: nefor.actors.reasoning_effort("authored-effort")}
    let start = nefor.graph.source("task", InvestigationInput {prompt: "answer"})
    let worker = nefor.actors.agent<nefor.actors.ResolvedModel, InvestigationInput, nefor.contracts.TextAnswer>("worker", exact_model, nefor.actors.AgentConfig<nefor.actors.ResolvedModel> {model: resolved, system: "", tools: [], tool_approval_policy: named(nefor.contracts.ToolApprovalPolicy, Default, nil), max_corrections: 2})
    let result = nefor.graph.output<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>("result")
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, worker), nefor.graph.edge(worker, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
    "#;
            let structured = direct
                .replace(
                    "import nefor.actors.{}",
                    "import nefor.actors.{}\n    type Answer {answer: String}",
                )
                .replace(
                    "nefor.actors.agent<nefor.actors.ResolvedModel, InvestigationInput, nefor.contracts.TextAnswer>",
                    "nefor.actors.agent<nefor.actors.ResolvedModel, InvestigationInput, Answer>",
                )
                .replace(
                    "nefor.actors.agent<Model, InvestigationInput, nefor.contracts.TextAnswer>",
                    "nefor.actors.agent<Model, InvestigationInput, Answer>",
                )
                .replace(
                    "nefor.graph.output<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>",
                    "nefor.graph.output<core.types.Result<nefor.contracts.AgentError, Answer>>",
                );
            for (run_id, source, factory) in [
                ("snapshot-direct", direct.to_owned(), "nefor.factory.llm"),
                (
                    "snapshot-structured",
                    structured,
                    "nefor.factory.structured-output",
                ),
            ] {
                let modification = compile_mag_source(&host, run_id, &source);
                let mut snapshot = ExecutionModelSnapshot {
                    provider: "snapshot-provider".to_owned(),
                    model: "snapshot-model".to_owned(),
                    reasoning_effort: None,
                    provider_options: Some(serde_json::Map::from_iter([(
                        "service_tier".to_owned(),
                        serde_json::Value::String("fast".to_owned()),
                    )])),
                    profiles: Default::default(),
                };
                let begun = host
                    .begin_run_with_principal(
                        run_id,
                        run_id,
                        Some("session"),
                        Some("subagent"),
                        Some("conversation"),
                        Some(&snapshot),
                    )
                    .expect("begin snapshotted run");
                assert!(begun.ok, "begin failed: {:?}", begun.error);
                snapshot
                    .provider_options
                    .as_mut()
                    .unwrap()
                    .insert("service_tier".to_owned(), serde_json::json!("mutated"));
                host.drain_emits().expect("drain begin");
                let outcome = host.start(run_id, &modification).expect("start run");
                assert!(outcome.ok, "start failed: {:?}", outcome.error);
                let emits = host.drain_emits().expect("drain start");
                let spawned = emits
                    .iter()
                    .find(|event| {
                        event["kind"] == "mag.actor_spawned" && event["id"] == "worker.llm"
                    })
                    .expect("llm spawn event");
                assert_eq!(spawned["factory"], factory);
                assert_eq!(spawned["spec"]["params"]["provider"], "authored-provider");
                assert_eq!(spawned["spec"]["params"]["model"], "authored-model");
                assert_eq!(
                    spawned["spec"]["params"]["reasoning_effort"]["present"], true,
                    "inventory retains authored optional effort"
                );
                let invoke = tool_invoke(&emits, "snapshot-provider");
                assert_eq!(invoke["args"]["model"], "snapshot-model");
                assert_eq!(invoke["args"]["provider_options"]["service_tier"], "fast");
                assert!(
                    invoke["args"].get("reasoning_effort").is_none(),
                    "snapshot omission clears authored effort: {invoke:?}"
                );

                if run_id == "snapshot-direct" {
                    let later_snapshot = ExecutionModelSnapshot {
                        provider: "later-provider".to_owned(),
                        model: "later-model".to_owned(),
                        reasoning_effort: Some("low".to_owned()),
                        provider_options: None,
                        profiles: Default::default(),
                    };
                    let begun = host
                        .begin_run_with_principal(
                            "snapshot-later",
                            "snapshot-later",
                            Some("session"),
                            Some("subagent"),
                            Some("conversation-later"),
                            Some(&later_snapshot),
                        )
                        .expect("begin later run");
                    assert!(begun.ok);
                    host.drain_emits().expect("drain later begin");
                    let outcome = host
                        .start("snapshot-later", &modification)
                        .expect("start later run");
                    assert!(outcome.ok, "later start failed: {:?}", outcome.error);
                    let later_emits = host.drain_emits().expect("drain later invoke");
                    let later_invoke = tool_invoke(&later_emits, "later-provider");
                    assert_eq!(later_invoke["args"]["model"], "later-model");

                    let patch_source = direct
                        .replace("\"task\"", "\"patch-task\"")
                        .replace("\"worker\"", "\"patch\"")
                        .replace("\"result\"", "\"patch-result\"");
                    let mut patch = compile_mag_source(&host, "snapshot-patch", &patch_source);
                    patch
                        .as_object_mut()
                        .expect("patch object")
                        .remove("result");
                    let outcome = host.apply(run_id, &patch).expect("apply snapshotted patch");
                    assert!(outcome.ok, "patch failed: {:?}", outcome.error);
                    let patch_emits = host.drain_emits().expect("drain patch invoke");
                    let patch_invoke = tool_invoke(&patch_emits, "snapshot-provider");
                    assert_eq!(patch_invoke["args"]["model"], "snapshot-model");
                    assert!(patch_invoke["args"].get("reasoning_effort").is_none());
                    assert_eq!(
                        patch_invoke["args"]["provider_options"]["service_tier"],
                        "fast"
                    );
                    let patch_spawn = patch_emits
                        .iter()
                        .find(|event| {
                            event["kind"] == "mag.actor_spawned" && event["id"] == "patch.llm"
                        })
                        .expect("patch llm spawn");
                    assert_eq!(
                        patch_spawn["spec"]["params"]["provider"],
                        "authored-provider"
                    );

                    host.end_run("snapshot-later", TeardownReason::Killed)
                        .expect("end later run");
                    host.drain_emits().expect("drain later end");
                }

                host.end_run(run_id, TeardownReason::Killed)
                    .expect("end snapshot run");
                host.drain_emits().expect("drain end");
            }

            let modification = compile_mag_source(&host, "snapshot-effort", direct);
            let snapshot = ExecutionModelSnapshot {
                provider: "snapshot-provider".to_owned(),
                model: "snapshot-model".to_owned(),
                reasoning_effort: Some("high".to_owned()),
                provider_options: None,
                profiles: Default::default(),
            };
            let begun = host
                .begin_run_with_principal(
                    "snapshot-effort",
                    "snapshot-effort",
                    Some("session"),
                    Some("subagent"),
                    Some("conversation"),
                    Some(&snapshot),
                )
                .expect("begin effort run");
            assert!(begun.ok);
            host.drain_emits().expect("drain begin");
            let outcome = host
                .start("snapshot-effort", &modification)
                .expect("start effort run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let emits = host.drain_emits().expect("drain effort invoke");
            let invoke = tool_invoke(&emits, "snapshot-provider");
            assert_eq!(invoke["args"]["reasoning_effort"], "high");
            host.end_run("snapshot-effort", TeardownReason::Killed)
                .expect("end effort run");
            host.drain_emits().expect("drain effort end");

            let begun = host
                .begin_run("snapshot-fallback", "snapshot-fallback", Some("session"))
                .expect("begin fallback run");
            assert!(begun.ok);
            host.drain_emits().expect("drain fallback begin");
            let outcome = host
                .start("snapshot-fallback", &modification)
                .expect("start fallback run");
            assert!(outcome.ok, "fallback start failed: {:?}", outcome.error);
            let emits = host.drain_emits().expect("drain fallback invoke");
            let invoke = tool_invoke(&emits, "authored-provider");
            assert_eq!(invoke["args"]["model"], "authored-model");
            assert_eq!(invoke["args"]["reasoning_effort"], "authored-effort");
            host.end_run("snapshot-fallback", TeardownReason::Killed)
                .expect("end fallback run");
            host.drain_emits().expect("drain fallback end");

            let mut malformed = modification;
            let llm = malformed["actors"]
                .as_array_mut()
                .expect("actors")
                .iter_mut()
                .find(|actor| actor["id"] == "worker.llm")
                .expect("worker llm");
            llm["params"]["reasoning_effort"] = serde_json::json!({"present": true, "value": ""});
            let begun = host
                .begin_run("malformed-effort", "malformed-effort", Some("session"))
                .expect("begin malformed run");
            assert!(begun.ok);
            host.drain_emits().expect("drain malformed begin");
            let outcome = host
                .start("malformed-effort", &malformed)
                .expect("start malformed run");
            assert!(outcome.ok, "initial modification still applies");
            let failure = host
                .take_run_failed("malformed-effort")
                .expect("take malformed failure")
                .expect("malformed effort fails construction");
            assert!(failure.contains("present=true requires a non-empty value"));
            assert!(host
                .drain_emits()
                .expect("drain malformed failure")
                .iter()
                .all(|event| event["kind"] != "tool.invoke"));
        }

        #[test]
        fn authored_model_profiles_resolve_from_the_owned_run_snapshot() {
            let host = shipped_host();
            let direct = r#"
    import core.types.{}
    import nefor.actors.{}
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    type InvestigationInput {prompt: String}
    type Model = Current(Unit) | Fast(Unit)
    let fast = Model.Fast(nil)
    let resolve_model: fn(Model) -> nefor.actors.AuthoredModel = |model| => match model {
      case Current(value) => named(nefor.actors.AuthoredModel, ResolvedModel, nefor.actors.ResolvedModel {provider: "authored-provider", model: "authored-model", reasoning_effort: nefor.actors.no_reasoning_effort}),
      case Fast(value) => named(nefor.actors.AuthoredModel, ModelProfile, nefor.actors.model_profile("fast")),
    }
    let start = nefor.graph.source("task", InvestigationInput {prompt: "answer"})
    let worker = nefor.actors.agent<Model, InvestigationInput, nefor.contracts.TextAnswer>("worker", resolve_model, nefor.actors.AgentConfig<Model> {model: fast, system: "", tools: [], tool_approval_policy: named(nefor.contracts.ToolApprovalPolicy, Default, nil), max_corrections: 2})
    let result = nefor.graph.output<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>("result")
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, worker), nefor.graph.edge(worker, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
    "#;
            let structured = direct
                .replace(
                    "import nefor.actors.{}",
                    "import nefor.actors.{}\n    type Answer {answer: String}",
                )
                .replace(
                    "nefor.actors.agent<nefor.actors.ResolvedModel, InvestigationInput, nefor.contracts.TextAnswer>",
                    "nefor.actors.agent<nefor.actors.ResolvedModel, InvestigationInput, Answer>",
                )
                .replace(
                    "nefor.actors.agent<Model, InvestigationInput, nefor.contracts.TextAnswer>",
                    "nefor.actors.agent<Model, InvestigationInput, Answer>",
                )
                .replace(
                    "nefor.graph.output<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>",
                    "nefor.graph.output<core.types.Result<nefor.contracts.AgentError, Answer>>",
                );
            for (run_id, source, factory) in [
                ("profile-direct", direct.to_owned(), "nefor.factory.llm"),
                (
                    "profile-structured",
                    structured,
                    "nefor.factory.structured-output",
                ),
            ] {
                let modification = compile_mag_source(&host, run_id, &source);
                assert_eq!(
                    actor_params(&modification, "worker.llm")["model_profile"],
                    serde_json::json!({"present": true, "value": "fast"}),
                    "the compiled actor carries the typed authored selector"
                );
                let mut profiles = BTreeMap::new();
                profiles.insert(
                    "fast".to_owned(),
                    ExecutionResolvedModel {
                        provider: "fast-provider".to_owned(),
                        model: "fast-model".to_owned(),
                        reasoning_effort: Some("low".to_owned()),
                        provider_options: Some(serde_json::Map::from_iter([(
                            "service_tier".to_owned(),
                            serde_json::Value::String("fast".to_owned()),
                        )])),
                    },
                );
                let snapshot = ExecutionModelSnapshot {
                    provider: "current-provider".to_owned(),
                    model: "current-model".to_owned(),
                    reasoning_effort: Some("high".to_owned()),
                    provider_options: None,
                    profiles,
                };
                let begun = host
                    .begin_run_with_principal(
                        run_id,
                        run_id,
                        Some("session"),
                        Some("subagent"),
                        Some("conversation"),
                        Some(&snapshot),
                    )
                    .expect("begin profiled run");
                assert!(begun.ok, "begin failed: {:?}", begun.error);
                host.drain_emits().expect("drain begin");
                let outcome = host
                    .start(run_id, &modification)
                    .expect("start profiled run");
                assert!(outcome.ok, "start failed: {:?}", outcome.error);
                let emits = host.drain_emits().expect("drain profiled invoke");
                let spawned = emits
                    .iter()
                    .find(|event| {
                        event["kind"] == "mag.actor_spawned" && event["id"] == "worker.llm"
                    })
                    .expect("profiled llm spawn");
                assert_eq!(spawned["factory"], factory);
                let invoke = tool_invoke(&emits, "fast-provider");
                assert_eq!(invoke["args"]["model"], "fast-model");
                assert_eq!(invoke["args"]["reasoning_effort"], "low");
                assert_eq!(invoke["args"]["provider_options"]["service_tier"], "fast");
                assert!(invoke["args"].get("model_profile").is_none());

                if run_id == "profile-direct" {
                    let patch_source = direct
                        .replace("\"task\"", "\"patch-task\"")
                        .replace("\"worker\"", "\"patch\"")
                        .replace("\"result\"", "\"patch-result\"");
                    let mut patch = compile_mag_source(&host, "profile-patch", &patch_source);
                    patch
                        .as_object_mut()
                        .expect("patch object")
                        .remove("result");
                    let outcome = host.apply(run_id, &patch).expect("apply profiled patch");
                    assert!(outcome.ok, "patch failed: {:?}", outcome.error);
                    let patch_emits = host.drain_emits().expect("drain profiled patch");
                    let patch_invoke = tool_invoke(&patch_emits, "fast-provider");
                    assert_eq!(patch_invoke["args"]["model"], "fast-model");
                }

                host.end_run(run_id, TeardownReason::Killed)
                    .expect("end profiled run");
                host.drain_emits().expect("drain profiled end");
            }

            let standard_source =
                direct.replace("model_profile(\"fast\")", "model_profile(\"standard\")");
            let modification =
                compile_mag_source(&host, "profile-clears-options", &standard_source);
            let mut profiles = BTreeMap::new();
            profiles.insert(
                "standard".to_owned(),
                ExecutionResolvedModel {
                    provider: "standard-provider".to_owned(),
                    model: "standard-model".to_owned(),
                    reasoning_effort: Some("medium".to_owned()),
                    provider_options: None,
                },
            );
            let snapshot = ExecutionModelSnapshot {
                provider: "current-provider".to_owned(),
                model: "current-model".to_owned(),
                reasoning_effort: Some("high".to_owned()),
                provider_options: Some(serde_json::Map::from_iter([(
                    "service_tier".to_owned(),
                    serde_json::Value::String("fast".to_owned()),
                )])),
                profiles,
            };
            let begun = host
                .begin_run_with_principal(
                    "profile-clears-options",
                    "profile-clears-options",
                    Some("session"),
                    Some("subagent"),
                    Some("conversation"),
                    Some(&snapshot),
                )
                .expect("begin standard-profile run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin");
            let outcome = host
                .start("profile-clears-options", &modification)
                .expect("start standard-profile run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let emits = host.drain_emits().expect("drain standard-profile invoke");
            let invoke = tool_invoke(&emits, "standard-provider");
            assert_eq!(invoke["args"]["model"], "standard-model");
            assert!(
                invoke["args"].get("provider_options").is_none(),
                "profile omission clears root provider options: {invoke:?}"
            );
            host.end_run("profile-clears-options", TeardownReason::Killed)
                .expect("end standard-profile run");
            host.drain_emits().expect("drain standard-profile end");

            let modification = compile_mag_source(&host, "missing-profile", direct);
            let snapshot = ExecutionModelSnapshot {
                provider: "current-provider".to_owned(),
                model: "current-model".to_owned(),
                reasoning_effort: None,
                provider_options: None,
                profiles: Default::default(),
            };
            let begun = host
                .begin_run_with_principal(
                    "missing-profile",
                    "missing-profile",
                    Some("session"),
                    Some("subagent"),
                    Some("conversation"),
                    Some(&snapshot),
                )
                .expect("begin missing-profile run");
            assert!(begun.ok);
            host.drain_emits().expect("drain missing-profile begin");
            let outcome = host
                .start("missing-profile", &modification)
                .expect("start missing-profile run");
            assert!(outcome.ok, "initial modification still applies");
            let failure = host
                .take_run_failed("missing-profile")
                .expect("take missing-profile failure")
                .expect("missing profile fails construction");
            assert!(failure.contains("model profile \"fast\" is absent"));
            assert!(host
                .drain_emits()
                .expect("drain missing-profile failure")
                .iter()
                .all(|event| event["kind"] != "tool.invoke"));
        }

        #[test]
        fn task_source_preserves_task_type_and_value_at_runtime() {
            let host = shipped_host();
            let source = r#"
    import core.types.{}
    import nefor.actors.{}
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    type InvestigationInput {prompt: String}
    let start = nefor.graph.source("task-input", InvestigationInput {prompt: "runtime prompt"})
    let result = nefor.graph.output<InvestigationInput>("result")
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
    "#;
            let modification = compile_mag_source(&host, "task-source-runtime", source);
            let begun = host
                .begin_run("task-source-runtime", "task-source-runtime", None)
                .expect("begin Task source run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin event");
            let outcome = host
                .start("task-source-runtime", &modification)
                .expect("start Task source run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let completion = host
                .take_run_complete("task-source-runtime")
                .expect("take Task source completion")
                .expect("Task source run completed");
            assert_eq!(
                completion
                    .result
                    .as_ref()
                    .and_then(|result| result.get("value")),
                Some(&serde_json::json!({"prompt": "runtime prompt"}))
            );
            assert_eq!(
                completion
                    .result
                    .as_ref()
                    .and_then(|result| result.pointer("/semantic_type/name"))
                    .and_then(JsonValue::as_str),
                Some("main.InvestigationInput")
            );
        }

        #[test]
        fn fixed_sequence_of_task_sources_bootstraps_once_at_its_outer_unit_boundary() {
            let host = shipped_host();
            let source = r#"
    import core.types.{}
    import nefor.actors.{}
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    import nefor.node.{}
    type InvestigationInput {prompt: String}
    let first = nefor.graph.source("first", InvestigationInput {prompt: "first task"})
    let second = nefor.graph.source("second", InvestigationInput {prompt: "second task"})
    let tasks = nefor.node.sequence([first, second])
    let result = nefor.graph.output_for("result", tasks)
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(tasks, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
    "#;
            let modification = compile_mag_source(&host, "task-source-sequence", source);
            assert_eq!(
                modification["messages"].as_array().map(Vec::len),
                Some(1),
                "only the completed sequence root receives initial activation"
            );
            assert!(modification["messages"][0]["to"]
                .as_str()
                .is_some_and(|target| target.ends_with(".input")));

            let begun = host
                .begin_run("task-source-sequence", "task-source-sequence", None)
                .expect("begin source sequence run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin event");
            let outcome = host
                .start("task-source-sequence", &modification)
                .expect("start source sequence run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let completion = host
                .take_run_complete("task-source-sequence")
                .expect("take source sequence completion")
                .expect("source sequence completed");
            assert_eq!(
                completion
                    .result
                    .as_ref()
                    .and_then(|result| result.get("value")),
                Some(&serde_json::json!([
                    {"prompt": "first task"},
                    {"prompt": "second task"}
                ]))
            );
        }

        #[test]
        fn shared_input_sequence_and_left_sequencing_retain_runtime_values() {
            for (run_id, source, expected) in [
                (
                    "shared-input-sequence",
                    r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.node.{}
let start = nefor.graph.source("shared", "shared value")
let left = nefor.graph.identity<String>("left")
let right = nefor.graph.identity<String>("right")
let operation = nefor.node.`>>>`(start, nefor.node.sequence([left, right]))
nefor.artifact.compile_graph(operation)
"#,
                    serde_json::json!(["shared value", "shared value"]),
                ),
                (
                    "retain-left-sequencing",
                    r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.node.{}
let left = nefor.graph.source("left", "retained")
let right = nefor.graph.source("right", "discarded")
let operation = nefor.node.`<*`(left, right)
nefor.artifact.compile_graph(operation)
"#,
                    serde_json::json!("retained"),
                ),
                (
                    "explicit-empty-sequence",
                    r#"
import nefor.artifact.{}
import nefor.node.{}
let operation = nefor.node.sequence_empty<Unit, String>("empty")
nefor.artifact.compile_graph(operation)
"#,
                    serde_json::json!([]),
                ),
            ] {
                let host = shipped_host();
                let modification = compile_mag_source(&host, run_id, source);
                let begun = host.begin_run(run_id, run_id, None).expect("begin run");
                assert!(begun.ok, "begin failed: {:?}", begun.error);
                host.drain_emits().expect("drain begin event");
                let outcome = host.start(run_id, &modification).expect("start run");
                assert!(outcome.ok, "start failed: {:?}", outcome.error);
                let completion = host
                    .take_run_complete(run_id)
                    .expect("read completion")
                    .expect("composition completed");
                assert_eq!(
                    completion.result.as_ref().map(|result| &result["value"]),
                    Some(&expected)
                );
            }
        }

        #[test]
        fn result_mapping_preserves_the_unmapped_runtime_branch() {
            for (run_id, operation, expected) in [
                (
                    "result-map-error-branch",
                    "let start = nefor.graph.source(\"start\", named(core.types.Result<String, Int>, Error, \"failed\"))\nlet mapper = nefor.graph.identity<Int>(\"mapper\")\nlet operation = nefor.result.map(start, mapper)",
                    serde_json::json!({"constructor": "Error", "value": "failed"}),
                ),
                (
                    "result-map-error-ok-branch",
                    "let start = nefor.graph.source(\"start\", named(core.types.Result<String, Int>, Ok, 7))\nlet mapper = nefor.graph.identity<String>(\"mapper\")\nlet operation = nefor.result.map_error(start, mapper)",
                    serde_json::json!({"constructor": "Ok", "value": 7}),
                ),
            ] {
                let host = shipped_host();
                let source = format!(
                    "import core.types.{{}}\nimport nefor.artifact.{{}}\nimport nefor.graph.{{}}\nimport nefor.result.{{}}\n{operation}\nnefor.artifact.compile_graph(operation)"
                );
                let modification = compile_mag_source(&host, run_id, &source);
                let begun = host.begin_run(run_id, run_id, None).expect("begin run");
                assert!(begun.ok, "begin failed: {:?}", begun.error);
                host.drain_emits().expect("drain begin event");
                let outcome = host.start(run_id, &modification).expect("start run");
                assert!(outcome.ok, "start failed: {:?}", outcome.error);
                let completion = host
                    .take_run_complete(run_id)
                    .expect("read completion")
                    .expect("result composition completed");
                assert_eq!(completion.result.as_ref().map(|result| &result["value"]), Some(&expected));
            }
        }

        fn unit_root_program(definitions: &str) -> String {
            format!(
                r#"import nefor.artifact.{{}}
import nefor.graph.{{}}
import nefor.node.{{}}
import nefor.shell.{{}}
import nefor.contracts.{{}}
type MessageContent {{content: String}}
let params = nefor.shell.ShellScriptParams {{script: "printf root-ok", cwd: ".", timeout: named(nefor.contracts.Timeout, Unlimited, nil)}}
{definitions}
let result = nefor.graph.output_for("result", operation)
nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(operation, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)"#
            )
        }

        #[test]
        fn unit_accepting_roots_lower_only_the_exposed_unfed_boundary() {
            let host = shipped_host();
            for (name, definitions, expected) in [
                (
                    "unit-root-run",
                    "let operation = nefor.shell.script(\"command\", params)",
                    "command",
                ),
                (
                    "unit-root-script",
                    "let operation = nefor.shell.script(\"command\", params)",
                    "command",
                ),
                (
                    "unit-root-sequence",
                    r#"
let one = nefor.shell.script("one", params)
let two = nefor.shell.script("two", params)
let inner = nefor.node.sequence([one, two])
let sequence = nefor.node.sequence([inner])
let operation = nefor.node.named("outer", sequence)"#,
                    ".input",
                ),
                (
                    "unit-root-dependent",
                    r#"
let operation = nefor.node.then("ordered", nefor.shell.script("dependency", params), nefor.shell.script("command", params))"#,
                    "dependency",
                ),
            ] {
                let source = unit_root_program(definitions);
                let modification = compile_mag_source(&host, name, &source);
                assert_eq!(
                    modification,
                    compile_mag_source(&host, name, &source),
                    "deterministic lowering"
                );
                let messages = modification["messages"].as_array().unwrap();
                assert_eq!(messages.len(), 1, "{name}: {messages:?}");
                let target = messages[0]["to"].as_str().unwrap();
                if expected.starts_with('.') {
                    assert!(target.ends_with(expected), "{name}: {target}");
                } else {
                    assert_eq!(target, expected);
                }
                assert_eq!(
                    messages[0]["semantic_type"],
                    serde_json::json!({"kind": "primitive", "name": "Unit"})
                );
                assert_eq!(messages[0]["content"]["value"], JsonValue::Null);
            }
        }

        #[test]
        fn unit_accepting_roots_respect_explicit_messages_and_reject_missing_inputs() {
            let host = shipped_host();
            let explicit = r#"
import nefor.artifact.{}
import nefor.graph.{}
import nefor.shell.{}
import nefor.contracts.{}
let command = nefor.shell.script("command", nefor.shell.ShellScriptParams {script: "cat", cwd: ".", timeout: named(nefor.contracts.Timeout, Unlimited, nil)})
let text = nil
nefor.artifact.delta(nefor.graph.delta_message(nefor.graph.node_delta(command), get(command, "input"), text))"#;
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let artifact = nefor_mag::compile_with_inputs_and_module_roots_and_options_and_syntax(
                explicit,
                &manifest,
                serde_json::json!({"factory_contracts": host.registry_contracts().unwrap()}),
                &[manifest.join("../../mag/lib")],
                nefor_mag::CompilerOptions::default(),
                nefor_mag::SyntaxMode::New,
            )
            .expect("compile explicit delta");
            let modification = crate::artifact_delta(&artifact).expect("normalize delta envelope");
            let messages = modification["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0]["content"]["value"], JsonValue::Null);
            assert_eq!(messages[0]["semantic_type"]["name"], "Unit");

            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            for (name, definitions, expected) in [
                (
                    "text",
                    "let operation = nefor.graph.identity<MessageContent>(\"missing\")",
                    ["root validation failed", "main.MessageContent"],
                ),
                (
                    "product",
                    "let operation = nefor.graph.identity<(Unit, Unit)>(\"missing\")",
                    ["root validation failed", "product"],
                ),
                (
                    "non-unit",
                    "let operation = nefor.graph.identity<MessageContent>(\"missing\")",
                    ["root validation failed", "MessageContent"],
                ),
                (
                    "internal",
                    r#"
let command = nefor.shell.script("command", params)
let hidden = nefor.graph.identity<Unit>("hidden")
let operation = nefor.graph.node_with_operations_and_nodes("wrapper", "ordinary", concat(get(command, "actors"), get(hidden, "actors")), get(command, "routes"), [], [], concat(get(command, "nodes"), get(hidden, "nodes")), get(command, "input"), get(command, "output"))"#,
                    ["input coverage failed", "hidden.nefor.graph.Value"],
                ),
            ] {
                let error = nefor_mag::compile_with_inputs_and_module_roots_and_options_and_syntax(
                    &unit_root_program(definitions),
                    &manifest,
                    serde_json::json!({"factory_contracts": host.registry_contracts().unwrap()}),
                    &[manifest.join("../../mag/lib")],
                    nefor_mag::CompilerOptions::default(),
                    nefor_mag::SyntaxMode::New,
                )
                .expect_err(name)
                .to_string();
                for fragment in expected {
                    assert!(
                        error.contains(fragment),
                        "{name}: missing {fragment:?}: {error}"
                    );
                }
            }

            let fixture_root = manifest.join("../../examples/nefor-agent/mag/tests");
            for (fixture, expected) in [
                (
                    "invalid-product-underfill.mag",
                    [
                        "input coverage failed",
                        "join.test.Value",
                        "left.nefor.graph.Value",
                        "missing, extra, or ambiguous occurrences",
                    ],
                ),
                (
                    "invalid-uncovered-sum-arm.mag",
                    [
                        "output coverage failed",
                        "split.nefor.adt.Second",
                        "main.Right",
                        "coverage is incomplete",
                    ],
                ),
            ] {
                let source = std::fs::read_to_string(fixture_root.join(fixture)).unwrap();
                let error = nefor_mag::compile_with_inputs_and_module_roots_and_options_and_syntax(
                    &source,
                    &fixture_root,
                    serde_json::json!({"factory_contracts": host.registry_contracts().unwrap()}),
                    &[manifest.join("../../mag/lib")],
                    nefor_mag::CompilerOptions::default(),
                    nefor_mag::SyntaxMode::New,
                )
                .expect_err(fixture)
                .to_string();
                for fragment in expected {
                    assert!(
                        error.contains(fragment),
                        "{fixture}: missing {fragment:?}: {error}"
                    );
                }
            }
        }

        // Execute only the harmless command requested by the shipped shell factory;
        // the capability response then re-enters the real kernel routing path.
        fn execute_unit_root_command(host: &LuaHost, invoke: &Map<String, JsonValue>) {
            let args = &invoke["args"]["args"];
            let output = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(args["script"].as_str().unwrap())
                .current_dir(args["cwd"].as_str().unwrap())
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(output.status.success());
            host.bus_response(
                invoke["id"].as_str().unwrap(),
                Some(&serde_json::json!({
                    "stdout": String::from_utf8(output.stdout).unwrap(),
                    "stderr": String::from_utf8(output.stderr).unwrap(),
                    "termination": {"kind": "code", "code": output.status.code().unwrap()}
                })),
                None,
                Some("async"),
            )
            .unwrap();
        }

        #[test]
        fn nefor_mag_single_command_guide_executes_without_agent() {
            let host = shipped_host();
            let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
            let markdown = std::fs::read_to_string(
                repository.join("mag/book/02. nefor/00. Nefor MAG in Five Minutes.md"),
            )
            .expect("read Nefor guide");
            let source = markdown
                .split_once("```mag\n")
                .and_then(|(_, rest)| rest.split_once("\n```").map(|(source, _)| source))
                .expect("Nefor guide starts with a complete single-command program");
            let run_id = "guide-single-command";
            let modification = compile_mag_source(&host, run_id, source);
            assert!(host.begin_run(run_id, run_id, None).unwrap().ok);
            host.drain_emits().unwrap();
            let started = host.start(run_id, &modification).unwrap();
            assert!(started.ok, "{:?}", started.error);
            let emits = host.drain_emits().unwrap();
            assert_eq!(
                emits
                    .iter()
                    .filter(|event| event["kind"] == "tool.invoke")
                    .count(),
                1,
                "the documented graph dispatches one shell command"
            );
            assert!(host.take_run_complete(run_id).unwrap().is_none());
            execute_unit_root_command(&host, tool_invoke(&emits, "shell.script"));
            let completion = host
                .take_run_complete(run_id)
                .unwrap()
                .expect("shell output completes the documented graph");
            let result = completion.result.unwrap();
            assert_eq!(result["value"]["stdout"], "hello\n");
            assert_eq!(result["value"]["stderr"], "");
            assert!(
                !host
                    .drain_emits()
                    .unwrap()
                    .iter()
                    .any(|event| event["kind"] == "tool.invoke"),
                "command completion needs no agent or further tool invocation"
            );
        }

        #[test]
        fn unit_accepting_roots_execute_once_and_dependencies_do_not_start_early() {
            for (name, definitions, dependent) in [
                (
                    "unit-runtime-run",
                    "let operation = nefor.shell.script(\"command\", params)",
                    false,
                ),
                (
                    "unit-runtime-script",
                    "let operation = nefor.shell.script(\"command\", params)",
                    false,
                ),
                (
                    "unit-runtime-dependent",
                    r#"
let operation = nefor.node.then("ordered", nefor.shell.script("dependency", params), nefor.shell.script("command", params))"#,
                    true,
                ),
            ] {
                let host = shipped_host();
                let modification = compile_mag_source(&host, name, &unit_root_program(definitions));
                assert!(host.begin_run(name, name, None).unwrap().ok);
                host.drain_emits().unwrap();
                let started = host.start(name, &modification).unwrap();
                assert!(started.ok, "{:?}", started.error);
                let emits = host.drain_emits().unwrap();
                let invocations: Vec<_> = emits
                    .iter()
                    .filter(|event| event["kind"] == "tool.invoke")
                    .collect();
                assert_eq!(invocations.len(), 1, "only the root starts");
                let invoke = tool_invoke(&emits, "shell.script");
                assert_eq!(
                    invoke["from"],
                    if dependent { "dependency" } else { "command" }
                );
                assert!(host.take_run_complete(name).unwrap().is_none());
                execute_unit_root_command(&host, invoke);
                let emits = host.drain_emits().unwrap();
                if dependent {
                    assert!(host.take_run_complete(name).unwrap().is_none());
                    assert_eq!(
                        emits
                            .iter()
                            .filter(|event| event["kind"] == "tool.invoke")
                            .count(),
                        1
                    );
                    let invoke = tool_invoke(&emits, "shell.script");
                    assert_eq!(invoke["from"], "command");
                    execute_unit_root_command(&host, invoke);
                } else {
                    assert!(!emits.iter().any(|event| event["kind"] == "tool.invoke"));
                }
                let completion = host
                    .take_run_complete(name)
                    .unwrap()
                    .expect("completed command");
                assert_eq!(completion.result.unwrap()["value"]["stdout"], "root-ok");
                assert!(
                    !host
                        .drain_emits()
                        .unwrap()
                        .iter()
                        .any(|event| event["kind"] == "tool.invoke"),
                    "no duplicate execution after completion"
                );
            }
        }

        fn start_shell_expression(
            host: &LuaHost,
            run_id: &str,
            expression: &str,
        ) -> Vec<Map<String, JsonValue>> {
            let modification = compile_mag_eval_expression(host, run_id, expression);
            let begun = host
                .begin_run(run_id, run_id, None)
                .expect("begin shell run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin event");
            let outcome = host.start(run_id, &modification).expect("start shell run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            host.drain_emits().expect("drain shell start")
        }

        fn tool_invoke<'a>(
            emits: &'a [Map<String, JsonValue>],
            command: &str,
        ) -> &'a Map<String, JsonValue> {
            emits
                .iter()
                .find(|event| {
                    event.get("kind").and_then(JsonValue::as_str) == Some("tool.invoke")
                        && event.get("name").and_then(JsonValue::as_str) == Some(command)
                })
                .unwrap_or_else(|| panic!("missing tool.invoke for {command}: {emits:#?}"))
        }

        #[test]
        fn loads_a_table_returning_kernel() {
            let dir = std::env::temp_dir().join(format!("mag-kernel-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("mkdir");
            let path = write_kernel(&dir, "nefor.log(\"hi\")\nreturn { name = \"k\" }");
            let host = LuaHost::load_kernel(&path, None).expect("load");
            assert_eq!(host.kernel_name().as_deref(), Some("k"));
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn rejects_non_table_kernel() {
            let dir = std::env::temp_dir().join(format!("mag-kernel-nt-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("mkdir");
            let path = write_kernel(&dir, "return 42");
            let err = match LuaHost::load_kernel(&path, None) {
                Ok(_) => panic!("expected KernelNotTable error"),
                Err(e) => e,
            };
            assert!(matches!(err, MagError::KernelNotTable { .. }));
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn surfaces_missing_kernel_file() {
            let err = match LuaHost::load_kernel(
                std::path::Path::new("/nonexistent/mag/kernel.lua"),
                None,
            ) {
                Ok(_) => panic!("expected KernelRead error"),
                Err(e) => e,
            };
            assert!(matches!(err, MagError::KernelRead { .. }));
        }

        #[test]
        fn json_and_now_and_emit_bindings_are_installed() {
            let dir = std::env::temp_dir().join(format!("mag-kernel-bind-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("mkdir");
            // A kernel that exercises the new native surface and returns a table.
            let path = write_kernel(
                &dir,
                r#"
                assert(type(nefor.json) == "table", "json missing")
                assert(nefor.json.decode(nefor.json.encode({a=1})).a == 1, "json roundtrip")
                assert(type(nefor.now_ms) == "function" and nefor.now_ms() > 0, "now_ms")
                assert(type(nefor.fs.data_root) == "function", "fs.data_root")
                nefor.emit({ kind = "test.event", n = 7 })
                return { name = "bindings" }
                "#,
            );
            let host = LuaHost::load_kernel(&path, None).expect("load");
            let drained = host.drain_emits().expect("drain");
            assert_eq!(drained.len(), 1, "one queued emit");
            assert_eq!(
                drained[0].get("kind").and_then(JsonValue::as_str),
                Some("test.event")
            );
            // Draining again yields an empty queue.
            assert!(host.drain_emits().expect("drain2").is_empty());
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn reads_registry_contract_snapshot_as_json() {
            let dir =
                std::env::temp_dir().join(format!("mag-kernel-contract-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("mkdir");
            let path = write_kernel(
                &dir,
                r#"
                return {
                  registry_contracts = function()
                    return {{
                      identity = "nefor.factory.example",
                      implementation = "example",
                      params = { count = "int" },
                      type_scheme = {
                        variables = { "T" },
                        inputs = { value = "T" },
                        outputs = { "T" },
                      },
                      signals = {},
                    }}
                  end,
                }
                "#,
            );
            let host = LuaHost::load_kernel(&path, None).expect("load");
            let contracts = host.registry_contracts().expect("contracts");
            assert_eq!(contracts[0]["identity"], "nefor.factory.example");
            assert_eq!(contracts[0]["type_scheme"]["variables"][0], "T");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn process_capability_invocation_carries_authoritative_run_provenance() {
            let host = shipped_host();
            let run_id = "provenance-run";
            let expression = r#"nefor.process.exec("command", nefor.process.ProcessExecParams {argv: ["printf", "provenance"], cwd: nefor.process.cwd, timeout: named(nefor.contracts.Timeout, Unlimited, nil)})"#;
            let modification = compile_mag_eval_expression(&host, run_id, expression);
            let begun = host
                .begin_run_with_principal(
                    run_id,
                    "scout",
                    Some("session-1"),
                    Some("subagent"),
                    Some("conversation-1"),
                    None,
                )
                .expect("begin provenance run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin event");
            let outcome = host.start(run_id, &modification).expect("start run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let emits = host.drain_emits().expect("drain start");
            let invoke = tool_invoke(&emits, "process.exec");
            assert_eq!(
                invoke["args"]["args"]["argv"],
                serde_json::json!(["printf", "provenance"])
            );
            let provenance = invoke["invocation"].as_object().expect("provenance");
            assert_eq!(provenance["session_id"], "session-1");
            assert_eq!(provenance["run_id"], run_id);
            assert_eq!(provenance["principal"], "subagent");
            assert_eq!(provenance["actor_id"], invoke["from"]);
            assert_eq!(provenance["capability_id"], invoke["id"]);
            assert_eq!(provenance["root_conversation_id"], "conversation-1");
        }

        #[test]
        fn process_and_script_keep_structured_results_and_explicit_timeouts() {
            let host = shipped_host();
            let expression = r#"nefor.shell.script("script", nefor.shell.ShellScriptParams {script: "printf output", cwd: nefor.process.cwd, timeout: named(nefor.contracts.Timeout, Milliseconds, 30000)})"#;
            let emits = start_shell_expression(&host, "shell-script", expression);
            let invoke = tool_invoke(&emits, "shell.script");
            assert_eq!(invoke["args"]["args"]["cwd"], ".");
            assert_eq!(
                invoke["args"]["args"]["timeout"],
                serde_json::json!({"present": true, "milliseconds": 30000})
            );
            let id = invoke["id"].as_str().expect("correlation id");
            assert_eq!(
                host.bus_response(
                    id,
                    Some(&serde_json::json!({
                        "stdout": "output", "stderr": "warning",
                        "termination": {"kind": "code", "code": 9}
                    })),
                    None,
                    Some("async")
                )
                .expect("structured response"),
                Some("shell-script".into())
            );
            let completion = host
                .take_run_complete("shell-script")
                .expect("completion")
                .expect("nonzero process result completes normally");
            let value = completion.result.expect("typed result");
            assert_eq!(value["value"]["stdout"], "output");
            assert_eq!(value["value"]["stderr"], "warning");
            assert_eq!(
                value["value"]["termination"]["constructor"],
                "ProcessExited"
            );
            assert_eq!(value["value"]["termination"]["value"]["code"], 9);

            let expression = r#"nefor.process.exec("signaled", nefor.process.ProcessExecParams {argv: ["sleep", "5"], cwd: nefor.process.cwd, timeout: named(nefor.contracts.Timeout, Unlimited, nil)})"#;
            let emits = start_shell_expression(&host, "process-signaled", expression);
            let invoke = tool_invoke(&emits, "process.exec");
            assert_eq!(
                invoke["args"]["args"]["argv"],
                serde_json::json!(["sleep", "5"])
            );
            assert!(invoke["args"]["args"].get("exited_type").is_none());
            assert!(invoke["args"]["args"].get("signaled_type").is_none());
            let id = invoke["id"].as_str().expect("correlation id");
            assert_eq!(
                host.bus_response(
                    id,
                    Some(&serde_json::json!({
                        "stdout": "partial", "stderr": "terminated",
                        "termination": {"kind": "signal", "signal": 15}
                    })),
                    None,
                    Some("async")
                )
                .expect("structured response"),
                Some("process-signaled".into())
            );
            let completion = host
                .take_run_complete("process-signaled")
                .expect("completion")
                .expect("signal termination completes normally");
            let value = completion.result.expect("typed result");
            let signaled = &value["value"]["termination"];
            assert_eq!(signaled["constructor"], "ProcessSignaled");
            assert_eq!(signaled["value"]["signal"], 15);
        }

        #[test]
        fn malformed_and_nonpositive_process_params_fail_before_invocation() {
            let host = shipped_host();
            for milliseconds in [0, -1] {
                let source = format!(
                    r#"import nefor.artifact.{{}}
import nefor.contracts.{{}}
import nefor.process.{{}}
let operation = nefor.process.exec("invalid", nefor.process.ProcessExecParams {{argv: ["true"], cwd: ".", timeout: named(nefor.contracts.Timeout, Milliseconds, {milliseconds})}})
nefor.artifact.compile_graph(operation)"#
                );
                let error = compile_mag_source_error(&host, &source);
                assert!(
                    error.contains("Timeout must be strictly positive"),
                    "{error}"
                );
            }

            let run_id = "process-empty";
            let expression = r#"nefor.process.exec("invalid", nefor.process.ProcessExecParams {argv: ([]: List<String>), cwd: ".", timeout: named(nefor.contracts.Timeout, Unlimited, nil)})"#;
            let emits = start_shell_expression(&host, run_id, expression);
            assert!(emits.iter().all(|event| {
                event.get("kind").and_then(JsonValue::as_str) != Some("tool.invoke")
            }));
            assert!(host
                .take_run_failed(run_id)
                .expect("invalid run failure")
                .is_some());
        }

        #[test]
        fn ordinary_source_node_output_graph_executes_to_its_typed_result() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "ordinary-source-node-output",
                r#"
    import core.map.{}
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    type MessageContent {content: String}

    let start = nefor.graph.source("start", MessageContent {content: "ordinary"})
    let input = nefor.graph.port("echo", type_tag<MessageContent>(), "stub.In")
    let output = nefor.graph.port("echo", type_tag<MessageContent>(), "stub.Out")
    let actor = nefor.graph.actor("echo", "nefor.factory.stub", [], core.map.empty<String, String>(), nefor.graph.store_port(input), [nefor.graph.store_port(output)])
    let echo = nefor.graph.node("echo", "ordinary", [actor], ([]: List<nefor.graph.StoredRoute>), ([]: List<nefor.graph.Message>), input, output)
    let result = nefor.graph.output_for("result", echo)
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, echo), nefor.graph.edge(echo, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
                "#,
            );

            let begun = host
                .begin_run(
                    "ordinary-source-node-output",
                    "ordinary-source-node-output",
                    None,
                )
                .expect("begin ordinary run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin event");
            let outcome = host
                .start("ordinary-source-node-output", &modification)
                .expect("start ordinary run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let completion = host
                .take_run_complete("ordinary-source-node-output")
                .expect("read completion")
                .expect("ordinary graph completes");
            let result = completion.result.expect("typed output result");
            assert_eq!(result["kind"], "nefor.graph.Value");
            assert_eq!(result["value"]["content"], "ordinary");
            assert!(host
                .take_run_failed("ordinary-source-node-output")
                .expect("read failure")
                .is_none());
        }

        #[test]
        fn factory_output_with_correct_type_id_but_malformed_value_fails_the_run() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "malformed-typed-output",
                r#"
    import core.map.{}
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    type MessageContent {content: String}

    let start = nefor.graph.source("start", MessageContent {content: "valid"})
    let input = nefor.graph.port("broken", type_tag<MessageContent>(), "stub.In")
    let output = nefor.graph.port("broken", type_tag<MessageContent>(), "stub.Out")
    let actor = nefor.graph.actor("broken", "nefor.factory.stub", [], core.map.insert(core.map.empty<String, String>(), "value", "not-a-Text-record"), nefor.graph.store_port(input), [nefor.graph.store_port(output)])
    let broken = nefor.graph.node("broken", "ordinary", [actor], ([]: List<nefor.graph.StoredRoute>), ([]: List<nefor.graph.Message>), input, output)
    let result = nefor.graph.output_for("result", broken)
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, broken), nefor.graph.edge(broken, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
                "#,
            );
            let run_id = "malformed-typed-output";
            assert!(host.begin_run(run_id, run_id, None).expect("begin").ok);
            host.drain_emits().expect("drain begin event");
            let outcome = host.start(run_id, &modification).expect("start");
            assert!(outcome.ok, "modification remains structurally valid");
            let failure = host
                .take_run_failed(run_id)
                .expect("read failure")
                .expect("malformed factory value fails");
            assert!(failure.contains("malformed semantic value"), "{failure}");
            assert!(
                failure.contains("expected main.MessageContent"),
                "{failure}"
            );
            assert!(host
                .take_run_complete(run_id)
                .expect("read completion")
                .is_none());
        }

        #[test]
        fn whole_product_reaches_output_through_ordinary_firing() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "whole-product-output",
                r#"
    import nefor.artifact.{}
    import nefor.contracts.{}
    import nefor.graph.{}
    type MessageContent {content: String}

    let start = nefor.graph.source("start", (MessageContent {content: "left"}, MessageContent {content: "right"}))
    let result = nefor.graph.output<(MessageContent, MessageContent)>("result")
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
                "#,
            );

            let begun = host
                .begin_run("whole-product-output", "whole-product-output", None)
                .expect("begin product run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin event");
            let outcome = host
                .start("whole-product-output", &modification)
                .expect("start product run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let completion = host
                .take_run_complete("whole-product-output")
                .expect("read completion")
                .expect("whole product graph completes");
            let result = completion.result.expect("typed product result");
            assert_eq!(result["kind"], "nefor.graph.Value");
            assert_eq!(result["value"][0]["content"], "left");
            assert_eq!(result["value"][1]["content"], "right");
            assert!(result["semantic_type_id"].as_str().is_some());
            assert!(result["constructor_id"].is_null());
        }

        #[test]
        fn component_edges_assemble_an_ordered_product_at_the_output() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "component-product-output",
                r#"
    import nefor.artifact.{}
    import nefor.graph.{}

    type Left {content: String}
    type Right {count: Int}

    let left = nefor.graph.source("left", Left {content: "first"})
    let right = nefor.graph.source("right", Right {count: 2})
    let result = nefor.graph.output<(Left, Right)>("result")
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(left, result), nefor.graph.edge(right, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
                "#,
            );
            let run_id = "component-product-output";
            assert!(host.begin_run(run_id, run_id, None).expect("begin").ok);
            host.drain_emits().expect("drain begin event");
            let outcome = host.start(run_id, &modification).expect("start");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let emits = host.drain_emits().expect("drain product events");
            let completion = host.take_run_complete(run_id).expect("read completion");
            assert!(
                completion.is_some(),
                "component product graph did not complete: {emits:?}"
            );
            let completion = completion.expect("checked above");
            let result = completion.result.expect("typed product result");
            assert_eq!(result["value"][0]["content"], "first");
            assert_eq!(result["value"][1]["count"], 2);
            assert!(result["semantic_type_id"].as_str().is_some());
            assert!(result["constructor_id"].is_null());
        }

        #[test]
        fn adt_arrival_routes_only_to_its_explicit_branch_and_keeps_constructor_id() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "direct-sum-routing",
                r#"
    import core.map.{}
    import core.types.{}
    import nefor.artifact.{}
    import nefor.graph.{}
    import nefor.node.{}

    type Left {value: String}
    type Right {value: Int}

    let branch<T>: fn(String, TypeTag<T>) -> nefor.graph.Node<T, T> = |id, `type`| => {
      let input = nefor.graph.port(id, `type`, "stub.In")
      let output = nefor.graph.port(id, `type`, "stub.Out")
      let actor = nefor.graph.actor(id, "nefor.factory.stub", [], core.map.empty<String, String>(), nefor.graph.store_port(input), [nefor.graph.store_port(output)])
      nefor.graph.node(id, "ordinary", [actor], ([]: List<nefor.graph.StoredRoute>), ([]: List<nefor.graph.Message>), input, output)
    }

    let start = nefor.graph.source("start", named(core.types.Either<Left, Right>, Left, Left {value: "chosen"}))
    let left = branch("left", type_tag<Left>())
    let right = branch("right", type_tag<Right>())
    let selected = nefor.node.choose("selected", left, right)
    let result = nefor.graph.output<core.types.Either<Left, Right>>("result")
    nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, selected), nefor.graph.edge(selected, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
                "#,
            );
            let begun = host
                .begin_run("direct-sum-routing", "direct-sum-routing", None)
                .expect("begin direct sum run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            host.drain_emits().expect("drain begin event");
            let outcome = host
                .start("direct-sum-routing", &modification)
                .expect("start direct sum run");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let emits = host.drain_emits().expect("drain direct sum events");
            assert!(
                emits.iter().any(|event| {
                    event.get("kind").and_then(JsonValue::as_str) == Some("mag.actor_ready")
                        && event.get("id").and_then(JsonValue::as_str) == Some("left")
                }),
                "{emits:?}"
            );
            assert!(!emits.iter().any(|event| {
                event.get("kind").and_then(JsonValue::as_str) == Some("mag.actor_ready")
                    && event.get("id").and_then(JsonValue::as_str) == Some("right")
            }));
            let completion = host
                .take_run_complete("direct-sum-routing")
                .expect("read completion");
            assert!(
                completion.is_some(),
                "sum graph did not complete: {emits:?}"
            );
            let completion = completion.expect("checked above");
            let result = completion.result.expect("typed ADT result");
            assert_eq!(result["value"]["constructor"], "Left");
            assert_eq!(result["value"]["value"]["value"], "chosen");
            assert_ne!(result["semantic_type_id"], result["constructor_id"]);
            assert!(result["constructor_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("sha256:")));
        }

        #[test]
        fn result_bind_routes_ok_and_reconstructs_error_without_erasure() {
            let host = shipped_host();
            for (run_id, constructor, payload, expected) in [
                (
                    "result-bind-ok",
                    "Ok",
                    r#""accepted""#,
                    serde_json::json!({
                        "constructor":"Ok", "value":"accepted"
                    }),
                ),
                (
                    "result-bind-error",
                    "Error",
                    r#"Failure {message: "rejected"}"#,
                    serde_json::json!({"constructor":"Error","value":{"message":"rejected"}}),
                ),
            ] {
                let source = format!(
                    r#"
import core.types.{{}}
import nefor.artifact.{{}}
import nefor.graph.{{}}
import nefor.node.{{}}
import nefor.result.{{}}
type Failure {{message: String}}
let start = nefor.graph.source("start", named(core.types.Result<Failure, String>, {constructor}, {payload}))
let continuation = nefor.result.lift<String, Failure, String>("continued", nefor.graph.identity<String>("right"))
let bound = nefor.result.`>=>`(start, continuation)
let result = nefor.graph.output_for("result", bound)
nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(bound, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
"#
                );
                let modification = compile_mag_source(&host, run_id, &source);
                assert!(host.begin_run(run_id, run_id, None).expect("begin").ok);
                host.drain_emits().expect("drain begin");
                let outcome = host.start(run_id, &modification).expect("start");
                assert!(outcome.ok, "{run_id}: {:?}", outcome.error);
                let emits = host.drain_emits().expect("drain");
                let completion = host
                    .take_run_complete(run_id)
                    .expect("take")
                    .expect("complete");
                assert_eq!(completion.result.expect("result")["value"], expected);
                let right_ran = emits
                    .iter()
                    .any(|event| event["kind"] == "mag.actor_ready" && event["id"] == "right");
                assert_eq!(right_ran, constructor == "Ok");
                host.end_run(run_id, TeardownReason::RunComplete)
                    .expect("end");
                host.drain_emits().expect("drain end");
            }
        }

        #[test]
        fn result_map_error_transforms_error_and_preserves_ok() {
            let host = shipped_host();
            for (run_id, constructor, payload, expected) in [
                (
                    "result-map-error",
                    "Error",
                    r#"Failure {message: "rejected"}"#,
                    serde_json::json!({"constructor":"Error","value":"mapped"}),
                ),
                (
                    "result-map-ok",
                    "Ok",
                    r#""accepted""#,
                    serde_json::json!({"constructor":"Ok","value":"accepted"}),
                ),
            ] {
                let source = format!(
                    r#"
import core.map.{{}}
import core.types.{{}}
import nefor.artifact.{{}}
import nefor.graph.{{}}
import nefor.result.{{}}
type Failure {{message: String}}
let start = nefor.graph.source("start", named(core.types.Result<Failure, String>, {constructor}, {payload}))
let mapper_input = nefor.graph.port("map-error", type_tag<Failure>(), "stub.In")
let mapper_output = nefor.graph.port("map-error", type_tag<String>(), "stub.Out")
let mapper_actor = nefor.graph.actor("map-error", "nefor.factory.stub", [], core.map.insert(core.map.empty<String, String>(), "value", "mapped"), nefor.graph.store_port(mapper_input), [nefor.graph.store_port(mapper_output)])
let mapper = nefor.graph.node("map-error", "ordinary", [mapper_actor], ([]: List<nefor.graph.StoredRoute>), ([]: List<nefor.graph.Message>), mapper_input, mapper_output)
let mapped = nefor.result.map_error(start, mapper)
let result = nefor.graph.output_for("result", mapped)
nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(mapped, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
"#
                );
                let modification = compile_mag_source(&host, run_id, &source);
                assert!(host.begin_run(run_id, run_id, None).expect("begin").ok);
                host.drain_emits().expect("drain begin");
                let outcome = host.start(run_id, &modification).expect("start");
                assert!(outcome.ok, "{run_id}: {:?}", outcome.error);
                let emits = host.drain_emits().expect("drain");
                let completion = host
                    .take_run_complete(run_id)
                    .expect("take")
                    .unwrap_or_else(|| panic!("{run_id} did not complete: {emits:?}"));
                assert_eq!(completion.result.expect("result")["value"], expected);
                let mapper_ran = emits
                    .iter()
                    .any(|event| event["kind"] == "mag.actor_ready" && event["id"] == "map-error");
                assert_eq!(mapper_ran, constructor == "Error");
                host.end_run(run_id, TeardownReason::RunComplete)
                    .expect("end");
                host.drain_emits().expect("drain end");
            }
        }

        #[test]
        fn nonpositive_timeout_is_rejected_during_compilation() {
            let host = shipped_host();
            let source = r#"import nefor.artifact.{}
import nefor.contracts.{}
import nefor.process.{}
let operation = nefor.process.exec("broken", nefor.process.ProcessExecParams {argv: ["true"], cwd: nefor.process.cwd, timeout: named(nefor.contracts.Timeout, Milliseconds, 0)})
nefor.artifact.compile_graph(operation)"#;
            let error = compile_mag_source_error(&host, source);
            assert!(
                error.contains("Timeout must be strictly positive"),
                "{error}"
            );
        }

        #[test]
        fn structural_result_boundary_accepts_arbitrary_declared_wire() {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let path = manifest.join("lua/mag-kernel/init.lua");
            let lua_root = manifest.join("../../lua");
            let host = LuaHost::load_kernel(&path, Some(&lua_root)).expect("load shipped kernel");
            let begun = host
                .begin_run("custom-result", "custom-result", None)
                .expect("begin run");
            assert!(begun.ok, "begin failed: {:?}", begun.error);
            let modification = serde_json::json!({
                "actors": [{
                    "id": "custom",
                    "factory": "nefor.factory.stub",
                    "type_arguments": [],
                    "params": {"greeting": "done"},
                    "routes": {}
                }],
                "messages": [{"to": "custom", "content": {"kind": "stub.In"}}],
                "kills": [],
                "result": {"from": {
                    "actor": "custom",
                    "type": "example.CustomResult",
                    "wire": "stub.Out"
                }}
            });
            let outcome = host.start("custom-result", &modification).expect("start");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            let completion = host
                .take_run_complete("custom-result")
                .expect("read completion")
                .expect("custom result completed");
            assert_eq!(
                completion.result.as_ref().and_then(|v| v["kind"].as_str()),
                Some("stub.Out")
            );
            assert_eq!(
                completion
                    .result
                    .as_ref()
                    .and_then(|v| v["greeting"].as_str()),
                Some("done")
            );
        }

        #[test]
        fn rejected_modifications_leave_no_partial_inventory_changes() {
            let host = shipped_host();
            let run_id = "atomic-rejection";
            assert!(host.begin_run(run_id, run_id, None).expect("begin").ok);
            host.drain_emits().expect("drain begin event");

            let rejected = host
                .apply(
                    run_id,
                    &serde_json::json!({
                        "types": {},
                        "actors": [{
                            "id": "tentative",
                            "factory": "nefor.factory.stub",
                            "type_arguments": [],
                            "params": {},
                            "routes": {}
                        }],
                        "messages": [{"to": "missing", "content": {"kind": "stub.In"}}],
                        "nodes": [{"path": ["tentative"], "members": ["tentative"]}],
                        "kills": []
                    }),
                )
                .expect("apply rejected modification");
            assert!(!rejected.ok);
            assert!(
                rejected
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("unknown message target 'missing'")),
                "{rejected:?}"
            );

            let probe = host
                .apply(
                    run_id,
                    &serde_json::json!({
                        "types": {},
                        "actors": [],
                        "messages": [{"to": "tentative", "content": {"kind": "stub.In"}}],
                        "nodes": [],
                        "kills": []
                    }),
                )
                .expect("probe inventory after rejection");
            assert!(!probe.ok);
            assert!(probe
                .error
                .as_deref()
                .is_some_and(|error| { error.contains("unknown message target 'tentative'") }));
        }
    }
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn unsupported_provider_schema_rejects_before_run_start_or_provider_dispatch() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let host = LuaHost::load_kernel(
            &manifest.join("lua/mag-kernel/init.lua"),
            Some(&manifest.join("../../lua")),
        )
        .expect("kernel");
        let body = serde_json::json!({
            "run_id": "unsupported-schema",
            "session_id": "session-1",
            "artifact": {"format":"nefor.mag","version":2,"kind":"program","program":{
                "initial": {
                    "types": {},
                    "actors": [{
                        "id": "answer",
                        "factory": "structured-output",
                        "params": {"$mag":"packed-value","value":{"schema": {"version": 1, "root": {"kind": "json_value"}}}}
                    }],
                    "messages": [], "nodes": [], "kills": [], "result": {}
                },
                "operations": []
            }}
        });
        let (out_tx, mut out_rx) = mpsc::channel(CHANNEL_CAP);
        let mut active = ActiveExecutes::new();
        let mut bridge = CapabilityBridge::new("tool-gate");
        handle_execute(
            &out_tx,
            "direct",
            body.as_object().expect("execute body"),
            Some("execute-1"),
            (&host, &mut active, &mut bridge),
        )
        .await
        .expect("rejection is a protocol response");

        let outgoing = out_rx.try_recv().expect("one rejection response");
        let Body::Event(body) = outgoing.body else {
            panic!("expected event response")
        };
        assert_eq!(body["kind"], ERROR_KIND);
        assert!(body["message"]
            .as_str()
            .is_some_and(|message| message.contains("no faithful")));
        assert!(
            out_rx.try_recv().is_err(),
            "no provider dispatch follows rejection"
        );
        assert!(active.is_empty(), "rejected execute never becomes active");
        assert!(
            host.drain_emits().expect("kernel emits").is_empty(),
            "begin_run was never called, so mag.run_started was not emitted"
        );
    }

    #[tokio::test]
    async fn synchronous_execute_publishes_runtime_owned_duration() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let host = LuaHost::load_kernel(
            &manifest.join("lua/mag-kernel/init.lua"),
            Some(&manifest.join("../../lua")),
        )
        .expect("kernel");
        let body = serde_json::json!({
            "run_id": "synchronous-duration",
            "session_id": "session-1",
            "artifact": {"format":"nefor.mag","version":2,"kind":"program","program":{
                "initial": {
                    "types": {},
                    "actors": [{
                        "id": "sync",
                        "factory": "nefor.factory.stub",
                        "type_arguments": [],
                        "params": {"$mag": "packed-value", "value": {"greeting": "done"}},
                        "routes": {}
                    }],
                    "messages": [{"to": "sync", "content": {"$mag": "packed-value", "value": {"kind": "stub.In"}}}],
                    "nodes": [{"path": ["sync"], "members": ["sync"]}],
                    "kills": [],
                    "result": {"from": {"actor": "sync", "type": "example.Result", "type_id": "sha256:test-result", "wire": "stub.Out"}}
                },
                "operations": []
            }}
        });
        let (out_tx, mut out_rx) = mpsc::channel(CHANNEL_CAP);
        let mut active = ActiveExecutes::new();
        let mut bridge = CapabilityBridge::new("tool-gate");
        handle_execute(
            &out_tx,
            "direct",
            body.as_object().expect("execute body"),
            Some("execute-sync"),
            (&host, &mut active, &mut bridge),
        )
        .await
        .expect("synchronous execution");

        let mut events = Vec::new();
        while let Ok(outgoing) = out_rx.try_recv() {
            let Body::Event(body) = outgoing.body else {
                continue;
            };
            events.push(body);
        }
        let result = events
            .iter()
            .find(|body| body.get("kind").and_then(Value::as_str) == Some(RUN_RESULT_KIND))
            .unwrap_or_else(|| panic!("terminal run result missing from {events:?}"));
        assert_eq!(
            result.get("status").and_then(Value::as_str),
            Some("completed"),
            "{result:?}"
        );
        assert!(result.get("duration_ms").and_then(Value::as_u64).is_some());
        assert!(active.is_empty(), "synchronous execution is never retained");
    }

    #[tokio::test]
    async fn malformed_agent_error_output_rejects_during_load_before_run_registration() {
        let root =
            std::env::temp_dir().join(format!("mag-malformed-agent-output-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace");
        fs::write(
            root.join("main.mag"),
            r#"
import core.types.{}
import nefor.actors.{}
import nefor.artifact.{}
import nefor.contracts.{}
import nefor.graph.{}
type InvestigationInput {prompt: String}
let exact_model: fn(nefor.actors.ResolvedModel) -> nefor.actors.AuthoredModel = |selected| => named(nefor.actors.AuthoredModel, ResolvedModel, selected)
let configured_model = nefor.actors.ResolvedModel {provider: "mock-provider", model: "mock-model", reasoning_effort: nefor.actors.reasoning_effort("medium")}
let start = nefor.graph.source("task", InvestigationInput {prompt: "test"})
let worker = nefor.actors.agent<nefor.actors.ResolvedModel, InvestigationInput, core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>("worker", exact_model, nefor.actors.AgentConfig<nefor.actors.ResolvedModel> {model: configured_model, system: "Answer.", tools: ([]: List<String>), tool_approval_policy: named(nefor.contracts.ToolApprovalPolicy, Default, nil), max_corrections: 0})
let result = nefor.graph.output<core.types.Result<nefor.contracts.AgentError, core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>> >("result")
nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, worker), nefor.graph.edge(worker, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
"#,
        )
        .expect("program");
        let module_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
        let config_module_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/nefor-agent/mag/lib");
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let host = LuaHost::load_kernel(
            &manifest.join("lua/mag-kernel/init.lua"),
            Some(&manifest.join("../../lua")),
        )
        .expect("kernel");
        let body = serde_json::json!({
            "id": "load-malformed",
            "source_dir": root,
            "module_roots": [module_root, config_module_root],
            "entry": "main.mag"
        });
        let (out_tx, mut out_rx) = mpsc::channel(CHANNEL_CAP);
        handle_load(
            &out_tx,
            body.as_object().expect("load body"),
            Some("load-malformed"),
            &host,
        )
        .await
        .expect("load rejection is a protocol response");

        let outgoing = out_rx.try_recv().expect("load rejection");
        let Body::Event(body) = outgoing.body else {
            panic!("expected event response")
        };
        assert_eq!(body["kind"], ERROR_KIND);
        let message = body["message"].as_str().expect("actionable error");
        assert!(message.contains("worker.llm"), "{message}");
        assert!(message.contains("nefor.contracts.AgentError"), "{message}");
        assert!(message.contains("last_output"), "{message}");
        assert!(
            message.contains("pass only the success output type"),
            "{message}"
        );
        assert!(
            host.drain_emits().expect("kernel emits").is_empty(),
            "load rejection cannot emit mag.run_started"
        );
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn provider_events_append_only_deltas_and_preserve_terminal_semantic_result() {
        fn named(name: &str) -> Value {
            serde_json::json!({"kind":"named","name":name,"arguments":[]})
        }
        fn event(request_id: &str, event: &str, fields: Value) -> Map<String, Value> {
            let mut body = fields.as_object().expect("event fields").clone();
            body.insert(
                "kind".into(),
                Value::String("conversation.provider.event".into()),
            );
            body.insert("provider".into(), Value::String("provider-a".into()));
            body.insert("request_id".into(), Value::String(request_id.into()));
            body.insert("event".into(), Value::String(event.into()));
            body
        }

        let provider_input = named("nefor.contracts.ProviderInput");
        let tool_calls = named("nefor.contracts.ToolCalls");
        let text_answer = named("nefor.contracts.TextAnswer");
        let agent_error = named("nefor.contracts.AgentError");
        let result = serde_json::json!({
            "kind":"adt", "name":"core.types.Result",
            "arguments":[agent_error.clone(), text_answer.clone()],
            "constructors":[
                {"name":"Error","payload":agent_error.clone()},
                {"name":"Ok","payload":text_answer.clone()}
            ]
        });
        let modification = serde_json::json!({
            "actors": [{
                "id": "answer",
                "factory": "llm",
                "type_arguments": [result.clone()],
                "input": {"wire":"generic-provider.ProviderOut","type":provider_input},
                "outputs": [
                    {"wire":"generic-tool.ToolCalls","type":tool_calls},
                    {"wire":"nefor.agent.Result","type":result.clone()}
                ],
                "params": {
                    "provider":"provider-a",
                    "output_type":"text-answer",
                    "error_type":"agent-error",
                    "provider_error_type":"provider-error"
                },
                "routes": {"generic-tool.ToolCalls":[],"nefor.agent.Result":[]}
            }],
            "messages": [{
                "to": "answer",
                "content": {"kind":"generic-provider.ProviderOut","messages":[{"role":"user","content":"go"}]}
            }],
            "kills": [],
            "result": {"from": {
                "actor":"answer",
                "type":"nefor.agent.Result",
                "wire":"nefor.agent.Result"
            }}
        });

        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let host = LuaHost::load_kernel(
            &manifest.join("lua/mag-kernel/init.lua"),
            Some(&manifest.join("../../lua")),
        )
        .expect("kernel");
        assert!(
            host.begin_run("provider-events", "provider-events", None)
                .expect("begin")
                .ok
        );
        let started = host.start("provider-events", &modification).expect("start");
        assert!(
            started.ok,
            "{}",
            started.error.as_deref().unwrap_or("unknown error")
        );

        let mut bridge = CapabilityBridge::new("tool-gate");
        let request = host
            .drain_emits()
            .expect("initial emits")
            .into_iter()
            .flat_map(|body| bridge.translate_emit(body))
            .find(|body| {
                body.get("kind").and_then(Value::as_str)
                    == Some("conversation.provider.invoke.request")
            })
            .expect("provider request");
        let request_id = request["request_id"]
            .as_str()
            .expect("request id")
            .to_owned();
        let (out_tx, mut out_rx) = mpsc::channel(CHANNEL_CAP);
        let mut active = HashMap::from([(
            "provider-events".to_owned(),
            ActiveExecute {
                in_reply_to: None,
                started_at: Instant::now(),
            },
        )]);

        for body in [
            event(
                &request_id,
                "text_delta",
                serde_json::json!({"text":"visible"}),
            ),
            event(
                &request_id,
                "reasoning_delta",
                serde_json::json!({"text":"thinking"}),
            ),
            event(
                &request_id,
                "reasoning_end",
                serde_json::json!({"text":"must-not-append"}),
            ),
            event(
                &request_id,
                "tool_execution_started",
                serde_json::json!({
                    "tool_call_id":"web-1", "name":"web_search",
                    "arguments":{"action":"search","query":"rust"},
                    "provider_context":{"encrypted":"must-not-leak"}
                }),
            ),
            event(
                &request_id,
                "tool_execution_completed",
                serde_json::json!({
                    "tool_call_id":"web-1", "name":"web_search",
                    "arguments":{"action":"search","query":"rust language"},
                    "result":{"status":"completed"},
                    "extra":{"response_body":"must-not-leak"}
                }),
            ),
            event(
                &request_id,
                "tool_execution_completed",
                serde_json::json!({
                    "tool_call_id":"web-1", "name":"web_search",
                    "arguments":{"action":"search","query":"duplicate"},
                    "result":{"status":"completed"}
                }),
            ),
            event(
                &request_id,
                "completed",
                serde_json::json!({
                    "text":"terminal-aggregate-must-not-append",
                    "result": {
                        "text_answer":"semantic-only",
                        "text":"terminal-result-must-not-append"
                    }
                }),
            ),
        ] {
            handle_event(
                &out_tx,
                "conversation-manager",
                &body,
                &host,
                &mut active,
                &mut bridge,
            )
            .await
            .expect("provider event");
        }
        drop(out_tx);

        let mut terminal_result = None;
        let mut facts = Vec::new();
        while let Some(outgoing) = out_rx.recv().await {
            let Body::Event(body) = outgoing.body else {
                continue;
            };
            let kind = body.get("kind").and_then(Value::as_str);
            assert!(
                !kind
                    .is_some_and(|value| value == "tool.invoke" || value.ends_with(".tool.invoke")),
                "provider-executed lifecycle reached tool execution: {body:?}"
            );
            if kind == Some("conversation.fact.append") {
                facts.push(body["fact"].clone());
            }
            if kind == Some(RUN_RESULT_KIND) {
                terminal_result = body.get("result").cloned();
            }
        }

        let fact_count = |kind: &str| {
            facts
                .iter()
                .filter(|fact| fact.get("kind").and_then(Value::as_str) == Some(kind))
                .count()
        };
        assert_eq!(fact_count("tool_exchange_started"), 1);
        assert_eq!(fact_count("tool_call_fragment_appended"), 1);
        assert_eq!(fact_count("tool_call_completed"), 1);
        assert_eq!(fact_count("tool_result_recorded"), 1);
        let wire = serde_json::to_string(&facts).expect("serialize canonical facts");
        assert!(!wire.contains("must-not-leak"));
        assert!(!wire.contains("provider_context"));
        assert!(!wire.contains("response_body"));
        assert!(wire.contains("rust language"));

        let result = terminal_result.expect("durable terminal result");
        assert_eq!(result["value"]["constructor"], "Ok");
        assert_eq!(result["value"]["value"], "semantic-only");
        assert_eq!(result["result"]["text_answer"], "semantic-only");
        assert!(result["result"].get("tool_calls").is_none());
    }
}
