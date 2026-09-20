pub mod bridge {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bridge.rs"));
}
mod error {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs"));
}
pub mod kernel {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/kernel.rs"));

    pub fn deferred_output(host: &LuaHost, run_id: &str, invalid: bool) {
        let install: Function = host.lua.load(r#"
            return function(kernel, run_id, invalid)
              local router = kernel.context(run_id).router
              local emit = router:emitter("broken")
              router:bind("broken", {deliver=function()
                emit({kind="stub.Out", value={content=invalid and string.char(0xD0) or "recovered"}})
                return {status="ok"}
              end})
              emit({kind="capability.invoke", capability="read_file", request={path="fixture"}})
            end
        "#).eval().unwrap();
        install
            .call::<()>((host.kernel.clone(), run_id, invalid))
            .unwrap();
    }
}
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime.rs"));

fn pending_program(host: &LuaHost) -> Value {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let artifact = nefor_mag::compile_with_inputs_and_module_roots_and_options_and_syntax(
        r#"
import core.map.{}
import nefor.artifact.{}
import nefor.contracts.{}
import nefor.graph.{}
type MessageContent {content: String}
let start = nefor.graph.source("start", MessageContent {content: "valid"})
let input = nefor.graph.port("broken", type_tag<MessageContent>(), "stub.In")
let output = nefor.graph.port("broken", type_tag<MessageContent>(), "stub.Out")
let actor = nefor.graph.actor("broken", "nefor.factory.stub", [], core.map.empty<String, String>(), nefor.graph.store_port(input), [nefor.graph.store_port(output)])
let worker = nefor.graph.node("broken", "ordinary", [actor], ([]: List<nefor.graph.StoredRoute>), ([]: List<nefor.graph.Message>), input, output)
let result = nefor.graph.output_for("result", worker)
nefor.artifact.compile((|graph| => nefor.graph.add_edges(graph, [nefor.graph.edge(start, worker), nefor.graph.edge(worker, result)])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
        "#,
        &manifest,
        serde_json::json!({"factory_contracts":host.registry_contracts().unwrap()}),
        &[manifest.join("../../mag/lib"), manifest.join("../../examples/nefor-agent/mag/lib")],
        nefor_mag::CompilerOptions::default(),
        nefor_mag::SyntaxMode::New,
    ).unwrap();
    let mut modification = artifact_modification(&artifact).unwrap();
    modification["messages"] = serde_json::json!([]);
    modification
}

fn begin(
    host: &LuaHost,
    program: &Value,
    active: &mut ActiveExecutes,
    run_id: &str,
    invalid: bool,
) -> String {
    assert!(
        host.begin_run_with_principal(run_id, run_id, Some("test-session"), None, None, None)
            .unwrap()
            .ok
    );
    assert!(host.start(run_id, program).unwrap().ok);
    kernel::deferred_output(host, run_id, invalid);
    active.insert(
        run_id.to_owned(),
        ActiveExecute {
            in_reply_to: Some(format!("execute-{run_id}")),
            started_at: Instant::now(),
        },
    );
    host.drain_emits()
        .unwrap()
        .into_iter()
        .find(|event| event["kind"] == "tool.invoke")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn terminal(rx: &mut mpsc::Receiver<PluginOutgoing>) -> Map<String, Value> {
    let mut result = None;
    while let Ok(envelope) = rx.try_recv() {
        if let Body::Event(body) = envelope.body {
            if body["kind"] == "mag.run_result" {
                assert!(result.replace(body).is_none(), "run settled twice");
            }
        }
    }
    result.expect("terminal result reaches the caller")
}

#[tokio::test]
async fn invalid_utf8_fails_only_its_run_and_reports_the_reason_to_the_caller() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let host = LuaHost::load_kernel(
        &manifest.join("lua/mag-kernel/init.lua"),
        Some(&manifest.join("../../lua")),
    )
    .unwrap();
    let program = pending_program(&host);
    let mut active = ActiveExecutes::new();
    let bad = begin(&host, &program, &mut active, "invalid", true);
    let good = begin(&host, &program, &mut active, "healthy", false);
    let mut bridge = CapabilityBridge::new("tool-gate");
    let (tx, mut rx) = mpsc::channel(128);
    handle_tool_result(
        &tx,
        serde_json::json!({"id":bad,"result":"done"})
            .as_object()
            .unwrap(),
        &host,
        &mut active,
        &mut bridge,
    )
    .await
    .unwrap();
    let failed = terminal(&mut rx);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["run_id"], "invalid");
    assert_eq!(failed["in_reply_to"], "execute-invalid");
    let error = failed["error"].as_str().unwrap();
    assert!(
        error.contains("broken") && error.contains("stub.Out"),
        "{error}"
    );
    assert!(error.contains("invalid type: byte array"), "{error}");
    assert!(!active.contains_key("invalid"));
    assert!(active.contains_key("healthy"));
    handle_tool_result(
        &tx,
        serde_json::json!({"id":good,"result":"done"})
            .as_object()
            .unwrap(),
        &host,
        &mut active,
        &mut bridge,
    )
    .await
    .unwrap();
    let completed = terminal(&mut rx);
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["run_id"], "healthy");
    assert!(serde_json::to_string(&completed)
        .unwrap()
        .contains("recovered"));
    assert!(active.is_empty());
}
