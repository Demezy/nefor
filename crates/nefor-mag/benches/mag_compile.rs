mod bench_support;

use bench_support::*;
use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_SAMPLES: usize = 30;
const DEFAULT_WARMUPS: usize = 3;

fn main() {
    let root = workspace_root();
    let samples = positive_env("MAG_BENCH_SAMPLES", DEFAULT_SAMPLES);
    let warmups = positive_env("MAG_BENCH_WARMUPS", DEFAULT_WARMUPS);
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let baseline = value_after(&args, "--baseline");
    let output_path = value_after(&args, "--output");
    let comparison_path = value_after(&args, "--comparison-output");
    let gate = args.iter().any(|arg| arg == "--gate");
    for arg in &args {
        if !matches!(
            arg.as_str(),
            "--bench" | "--baseline" | "--output" | "--comparison-output" | "--gate"
        ) && !args.windows(2).any(|pair| {
            pair[1] == *arg
                && matches!(
                    pair[0].as_str(),
                    "--baseline" | "--output" | "--comparison-output"
                )
        }) {
            panic!("unknown benchmark argument: {arg}");
        }
    }

    let scratch = fresh_scratch(&root);
    let contracts = load_runtime_contracts(&root.join("plugins/mag/lua/mag-kernel/init.lua"));
    let mut timed = timed_cases(&root, &scratch, &contracts);
    let mut oracle_fixtures = oracle_cases(&root, &scratch, &contracts);
    refresh_fixture_fingerprints(&mut timed);
    refresh_fixture_fingerprints(&mut oracle_fixtures);
    let definition_hash = definition_hash(&timed, &oracle_fixtures);
    let cases = timed
        .iter()
        .map(|case| run_case(case, samples, warmups))
        .collect::<Vec<_>>();
    let oracles = oracle_fixtures.iter().map(observe).collect::<Vec<_>>();
    let report = Report {
        schema_version: SCHEMA_VERSION,
        metadata: metadata(&root, samples, warmups, definition_hash),
        counter_semantics: counter_semantics(),
        recommendation: recommendation(&cases),
        cases,
        oracles,
    };
    let comparison = baseline.map(|path| {
        let baseline_path = if Path::new(&path).is_absolute() {
            PathBuf::from(&path)
        } else {
            root.join(&path)
        };
        let baseline: Report =
            serde_json::from_slice(&fs::read(&baseline_path).unwrap_or_else(|error| {
                panic!("read baseline {}: {error}", baseline_path.display())
            }))
            .unwrap_or_else(|error| panic!("parse baseline {path}: {error}"));
        compare_reports(&baseline, &report, gate)
    });
    if gate && comparison.is_none() {
        panic!("--gate requires --baseline");
    }
    if comparison.is_some() && comparison_path.is_none() {
        panic!("comparison requires --comparison-output so the verdict is persistent");
    }
    if let (Some(comparison), Some(path)) = (&comparison, comparison_path) {
        write_json(&root, &path, comparison, "comparison artifact");
    }
    let encoded = serde_json::to_string_pretty(&report).expect("serialize report");
    if let Some(path) = output_path {
        write_json(&root, &path, &report, "benchmark report");
    } else {
        println!("{encoded}");
    }
    fs::remove_dir_all(&scratch).ok();
    if gate
        && comparison
            .as_ref()
            .is_some_and(|artifact| !artifact.overall.passed)
    {
        std::process::exit(2);
    }
}

fn write_json(root: &Path, path: &str, value: &impl serde::Serialize, label: &str) {
    let path = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create artifact directory");
    }
    let encoded = serde_json::to_string_pretty(value).expect("serialize artifact");
    fs::write(&path, format!("{encoded}\n")).expect("write artifact");
    eprintln!("wrote {label} {}", path.display());
}

fn timed_cases(root: &Path, scratch: &Path, contracts: &Value) -> Vec<Fixture> {
    let core_roots = vec![];
    let nefor_roots = vec![
        root.join("mag/lib"),
        root.join("examples/nefor-agent/mag/lib"),
    ];
    let nefor_inputs = json!({"factory_contracts": contracts});
    let mut cases = vec![fixture(
        scratch,
        "trivial",
        "trivial",
        "compile",
        None,
        "(artifact {})",
        core_roots.clone(),
        json!({}),
        None,
        "timed",
        None,
        vec![],
    )];
    for size in [16, 64, 256] {
        for (family, source) in [
            ("core-dead-locals", core_dead_locals(size)),
            ("core-generic-calls", core_generic_calls(size)),
            ("core-concat-growth", core_concat_growth(size)),
            ("core-artifact-control", core_artifact_control(size)),
        ] {
            cases.push(fixture(
                scratch,
                &format!("{family}-{size}"),
                family,
                "compile",
                Some(size),
                &source,
                core_roots.clone(),
                json!({}),
                None,
                "timed",
                None,
                vec![],
            ));
        }
    }
    for size in [2, 8, 12] {
        for stage in ["build", "validate", "lower", "compile"] {
            let source = linear_graph(size, stage);
            let name = if stage == "compile" {
                format!("linear-{size}")
            } else {
                format!("nefor-linear-{stage}-{size}")
            };
            cases.push(fixture(
                scratch,
                &name,
                "nefor-linear",
                stage,
                Some(size),
                &source,
                nefor_roots.clone(),
                nefor_inputs.clone(),
                None,
                "timed",
                None,
                vec![],
            ));
        }
    }
    for size in [2, 8, 16] {
        for stage in ["build", "lower", "compile"] {
            let source = fan_in_graph(size, stage);
            let name = if stage == "compile" {
                format!("product-fan-in-{size}")
            } else {
                format!("nefor-fan-in-{stage}-{size}")
            };
            cases.push(fixture(
                scratch,
                &name,
                "nefor-fan-in",
                stage,
                Some(size),
                &source,
                nefor_roots.clone(),
                nefor_inputs.clone(),
                None,
                "timed",
                None,
                vec![],
            ));
        }
    }
    cases.push(Fixture {
        name: "shipped-lead-turn".into(),
        family: "production".into(),
        stage: "compile".into(),
        size: None,
        source_dir: root.join("examples/nefor-agent"),
        entry: "agentic-loop/lead-turn.mag".into(),
        module_roots: nefor_roots,
        inputs: nefor_inputs,
        fixture_fingerprint: String::new(),
        fixture_files: vec![PathBuf::from("agentic-loop/lead-turn.mag")],
        expected_error: None,
        policy: "timed".into(),
        expected_artifact: None,
        probes: vec![],
    });
    cases
}

fn oracle_cases(root: &Path, scratch: &Path, contracts: &Value) -> Vec<Fixture> {
    let core = vec![];
    let nefor = vec![
        root.join("mag/lib"),
        root.join("examples/nefor-agent/mag/lib"),
    ];
    let inputs = json!({"factory_contracts": contracts});
    let mut out = Vec::new();
    let errors = [
        ("dead-binding-unresolved-symbol", "(let run (fn [] -> Artifact (let dead missing) (artifact {})))\n(run)", "unresolved", "static"),
        ("dead-function-body-return-type-error", "(let run (fn [] -> Int \"wrong\"))\n(artifact {})", "type", "static"),
        ("dead-strict-inference-cycle", "(let a b)\n(let b a)\n(artifact {})", "type", "static"),
        ("unused-required-module-resolution-error", "(require \"missing.module\")\n(artifact {})", "evaluation", "module"),
        ("demanded-local-partial-builtin", "(let run (fn [] -> Artifact (let bad (remove-at [1] 9)) (artifact bad)))\n(run)", "evaluation", "demanded-runtime"),
        ("dead-local-recursion-budget", "(let loop (fn [[n Int]] -> Int (loop n)))\n(let run (fn [] -> Artifact (let dead (loop 0)) (artifact {:ok true})))\n(run)", "budget", "dead_local_may_elide"),
        ("demanded-local-recursion-budget", "(let loop (fn [[n Int]] -> Int (loop n)))\n(let run (fn [] -> Artifact (let dead (loop 0)) (artifact dead)))\n(run)", "budget", "demanded-runtime"),
        ("top-level-dead-partial-builtin-remains-eager", "(let dead (remove-at [1] 9))\n(artifact {:ok true})", "evaluation", "demanded-runtime"),
    ];
    for (name, source, class, policy) in errors {
        let expected = (policy == "dead_local_may_elide").then(|| json!({"ok": true}));
        out.push(fixture(
            scratch,
            name,
            "oracle",
            "oracle",
            None,
            source,
            core.clone(),
            json!({}),
            Some(class),
            policy,
            expected,
            vec![],
        ));
    }
    out.push(fixture(
        scratch,
        "dead-local-partial-builtin",
        "oracle",
        "oracle",
        None,
        "(let run (fn [] -> Artifact (let bad (remove-at [1] 9)) (artifact {:ok true})))\n(run)",
        core.clone(),
        json!({}),
        Some("evaluation"),
        "dead_local_may_elide",
        Some(json!({"ok": true})),
        vec![],
    ));
    out.push(fixture(scratch, "untaken-branch-does-not-demand-local", "oracle", "oracle", None, "(let run (fn [] -> Artifact (if false (artifact (remove-at [1] 9)) (artifact {:ok true}))))\n(run)", core.clone(), json!({}), None, "demanded-runtime", Some(json!({"ok": true})), vec![]));

    let mut unused_module = fixture(
        scratch,
        "unused-required-module-static-error",
        "oracle",
        "oracle",
        None,
        "(require \"broken\")\n(artifact {})",
        core.clone(),
        json!({}),
        Some("unresolved"),
        "module",
        None,
        vec![],
    );
    write_module(&mut unused_module, "broken.mag", "(let broken missing)");
    out.push(unused_module);

    out.push(fixture(scratch, "artifact-dead-named-resident-function-remains-callable", "oracle", "oracle", None, "(let hidden (fn [[value Int]] -> Artifact (artifact {:value value})))\n(artifact {:loaded true})", core.clone(), json!({}), None, "resident", Some(json!({"loaded": true})), vec![probe_success("hidden", json!(7), json!({"value": 7})), probe_error("hidden", json!("wrong"), "type")]));
    out.push(fixture(scratch, "resident-function-reads-captured-top-level-peer", "oracle", "oracle", None, "(let peer 41)\n(let read-peer (fn [[value Int]] -> Artifact (artifact {:peer peer :value value})))\n(artifact {:loaded true})", core.clone(), json!({}), None, "resident", Some(json!({"loaded": true})), vec![probe_success("read-peer", json!(1), json!({"peer": 41, "value": 1}))]));
    let mut module_export = fixture(
        scratch,
        "required-module-export-remains-available",
        "oracle",
        "oracle",
        None,
        "(require \"library\")\n(artifact {:loaded true})",
        core.clone(),
        json!({}),
        None,
        "module",
        Some(json!({"loaded": true})),
        vec![probe_success("library.run", json!(3), json!({"module": 3}))],
    );
    write_module(
        &mut module_export,
        "library.mag",
        "(let run (fn [[value Int]] -> Artifact (artifact {:module value})))",
    );
    out.push(module_export);

    let mut files = fixture(
        scratch,
        "read-and-read-json",
        "oracle",
        "oracle",
        None,
        "(let text (read \"message.txt\"))\n(let data (read-json \"manifest.json\"))\n(artifact {:text text :items (get data \"items\")})",
        core.clone(),
        json!({}),
        None,
        "file-input",
        Some(json!({"text":"hello\n","items":["second","first"]})),
        vec![],
    );
    write_fixture_file(&mut files, "message.txt", b"hello\n");
    write_fixture_file(
        &mut files,
        "manifest.json",
        br#"{"items":["second","first"]}"#,
    );
    out.push(files);

    out.push(fixture(scratch, "nested-host-input", "oracle", "oracle", None,
        "(type Config {:steps (List {:enabled Bool :label String})})\n(artifact (host-input \"config\" (type-tag Config)))",
        core.clone(), json!({"config":{"steps":[{"enabled":true,"label":"build"}]}}), None, "host-input",
        Some(json!({"steps":[{"enabled":true,"label":"build"}]})), vec![]));
    out.push(fixture(
        scratch,
        "missing-host-input",
        "oracle",
        "oracle",
        None,
        "(artifact (host-input \"count\" (type-tag Int)))",
        core.clone(),
        json!({}),
        Some("type"),
        "host-input",
        None,
        vec![],
    ));
    out.push(fixture(scratch, "wrong-nested-host-input", "oracle", "oracle", None,
        "(type Config {:steps (List {:enabled Bool :label String})})\n(artifact (host-input \"config\" (type-tag Config)))",
        core.clone(), json!({"config":{"steps":[{"enabled":"yes","label":"build"}]}}), Some("type"), "host-input", None, vec![]));

    out.push(fixture(scratch, "function-local-closure-capture", "oracle", "oracle", None,
        "(let run (fn [[value Int]] -> Artifact (let captured value) (let emit (fn [[suffix String]] -> Artifact (artifact {:captured captured :suffix suffix}))) (emit \"ok\")))\n(run 6)",
        core.clone(), json!({}), None, "closure", Some(json!({"captured":6,"suffix":"ok"})), vec![]));
    out.push(fixture(scratch, "closures-in-strict-values", "oracle", "oracle", None,
        "(let handlers {:even (fn [[items (List Int)]] -> Bool (if (= (count items) 0) true ((get handlers \"odd\") (remove-at items 0)))) :odd (fn [[items (List Int)]] -> Bool (if (= (count items) 0) false ((get handlers \"even\") (remove-at items 0))))})\n(artifact ((get handlers \"even\") [1 2]))",
        core.clone(), json!({}), None, "closure", Some(json!(true)), vec![]));

    let mut nominal = fixture(scratch, "same-shaped-module-nominals", "oracle", "oracle", None,
        "(require \"left.types\")\n(require \"right.types\")\n(let accept-left (fn [[value left.types.Payload]] -> left.types.Payload value))\n(artifact (accept-left (as right.types.Payload {:value 1})))",
        core.clone(), json!({}), Some("type"), "nominal", None, vec![]);
    write_module(
        &mut nominal,
        "left/types.mag",
        "(type Payload {:value Int})",
    );
    write_module(
        &mut nominal,
        "right/types.mag",
        "(type Payload {:value Int})",
    );
    out.push(nominal);

    let mut ordering = fixture(scratch, "deterministic-derived-ordering", "oracle", "oracle", None,
        "(require \"ordered.values\")\n(let manifest (read-json \"order.json\"))\n(artifact {:collection (map (fn [[entry {:rank String :label String}]] -> String (get entry \"label\")) (sort-by (fn [[entry {:rank String :label String}]] -> String (get entry \"rank\")) [{:rank \"2\" :label \"b\"} {:rank \"1\" :label \"a\"}])) :module ordered.values.items :file (get manifest \"items\")})",
        core.clone(), json!({}), None, "ordering",
        Some(json!({"collection":["a","b"],"module":["module-z","module-a"],"file":["file-2","file-1"]})), vec![]);
    write_module(
        &mut ordering,
        "ordered/values.mag",
        "(let items [\"module-z\" \"module-a\"])",
    );
    write_fixture_file(
        &mut ordering,
        "order.json",
        br#"{"items":["file-2","file-1"]}"#,
    );
    out.push(ordering);

    let type_descriptor = json!({"kind":"named","name":"main.Choice","arguments":[],"body":{"kind":"record","fields":[{"name":"label","type":{"kind":"primitive","name":"String"}}]}});
    let type_schema = json!({"version":1,"root":{"kind":"named","name":"main.Choice","body":{"kind":"record","fields":[{"name":"label","schema":{"kind":"string"}}]}}});
    out.push(fixture(scratch, "evidence-artifact-identity", "oracle", "oracle", None, "(type Choice {:label String})\n(type Selected (| Choice Int))\n(let value (as Selected (as Choice {:label \"yes\"})))\n(artifact {:descriptor (type-evidence (type-tag Choice)) :schema (type-schema (type-tag Choice)) :semantic_id (type-id (type-evidence (type-tag Choice))) :selected value})", core.clone(), json!({}), None, "static", Some(json!({"descriptor":type_descriptor,"schema":type_schema,"semantic_id":"sha256:604d7d96efdd1a0250532974cc8fd2729a659f6d2f67fc25e28d06b32d97dd10","selected":{"type":"sha256:604d7d96efdd1a0250532974cc8fd2729a659f6d2f67fc25e28d06b32d97dd10","value":{"label":"yes"}}})), vec![]));

    out.push(fixture(
        scratch,
        "graph-conflicting-node-definition",
        "oracle",
        "oracle",
        None,
        &invalid_conflict_graph(),
        nefor.clone(),
        inputs.clone(),
        Some("evaluation"),
        "graph-validation",
        None,
        vec![],
    ));
    out.push(fixture(scratch, "graph-validation-priority", "oracle", "oracle", None, "(require \"nefor.artifact\")\n(require \"nefor.graph\")\n(let topology (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph graph))\n(nefor.artifact.compile topology)", nefor, inputs, Some("evaluation"), "graph-validation", None, vec![]));
    out
}

fn core_dead_locals(size: usize) -> String {
    let mut source = String::from("(let work (fn [[x Int]] -> Int (count (map (fn [[v Int]] -> Int v) [1 2 3 4]))))\n(let run (fn [] -> Artifact\n");
    for index in 0..size {
        source.push_str(&format!("  (let dead{index} (work {index}))\n"));
    }
    source.push_str("  (artifact {:ok true})))\n(run)");
    source
}
fn core_generic_calls(size: usize) -> String {
    let values = (0..size)
        .map(|i| format!("{{:value {i}}}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("(let identity (fn [T] [[value T]] -> T value))\n(let values [{values}])\n(let copied (map (fn [[value {{:value Int}}]] -> {{:value Int}} (identity value)) values))\n(artifact {{:count (count copied)}})")
}
fn core_concat_growth(size: usize) -> String {
    let values = (0..size)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!("(let values [{values}])\n(let grown (fold (fn [[out (List Int)] [value Int]] -> (List Int) (concat out [value])) (as (List Int) []) values))\n(artifact {{:count (count grown)}})")
}
fn core_artifact_control(size: usize) -> String {
    let values = (0..size)
        .map(|i| format!("{{:index {i} :label \"item-{i}\"}}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("(artifact {{:items [{values}]}})")
}

fn graph_prelude() -> String {
    "(require \"core.validated\")\n(require \"nefor.artifact\")\n(require \"nefor.contracts\")\n(require \"nefor.graph\")\n(let contracts (host-input \"factory_contracts\" (type-tag (List nefor.graph.FactoryContract))))\n(let pass (fn [[id String]] -> (nefor.graph.Node Int Int) (let input (nefor.graph.port id (type-tag Int) \"nefor.graph.Value\")) (let output (nefor.graph.port id (type-tag Int) \"nefor.graph.Value\")) (let actor (nefor.graph.actor id \"nefor.factory.output\" [(type-evidence (type-tag Int))] (as nefor.graph.OutputParams {}) (nefor.graph.store-port input) [(nefor.graph.store-port output)])) (nefor.graph.node id \"ordinary\" [actor] (as (List nefor.graph.StoredRoute) []) (as (List nefor.graph.Message) []) input output)))\n".into()
}
fn linear_graph(size: usize, stage: &str) -> String {
    let mut source = graph_prelude();
    source.push_str("(let start (nefor.graph.source \"start\" (type-tag Int) 1))\n");
    for index in 0..size {
        source.push_str(&format!("(let n{index} (pass \"n{index}\"))\n"));
    }
    source.push_str("(let out (nefor.graph.output \"out\" (type-tag Int)))\n(let topology (nefor.graph.add-edges nefor.graph.empty-graph [");
    source.push_str("(nefor.graph.edge start n0) ");
    for index in 0..size - 1 {
        source.push_str(&format!("(nefor.graph.edge n{index} n{}) ", index + 1));
    }
    source.push_str(&format!("(nefor.graph.edge n{} out)]))\n", size - 1));
    source.push_str(&stage_artifact(stage));
    source
}
fn fan_in_graph(size: usize, stage: &str) -> String {
    let mut source = graph_prelude();
    for index in 0..size {
        source.push_str(&format!(
            "(let s{index} (nefor.graph.source \"s{index}\" (type-tag Int) {index}))\n"
        ));
    }
    let types = (0..size).map(|_| "Int").collect::<Vec<_>>().join(" ");
    source.push_str(&format!("(let out (nefor.graph.output \"out\" (type-tag (+ {types}))))\n(let topology (nefor.graph.add-edges nefor.graph.empty-graph ["));
    for index in 0..size {
        source.push_str(&format!("(nefor.graph.edge s{index} out) "));
    }
    source.push_str("]))\n");
    source.push_str(&stage_artifact(stage));
    source
}
fn stage_artifact(stage: &str) -> String {
    match stage {
        "build" => "(artifact {:edges (count (get topology \"edges\"))})".into(),
        "validate" => "(let checked (nefor.graph.validate topology contracts))\n(match checked [(core.validated.Valid nefor.graph.Graph) accepted (artifact {:edges (count (get (get accepted \"value\") \"edges\"))})] [(core.validated.Invalid String) rejected (fail (get rejected \"errors\"))])".into(),
        "lower" => "(let lowered (nefor.graph.lower topology))\n(let forced (canonical lowered))\n(artifact {:actors (count (get lowered \"actors\")) :messages (count (get lowered \"messages\")) :forced (not (= forced \"\"))})".into(),
        "compile" => "(let topology-fn (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph (nefor.graph.add-edges graph (get topology \"edges\"))))\n(nefor.artifact.compile topology-fn)".into(),
        _ => unreachable!(),
    }
}
fn invalid_conflict_graph() -> String {
    let mut source = graph_prelude();
    source.push_str("(let start (nefor.graph.source \"start\" (type-tag Int) 1))\n(let left (pass \"same\"))\n(let right-input (nefor.graph.port \"same\" (type-tag Int) \"nefor.graph.Value\"))\n(let right-output (nefor.graph.port \"same\" (type-tag Int) \"different\"))\n(let right-actor (nefor.graph.actor \"same\" \"nefor.factory.output\" [(type-evidence (type-tag Int))] (as nefor.graph.OutputParams {}) (nefor.graph.store-port right-input) [(nefor.graph.store-port right-output)]))\n(let right (nefor.graph.node \"same\" \"ordinary\" [right-actor] (as (List nefor.graph.StoredRoute) []) (as (List nefor.graph.Message) []) right-input right-output))\n(let out (nefor.graph.output \"out\" (type-tag Int)))\n(let topology (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph (nefor.graph.add-edges graph [(nefor.graph.edge start left) (nefor.graph.edge start right) (nefor.graph.edge left out)])))\n(nefor.artifact.compile topology)");
    source
}

fn metadata(root: &Path, samples: usize, warmups: usize, case_definition_hash: String) -> Metadata {
    Metadata {
        git_commit: command_output(root, &["git", "rev-parse", "HEAD"]),
        git_dirty: !command_output(root, &["git", "status", "--porcelain"]).is_empty(),
        git_tree: command_output(root, &["git", "rev-parse", "HEAD^{tree}"]),
        git_diff_digest: dirty_diff_digest(root),
        package_version: env!("CARGO_PKG_VERSION").into(),
        rustc: command_output(root, &["rustc", "-Vv"]),
        target: rustc_host(root),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        profile: "bench (optimized)".into(),
        samples_per_case: samples,
        warmup_iterations: warmups,
        quantile_policy: "nearest-rank empirical quantile: rank = ceil(p*n), one-based; n=3 p90 is max, n=30 p90 is rank 27".into(),
        case_definition_hash,
        size_replacements: std::collections::BTreeMap::from([(
            "nefor-linear requested 16 (expression nesting limit)".into(),
            12,
        )]),
    }
}
fn dirty_diff_digest(root: &Path) -> Option<String> {
    let status = command_output(root, &["git", "status", "--porcelain"]);
    if status.is_empty() {
        return None;
    }
    let output = Command::new("git")
        .args(["diff", "--binary", "HEAD"])
        .current_dir(root)
        .output()
        .expect("git diff for candidate identity");
    Some(fingerprint(&output.stdout))
}

fn definition_hash(cases: &[Fixture], oracles: &[Fixture]) -> String {
    fingerprint(
        cases
            .iter()
            .chain(oracles)
            .flat_map(|case| {
                format!(
                    "{}:{}:{}:{}:{}\n",
                    case.name,
                    case.family,
                    case.stage,
                    case.size.map_or_else(|| "none".into(), |v| v.to_string()),
                    case.fixture_fingerprint
                )
                .into_bytes()
            })
            .collect::<Vec<_>>()
            .as_slice(),
    )
}
fn positive_env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .parse::<std::num::NonZeroUsize>()
                .unwrap_or_else(|_| panic!("{name} must be positive"))
                .get()
        })
        .unwrap_or(default)
}
fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|arg| arg == flag).map(|index| {
        args.get(index + 1)
            .unwrap_or_else(|| panic!("{flag} requires a path"))
            .clone()
    })
}
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}
fn fresh_scratch(root: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = root
        .join("tmp/mag-optimization-cycle-1")
        .join(format!("scratch-{nonce}"));
    fs::create_dir_all(&path).expect("create scratch");
    path
}
fn rustc_host(root: &Path) -> String {
    command_output(root, &["rustc", "-Vv"])
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or("unknown")
        .to_owned()
}
fn command_output(root: &Path, args: &[&str]) -> String {
    Command::new(args[0])
        .args(&args[1..])
        .current_dir(root)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn load_runtime_contracts(path: &Path) -> Value {
    let source = fs::read_to_string(path).expect("read shipped MAG registry");
    let lua = Lua::new();
    install_runtime_registry_host(&lua);
    let directory = path.parent().expect("registry parent");
    let package: Table = lua.globals().get("package").expect("Lua package table");
    let current: String = package.get("path").expect("Lua package path");
    let prefix = [
        directory.join("?.lua"),
        directory.join("?/init.lua"),
        directory.join("../../../../lua/?.lua"),
        directory.join("../../../../lua/?/init.lua"),
    ]
    .iter()
    .map(|pattern| pattern.display().to_string())
    .collect::<Vec<_>>()
    .join(";");
    package
        .set("path", format!("{prefix};{current}"))
        .expect("set Lua package path");
    let registry: Table = lua
        .load(&source)
        .set_name(path.display().to_string())
        .eval()
        .expect("load registry");
    let contracts: mlua::Function = registry
        .get("registry_contracts")
        .expect("registry_contracts");
    let value: LuaValue = contracts
        .call(lua.array_metatable())
        .expect("read registry contracts");
    lua.from_value(value).expect("serialize registry contracts")
}
fn install_runtime_registry_host(lua: &Lua) {
    let nefor = lua.create_table().expect("host");
    nefor
        .set(
            "log",
            lua.create_function(|_, _: String| Ok(())).expect("log"),
        )
        .expect("install log");
    let semantic = lua.create_table().expect("semantic");
    semantic
        .set(
            "id",
            lua.create_function(|lua, descriptor: LuaValue| {
                let descriptor: Value = lua.from_value(descriptor)?;
                let descriptor = nefor_mag::json::concrete_type_from_json(&descriptor)
                    .map_err(|error| mlua::Error::runtime(error.to_string()))?;
                Ok(descriptor.stable_id().to_string())
            })
            .expect("id"),
        )
        .expect("install id");
    nefor.set("semantic_type", semantic).expect("semantic host");
    lua.globals().set("nefor", nefor).expect("registry host");
}
