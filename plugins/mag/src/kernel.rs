// Embedded Lua VM that hosts the MAG kernel.
//
// The kernel proper (actor inventory, lazy construction, routing, the fold
// over graph modifications) is Lua-resident — see
// `plugins/mag/docs/actor-model.md` and `docs/ir.md`. This module is the
// Rust host: it creates the VM, installs the native surface the kernel
// needs (log, json, fs, a millisecond clock, and a bus-emit queue), loads
// the kernel entry file, and drives the kernel's execute seams
// (`begin_run`, `start`, `bus_response`) from the plugin's dispatch loop.
//
// The bus seam is a queue, not an async callback: the kernel modules call
// `nefor.emit` synchronously from inside the fold (never a coroutine), so
// emitted bodies land on an in-VM array that the host drains after each
// kernel call and forwards to the NCP writer. This mirrors the nefor-tui
// plugin's emit-drain pattern.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use mlua::{Function, Lua, LuaSerdeExt, Table, Value};
use serde_json::{Map, Value as JsonValue};

use crate::error::MagError;

/// Global Lua array `nefor.emit` appends to; drained by [`LuaHost::drain_emits`].
const EMIT_QUEUE: &str = "__mag_emit_queue";

/// The fold's verdict on one applied modification (`{ ok, error }` from the
/// kernel's `start` / `apply` seams).
#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    pub ok: bool,
    pub error: Option<String>,
}

/// `begin_run`'s verdict: whether the run context was created, plus the ids
/// of stale runs the kernel reaped at the session boundary (contexts left by
/// a previous session's never-terminated runs) — the host fails their
/// still-pending execute replies.
#[derive(Debug, Clone)]
pub struct BeginRunOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub reaped: Vec<String>,
}

/// Why a run context is torn down. Threaded through `end_run` onto every
/// `mag.actor_killed` the teardown emits — display semantics for consumers
/// (a completed run's sweep must not read as death), not mechanics: kill
/// handlers run and abort envelopes flush identically for every reason.
/// The kernel's fourth reason, `reaped` (session-boundary sweep), is minted
/// Lua-side inside `begin_run` and never passes through here.
#[derive(Debug, Clone, Copy)]
pub enum TeardownReason {
    RunComplete,
    RunFailed,
    Killed,
}

impl TeardownReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::RunComplete => "run_complete",
            Self::RunFailed => "run_failed",
            Self::Killed => "killed",
        }
    }
}

/// A run's terminal signal, surfaced from the sink through the kernel
/// (`mag.run_complete`). Carries the sink's final result INLINE plus the
/// persisted output path: the terminal reply is the one place the control
/// plane consumes the result itself (the lead relays it to the model), so
/// the result rides here; everything mid-run stays paths-only (docs/ir.md).
#[derive(Debug, Clone)]
pub struct RunCompletion {
    pub output_path: Option<String>,
    pub persisted: bool,
    pub result: Option<JsonValue>,
}

#[derive(Debug, Clone)]
pub struct RuleTrigger {
    pub rule_id: String,
    pub function: String,
    pub source_actor: String,
    pub source_wire: String,
    pub emission_seq: u64,
    pub value: JsonValue,
}

/// Owns the Lua VM and the kernel table it produced.
///
/// Kept alive for the whole session: the VM is the kernel's entire world
/// (per the actor model), so dropping it would tear the kernel down.
pub struct LuaHost {
    kernel: Table,
    lua: Lua,
}

impl LuaHost {
    /// Read, evaluate, and hold the kernel at `path`.
    ///
    /// `lua_root` is the composition-owned shared Lua tree (the repo/checkout
    /// `lua/` directory holding `libs/output-persistence` etc.), threaded in
    /// via `--lua-root` argv; `None` falls back to the search in
    /// [`set_kernel_path`].
    ///
    /// The chunk is expected to return a table (the kernel). Anything else is
    /// a [`MagError::KernelNotTable`]. The kernel may call `nefor.log(msg)`
    /// during load; those messages route to the plugin's tracing subscriber
    /// (stderr), never stdout — stdout is the NCP wire.
    pub fn load_kernel(path: &Path, lua_root: Option<&Path>) -> Result<Self, MagError> {
        let source = std::fs::read_to_string(path).map_err(|source| MagError::KernelRead {
            path: path.display().to_string(),
            source,
        })?;

        let lua = Lua::new();
        let data_root = resolve_data_root();
        let sessions_root = resolve_sessions_root(&data_root);
        install_nefor(&lua, data_root.clone(), sessions_root)?;
        if let Some(dir) = path.parent() {
            set_kernel_path(&lua, dir, lua_root, &data_root)?;
        }

        let chunk_name = format!("@{}", path.display());
        let value: Value = lua.load(&source).set_name(chunk_name.as_str()).eval()?;

        let kernel = match value {
            Value::Table(t) => t,
            other => {
                return Err(MagError::KernelNotTable {
                    path: path.display().to_string(),
                    got: other.type_name().to_string(),
                })
            }
        };

        let name = kernel_name(&kernel);
        tracing::info!(kernel = %name.as_deref().unwrap_or("<unnamed>"), "mag kernel loaded");

        Ok(LuaHost { kernel, lua })
    }

    /// The kernel table's `name` field, if it exposed one.
    pub fn kernel_name(&self) -> Option<String> {
        kernel_name(&self.kernel)
    }

    /// The names of the factories the kernel's registry knows — the source of
    /// truth the control plane validates reasoner types against (replaces the
    /// lead's hand-synced allowlist). Empty when the kernel predates the
    /// `registry_names` surface.
    pub fn registry_names(&self) -> Result<Vec<String>, MagError> {
        let f: Option<Function> = self.kernel.get("registry_names")?;
        match f {
            Some(f) => Ok(f.call::<Vec<String>>(())?),
            None => Ok(Vec::new()),
        }
    }

    /// Serializable foreign declarations for MAG library validation. This is
    /// plain immutable data: no Lua constructor or other runtime capability
    /// crosses the evaluation boundary.
    pub fn registry_contracts(&self) -> Result<JsonValue, MagError> {
        let f: Option<Function> = self.kernel.get("registry_contracts")?;
        match f {
            Some(f) => {
                let value: Value = f.call(self.lua.array_metatable())?;
                Ok(self.lua.from_value(value)?)
            }
            None => Ok(JsonValue::Array(Vec::new())),
        }
    }

    pub fn begin_run_with_principal(
        &self,
        run_id: &str,
        run_name: &str,
        session_id: Option<&str>,
        principal: Option<&str>,
        conversation_id: Option<&str>,
    ) -> Result<BeginRunOutcome, MagError> {
        let meta = self.lua.create_table()?;
        meta.set("run_id", run_id)?;
        meta.set("run_name", run_name)?;
        if let Some(s) = session_id {
            meta.set("session_id", s)?;
        }
        if let Some(p) = principal {
            meta.set("principal", p)?;
        }
        if let Some(id) = conversation_id {
            meta.set("conversation_id", id)?;
        }
        let f: Function = self.kernel.get("begin_run")?;
        let res: Table = f.call::<Table>(meta)?;
        Ok(BeginRunOutcome {
            ok: res.get::<Option<bool>>("ok")?.unwrap_or(false),
            error: res.get::<Option<String>>("error")?,
            reaped: res
                .get::<Option<Vec<String>>>("reaped")?
                .unwrap_or_default(),
        })
    }

    /// Apply a program's initial modification through the run's fold. Actors
    /// register at apply and construct lazily at their first satisfied input
    /// contract, so a synchronous program runs to completion inside this call;
    /// an async one (a provider round-trip pending) progresses via
    /// [`LuaHost::bus_response`].
    pub fn start(&self, run_id: &str, modification: &JsonValue) -> Result<ApplyOutcome, MagError> {
        let mod_val = self.lua.to_value(modification)?;
        let f: Function = self.kernel.get("start")?;
        let res: Table = f.call::<Table>((run_id, mod_val))?;
        apply_outcome(&res)
    }

    /// Apply one graph modification directly through a run's fold — the control
    /// plane's direct kernel op (docs/ir.md, "Kernel operations": modifications
    /// reach `actors`/`kills`/`messages` through the fold, and "the control
    /// plane reaches them directly"). The mid-run kill surface uses it: a
    /// `{ kills = [...] }` modification unroutes the target, hands it its final
    /// kill message (the factory's abort envelope reaches the bus), and drops
    /// its correlations. Returns the fold's verbatim `{ ok, error }`.
    pub fn apply(&self, run_id: &str, modification: &JsonValue) -> Result<ApplyOutcome, MagError> {
        let mod_val = self.lua.to_value(modification)?;
        let f: Function = self.kernel.get("apply")?;
        let res: Table = f.call::<Table>((run_id, mod_val))?;
        apply_outcome(&res)
    }

    pub fn take_rule_trigger(&self, run_id: &str) -> Result<Option<RuleTrigger>, MagError> {
        let f: Function = self.kernel.get("take_rule_trigger")?;
        let trigger: Option<Table> = f.call::<Option<Table>>(run_id)?;
        trigger
            .map(|trigger| {
                let source: Table = trigger.get("source")?;
                Ok(RuleTrigger {
                    rule_id: trigger.get("rule_id")?,
                    function: trigger.get("fn")?,
                    source_actor: source.get("actor")?,
                    source_wire: source.get("wire")?,
                    emission_seq: trigger.get("emission_seq")?,
                    value: self.lua.from_value(trigger.get::<Value>("value")?)?,
                })
            })
            .transpose()
    }

    pub fn fail_run(&self, run_id: &str, error: &str) -> Result<bool, MagError> {
        let f: Function = self.kernel.get("fail_run")?;
        Ok(f.call::<bool>((run_id, error))?)
    }

    pub fn steer_run(
        &self,
        run_id: &str,
        actor_id: &str,
        message: &JsonValue,
    ) -> Result<bool, MagError> {
        let f: Function = self.kernel.get("steer_run")?;
        let message = self.lua.to_value(message)?;
        Ok(f.call::<bool>((run_id, actor_id, message))?)
    }

    pub fn resume_actor(
        &self,
        run_id: &str,
        actor_id: &str,
        message: &JsonValue,
    ) -> Result<bool, MagError> {
        let f: Function = self.kernel.get("resume_actor")?;
        let message = self.lua.to_value(message)?;
        Ok(f.call::<bool>((run_id, actor_id, message))?)
    }

    /// End a run: the kernel reaps the context's live actors through the fold
    /// (kill handlers run — abort/cancel envelopes land on the emit queue; the
    /// caller drains them) and drops the context. The reason stamps every
    /// `mag.actor_killed` the teardown emits, so consumers can tell a
    /// completed run's bookkeeping sweep from a real termination
    /// (docs/actor-model.md, Kill reasons). Returns whether a live context
    /// existed.
    pub fn end_run(&self, run_id: &str, reason: TeardownReason) -> Result<bool, MagError> {
        let f: Function = self.kernel.get("end_run")?;
        Ok(f.call::<bool>((run_id, reason.as_str()))?)
    }

    /// Interrupt a live run's in-flight work. Two shapes selected by
    /// `terminate`:
    ///
    /// * `terminate == false` (GRACEFUL — the lead's own turn): settle every
    ///   in-flight capability correlation as a failed reply ("interrupted by
    ///   user") and emit a `tool.cancel` for each. The failure routes through
    ///   the normal tool-failure path and the run STAYS ALIVE, winding down to a
    ///   real final answer (contrast [`LuaHost::end_run`]). The caller drains
    ///   the emit queue (the cancels + any re-fire the settle produced) and
    ///   settles the run only if it reached a terminal state.
    /// * `terminate == true` (TERMINATING — a dispatched sub-run): emit a
    ///   `tool.cancel` per open correlation so the real work dies, but deliver
    ///   NO reply — the run's llm never re-fires. The caller then ends the run
    ///   failed, so it settles `mag.run_result status:"failed"`.
    ///
    /// Returns `(count, terminated)`: correlations touched, and whether the
    /// terminating path ran. An unknown/ended run returns `(0, false)`.
    pub fn interrupt_run(
        &self,
        run_id: &str,
        failure: &str,
        terminate: bool,
    ) -> Result<(u64, bool), MagError> {
        let f: Function = self.kernel.get("interrupt_run")?;
        let res: Table = f.call::<Table>((run_id, failure, terminate))?;
        let count = res.get::<Option<u64>>("interrupted")?.unwrap_or(0);
        let terminated = res.get::<Option<bool>>("terminated")?.unwrap_or(false);
        Ok((count, terminated))
    }

    pub fn bus_observation(
        &self,
        id: &str,
        operation: &str,
        binding: &str,
        value: &JsonValue,
    ) -> Result<Option<String>, MagError> {
        let observation = self.lua.create_table()?;
        observation.set("id", id)?;
        observation.set("operation", operation)?;
        observation.set("binding", binding)?;
        observation.set("value", self.lua.to_value(value)?)?;
        let f: Function = self.kernel.get("bus_observation")?;
        Ok(f.call::<Option<String>>(observation)?)
    }

    /// Deliver a correlated capability response (tool.result-shaped) back to
    /// the requesting actor, advancing any deferred activation it unblocks.
    /// Correlation ids are run-scoped, so the kernel dispatches to the owning
    /// run context and returns its run_id — the caller settles exactly that
    /// run. `None` means the id names no open correlation of ours.
    pub fn bus_response(
        &self,
        id: &str,
        result: Option<&JsonValue>,
        error: Option<&str>,
        completion_delivery: Option<&str>,
    ) -> Result<Option<String>, MagError> {
        let resp = self.lua.create_table()?;
        resp.set("id", id)?;
        if let Some(r) = result {
            resp.set("result", self.lua.to_value(r)?)?;
        }
        if let Some(e) = error {
            resp.set("error", e)?;
        }
        if let Some(delivery) = completion_delivery {
            resp.set("completion_delivery", delivery)?;
        }
        let f: Function = self.kernel.get("bus_response")?;
        Ok(f.call::<Option<String>>(resp)?)
    }

    /// Take a run's completion signal, if that run has finished.
    /// One-shot: clears the slot.
    pub fn take_run_complete(&self, run_id: &str) -> Result<Option<RunCompletion>, MagError> {
        let f: Option<Function> = self.kernel.get("take_run_complete")?;
        let f = match f {
            Some(f) => f,
            None => return Ok(None),
        };
        let rc: Option<Table> = f.call::<Option<Table>>(run_id)?;
        match rc {
            None => Ok(None),
            Some(t) => {
                let result = match t.get::<Value>("result")? {
                    Value::Nil => None,
                    v => Some(self.lua.from_value(v)?),
                };
                Ok(Some(RunCompletion {
                    output_path: t.get::<Option<String>>("output_path")?,
                    persisted: t.get::<Option<bool>>("persisted")?.unwrap_or(false),
                    result,
                }))
            }
        }
    }

    /// Take a run's unhandled-failure signal, if an actor failure escalated to
    /// a run failure (an unrouted failure tag — routing.lua apply_completion →
    /// `mag.run_failed`). One-shot: clears the slot. Returns the failure detail
    /// the run's terminal reply surfaces.
    pub fn take_run_failed(&self, run_id: &str) -> Result<Option<String>, MagError> {
        let f: Option<Function> = self.kernel.get("take_run_failed")?;
        let f = match f {
            Some(f) => f,
            None => return Ok(None),
        };
        let rf: Option<Table> = f.call::<Option<Table>>(run_id)?;
        match rf {
            None => Ok(None),
            Some(t) => {
                let error = t
                    .get::<Option<String>>("error")?
                    .unwrap_or_else(|| "mag run failed".into());
                Ok(Some(error))
            }
        }
    }

    /// Drain everything the kernel emitted since the last drain (bus + lifecycle
    /// events), converting each to an NCP event body. The queue is reset atomically.
    pub fn drain_emits(&self) -> Result<Vec<Map<String, JsonValue>>, MagError> {
        let queue: Table = self.lua.globals().get(EMIT_QUEUE)?;
        let mut out = Vec::new();
        for pair in queue.clone().pairs::<i64, Value>() {
            let (_, v) = pair?;
            let json: JsonValue = self.lua.from_value(v)?;
            if let JsonValue::Object(map) = json {
                out.push(map);
            }
        }
        // Reset to a fresh array so the next drain starts empty.
        self.lua
            .globals()
            .set(EMIT_QUEUE, self.lua.create_table()?)?;
        Ok(out)
    }
}

/// Read `{ ok, error }` off a fold-result table.
fn apply_outcome(res: &Table) -> Result<ApplyOutcome, MagError> {
    Ok(ApplyOutcome {
        ok: res.get::<Option<bool>>("ok")?.unwrap_or(false),
        error: res.get::<Option<String>>("error")?,
    })
}

/// Read the `name` field off a kernel table, tolerating its absence or a
/// non-string value.
fn kernel_name(kernel: &Table) -> Option<String> {
    kernel.get::<Option<String>>("name").ok().flatten()
}

/// Resolve the data root the same way the ecosystem does: `NEFOR_DATA_DIR`,
/// then `XDG_DATA_HOME/nefor`, then `~/.local/share/nefor`. Used for the plugin
/// VM's `nefor.fs.data_root()` and `nefor.fs.sessions_root()` bindings.
fn resolve_data_root() -> String {
    if let Some(d) = std::env::var_os("NEFOR_DATA_DIR") {
        if !d.is_empty() {
            return d.to_string_lossy().into_owned();
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return format!("{}/nefor", xdg.to_string_lossy());
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return format!("{}/.local/share/nefor", home.to_string_lossy());
    }
    String::from("/tmp/nefor")
}

fn resolve_sessions_root(data_root: &str) -> String {
    std::env::var_os("NEFOR_SESSIONS_DIR")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{data_root}/sessions"))
}

/// Point `package.path` at the kernel file's directory (so the entry chunk can
/// `require` sibling modules by bare name) plus the shared `lua/` and
/// `lua/libs/` trees (so `output-persistence` resolves). Lua trees, highest
/// precedence first:
///
/// 1. `lua_root` — the composition-owned `--lua-root` (examples/nefor-agent/init.lua
///    threads its resolved `NEFOR_ROOT/lua` here).
/// 2. `NEFOR_DEV_DIR/lua` — in-checkout dev mode.
/// 3. the repo root's `lua/`, four levels above the kernel dir
///    (`.../plugins/mag/lua/mag-kernel` → root) — covers a bare `--kernel`
///    pointing into a checkout.
/// 4. `<data_root>/nefor/lua` — the pm-managed sparse-clone every installed
///    config bootstraps (examples/nefor-agent/init.lua), so an installed kernel whose
///    config dir carries no `lua/` tree still resolves the shared libs.
fn set_kernel_path(
    lua: &Lua,
    dir: &Path,
    lua_root: Option<&Path>,
    data_root: &str,
) -> Result<(), MagError> {
    let package: Table = lua.globals().get("package")?;
    let current: String = package.get("path")?;

    let mut entries: Vec<String> = Vec::new();
    let kdir = dir.display().to_string();
    entries.push(format!("{kdir}/?.lua"));
    entries.push(format!("{kdir}/?/init.lua"));

    let mut trees: Vec<PathBuf> = Vec::new();
    if let Some(root) = lua_root {
        trees.push(root.to_path_buf());
    }
    if let Some(dev) = std::env::var_os("NEFOR_DEV_DIR") {
        if !dev.is_empty() {
            trees.push(PathBuf::from(dev).join("lua"));
        }
    }
    // .../plugins/mag/lua/mag-kernel → the repo/config root is four levels up
    // (mag-kernel → lua → mag → plugins → root).
    if let Some(root) = dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        trees.push(root.join("lua"));
    }
    trees.push(PathBuf::from(data_root).join("nefor/lua"));
    for tree in trees {
        for base in [tree.clone(), tree.join("libs")] {
            let base = base.display().to_string();
            entries.push(format!("{base}/?.lua"));
            entries.push(format!("{base}/?/init.lua"));
        }
    }

    entries.push(current);
    package.set("path", entries.join(";"))?;
    Ok(())
}

/// Install the `nefor` global the kernel needs: `log`, `json`, `fs`, a
/// millisecond clock, and the bus-emit queue.
fn install_nefor(lua: &Lua, data_root: String, sessions_root: String) -> Result<(), MagError> {
    let nefor = lua.create_table()?;

    let log = lua.create_function(|_, msg: String| {
        tracing::info!(target: "mag::kernel", "{msg}");
        Ok(())
    })?;
    nefor.set("log", log)?;

    install_json(lua, &nefor)?;
    install_typed_json(lua, &nefor)?;
    install_semantic_type(lua, &nefor)?;
    install_fs(lua, &nefor, data_root, sessions_root)?;

    let now_ms = lua.create_function(|_, _: ()| {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Ok(ms)
    })?;
    nefor.set("now_ms", now_ms)?;

    let opaque_id = lua.create_function(|_, _: ()| Ok(uuid::Uuid::new_v4().to_string()))?;
    nefor.set("opaque_id", opaque_id)?;

    // Bus-emit queue. `nefor.emit(body)` appends onto a global array the host
    // drains after each kernel call; the kernel's injected `bus_emit`/
    // `emit_event` seams call it synchronously from inside the fold.
    lua.globals().set(EMIT_QUEUE, lua.create_table()?)?;
    let emit = lua.create_function(|lua, body: Table| {
        let queue: Table = lua.globals().get(EMIT_QUEUE)?;
        let n = queue.raw_len();
        queue.raw_set(n + 1, body)?;
        Ok(())
    })?;
    nefor.set("emit", emit)?;

    lua.globals().set("nefor", nefor)?;
    Ok(())
}

/// Compiler-owned semantic descriptor operations used by runtime defensive
/// checks. Lua may inspect declaration syntax, but graph compatibility and
/// product coverage have exactly one implementation: `ConcreteType`.
fn install_semantic_type(lua: &Lua, nefor_tbl: &Table) -> Result<(), MagError> {
    let semantic_type = lua.create_table()?;
    let id = lua.create_function(|lua, descriptor: Value| {
        let descriptor: JsonValue = lua.from_value(descriptor)?;
        let descriptor = nefor_mag::json::concrete_type_from_json(&descriptor)
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        Ok(descriptor.stable_id().to_string())
    })?;
    semantic_type.set("id", id)?;

    let validate_declarations = lua.create_function(|lua, declarations: Value| {
        let declarations: JsonValue = lua.from_value(declarations)?;
        let declarations = declarations
            .as_object()
            .ok_or_else(|| mlua::Error::runtime("semantic declarations must be an object"))?;
        for (id, descriptor) in declarations {
            let descriptor = nefor_mag::json::concrete_type_from_json(descriptor)
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            let actual = descriptor.stable_id();
            if actual.as_str() != id {
                return Err(mlua::Error::runtime(format!(
                    "semantic declaration key {id} does not match descriptor identity {actual}"
                )));
            }
        }
        Ok(true)
    })?;
    semantic_type.set("validate_declarations", validate_declarations)?;

    let accepts = lua.create_function(|lua, (target, source): (Value, Value)| {
        let target: JsonValue = lua.from_value(target)?;
        let source: JsonValue = lua.from_value(source)?;
        let target = nefor_mag::json::concrete_type_from_json(&target)
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        let source = nefor_mag::json::concrete_type_from_json(&source)
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        Ok(target.accepts_edge_source(&source))
    })?;
    semantic_type.set("accepts", accepts)?;

    let validate_value = lua.create_function(|lua, (descriptor, value): (Value, Value)| {
        let descriptor: JsonValue = lua.from_value(descriptor)?;
        let descriptor = nefor_mag::json::concrete_type_from_json(&descriptor)
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        let schema = nefor_mag::schema::TypeSchema::from_concrete(&descriptor)
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        let value: JsonValue = lua.from_value(value)?;
        lua.to_value(&schema.validate_value(value))
    })?;
    semantic_type.set("validate_value", validate_value)?;

    let input_covered_by = lua.create_function(|lua, (target, sources): (Value, Value)| {
        let target: JsonValue = lua.from_value(target)?;
        let sources: JsonValue = lua.from_value(sources)?;
        let target = nefor_mag::json::concrete_type_from_json(&target)
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        let sources = sources
            .as_array()
            .ok_or_else(|| mlua::Error::runtime("semantic product sources must be a list"))?;
        let sources = sources
            .iter()
            .map(nefor_mag::json::concrete_type_from_json)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        Ok(target.input_is_covered_by(&sources))
    })?;
    semantic_type.set("input_covered_by", input_covered_by)?;
    nefor_tbl.set("semantic_type", semantic_type)?;
    Ok(())
}

/// Rust-owned strict JSON parsing plus MAG schema validation. Keeping parsing
/// and validation in one host call avoids Lua's empty-table array/object
/// ambiguity and ensures runtime acceptance uses the compiler's descriptor.
fn install_typed_json(lua: &Lua, nefor_tbl: &Table) -> Result<(), MagError> {
    let typed_json = lua.create_table()?;
    let validate = lua.create_function(|lua, (schema, source): (Value, String)| {
        let encoded: JsonValue = lua.from_value(schema)?;
        let schema: nefor_mag::schema::TypeSchema = serde_json::from_value(encoded)
            .map_err(|error| mlua::Error::runtime(format!("invalid MAG type schema: {error}")))?;
        lua.to_value(&schema.validate_json(&source))
    })?;
    typed_json.set("validate", validate)?;
    let provider_schema = lua.create_function(|lua, schema: Value| {
        let encoded: JsonValue = lua.from_value(schema)?;
        let schema: nefor_mag::schema::TypeSchema = serde_json::from_value(encoded)
            .map_err(|error| mlua::Error::runtime(format!("invalid MAG type schema: {error}")))?;
        let provider = schema
            .to_provider_schema()
            .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        lua.to_value(&provider)
    })?;
    typed_json.set("provider_schema", provider_schema)?;
    let validate_provider = lua.create_function(|lua, (schema, source): (Value, String)| {
        let encoded: JsonValue = lua.from_value(schema)?;
        let schema: nefor_mag::schema::TypeSchema = serde_json::from_value(encoded)
            .map_err(|error| mlua::Error::runtime(format!("invalid MAG type schema: {error}")))?;
        lua.to_value(&schema.validate_provider_json(&source))
    })?;
    typed_json.set("validate_provider", validate_provider)?;
    nefor_tbl.set("typed_json", typed_json)?;
    Ok(())
}

/// `nefor.json.{encode, decode}` over serde_json (mlua serialize bridge).
/// Mirrors the engine's `nefor::lua::bindings::install_json`.
fn install_json(lua: &Lua, nefor_tbl: &Table) -> Result<(), MagError> {
    let json = lua.create_table()?;

    let encode = lua.create_function(|lua, value: Value| {
        let v: JsonValue = lua.from_value(value)?;
        serde_json::to_string(&v)
            .map_err(|e| mlua::Error::runtime(format!("nefor.json.encode: {e}")))
    })?;
    json.set("encode", encode)?;

    let decode = lua.create_function(|lua, s: String| {
        let v: JsonValue = serde_json::from_str(&s)
            .map_err(|e| mlua::Error::runtime(format!("nefor.json.decode: {e}")))?;
        lua.to_value(&v)
    })?;
    json.set("decode", decode)?;

    // serde_json null and arrays cross into Lua with mlua-owned identities:
    // null is a dedicated userdata sentinel and arrays carry a private
    // metatable (including empty arrays). Expose exact predicates so preview
    // validation can admit JSON-native values without admitting arbitrary
    // userdata or metatable-bearing tables.
    let is_null = lua.create_function(|_, value: Value| Ok(value.is_null()))?;
    json.set("is_null", is_null)?;
    let array_metatable = lua.array_metatable();
    let is_array = lua.create_function(move |_, value: Value| {
        Ok(matches!(value, Value::Table(ref table)
            if table.metatable().is_some_and(|mt| mt.to_pointer() == array_metatable.to_pointer())))
    })?;
    json.set("is_array", is_array)?;
    let array_metatable = lua.array_metatable();
    let mark_array = lua.create_function(move |_, table: Table| {
        table.set_metatable(Some(array_metatable.clone()));
        Ok(table)
    })?;
    json.set("mark_array", mark_array)?;

    nefor_tbl.set("json", json)?;
    Ok(())
}

/// `nefor.fs.*` — the subset the shared `output-persistence` lib needs:
/// `data_root`, `sessions_root`, `mkdir_p`, `read_file`, `write_file`, `exists`.
/// Errors return as data (`{ ok, error }`), matching the engine's `install_fs` convention.
fn install_fs(
    lua: &Lua,
    nefor_tbl: &Table,
    data_root: String,
    sessions_root: String,
) -> Result<(), MagError> {
    let fs_tbl = lua.create_table()?;

    fs_tbl.set(
        "data_root",
        lua.create_function(move |_, _: ()| Ok(data_root.clone()))?,
    )?;
    fs_tbl.set(
        "sessions_root",
        lua.create_function(move |_, _: ()| Ok(sessions_root.clone()))?,
    )?;
    fs_tbl.set(
        "mkdir_p",
        lua.create_function(|lua, path: String| ok_or_err(lua, std::fs::create_dir_all(&path)))?,
    )?;
    fs_tbl.set(
        "exists",
        lua.create_function(|_, path: String| Ok(Path::new(&path).exists()))?,
    )?;
    fs_tbl.set(
        "write_file",
        lua.create_function(|lua, (path, content): (String, String)| {
            ok_or_err(lua, std::fs::write(&path, content))
        })?,
    )?;
    fs_tbl.set(
        "read_file",
        lua.create_function(|lua, path: String| {
            let t = lua.create_table()?;
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    t.set("ok", true)?;
                    t.set("content", content)?;
                }
                Err(e) => {
                    t.set("ok", false)?;
                    t.set("error", e.to_string())?;
                }
            }
            Ok(t)
        })?,
    )?;

    nefor_tbl.set("fs", fs_tbl)?;
    Ok(())
}

fn ok_or_err(lua: &Lua, result: std::io::Result<()>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    match result {
        Ok(()) => t.set("ok", true)?,
        Err(e) => {
            t.set("ok", false)?;
            t.set("error", e.to_string())?;
        }
    }
    Ok(t)
}
