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
            let loaded = nefor_mag::load_with_inputs_and_module_roots(
                &source_dir,
                "main.mag",
                serde_json::json!({"factory_contracts": contracts}),
                &[
                    manifest.join("../../mag/lib"),
                    manifest.join("../../examples/nefor-agent/mag/lib"),
                ],
            )
            .expect("compile MAG test program");
            let artifact = serde_json::to_value(loaded.artifact).expect("serialize shell artifact");
            let modification =
                crate::artifact_modification(&artifact).expect("normalize shell artifact");
            let _ = std::fs::remove_dir_all(source_dir);
            modification
        }

        fn compile_mag_eval_expression(host: &LuaHost, name: &str, expression: &str) -> JsonValue {
            let source = format!(
                r#"(require "nefor.artifact")
    (require "nefor.graph")
    (require "nefor.shell")
    (require "nefor.process")
    (let start (nefor.graph.source "start" (type-tag Unit) nil))
    (let operation {expression})
    (let result (nefor.graph.output-for "result" operation))
    (nefor.artifact.compile
        (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
          (nefor.graph.add-edges graph
            [(nefor.graph.edge start operation)
             (nefor.graph.edge operation result)])))
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
                .split_once("```lisp\n")
                .and_then(|(_, rest)| rest.split_once("\n```").map(|(source, _)| source))
                .expect("Nefor guide contains one complete Lisp program");
            let workspace = repository
                .join("tmp")
                .join(format!("mag-book-host-contracts-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&workspace);
            std::fs::create_dir_all(&workspace).expect("create guide host workspace");
            std::fs::write(workspace.join("main.mag"), source).expect("write guide program");

            let contracts = host.registry_contracts().expect("runtime contracts");
            let loaded = nefor_mag::load_with_inputs_and_module_roots(
                &workspace,
                "main.mag",
                serde_json::json!({"factory_contracts": contracts}),
                &[
                    repository.join("mag/lib"),
                    repository.join("examples/nefor-agent/mag/lib"),
                ],
            )
            .expect("compile Nefor guide with runtime contracts");
            let initial = crate::artifact_modification(&loaded.artifact)
                .expect("normalize guide initial modification");

            assert!(
                host.begin_run("mag-book-contracts", "mag-book-contracts", None)
                    .expect("begin guide run")
                    .ok
            );
            let started = host
                .start("mag-book-contracts", &initial)
                .expect("start guide modification");
            assert!(
                started.ok,
                "guide host validation failed: {:?}",
                started.error
            );

            let dynamic = nefor_mag::eval_fn(
                &loaded,
                "expand-swarm",
                serde_json::json!([
                    {"title": "inspect", "instructions": "Inspect the implementation."}
                ]),
            )
            .expect("evaluate guide dynamic swarm");
            let applied = host
                .apply("mag-book-contracts", &dynamic)
                .expect("apply dynamic guide modification");
            assert!(
                applied.ok,
                "dynamic guide host validation failed: {:?}",
                applied.error
            );

            host.end_run("mag-book-contracts", TeardownReason::RunComplete)
                .expect("end guide host run");
            std::fs::remove_dir_all(workspace).expect("remove guide host workspace");
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
                    "rules": [],
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
    (require "nefor.actors")
    (require "nefor.artifact")
    (require "nefor.contracts")
    (require "nefor.graph")

    (let exact-model (fn [[model nefor.actors.ResolvedModel]]
      -> nefor.actors.ResolvedModel model))
    (let model (as nefor.actors.ResolvedModel
      {:provider "test-provider" :model "test-model"
       :reasoning-effort "medium"}))

    (let make-agent (fn [I O] [[id String] [input-type (TypeTag I)]
                                [output-type (TypeTag O)]]
      -> (nefor.graph.Node I (| O nefor.contracts.AgentError))
      (nefor.actors.agent exact-model
        (as (nefor.actors.AgentConfig nefor.actors.ResolvedModel)
          {:id id :model model
           :system "" :tools [] :da-policy (nefor.contracts.no-da-policy)
           :max-corrections 2})
        input-type output-type)))

    (let left-task (nefor.graph.source "left-task" (type-tag nefor.contracts.Task)
                       (as nefor.contracts.Task {:prompt "left"})))
    (let middle-task (nefor.graph.source "middle-task" (type-tag nefor.contracts.Task)
                         (as nefor.contracts.Task {:prompt "middle"})))
    (let right-task (nefor.graph.source "right-task" (type-tag nefor.contracts.Task)
                        (as nefor.contracts.Task {:prompt "right"})))
    (let left (make-agent "left" (type-tag nefor.contracts.Task)
                 (type-tag nefor.contracts.TextAnswer)))
    (let middle (make-agent "middle" (type-tag nefor.contracts.Task)
                   (type-tag nefor.contracts.TextAnswer)))
    (let right (make-agent "right" (type-tag nefor.contracts.Task)
                  (type-tag nefor.contracts.TextAnswer)))
    (let synthesis (make-agent "synthesis"
                      (type-tag (+ (| nefor.contracts.TextAnswer nefor.contracts.AgentError)
                                   (| nefor.contracts.TextAnswer nefor.contracts.AgentError)
                                   (| nefor.contracts.TextAnswer nefor.contracts.AgentError)))
                      (type-tag nefor.contracts.TextAnswer)))
    (let result (nefor.graph.output "result"
                   (type-tag (| nefor.contracts.TextAnswer nefor.contracts.AgentError))))
    (let topology (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
                     (nefor.graph.add-edges graph
                       [(nefor.graph.edge left-task left)
                        (nefor.graph.edge middle-task middle)
                        (nefor.graph.edge right-task right)
                        (nefor.graph.edge left synthesis)
                        (nefor.graph.edge middle synthesis)
                        (nefor.graph.edge right synthesis)
                        (nefor.graph.edge synthesis result)])))
    (nefor.artifact.compile topology)
    "#;

            let modification = compile_mag_source(&host, "shared-multi-agent", source);
            assert_eq!(
                modification["actors"].as_array().map(Vec::len),
                Some(20),
                "each node is lowered once even when it appears at multiple edge boundaries"
            );
        }

        #[test]
        fn task_source_preserves_task_type_and_value_at_runtime() {
            let host = shipped_host();
            let source = r#"
    (require "nefor.actors")
    (require "nefor.artifact")
    (require "nefor.contracts")
    (require "nefor.graph")
    (let start (nefor.actors.task-source "task-input" "runtime prompt"))
    (let result (nefor.graph.output "result" (type-tag nefor.contracts.Task)))
    (nefor.artifact.compile
        (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
          (nefor.graph.add-edges graph [(nefor.graph.edge start result)])))
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
                Some("nefor.contracts.Task")
            );
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
            let expression = r#"(nefor.process.exec "command"
              (as nefor.process.ProcessExecParams
                {:argv ["printf" "provenance"] :cwd nefor.process.cwd
                 :timeout (nefor.contracts.no-timeout)}))"#;
            let modification = compile_mag_eval_expression(&host, run_id, expression);
            let begun = host
                .begin_run_with_principal(
                    run_id,
                    "scout",
                    Some("session-1"),
                    Some("subagent"),
                    Some("conversation-1"),
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
            let expression = r#"(nefor.shell.script "script"
              (as nefor.shell.ShellScriptParams
                {:script "printf output" :cwd nefor.process.cwd
                 :timeout (nefor.contracts.timeout-ms 30000)}))"#;
            let emits = start_shell_expression(&host, "shell-script", expression);
            let invoke = tool_invoke(&emits, "shell.script");
            assert_eq!(invoke["args"]["args"]["cwd"], ".");
            assert_eq!(
                invoke["args"]["args"]["timeout"],
                serde_json::json!({
                    "present": true, "milliseconds": 30000
                })
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
            assert!(value["value"]["termination"]["type"]
                .as_str()
                .is_some_and(|id| id.starts_with("sha256:")));
            assert_eq!(value["value"]["termination"]["value"]["code"], 9);

            let expression = r#"(nefor.process.exec "signaled"
              (as nefor.process.ProcessExecParams
                {:argv ["sleep" "5"] :cwd nefor.process.cwd
                 :timeout (nefor.contracts.no-timeout)}))"#;
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
            assert!(signaled["type"]
                .as_str()
                .is_some_and(|id| id.starts_with("sha256:")));
            assert_eq!(signaled["value"]["signal"], 15);
        }

        #[test]
        fn malformed_and_nonpositive_process_params_fail_before_invocation() {
            let host = shipped_host();
            for (run_id, params) in [
                (
                    "process-zero",
                    r#"{:argv ["true"] :cwd "." :timeout (nefor.contracts.timeout-ms 0)}"#,
                ),
                (
                    "process-negative",
                    r#"{:argv ["true"] :cwd "." :timeout (nefor.contracts.timeout-ms -1)}"#,
                ),
                (
                    "process-empty",
                    r#"{:argv (as (List String) []) :cwd "." :timeout (nefor.contracts.no-timeout)}"#,
                ),
            ] {
                let expression = format!(
                    "(nefor.process.exec \"invalid\" (as nefor.process.ProcessExecParams {params}))"
                );
                let emits = start_shell_expression(&host, run_id, &expression);
                assert!(emits.iter().all(|event| {
                    event.get("kind").and_then(JsonValue::as_str) != Some("tool.invoke")
                }));
                assert!(host
                    .take_run_failed(run_id)
                    .expect("invalid run failure")
                    .is_some());
            }
        }

        #[test]
        fn ordinary_source_node_output_graph_executes_to_its_typed_result() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "ordinary-source-node-output",
                r#"
    (require "nefor.artifact")
    (require "nefor.contracts")
    (require "nefor.graph")

    (let start (nefor.graph.source "start" (type-tag nefor.contracts.Text)
                  (as nefor.contracts.Text {:content "ordinary"})))
    (let input (nefor.graph.port "echo" (type-tag nefor.contracts.Text) "stub.In"))
    (let output (nefor.graph.port "echo" (type-tag nefor.contracts.Text) "stub.Out"))
    (let actor (nefor.graph.actor "echo"
                  "nefor.factory.stub" [] (as (Map String String) {})
                  (nefor.graph.store-port input) [(nefor.graph.store-port output)]))
    (let echo (nefor.graph.node "echo" "ordinary" [actor]
                 (as (List nefor.graph.StoredRoute) [])
                 (as (List nefor.graph.Message) []) input output))
    (let result (nefor.graph.output-for "result" echo))
    (nefor.artifact.compile
        (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
          (nefor.graph.add-edges graph
            [(nefor.graph.edge start echo)
             (nefor.graph.edge echo result)])))
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
    (require "nefor.artifact")
    (require "nefor.contracts")
    (require "nefor.graph")

    (let start (nefor.graph.source "start" (type-tag nefor.contracts.Text)
                  (as nefor.contracts.Text {:content "valid"})))
    (let input (nefor.graph.port "broken" (type-tag nefor.contracts.Text) "stub.In"))
    (let output (nefor.graph.port "broken" (type-tag nefor.contracts.Text) "stub.Out"))
    (let actor (nefor.graph.actor "broken" "nefor.factory.stub" []
                  (as (Map String String) {:value "not-a-Text-record"})
                  (nefor.graph.store-port input) [(nefor.graph.store-port output)]))
    (let broken (nefor.graph.node "broken" "ordinary" [actor]
                   (as (List nefor.graph.StoredRoute) [])
                   (as (List nefor.graph.Message) []) input output))
    (let result (nefor.graph.output-for "result" broken))
    (nefor.artifact.compile
        (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
          (nefor.graph.add-edges graph
            [(nefor.graph.edge start broken)
             (nefor.graph.edge broken result)])))
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
                failure.contains("expected nefor.contracts.Text"),
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
    (require "nefor.artifact")
    (require "nefor.contracts")
    (require "nefor.graph")

    (let pair-type (type-tag (+ nefor.contracts.Text nefor.contracts.Text)))
    (let start (nefor.graph.source "start" pair-type
                  (as (+ nefor.contracts.Text nefor.contracts.Text)
                    [(as nefor.contracts.Text {:content "left"})
                     (as nefor.contracts.Text {:content "right"})])))
    (let result (nefor.graph.output "result" pair-type))
    (nefor.artifact.compile
        (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
          (nefor.graph.add-edges graph [(nefor.graph.edge start result)])))
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
            assert_eq!(result["semantic_type_id"], result["constructor_id"]);
        }

        #[test]
        fn component_edges_assemble_an_ordered_product_at_the_output() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "component-product-output",
                r#"
    (require "nefor.artifact")
    (require "nefor.graph")

    (type Left {:content String})
    (type Right {:count Int})

    (let left (nefor.graph.source "left" (type-tag Left)
                  (as Left {:content "first"})))
    (let right (nefor.graph.source "right" (type-tag Right)
                   (as Right {:count 2})))
    (let result (nefor.graph.output "result" (type-tag (+ Left Right))))
    (nefor.artifact.compile
        (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
          (nefor.graph.add-edges graph
            [(nefor.graph.edge left result)
             (nefor.graph.edge right result)])))
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
            assert_eq!(result["semantic_type_id"], result["constructor_id"]);
        }

        #[test]
        fn sum_arrival_routes_only_to_compatible_branches_and_keeps_constructor_id() {
            let host = shipped_host();
            let modification = compile_mag_source(
                &host,
                "direct-sum-routing",
                r#"
    (require "nefor.artifact")
    (require "nefor.graph")

    (type Left {:value String})
    (type Right {:value Int})
    (type Choice (| Left Right))

    (let branch (fn [T] [[id String] [type (TypeTag T)]] -> (nefor.graph.Node T T)
      (let input (nefor.graph.port id type "stub.In"))
      (let output (nefor.graph.port id type "stub.Out"))
      (let actor (nefor.graph.actor id
                    "nefor.factory.stub" []
                    (as (Map String String) {})
                    (nefor.graph.store-port input)
                    [(nefor.graph.store-port output)]))
      (nefor.graph.node id "ordinary" [actor]
          (as (List nefor.graph.StoredRoute) [])
          (as (List nefor.graph.Message) []) input output)))

    (let start (nefor.graph.source "start" (type-tag Choice)
                  (as Choice (as Left {:value "chosen"}))))
    (let left (branch "left" (type-tag Left)))
    (let right (branch "right" (type-tag Right)))
    (let result (nefor.graph.output "result" (type-tag Choice)))
    (nefor.artifact.compile
        (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
          (nefor.graph.add-edges graph
            [(nefor.graph.edge start left)
             (nefor.graph.edge start right)
             (nefor.graph.edge left result)
             (nefor.graph.edge right result)])))
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
            let result = completion.result.expect("typed sum result");
            assert_eq!(result["semantic_type_id"], result["constructor_id"]);
            assert!(result["semantic_type_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("sha256:")));
        }

        #[test]
        fn factory_construction_failure_is_an_out_of_band_run_failure() {
            let host = shipped_host();
            let run_id = "factory-construction-failure";
            let modification = compile_mag_eval_expression(
                &host,
                run_id,
                r#"(nefor.process.exec "broken"
                     (as nefor.process.ProcessExecParams
                       {:argv ["true"] :cwd nefor.process.cwd
                        :timeout (nefor.contracts.timeout-ms 0)}))"#,
            );

            assert!(host.begin_run(run_id, run_id, None).expect("begin").ok);
            host.drain_emits().expect("drain begin event");
            let outcome = host.start(run_id, &modification).expect("start");
            assert!(
                outcome.ok,
                "the modification itself remains valid: {:?}",
                outcome.error
            );
            let failure = host
                .take_run_failed(run_id)
                .expect("read run failure")
                .expect("construction failure is surfaced out of band");
            assert!(
                failure.contains("construct failed for 'broken'"),
                "{failure}"
            );
            assert!(
                failure.contains("positive integer number of milliseconds"),
                "{failure}"
            );
            assert!(host
                .take_run_complete(run_id)
                .expect("read completion")
                .is_none());
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
                "rules": [],
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
                        "actors": [{
                            "id": "tentative",
                            "factory": "nefor.factory.stub",
                            "type_arguments": [],
                            "params": {},
                            "routes": {}
                        }],
                        "messages": [{"to": "missing", "content": {"kind": "stub.In"}}],
                        "kills": [],
                        "rules": []
                    }),
                )
                .expect("apply rejected modification");
            assert!(!rejected.ok);
            assert!(rejected
                .error
                .as_deref()
                .is_some_and(|error| error.contains("unknown message target 'missing'")));

            let probe = host
                .apply(
                    run_id,
                    &serde_json::json!({
                        "actors": [],
                        "messages": [{"to": "tentative", "content": {"kind": "stub.In"}}],
                        "kills": [],
                        "rules": []
                    }),
                )
                .expect("probe inventory after rejection");
            assert!(!probe.ok);
            assert!(probe
                .error
                .as_deref()
                .is_some_and(|error| { error.contains("unknown message target 'tentative'") }));
        }

        #[test]
        fn rules_bind_concrete_ports_and_require_canonical_values() {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let host = LuaHost::load_kernel(
                &manifest.join("lua/mag-kernel/init.lua"),
                Some(&manifest.join("../../lua")),
            )
            .expect("load shipped kernel");

            host.begin_run("rule-payload", "rule-payload", None)
                .expect("begin");
            let graph = serde_json::json!({
                "actors": [
                    {"id":"source", "factory":"nefor.factory.stub", "type_arguments":[], "params":{}, "routes":{}},
                    {"id":"result", "factory":"nefor.factory.stub", "type_arguments":[], "params":{}, "routes":{}}
                ],
                "messages": [
                    {"to":"source", "content":{"kind":"stub.In"}},
                    {"to":"result", "content":{"kind":"stub.In"}}
                ],
                "kills": [],
                "rules": [{
                    "id":"expand", "on":{"actor":"source", "wire":"stub.Out", "type":"String"},
                    "fn":"expand"
                }],
                "result":{"from":{"actor":"result", "wire":"stub.Out", "type":"String"}}
            });
            let outcome = host.start("rule-payload", &graph).expect("start");
            assert!(outcome.ok, "start failed: {:?}", outcome.error);
            assert!(host
                .take_rule_trigger("rule-payload")
                .expect("trigger")
                .is_none());
            let failure = host
                .take_run_failed("rule-payload")
                .expect("failure")
                .expect("canonical payload failure");
            assert!(failure.contains("emitted no canonical value"), "{failure}");
            assert!(host
                .take_run_complete("rule-payload")
                .expect("completion")
                .is_none());

            host.begin_run("rule-ok", "rule-ok", None)
                .expect("begin canonical rule");
            let mut canonical = graph.clone();
            canonical["actors"][0]["params"]["value"] = serde_json::json!({"task":"one"});
            canonical["messages"] = serde_json::json!([
                {"to":"source", "content":{"kind":"stub.In", "n":1}},
                {"to":"source", "content":{"kind":"stub.In", "n":2}},
                {"to":"result", "content":{"kind":"stub.In"}}
            ]);
            let accepted = host.start("rule-ok", &canonical).expect("start canonical");
            assert!(accepted.ok, "start failed: {:?}", accepted.error);
            let trigger = host
                .take_rule_trigger("rule-ok")
                .expect("trigger")
                .expect("one canonical trigger");
            assert_eq!(trigger.rule_id, "expand");
            assert_eq!(trigger.source_actor, "source");
            assert_eq!(trigger.source_wire, "stub.Out");
            assert_eq!(trigger.emission_seq, 1);
            assert_eq!(trigger.value, serde_json::json!({"task":"one"}));

            host.begin_run("rule-isolated", "rule-isolated", None)
                .expect("begin isolated rule");
            let isolated = host
                .start("rule-isolated", &canonical)
                .expect("start isolated");
            assert!(isolated.ok);
            let isolated_trigger = host
                .take_rule_trigger("rule-isolated")
                .expect("isolated trigger")
                .expect("isolated queue");
            assert_eq!(isolated_trigger.emission_seq, 1);
            let second = host
                .take_rule_trigger("rule-ok")
                .expect("second trigger")
                .expect("FIFO second source emission");
            assert_eq!(second.emission_seq, 2);
            assert!(host
                .take_rule_trigger("rule-ok")
                .expect("quiescent first run")
                .is_none());

            host.begin_run("rule-fanout", "rule-fanout", None)
                .expect("begin rule fanout");
            let mut fanout = graph.clone();
            fanout["actors"][0]["params"]["value"] = serde_json::json!({"task":"fanout"});
            let mut second_rule = fanout["rules"][0].clone();
            second_rule["id"] = JsonValue::String("expand-again".into());
            fanout["rules"].as_array_mut().unwrap().push(second_rule);
            assert!(host.start("rule-fanout", &fanout).expect("fanout start").ok);
            assert_eq!(
                host.take_rule_trigger("rule-fanout")
                    .unwrap()
                    .unwrap()
                    .rule_id,
                "expand"
            );
            assert_eq!(
                host.take_rule_trigger("rule-fanout")
                    .unwrap()
                    .unwrap()
                    .rule_id,
                "expand-again"
            );

            host.begin_run("rule-result", "rule-result", None)
                .expect("begin result rule");
            let mut result_rule = graph;
            result_rule["rules"][0]["on"]["actor"] = JsonValue::String("result".into());
            let rejected = host
                .start("rule-result", &result_rule)
                .expect("start reject");
            assert!(!rejected.ok);
            assert!(rejected
                .error
                .as_deref()
                .is_some_and(|error| error.contains("may not bind the result boundary")));

            host.begin_run("rule-duplicate", "rule-duplicate", None)
                .expect("begin duplicate rule");
            let mut duplicate = result_rule;
            duplicate["rules"][0]["on"]["actor"] = JsonValue::String("source".into());
            let copied = duplicate["rules"][0].clone();
            duplicate["rules"].as_array_mut().unwrap().push(copied);
            let duplicate_result = host
                .start("rule-duplicate", &duplicate)
                .expect("duplicate reject");
            assert!(!duplicate_result.ok);
            assert!(duplicate_result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("duplicate rule id")));

            for (run_id, messages) in [
                (
                    "rule-invalid-valid",
                    serde_json::json!([
                        {"to":"source", "content":{"kind":"stub.In"}},
                        {"to":"source", "content":{"kind":"stub.In", "value":{"ok":true}}},
                        {"to":"result", "content":{"kind":"stub.In"}}
                    ]),
                ),
                (
                    "rule-valid-invalid",
                    serde_json::json!([
                        {"to":"source", "content":{"kind":"stub.In", "value":{"ok":true}}},
                        {"to":"source", "content":{"kind":"stub.In"}},
                        {"to":"result", "content":{"kind":"stub.In"}}
                    ]),
                ),
            ] {
                host.begin_run(run_id, run_id, None)
                    .expect("begin mixed emissions");
                let mut mixed = canonical.clone();
                mixed["actors"][0]["params"] = serde_json::json!({"canonical_from_message":true});
                mixed["messages"] = messages;
                let outcome = host.start(run_id, &mixed).expect("mixed start");
                assert!(outcome.ok);
                assert!(host
                    .take_rule_trigger(run_id)
                    .expect("disabled queue")
                    .is_none());
                assert!(host.take_run_failed(run_id).expect("failed run").is_some());
                assert!(host
                    .take_run_complete(run_id)
                    .expect("no completion")
                    .is_none());
            }

            host.begin_run("rule-stop-route", "rule-stop-route", None)
                .expect("begin stopped route");
            let stopped_route = serde_json::json!({
                "actors": [
                    {"id":"source", "factory":"nefor.factory.stub", "type_arguments":[], "params":{},
                     "routes":{"stub.Out":[{"actor":"consumer","wire":"stub.Out"}]}},
                    {"id":"consumer", "factory":"nefor.factory.stub", "type_arguments":[], "params":{}, "routes":{}}
                ],
                "messages":[{"to":"source", "content":{"kind":"stub.In"}}],
                "kills":[],
                "rules":[{"id":"expand", "on":{"actor":"source", "wire":"stub.Out", "type":"String"}, "fn":"expand"}],
                "result":{"from":{"actor":"consumer", "wire":"stub.Out", "type":"String"}}
            });
            assert!(
                host.start("rule-stop-route", &stopped_route)
                    .expect("stopped route start")
                    .ok
            );
            assert!(host
                .take_run_failed("rule-stop-route")
                .expect("route failure")
                .is_some());
            assert!(host
                .take_run_complete("rule-stop-route")
                .expect("consumer stayed unfired")
                .is_none());

            let immutable = host
                .apply(
                    "rule-payload",
                    &serde_json::json!({"actors":[], "messages":[], "kills":[], "rules":[{
                        "id":"late", "on":{"actor":"source", "wire":"stub.Out"}, "fn":"late"
                    }]}),
                )
                .expect("delta apply");
            assert!(!immutable.ok);
            assert!(immutable
                .error
                .as_deref()
                .is_some_and(|error| error.contains("immutable initial subscriptions")));
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
            "artifact": {
                "actors": [{
                    "id": "answer",
                    "factory": "structured-output",
                    "params": {
                        "schema": {"version": 1, "root": {"kind": "json_value"}}
                    }
                }],
                "messages": [], "kills": [], "rules": []
            }
        });
        let (out_tx, mut out_rx) = mpsc::channel(CHANNEL_CAP);
        let mut active = ActiveExecutes::new();
        let mut bridge = CapabilityBridge::new("tool-gate");
        handle_execute(
            &out_tx,
            "direct",
            body.as_object().expect("execute body"),
            Some("execute-1"),
            &None,
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
    async fn malformed_agent_error_output_rejects_during_load_before_run_registration() {
        let root =
            std::env::temp_dir().join(format!("mag-malformed-agent-output-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace");
        fs::write(
            root.join("main.mag"),
            r#"
(require "nefor.actors")
(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")
(let exact-model (fn [[model nefor.actors.ResolvedModel]] -> nefor.actors.ResolvedModel model))
(let model (as nefor.actors.ResolvedModel {:provider "mock-provider" :model "mock-model" :reasoning-effort "medium"}))
(let start (nefor.actors.task-source "task" "test"))
(let worker (nefor.actors.agent exact-model
        (as (nefor.actors.AgentConfig nefor.actors.ResolvedModel) {:id "worker"
         :model model
         :system "Answer."
         :tools (as (List String) [])
         :da-policy (nefor.contracts.no-da-policy)
         :max-corrections 0})
        (type-tag nefor.contracts.Task)
        (type-tag (| nefor.contracts.TextAnswer nefor.contracts.AgentError))))
(let result (nefor.graph.output "result"
        (type-tag (| (| nefor.contracts.TextAnswer nefor.contracts.AgentError)
                     nefor.contracts.AgentError))))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start worker)
         (nefor.graph.edge worker result)])))
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
        let mut program = None;
        handle_load(
            &out_tx,
            body.as_object().expect("load body"),
            Some("load-malformed"),
            &mut program,
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
        assert!(program.is_none(), "invalid program is not installed");
        assert!(
            host.drain_emits().expect("kernel emits").is_empty(),
            "load rejection cannot emit mag.run_started"
        );
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn malformed_resident_rules_are_rejected_by_the_plugin_not_mag_core() {
        let root = std::env::temp_dir().join(format!(
            "mag-malformed-resident-rules-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace");
        fs::write(
            root.join("main.mag"),
            r#"(artifact {:actors [] :messages [] :kills [] :rules "not-a-list"})"#,
        )
        .expect("program");
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let host = LuaHost::load_kernel(
            &manifest.join("lua/mag-kernel/init.lua"),
            Some(&manifest.join("../../lua")),
        )
        .expect("kernel");
        let body = serde_json::json!({
            "id": "load-malformed-rules",
            "source_dir": root,
            "entry": "main.mag"
        });
        let (out_tx, mut out_rx) = mpsc::channel(CHANNEL_CAP);
        let mut program = None;
        handle_load(
            &out_tx,
            body.as_object().expect("load body"),
            Some("load-malformed-rules"),
            &mut program,
            &host,
        )
        .await
        .expect("load rejection is a protocol response");

        let outgoing = out_rx.try_recv().expect("load rejection");
        let Body::Event(body) = outgoing.body else {
            panic!("expected event response")
        };
        assert_eq!(body["kind"], ERROR_KIND);
        assert!(body["message"]
            .as_str()
            .is_some_and(|message| message.contains("'rules' must be an array")));
        assert!(program.is_none(), "invalid program is not installed");
        assert!(host.drain_emits().expect("kernel emits").is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn valid_text_answer_agent_loads_through_existing_path() {
        let root =
            std::env::temp_dir().join(format!("mag-valid-text-agent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace");
        fs::write(
            root.join("main.mag"),
            r#"
(require "nefor.actors")
(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")
(let exact-model (fn [[model nefor.actors.ResolvedModel]] -> nefor.actors.ResolvedModel model))
(let model (as nefor.actors.ResolvedModel {:provider "mock-provider" :model "mock-model" :reasoning-effort "medium"}))
(let start (nefor.actors.task-source "task" "test"))
(let worker (nefor.actors.agent exact-model
        (as (nefor.actors.AgentConfig nefor.actors.ResolvedModel) {:id "worker"
         :model model
         :system "Answer."
         :tools (as (List String) [])
         :da-policy (nefor.contracts.no-da-policy)
         :max-corrections 0})
        (type-tag nefor.contracts.Task)
        (type-tag nefor.contracts.TextAnswer)))
(let result (nefor.graph.output "result"
        (type-tag (| nefor.contracts.TextAnswer nefor.contracts.AgentError))))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start worker)
         (nefor.graph.edge worker result)])))
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
            "id": "load-valid",
            "source_dir": root,
            "module_roots": [module_root, config_module_root],
            "entry": "main.mag"
        });
        let (out_tx, mut out_rx) = mpsc::channel(CHANNEL_CAP);
        let mut program = None;
        handle_load(
            &out_tx,
            body.as_object().expect("load body"),
            Some("load-valid"),
            &mut program,
            &host,
        )
        .await
        .expect("load succeeds");

        let outgoing = out_rx.try_recv().expect("load response");
        let Body::Event(body) = outgoing.body else {
            panic!("expected event response")
        };
        assert_eq!(body["kind"], LOADED_KIND);
        assert!(program.is_some(), "valid program becomes resident");

        let execute = serde_json::json!({
            "id": "execute-valid",
            "run_id": "valid-text-agent",
            "run_name": "valid-text-agent",
            "session_id": "session-1"
        });
        let mut active = ActiveExecutes::new();
        let mut bridge = CapabilityBridge::new("tool-gate");
        handle_execute(
            &out_tx,
            "direct",
            execute.as_object().expect("execute body"),
            Some("execute-valid"),
            &program,
            (&host, &mut active, &mut bridge),
        )
        .await
        .expect("valid execution starts");
        let emitted = std::iter::from_fn(|| out_rx.try_recv().ok())
            .filter_map(|outgoing| match outgoing.body {
                Body::Event(body) => Some(body),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            emitted.iter().any(|body| {
                body.get("kind").and_then(Value::as_str) == Some("mag.run_started")
                    && body.get("run_id").and_then(Value::as_str) == Some("valid-text-agent")
            }),
            "valid execution follows the normal run-start lifecycle"
        );
        assert!(
            emitted.iter().any(|body| {
                body.get("kind").and_then(Value::as_str)
                    == Some("conversation.provider.invoke.request")
            }),
            "valid TextAnswer agent reaches the provider path"
        );
        assert!(active.contains_key("valid-text-agent"));
        host.end_run("valid-text-agent", TeardownReason::Killed)
            .expect("cleanup run");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resident_rule_expands_a_typed_value_into_an_atomic_delta() {
        let root = std::env::temp_dir().join(format!("mag-rule-expansion-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace");
        fs::write(
            root.join("main.mag"),
            r#"
              (type Task {:task String})
              (type ExpandedResult {:greeting String})
              (let task-name (fn [[task Task]] -> String (get task "task")))
              (let expand (fn [[task Task]] -> Artifact
                (artifact {:actors []
                   :messages [{:to "middle" :content {:kind "stub.In" :task (task-name task)}}]
                   :kills []
                   :rules []})))
              (let finish (fn [[task Task]] -> Artifact
                (artifact {:actors []
                   :messages [{:to "result" :content {:kind "stub.In" :task task}}]
                   :kills []
                   :rules []})))
              (artifact {:actors [
                  {:id "source" :factory "nefor.factory.stub" :type_arguments []
                   :params {:value {:task "one"}} :routes {}}
                  {:id "middle" :factory "nefor.factory.stub" :type_arguments []
                   :params {:value {:task "nested"}} :routes {}}
                  {:id "result" :factory "nefor.factory.stub" :type_arguments []
                   :params {:greeting "expanded"} :routes {}}]
                 :messages [{:to "source" :content {:kind "stub.In"}}]
                 :kills []
                 :rules [{:id "expand" :on {:actor "source" :type (type-evidence (type-tag Task)) :wire "stub.Out"}
                          :fn "expand"}
                         {:id "finish" :on {:actor "middle" :type (type-evidence (type-tag Task)) :wire "stub.Out"}
                          :fn "finish"}]
                 :result {:from {:actor "result" :type (type-evidence (type-tag ExpandedResult)) :wire "stub.Out"}}})
            "#,
        )
        .expect("program");
        let module_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mag/lib");
        let config_module_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/nefor-agent/mag/lib");
        let program = nefor_mag::load_with_inputs_and_module_roots(
            &root,
            "main.mag",
            serde_json::json!({}),
            &[root.clone(), module_root, config_module_root],
        )
        .expect("load program");
        let artifact = program.artifact.clone();
        let modification = artifact_modification(&artifact).expect("normalize graph");
        let rules = resolve_resident_rules(&program, &modification).expect("rules valid");
        let program = ResidentProgram {
            loaded: program,
            rules,
        };

        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let host = LuaHost::load_kernel(
            &manifest.join("lua/mag-kernel/init.lua"),
            Some(&manifest.join("../../lua")),
        )
        .expect("kernel");
        assert!(
            host.begin_run("rule-e2e", "rule-e2e", None)
                .expect("begin")
                .ok
        );
        let started = host.start("rule-e2e", &modification).expect("start");
        assert!(started.ok, "start failed: {:?}", started.error);
        assert!(host
            .take_run_complete("rule-e2e")
            .expect("premature completion")
            .is_none());

        drain_rule_triggers(&host, Some(&program), "rule-e2e").expect("drain rules");
        let completion = host
            .take_run_complete("rule-e2e")
            .expect("completion")
            .unwrap_or_else(|| {
                let failure = host.take_run_failed("rule-e2e").expect("rule run failure");
                panic!("delta did not fire static result actor: {failure:?}")
            });
        assert_eq!(
            completion
                .result
                .as_ref()
                .and_then(|result| result["greeting"].as_str()),
            Some("expanded")
        );
        assert!(host
            .take_rule_trigger("rule-e2e")
            .expect("quiescent")
            .is_none());
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
        let result =
            serde_json::json!({"kind":"union","items":[text_answer.clone(),agent_error.clone()]});
        let modification = serde_json::json!({
            "actors": [{
                "id": "answer",
                "factory": "llm",
                "type_arguments": [],
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
            "rules": [],
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
        let mut program = None;
        let mut active = HashMap::from([(
            "provider-events".to_owned(),
            ActiveExecute {
                in_reply_to: None,
                program: None,
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
                &mut program,
                &host,
                &mut active,
                &mut bridge,
            )
            .await
            .expect("provider event");
        }
        drop(out_tx);

        let mut terminal_result = None;
        while let Some(outgoing) = out_rx.recv().await {
            let Body::Event(body) = outgoing.body else {
                continue;
            };
            if body.get("kind").and_then(Value::as_str) == Some(RUN_RESULT_KIND) {
                terminal_result = body.get("result").cloned();
            }
        }

        let result = terminal_result.expect("durable terminal result");
        assert_eq!(result["value"], "semantic-only");
        assert_eq!(result["result"]["text_answer"], "semantic-only");
    }
}
