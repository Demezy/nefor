pub mod bridge {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bridge.rs"));
}
mod error {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs"));
}
pub mod kernel {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/kernel.rs"));

    pub fn isolated_host() -> (LuaHost, tempfile::TempDir) {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let host = LuaHost::load_kernel(
            &manifest.join("lua/mag-kernel/init.lua"),
            Some(&manifest.join("../../lua")),
        )
        .unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let root = sessions.path().to_string_lossy().into_owned();
        let nefor: Table = host.lua.globals().get("nefor").unwrap();
        let fs: Table = nefor.get("fs").unwrap();
        fs.set(
            "sessions_root",
            host.lua
                .create_function(move |_, ()| Ok(root.clone()))
                .unwrap(),
        )
        .unwrap();
        (host, sessions)
    }

    pub fn verify_failure_latch(host: &LuaHost, request_id: &str) {
        let verify: Function = host
            .lua
            .load(
                r#"
            return function(kernel, request_id)
              local ctx = kernel.context("latched")
              local router = ctx.router
              ctx.operation_draining = true
              assert(router.settle_result("result", {value={content="tentative"}}))
              ctx.operation_draining = false
              assert(ctx.pending_completion, "pending success was not exercised")
              ctx.operation_queue = {{operation={id="must-not-run"}}}
              local emit = router:emitter("broken")
              emit({kind="stub.Out", value={content=string.char(0xD0)}})
              local failure = assert(kernel.take_run_failed("latched"))
              assert(failure.error:find("invalid type: byte array", 1, true))
              assert(ctx.pending_completion == nil and #ctx.operation_queue == 0)
              assert(ctx.terminal_settlement.status == "failed")
              assert(kernel.take_run_failed("latched") == nil)
              assert(not ctx.drain_operations())
              assert(not ctx.settle_quiescent())

              local deliveries, observations = 0, 0
              router:bind("broken", {
                deliver=function() deliveries=deliveries+1; return {status="ok"} end,
                handle_observation=function() observations=observations+1 end,
                handle_kill=function()
                  emit({kind="fixture.cleaned-up"})
                  emit({kind="mag.ApprovalCancel", correlation="approval"})
                end,
              })
              router:activate("broken", {messages={}})
              router:deliver_initial("broken", "fixture", {content={kind="stub.In"}})
              assert(router:bus_observation({id=request_id}))
              assert(router:bus_response({id=request_id, result="late"}))
              assert(deliveries == 0 and observations == 0)
              emit({kind="stub.Out", value={content="late success"}})
              emit({kind="capability.invoke", capability="read_file", request={path="late"}})
              ctx.emit_event({kind="mag.run_failed", from="broken", error="replacement"})
              assert(not router.settle_result("result", {value={content="direct late success"}}))
              assert(kernel.take_run_complete("latched") == nil)
              assert(kernel.take_run_failed("latched") == nil)
            end
        "#,
            )
            .eval()
            .unwrap();
        verify
            .call::<()>((host.kernel.clone(), request_id))
            .unwrap();
    }

    pub fn deferred_output(host: &LuaHost, run_id: &str, valid_outputs: &[bool]) {
        let install: Function = host.lua.load(r#"
            return function(kernel, run_id, valid_outputs)
              local router = kernel.context(run_id).router
              local emit = router:emitter("broken")
              router:bind("broken", {deliver=function()
                for _, valid in ipairs(valid_outputs) do
                  emit({kind="stub.Out", value={content=valid and "recovered" or string.char(0xD0)}})
                end
                emit({kind="mag.failed", failure="later-error", value={error="must not replace first outcome"}})
                emit({kind="capability.invoke", capability="read_file", request={path="after-outcome"}})
                return {status="ok"}
              end})
              emit({kind="capability.invoke", capability="read_file", request={path="fixture"}})
              emit({kind="capability.invoke", capability="read_file", request={path="background"}})
            end
        "#).eval().unwrap();
        install
            .call::<()>((host.kernel.clone(), run_id, valid_outputs.to_vec()))
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
    valid_outputs: &[bool],
) -> Pending {
    assert!(
        host.begin_run_with_principal(run_id, run_id, Some("test-session"), None, None, None)
            .unwrap()
            .ok
    );
    assert!(host.start(run_id, program).unwrap().ok);
    kernel::deferred_output(host, run_id, valid_outputs);
    active.insert(
        run_id.to_owned(),
        ActiveExecute {
            in_reply_to: Some(format!("execute-{run_id}")),
            started_at: Instant::now(),
        },
    );
    let events = host.drain_emits().unwrap();
    let request = |path| {
        events
            .iter()
            .find(|event| event["kind"] == "tool.invoke" && event["args"]["path"] == path)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    Pending {
        reply: request("fixture"),
        background: request("background"),
    }
}

struct Pending {
    reply: String,
    background: String,
}

fn terminal(
    rx: &mut mpsc::Receiver<PluginOutgoing>,
) -> (Map<String, Value>, Vec<Map<String, Value>>) {
    let mut result = None;
    let mut events = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        if let Body::Event(body) = envelope.body {
            if body["kind"] == "mag.run_result" {
                assert!(result.replace(body.clone()).is_none(), "run settled twice");
            }
            events.push(body);
        }
    }
    (result.expect("terminal result reaches the caller"), events)
}

async fn failed_sequence(valid_outputs: &[bool]) {
    let (host, sessions) = kernel::isolated_host();
    let program = pending_program(&host);
    let mut active = ActiveExecutes::new();
    let bad = begin(&host, &program, &mut active, "invalid", valid_outputs);
    let good = begin(&host, &program, &mut active, "healthy", &[true, false]);
    let mut bridge = CapabilityBridge::new("tool-gate");
    let (tx, mut rx) = mpsc::channel(128);
    handle_tool_result(
        &tx,
        serde_json::json!({"id":bad.reply,"result":"done"})
            .as_object()
            .unwrap(),
        &host,
        &mut active,
        &mut bridge,
    )
    .await
    .unwrap();
    let (failed, events) = terminal(&mut rx);
    assert!(
        !events
            .iter()
            .any(|event| event["kind"] == "mag.run_complete"),
        "failure published a success: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event["kind"] == "tool-gate.tool.invoke"),
        "work was launched after failure: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(
                |event| event["kind"] == "tool-gate.tool.cancel" && event["id"] == bad.background
            )
            .count(),
        1,
        "pending work must be cancelled exactly once"
    );
    assert!(
        !sessions
            .path()
            .join("test-session/mag/runs/invalid")
            .exists(),
        "output was persisted after failure"
    );
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
        serde_json::json!({"id":good.reply,"result":"done"})
            .as_object()
            .unwrap(),
        &host,
        &mut active,
        &mut bridge,
    )
    .await
    .unwrap();
    let (completed, events) = terminal(&mut rx);
    assert!(
        !events.iter().any(|event| event["kind"] == "mag.run_failed"),
        "accepted success must not publish a contradictory failure: {events:?}"
    );
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["run_id"], "healthy");
    assert!(serde_json::to_string(&completed)
        .unwrap()
        .contains("recovered"));
    assert!(active.is_empty());
}

#[tokio::test]
async fn invalid_utf8_fails_only_its_run_and_reports_the_reason_to_the_caller() {
    failed_sequence(&[false]).await;
}

#[tokio::test]
async fn invalid_output_then_valid_output_cannot_become_success() {
    failed_sequence(&[false, true]).await;
}

#[tokio::test]
async fn repeated_invalid_outputs_preserve_the_first_failure() {
    failed_sequence(&[false, false, true]).await;
}

#[test]
fn failure_remains_terminal_after_take_and_cleanup_still_runs() {
    let (host, sessions) = kernel::isolated_host();
    let program = pending_program(&host);
    let mut active = ActiveExecutes::new();
    let pending = begin(&host, &program, &mut active, "latched", &[true]);
    kernel::verify_failure_latch(&host, &pending.reply);
    let events = host.drain_emits().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "mag.run_failed")
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|event| event["kind"] == "mag.run_complete" || event["kind"] == "tool.invoke"));
    assert!(!sessions
        .path()
        .join("test-session/mag/runs/latched")
        .exists());
    assert!(host.end_run("latched", TeardownReason::RunFailed).unwrap());
    let cleanup = host.drain_emits().unwrap();
    assert_eq!(
        cleanup
            .iter()
            .filter(|event| event["kind"] == "tool.cancel" && event["id"] == pending.background)
            .count(),
        1
    );
    assert!(cleanup
        .iter()
        .any(|event| event["kind"] == "fixture.cleaned-up"));
    assert!(cleanup
        .iter()
        .any(|event| event["kind"] == "mag.approval_cancel" && event["correlation"] == "approval"));
}
