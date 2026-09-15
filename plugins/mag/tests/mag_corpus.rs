use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use nefor_protocol::{Body, Envelope, PluginName, PluginOutgoing, SystemBody, Timestamp};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::time::timeout;

const READ_TIMEOUT: Duration = Duration::from_secs(180);

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mag-plugin"))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn kernel_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua/mag-kernel/init.lua")
}

fn source_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    fn visit(dir: &Path, extension: &str, paths: &mut Vec<PathBuf>) {
        let mut entries = fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("read corpus directory {}: {error}", dir.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| panic!("read entry under {}: {error}", dir.display()));
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|name| name.to_str());
                if !matches!(name, Some(".git" | ".worktrees" | "target" | "tmp")) {
                    visit(&path, extension, paths);
                }
            } else if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
                paths.push(path);
            }
        }
    }

    let mut paths = Vec::new();
    visit(root, extension, &mut paths);
    paths.sort();
    paths
}

fn mag_files(root: &Path) -> Vec<PathBuf> {
    source_files(root, "mag")
}

fn module_name(lib_root: &Path, path: &Path) -> String {
    path.strip_prefix(lib_root)
        .unwrap_or_else(|_| panic!("{} is not under {}", path.display(), lib_root.display()))
        .with_extension("")
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join(".")
}

async fn spawn_mag(data_dir: &Path) -> Child {
    let mut cmd = tokio::process::Command::new(binary_path());
    cmd.arg("--kernel")
        .arg(kernel_path())
        .env("NEFOR_DATA_DIR", data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.spawn().expect("spawn mag-plugin")
}

async fn read_outgoing<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    expecting: &str,
) -> PluginOutgoing {
    let mut line = String::new();
    match timeout(READ_TIMEOUT, reader.read_line(&mut line)).await {
        Ok(Ok(0)) => panic!("mag stdout closed while expecting {expecting}"),
        Ok(Ok(_)) => PluginOutgoing::parse_line(line.trim_end()).expect("parse mag output"),
        Ok(Err(error)) => panic!("read mag stdout while expecting {expecting}: {error}"),
        Err(_) => panic!("timed out waiting for mag output while expecting {expecting}"),
    }
}

async fn write_envelope(stdin: &mut ChildStdin, envelope: Envelope) {
    stdin
        .write_all(envelope.to_line().as_bytes())
        .await
        .expect("write envelope");
    stdin.write_all(b"\n").await.expect("write newline");
    stdin.flush().await.expect("flush envelope");
}

async fn send_event(stdin: &mut ChildStdin, body: Map<String, Value>) {
    write_envelope(
        stdin,
        Envelope::event(PluginName::engine(), Timestamp::now(), body),
    )
    .await;
}

async fn handshake<R: AsyncBufReadExt + Unpin>(reader: &mut R, stdin: &mut ChildStdin) {
    let ready = read_outgoing(reader, "system ready").await;
    assert!(matches!(ready.body, Body::System(SystemBody::Ready { .. })));
    write_envelope(
        stdin,
        Envelope::system(
            PluginName::engine(),
            Timestamp::now(),
            SystemBody::ReadyOk {
                engine_version: "test".into(),
            },
        ),
    )
    .await;
}

async fn load<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    stdin: &mut ChildStdin,
    id: &str,
    source_dir: &Path,
    entry: &Path,
    module_roots: &[PathBuf],
) -> Map<String, Value> {
    send_event(
        stdin,
        json!({
            "kind": "mag.load",
            "id": id,
            "source_dir": source_dir.to_string_lossy(),
            "entry": entry.to_string_lossy(),
            "module_roots": module_roots,
        })
        .as_object()
        .expect("load body is an object")
        .clone(),
    )
    .await;

    loop {
        let outgoing = read_outgoing(reader, id).await;
        if let Body::Event(body) = outgoing.body {
            if body.get("in_reply_to").and_then(Value::as_str) == Some(id) {
                return body;
            }
        }
    }
}

async fn shutdown(mut stdin: ChildStdin, mut child: Child) {
    write_envelope(
        &mut stdin,
        Envelope::system(
            PluginName::engine(),
            Timestamp::now(),
            SystemBody::Shutdown {
                reason: None,
                grace_ms: None,
            },
        ),
    )
    .await;
    drop(stdin);
    let _ = timeout(Duration::from_secs(10), child.wait()).await;
}

#[test]
fn starter_prompts_defer_mag_reference_material_to_ambient_context() {
    let root = repo_root();
    let mut prompts = source_files(&root.join("examples/nefor-agent/prompts"), "md");
    prompts.extend(source_files(
        &root.join("examples/nefor-agent/mag/lib/prompts"),
        "md",
    ));
    let tutorial_markers = [
        "```lisp",
        "(require \"nefor.",
        "nefor.shell.ShellScriptParams",
        "nefor.process.ProcessExecParams",
        "## MAG programs",
        "A minimal agent program is",
        "Available MAG modules:",
        "Full MAG Book:",
        "Writable source directory:",
    ];

    for path in prompts {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read starter prompt {}: {error}", path.display()));
        for marker in tutorial_markers {
            assert!(
                !source.contains(marker),
                "starter prompt {} duplicates ambient MAG reference material {marker:?}",
                path.display()
            );
        }
    }

    let mut canonical_teaching = vec![
        root.join("mag/book/01. core/00. MAG in Five Minutes.md"),
        root.join("mag/book/02. nefor/00. Nefor MAG in Five Minutes.md"),
        root.join("examples/nefor-agent/mock-provider/init.lua"),
    ];
    let removed = [
        "(agent ",
        "(node ",
        "(graph ",
        "(subgraph ",
        "(bash ",
        " :terminal ",
        "`->` pipes",
        "-> pipes",
        "-> connects",
        "-> composes",
    ];

    for path in canonical_teaching.drain(..) {
        let source = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "read active MAG teaching source {}: {error}",
                path.display()
            )
        });
        for obsolete in removed {
            assert!(
                !source.contains(obsolete),
                "active MAG teaching source {} contains obsolete form {obsolete:?}",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn shipped_mag_corpus_compiles_with_runtime_contracts() {
    let root = repo_root();
    let starter = root.join("examples/nefor-agent");
    let lib_root = root.join("mag/lib");
    let config_lib_root = starter.join("mag/lib");
    let module_roots = vec![lib_root.clone(), config_lib_root.clone()];
    let fixture_root = starter.join("mag/tests");
    let book_root = root.join("mag/examples");
    let all_mag = mag_files(&root);
    let libraries = mag_files(&lib_root);
    let config_libraries = mag_files(&config_lib_root);
    let fixtures = mag_files(&fixture_root);
    let book_examples = mag_files(&book_root);
    let entrypoints = all_mag
        .iter()
        .filter(|path| {
            path.starts_with(&starter)
                && !path.starts_with(&lib_root)
                && !path.starts_with(&config_lib_root)
                && !path.starts_with(&fixture_root)
        })
        .cloned()
        .collect::<Vec<_>>();

    let unclassified = all_mag
        .iter()
        .filter(|path| {
            !libraries.contains(path)
                && !config_libraries.contains(path)
                && !fixtures.contains(path)
                && !book_examples.contains(path)
                && !entrypoints.contains(path)
        })
        .collect::<Vec<_>>();
    assert!(
        unclassified.is_empty(),
        "shipped MAG files are outside the known library, entrypoint, or failure-fixture categories: {unclassified:#?}"
    );

    assert!(
        !libraries.is_empty(),
        "no shipped MAG library modules found"
    );
    assert!(!entrypoints.is_empty(), "no shipped MAG entrypoints found");
    assert!(
        !book_examples.is_empty(),
        "no standalone MAG Book examples found"
    );

    let missing_expectations = fixtures
        .iter()
        .filter(|path| !path.with_extension("error").is_file())
        .collect::<Vec<_>>();
    assert!(
        missing_expectations.is_empty(),
        "MAG failure fixtures missing matching .error files: {missing_expectations:#?}"
    );

    let orphan_expectations = source_files(&fixture_root, "error")
        .into_iter()
        .filter(|path| !path.with_extension("mag").is_file())
        .collect::<Vec<_>>();
    assert!(
        orphan_expectations.is_empty(),
        "MAG .error files missing matching fixtures: {orphan_expectations:#?}"
    );

    let temp_root = std::env::temp_dir().join(format!("mag-corpus-{}", std::process::id()));
    fs::create_dir_all(&temp_root).expect("create MAG corpus temp directory");
    let synthetic = libraries
        .iter()
        .map(|path| format!("(require \"{}\")", module_name(&lib_root, path)))
        .chain(std::iter::once(
            "(nefor.artifact.delta (nefor.graph.delta [] [] [] []))".to_owned(),
        ))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(temp_root.join("all-libraries.mag"), synthetic)
        .expect("write synthetic MAG library entrypoint");

    let mut child = spawn_mag(&temp_root).await;
    let mut stdin = child.stdin.take().expect("mag stdin");
    let stdout = child.stdout.take().expect("mag stdout");
    let mut reader = BufReader::new(stdout);
    let stderr = child.stderr.take().expect("mag stderr");
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[mag stderr] {line}");
        }
    });
    handshake(&mut reader, &mut stdin).await;

    let configured_entrypoint = load(
        &mut reader,
        &mut stdin,
        "corpus-configured-entrypoint",
        &starter,
        Path::new("agentic-loop/lead-turn.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        configured_entrypoint.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "configured lead model module failed to compile: {configured_entrypoint:#?}"
    );

    let library_result = load(
        &mut reader,
        &mut stdin,
        "corpus-libraries",
        &temp_root,
        Path::new("all-libraries.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        library_result.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "shipped MAG libraries failed to compile: {library_result:#?}"
    );

    // The first Lisp fence in the Nefor MAG guide is the canonical progressive
    // agent/SDLC/dynamic-workflow program available to every lead. Compile that exact
    // text rather than maintaining a test-side approximation that can drift
    // from the documentation agents actually see.
    let guide_path = root.join("mag/book/02. nefor/00. Nefor MAG in Five Minutes.md");
    let guide = fs::read_to_string(&guide_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", guide_path.display()));
    let canonical = guide
        .split_once("```lisp\n")
        .and_then(|(_, rest)| rest.split_once("\n```").map(|(source, _)| source))
        .expect("the Nefor MAG guide contains a complete canonical Lisp fence");
    assert!(
        canonical.contains("(nefor.actors.task-source \"development-task\""),
        "the canonical example must use the public Task source helper"
    );
    assert!(
        canonical.contains("(nefor.actors.agent"),
        "the first Lisp fence must construct agents through the public helper"
    );
    assert!(
        canonical.contains("(nefor.worktree.create (str id \".worktree\")"),
        "the canonical example must construct its worktree inside the reusable SDLC function"
    );
    assert!(
        canonical.contains("(nefor.dynamic.traverse-template \"followups\" worker-template"),
        "the canonical example must derive dynamic workers through a node boundary"
    );
    fs::write(temp_root.join("canonical-agent.mag"), canonical)
        .expect("write exact canonical agent regression");
    let canonical_result = load(
        &mut reader,
        &mut stdin,
        "canonical-agent",
        &temp_root,
        Path::new("canonical-agent.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        canonical_result.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "the exact canonical guide program must compile against runtime contracts: {canonical_result:#?}"
    );

    fs::write(
        temp_root.join("task-source.mag"),
        r#"(require "core.types")
    (require "nefor.actors")
(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")
(let start (nefor.actors.task-source "task-input" "preserve this prompt"))
(let result (nefor.graph.output "result" (type-tag nefor.contracts.Task)))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph [(nefor.graph.edge start result)])))"#,
    )
    .expect("write task source regression");
    let task_source = load(
        &mut reader,
        &mut stdin,
        "task-source",
        &temp_root,
        Path::new("task-source.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        task_source.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "the public Task source helper must compile: {task_source:#?}"
    );
    let task_artifact = task_source.get("artifact").expect("task source artifact");
    let task_actor = task_artifact
        .pointer("/program/initial/actors")
        .and_then(Value::as_array)
        .and_then(|actors| {
            actors
                .iter()
                .find(|actor| actor.get("id").and_then(Value::as_str) == Some("task-input"))
        })
        .expect("task source actor");
    assert_eq!(
        task_actor.get("factory").and_then(Value::as_str),
        Some("nefor.factory.source")
    );
    assert_eq!(
        task_actor
            .pointer("/params/value/value/prompt")
            .and_then(Value::as_str),
        Some("preserve this prompt")
    );
    assert_eq!(
        task_actor.pointer("/params/value/value_type"),
        task_actor.pointer("/outputs/0/type_id"),
        "the source value type id must match its Task output"
    );
    assert_eq!(
        task_actor
            .pointer("/outputs/0/type/name")
            .and_then(Value::as_str),
        Some("nefor.contracts.Task")
    );

    fs::write(
        temp_root.join("retry-gate-graph.mag"),
        r#"(require "nefor.artifact")
(require "core.types")
    (require "nefor.actors")
(require "nefor.contracts")
(require "nefor.graph")
(let start (nefor.graph.source "start" (type-tag nefor.contracts.Task)
              (as nefor.contracts.Task {:prompt "retry"})))
(let gate (nefor.actors.retry-gate
             (as nefor.actors.RetryGateConfig {:id "retry" :max-retries 3})
             (type-tag nefor.contracts.Task)))
(let result (nefor.graph.output-for "result" gate))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start gate)
         (nefor.graph.edge gate result)])))"#,
    )
    .expect("write retry gate graph regression");
    let retry_gate = load(
        &mut reader,
        &mut stdin,
        "retry-gate-graph",
        &temp_root,
        Path::new("retry-gate-graph.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        retry_gate.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "the typed RetryGate graph must compile against runtime contracts: {retry_gate:#?}"
    );

    fs::write(
        temp_root.join("node-sequence.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.node")
(let start (nefor.graph.source "start" (type-tag String) "shared"))
(let first (nefor.graph.identity "first" (type-tag String)))
(let second (nefor.graph.identity "second" (type-tag String)))
(let workers (nefor.node.sequence "workers" [first second]))
(let result (nefor.graph.output-for "result" workers))
(nefor.artifact.compile
  (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
    (nefor.graph.add-edges graph
      [(nefor.graph.edge start workers)
       (nefor.graph.edge workers result)])))"#,
    )
    .expect("write node sequence regression");
    let node_sequence = load(
        &mut reader,
        &mut stdin,
        "node-sequence",
        &temp_root,
        Path::new("node-sequence.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        node_sequence.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "the node-oriented fixed sequence must compile against runtime contracts: {node_sequence:#?}"
    );
    let sequence_paths = node_sequence
        .get("artifact")
        .and_then(|artifact| artifact.pointer("/program/initial/nodes"))
        .and_then(Value::as_array)
        .expect("sequence logical node paths")
        .iter()
        .map(|node| node.get("path").cloned().expect("logical node path"))
        .collect::<Vec<_>>();
    assert!(sequence_paths.contains(&json!(["workers"])));
    assert!(sequence_paths.contains(&json!(["workers", "first"])));
    assert!(sequence_paths.contains(&json!(["workers", "second"])));

    fs::write(
        temp_root.join("node-sequence-sources.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.node")
(let first (nefor.graph.source "first" (type-tag String) "first"))
(let second (nefor.graph.source "second" (type-tag String) "second"))
(let workers (nefor.node.sequence "workers" [first second]))
(let result (nefor.graph.output-for "result" workers))
(nefor.artifact.compile
  (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
    (nefor.graph.add-edges graph
      [(nefor.graph.edge workers result)])))"#,
    )
    .expect("write nested source sequence regression");
    let source_sequence = load(
        &mut reader,
        &mut stdin,
        "node-sequence-sources",
        &temp_root,
        Path::new("node-sequence-sources.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        source_sequence.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "fixed sequence must accept independently authored Unit sources: {source_sequence:#?}"
    );
    let source_messages = source_sequence
        .get("artifact")
        .and_then(|artifact| artifact.pointer("/program/initial/messages"))
        .and_then(Value::as_array)
        .expect("source sequence initial messages");
    assert_eq!(
        source_messages.len(),
        1,
        "the completed graph must bootstrap exactly one outer Unit root"
    );
    assert_eq!(source_messages[0]["to"], "workers.input");
    assert_eq!(
        source_messages[0]["content"]["value"]["kind"],
        "nefor.graph.Value"
    );

    fs::write(
        temp_root.join("source-delta.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(let start (nefor.graph.source "start" (type-tag String) "started"))
(nefor.artifact.delta (nefor.graph.node-delta start))"#,
    )
    .expect("write source delta bootstrap regression");
    let source_delta = load(
        &mut reader,
        &mut stdin,
        "source-delta",
        &temp_root,
        Path::new("source-delta.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        source_delta.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "a Unit-input node introduced by a delta must bootstrap: {source_delta:#?}"
    );
    let delta_messages = source_delta
        .get("artifact")
        .and_then(|artifact| artifact.pointer("/delta/messages"))
        .and_then(Value::as_array)
        .expect("source delta initial messages");
    assert_eq!(delta_messages.len(), 1);
    assert_eq!(delta_messages[0]["to"], "start");
    assert_eq!(delta_messages[0]["content"]["value"]["kind"], "mag.Unit");

    fs::write(
        temp_root.join("node-products.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.node")
(let start (nefor.graph.source "start" (type-tag String) "shared"))
(let fork-left (nefor.graph.identity "fork-left" (type-tag String)))
(let fork-right (nefor.graph.identity "fork-right" (type-tag String)))
(let map-left (nefor.graph.identity "map-left" (type-tag String)))
(let map-right (nefor.graph.identity "map-right" (type-tag String)))
(let forked (nefor.node.fanout "forked" fork-left fork-right))
(let mapped (nefor.node.parallel "mapped" map-left map-right))
(let workflow (nefor.node.compose forked mapped))
(let result (nefor.graph.output-for "result" workflow))
(nefor.artifact.compile
  (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
    (nefor.graph.add-edges graph
      [(nefor.graph.edge start workflow)
       (nefor.graph.edge workflow result)])))"#,
    )
    .expect("write node product combinator regression");
    let node_products = load(
        &mut reader,
        &mut stdin,
        "node-products",
        &temp_root,
        Path::new("node-products.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        node_products.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "fanout and parallel must compile as ordinary product nodes: {node_products:#?}"
    );
    let product_paths = node_products
        .get("artifact")
        .and_then(|artifact| artifact.pointer("/program/initial/nodes"))
        .and_then(Value::as_array)
        .expect("product logical node paths")
        .iter()
        .map(|node| node.get("path").cloned().expect("logical node path"))
        .collect::<Vec<_>>();
    assert!(product_paths.contains(&json!(["forked"])));
    assert!(product_paths.contains(&json!(["mapped"])));
    assert!(product_paths
        .iter()
        .all(|path| path != &json!(["forked>>>mapped"])));

    fs::write(
        temp_root.join("duplicate-logical-path.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.node")
(let start (nefor.graph.source "start" (type-tag String) "shared"))
(let first (nefor.node.rename "duplicate" (nefor.graph.identity "first" (type-tag String))))
(let second (nefor.node.rename "duplicate" (nefor.graph.identity "second" (type-tag String))))
(let workflow (nefor.node.>>> first second))
(let result (nefor.graph.output-for "result" workflow))
(nefor.artifact.compile
  (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
    (nefor.graph.add-edges graph
      [(nefor.graph.edge start workflow)
       (nefor.graph.edge workflow result)])))"#,
    )
    .expect("write duplicate logical path regression");
    let duplicate_path = load(
        &mut reader,
        &mut stdin,
        "duplicate-logical-path",
        &temp_root,
        Path::new("duplicate-logical-path.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        duplicate_path.get("kind").and_then(Value::as_str),
        Some("mag.error"),
        "duplicate logical paths must fail while the graph compiles: {duplicate_path:#?}"
    );
    assert!(
        duplicate_path
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(
                |message| message.contains("logical node paths must be non-empty and unique")
            )
    );

    fs::write(
        temp_root.join("node-choice.mag"),
        r#"(require "core.types")
(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.node")
(type LeftValue {:value String})
(type RightValue {:value Int})
(let start
  (nefor.graph.source "start" (type-tag (core.types.Either LeftValue RightValue))
    (construct (core.types.Either LeftValue RightValue) Left
      (as LeftValue {:value "left"}))))
(let left (nefor.graph.identity "left" (type-tag LeftValue)))
(let right (nefor.graph.identity "right" (type-tag RightValue)))
(let selected (nefor.node.choose "selected" left right))
(let result (nefor.graph.output-for "result" selected))
(nefor.artifact.compile
  (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
    (nefor.graph.add-edges graph
      [(nefor.graph.edge start selected)
       (nefor.graph.edge selected result)])))"#,
    )
    .expect("write node choice regression");
    let node_choice = load(
        &mut reader,
        &mut stdin,
        "node-choice",
        &temp_root,
        Path::new("node-choice.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        node_choice.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "ordinary sum-output node choice must compile against runtime contracts: {node_choice:#?}"
    );

    // A library fragment may expose a useful subset of an actor's
    // runtime outputs. Shell script advertises a structured result plus typed capability failure; the
    // ordinary wrapper deliberately exposes only the structured result. Unknown
    // authored wires remain invalid — the runtime inventory is authoritative.
    fs::write(
        temp_root.join("shell-output-subset.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.shell")
(require "nefor.process")
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let operation (nefor.shell.script "x"
        (as nefor.shell.ShellScriptParams
          {:script "true" :cwd nefor.process.cwd
           :timeout (nefor.contracts.no-timeout)})))
(let result (nefor.graph.output-for "result" operation))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start operation)
         (nefor.graph.edge operation result)])))"#,
    )
    .expect("write shell subset regression");
    let subset = load(
        &mut reader,
        &mut stdin,
        "shell-output-subset",
        &temp_root,
        Path::new("shell-output-subset.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        subset.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "a shell fragment exposing the ProcessResult subset must compile: {subset:#?}"
    );

    fs::write(
        temp_root.join("process-path.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.path")
(require "nefor.process")
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let operation (nefor.process.exec "pwd"
        (as nefor.process.ProcessExecParams
          {:argv ["pwd"] :cwd (nefor.path.join nefor.process.cwd "../outside")
           :timeout (nefor.contracts.no-timeout)})))
(let result (nefor.graph.output-for "result" operation))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start operation)
         (nefor.graph.edge operation result)])))"#,
    )
    .expect("write process path regression");
    let process_path = load(
        &mut reader,
        &mut stdin,
        "process-path",
        &temp_root,
        Path::new("process-path.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        process_path.get("kind").and_then(Value::as_str),
        Some("mag.loaded")
    );
    let process_actor = process_path
        .get("artifact")
        .and_then(|value| value.pointer("/program/initial/actors"))
        .and_then(Value::as_array)
        .and_then(|actors| actors.iter().find(|actor| actor["id"] == "pwd"))
        .expect("compiled process actor");
    assert_eq!(
        process_actor.pointer("/params/value/cwd"),
        Some(&json!("./../outside")),
        "path.join remains lexical and nonconfining"
    );

    let graph_laws = [
        (
            "direct",
            "(nefor.graph.add-edges base [first second])",
        ),
        (
            "permutation",
            "(nefor.graph.add-edges base [second first])",
        ),
        (
            "associative",
            "(nefor.graph.add-edges (nefor.graph.add-edges base [first]) [second])",
        ),
        (
            "idempotent",
            "(nefor.graph.add-edges (nefor.graph.add-edges base [first second]) [second first first])",
        ),
        (
            "absent-removal",
            "(nefor.graph.remove-edges (nefor.graph.add-edges base [first second]) [absent absent])",
        ),
        (
            "remove-add-roundtrip",
            "(nefor.graph.add-edges (nefor.graph.remove-edges (nefor.graph.add-edges base [first second]) [first]) [first])",
        ),
    ];
    let mut algebra_results = Vec::new();
    for (name, expression) in graph_laws {
        let file_name = format!("graph-edge-algebra-{name}.mag");
        let source = format!(
            r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.shell")
(require "nefor.process")
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let operation (nefor.shell.script "operation"
        (as nefor.shell.ShellScriptParams
          {{:script "true" :cwd nefor.process.cwd
           :timeout (nefor.contracts.no-timeout)}})))
(let unused (nefor.shell.script "unused"
        (as nefor.shell.ShellScriptParams
          {{:script "false" :cwd nefor.process.cwd
           :timeout (nefor.contracts.no-timeout)}})))
(let result (nefor.graph.output-for "result" operation))
(let first (nefor.graph.edge start operation))
(let second (nefor.graph.edge operation result))
(let absent (nefor.graph.edge start unused))
(nefor.artifact.compile
  (fn [[base nefor.graph.Graph]] -> nefor.graph.Graph
    {expression}))"#
        );
        fs::write(temp_root.join(&file_name), source).expect("write graph edge algebra regression");
        let result = load(
            &mut reader,
            &mut stdin,
            &format!("graph-edge-algebra-{name}"),
            &temp_root,
            Path::new(&file_name),
            &module_roots,
        )
        .await;
        assert_eq!(
            result.get("kind").and_then(Value::as_str),
            Some("mag.loaded"),
            "pure graph set law {name} must compile: {result:#?}"
        );
        algebra_results.push((name, result));
    }
    let normalize_artifact_order = |result: &Map<String, Value>| {
        let mut artifact = result
            .get("artifact")
            .expect("compiled graph artifact")
            .clone();
        let initial = artifact
            .pointer_mut("/program/initial")
            .expect("program initial");
        initial["actors"]
            .as_array_mut()
            .expect("artifact actors")
            .sort_by_key(|actor| actor["id"].as_str().unwrap_or_default().to_owned());
        initial["nodes"]
            .as_array_mut()
            .expect("artifact nodes")
            .sort_by_key(|node| node["path"].to_string());
        artifact
    };
    let (_, algebra) = &algebra_results[0];
    let expected_artifact = normalize_artifact_order(algebra);
    for (name, result) in &algebra_results[1..] {
        assert_eq!(
            normalize_artifact_order(result),
            expected_artifact,
            "graph set law {name} changed the lowered graph beyond authored first-occurrence order"
        );
        assert!(result["hash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("sha256:")));
    }
    let algebra_actors = algebra
        .get("artifact")
        .and_then(|artifact| artifact.pointer("/program/initial/actors"))
        .and_then(Value::as_array)
        .expect("graph algebra artifact actors");
    assert_eq!(
        algebra_actors.len(),
        3,
        "duplicate additions collapse and absent removals introduce no nodes"
    );
    assert!(
        algebra_actors
            .iter()
            .all(|actor| actor.get("id").and_then(Value::as_str) != Some("unused")),
        "removing an absent edge must leave the graph unchanged"
    );
    let route_count = algebra_actors
        .iter()
        .filter_map(|actor| actor.get("routes").and_then(Value::as_object))
        .flat_map(|routes| routes.values())
        .filter_map(Value::as_array)
        .map(Vec::len)
        .sum::<usize>();
    assert_eq!(route_count, 2, "duplicate edges must not lower twice");

    fs::write(
        temp_root.join("worktree-create.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.worktree")
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let operation (nefor.worktree.create
                 "workspace"
                 (as nefor.worktree.CreateSpec
                   {:repository "/repo" :path "/worktrees/topic"
                    :branch "topic" :base "main"})))
(let result (nefor.graph.output-for "result" operation))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start operation)
         (nefor.graph.edge operation result)])))"#,
    )
    .expect("write worktree create regression");
    let worktree = load(
        &mut reader,
        &mut stdin,
        "worktree-create",
        &temp_root,
        Path::new("worktree-create.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        worktree.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "a native worktree fragment must compile against runtime contracts: {worktree:#?}"
    );
    let create_actor = worktree
        .get("artifact")
        .and_then(|artifact| artifact.pointer("/program/initial/actors"))
        .and_then(Value::as_array)
        .and_then(|actors| {
            actors
                .iter()
                .find(|actor| actor.get("id").and_then(Value::as_str) == Some("workspace"))
        })
        .expect("worktree create actor is visible in the compiled preview");
    assert_eq!(
        create_actor.get("factory").and_then(Value::as_str),
        Some("nefor.factory.worktree-create")
    );
    assert_eq!(
        create_actor.pointer("/params/value/repository"),
        Some(&json!("/repo"))
    );
    assert_eq!(
        create_actor.pointer("/params/value/path"),
        Some(&json!("/worktrees/topic"))
    );
    assert_eq!(
        create_actor.pointer("/params/value/branch"),
        Some(&json!("topic"))
    );
    assert_eq!(
        create_actor.pointer("/params/value/base"),
        Some(&json!("main"))
    );
    assert!(
        create_actor.pointer("/params/value/mode").is_none(),
        "create and open remain distinct identities rather than a mode flag"
    );

    fs::write(
        temp_root.join("worktree-open.mag"),
        r#"(require "nefor.artifact")
(require "nefor.graph")
(require "nefor.worktree")
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let operation (nefor.worktree.open
                 "workspace"
                 (as nefor.worktree.OpenSpec
                   {:repository "/repo" :path "/worktrees/topic"
                    :branch "topic"})))
(let result (nefor.graph.output-for "result" operation))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start operation)
         (nefor.graph.edge operation result)])))"#,
    )
    .expect("write worktree open regression");
    let open_worktree = load(
        &mut reader,
        &mut stdin,
        "worktree-open",
        &temp_root,
        Path::new("worktree-open.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        open_worktree.get("kind").and_then(Value::as_str),
        Some("mag.loaded"),
        "an explicit worktree open fragment must compile: {open_worktree:#?}"
    );
    let open_actor = open_worktree
        .get("artifact")
        .and_then(|artifact| artifact.pointer("/program/initial/actors"))
        .and_then(Value::as_array)
        .and_then(|actors| {
            actors
                .iter()
                .find(|actor| actor.get("id").and_then(Value::as_str) == Some("workspace"))
        })
        .expect("worktree open actor");
    assert_eq!(
        open_actor.get("factory").and_then(Value::as_str),
        Some("nefor.factory.worktree-open")
    );
    assert!(open_actor.pointer("/params/value/base").is_none());

    fs::write(
        temp_root.join("shell-output-unknown.mag"),
        r#"(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")
(require "nefor.shell")
(let input (nefor.graph.port "x" (type-tag Unit) "nefor.process.Input"))
(let output (nefor.graph.port "x" (type-tag nefor.contracts.ProcessResult) "mag.Unknown"))
(let actor (nefor.graph.actor
              "x"
              "nefor.factory.shell-script" []
              (as nefor.shell.ShellScriptParams
                {:script "true" :cwd "." :timeout (nefor.contracts.no-timeout)})
              (nefor.graph.store-port input)
              (as (List nefor.graph.StoredPort) [(nefor.graph.store-port output)])))
(let operation (as (nefor.graph.Node Unit nefor.contracts.ProcessResult)
                 {:id "x" :role "ordinary"
                  :actors (as (List nefor.graph.Actor) [actor])
                  :routes (as (List nefor.graph.StoredRoute) [])
                  :messages (as (List nefor.graph.Message) [])
                  :operations (as (List nefor.mag.ProgramOperation) [])
                  :nodes [(nefor.graph.logical-node ["x"] ["x"])]
                  :input input
                  :output output}))
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let result (nefor.graph.output-for "result" operation))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start operation)
         (nefor.graph.edge operation result)])))"#,
    )
    .expect("write unknown shell output regression");
    let unknown = load(
        &mut reader,
        &mut stdin,
        "shell-output-unknown",
        &temp_root,
        Path::new("shell-output-unknown.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        unknown.get("kind").and_then(Value::as_str),
        Some("mag.error"),
        "an authored output absent from the runtime scheme must fail: {unknown:#?}"
    );
    assert!(
        unknown
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message.contains("actor \\\"x\\\"")
                    && message.contains("factory \\\"nefor.factory.shell-script\\\"")
                    && message.contains("exposes output wires [\\\"mag.Unknown\\\"]")
                    && message.contains("accepted output wires:")
            }),
        "unknown output failure should identify the actor and accepted inventory outputs: {unknown:#?}"
    );

    fs::write(
        temp_root.join("shell-input-unknown.mag"),
        r#"(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")
(require "nefor.shell")
(let input (nefor.graph.port "x" (type-tag Unit) "mag.Unknown"))
(let output (nefor.graph.port "x" (type-tag nefor.contracts.ProcessResult) "nefor.process.Result"))
(let actor (nefor.graph.actor
              "x"
              "nefor.factory.shell-script" []
              (as nefor.shell.ShellScriptParams
                {:script "true" :cwd "." :timeout (nefor.contracts.no-timeout)})
              (nefor.graph.store-port input)
              (as (List nefor.graph.StoredPort) [(nefor.graph.store-port output)])))
(let operation (as (nefor.graph.Node Unit nefor.contracts.ProcessResult)
                 {:id "x" :role "ordinary"
                  :actors (as (List nefor.graph.Actor) [actor])
                  :routes (as (List nefor.graph.StoredRoute) [])
                  :messages (as (List nefor.graph.Message) [])
                  :operations (as (List nefor.mag.ProgramOperation) [])
                  :nodes [(nefor.graph.logical-node ["x"] ["x"])]
                  :input input
                  :output output}))
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let result (nefor.graph.output-for "result" operation))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start operation)
         (nefor.graph.edge operation result)])))"#,
    )
    .expect("write unknown shell input regression");
    let unknown_input = load(
        &mut reader,
        &mut stdin,
        "shell-input-unknown",
        &temp_root,
        Path::new("shell-input-unknown.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        unknown_input.get("kind").and_then(Value::as_str),
        Some("mag.error"),
        "an authored input absent from the runtime scheme must fail: {unknown_input:#?}"
    );
    assert!(
        unknown_input
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message.contains("actor \\\"x\\\"")
                    && message.contains("factory \\\"nefor.factory.shell-script\\\"")
                    && message.contains("rejects input wire \\\"mag.Unknown\\\"")
                    && message.contains("accepted input wires:")
            }),
        "unknown input failure should identify the actor and accepted inventory inputs: {unknown_input:#?}"
    );

    fs::write(
        temp_root.join("factory-identity-unknown.mag"),
        r#"(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")
(require "nefor.shell")
(let input (nefor.graph.port "x" (type-tag Unit) "nefor.process.Input"))
(let output (nefor.graph.port "x" (type-tag nefor.contracts.ProcessResult) "nefor.process.Result"))
(let actor (nefor.graph.actor
              "x"
              "missing.factory" []
              (as nefor.shell.ShellScriptParams
                {:script "true" :cwd "." :timeout (nefor.contracts.no-timeout)})
              (nefor.graph.store-port input)
              (as (List nefor.graph.StoredPort) [(nefor.graph.store-port output)])))
(let operation (as (nefor.graph.Node Unit nefor.contracts.ProcessResult)
                 {:id "x" :role "ordinary"
                  :actors (as (List nefor.graph.Actor) [actor])
                  :routes (as (List nefor.graph.StoredRoute) [])
                  :messages (as (List nefor.graph.Message) [])
                  :operations (as (List nefor.mag.ProgramOperation) [])
                  :nodes [(nefor.graph.logical-node ["x"] ["x"])]
                  :input input
                  :output output}))
(let start (nefor.graph.source "start" (type-tag Unit) nil))
(let result (nefor.graph.output-for "result" operation))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start operation)
         (nefor.graph.edge operation result)])))"#,
    )
    .expect("write unknown factory identity regression");
    let unknown_identity = load(
        &mut reader,
        &mut stdin,
        "factory-identity-unknown",
        &temp_root,
        Path::new("factory-identity-unknown.mag"),
        &module_roots,
    )
    .await;
    assert_eq!(
        unknown_identity.get("kind").and_then(Value::as_str),
        Some("mag.error"),
        "an authored factory absent from the runtime inventory must fail: {unknown_identity:#?}"
    );
    assert!(
        unknown_identity
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message.contains("actor \\\"x\\\"")
                    && message.contains("factory identity \\\"missing.factory\\\"")
                    && message.contains("is not present in the runtime inventory")
            }),
        "unknown identity failure should distinguish missing inventory entries: {unknown_identity:#?}"
    );

    for (index, path) in entrypoints.iter().enumerate() {
        let entry = path
            .strip_prefix(&starter)
            .expect("entrypoint under starter");
        let result = load(
            &mut reader,
            &mut stdin,
            &format!("corpus-entry-{index}"),
            &starter,
            entry,
            &module_roots,
        )
        .await;
        assert_eq!(
            result.get("kind").and_then(Value::as_str),
            Some("mag.loaded"),
            "shipped MAG entrypoint {} failed to compile: {result:#?}",
            path.display()
        );
    }

    for (index, path) in fixtures.iter().enumerate() {
        let expected = fs::read_to_string(path.with_extension("error"))
            .unwrap_or_else(|error| panic!("read expectation for {}: {error}", path.display()));
        assert!(
            !expected.trim().is_empty(),
            "MAG failure fixture {} has an empty .error expectation",
            path.display()
        );
        let entry = path
            .strip_prefix(&fixture_root)
            .expect("fixture under fixture root");
        let result = load(
            &mut reader,
            &mut stdin,
            &format!("corpus-failure-{index}"),
            &fixture_root,
            entry,
            &module_roots,
        )
        .await;
        assert_eq!(
            result.get("kind").and_then(Value::as_str),
            Some("mag.error"),
            "expected MAG fixture {} to fail, got: {result:#?}",
            path.display()
        );
        let message = result
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "mag.error for {} has no message: {result:#?}",
                    path.display()
                )
            });
        assert!(
            message.contains(expected.trim()),
            "MAG fixture {} produced unexpected diagnostic\nexpected substring: {:?}\nactual: {message}",
            path.display(),
            expected.trim()
        );
    }

    shutdown(stdin, child).await;
    let _ = fs::remove_dir_all(temp_root);
}
