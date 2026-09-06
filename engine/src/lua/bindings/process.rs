//! `nefor.process.spawn` — subprocess spawning.
//!
//! Spawns a child process via `tokio::process::Command`. Stdout/stderr are
//! line-buffered into an mpsc channel. A per-process serializer preserves
//! stdout/stderr/exit order and forwards callbacks to the broker-owned Lua
//! callback queue.
//!
//! ## Serialization story
//!
//! Async tasks never enter Lua. They enqueue [`RuntimeCallback`] values on
//! an unbounded channel whose receiver is polled by the broker. Channel
//! readiness wakes the broker's idle `select!`; the broker invokes callbacks
//! and drains any bus events they emit before selecting again. This preserves
//! single-owner Lua ordering rather than relying on mlua's internal mutex.
//!
//! ## Process userdata
//!
//! `nefor.process.spawn(...)` returns a userdata with `kill()` /
//! `wait()` / `write_stdin(bytes)` methods. The userdata holds the kill
//! handle and a one-shot for the exit-wait future.
//!
//! ## Stdin policy
//!
//! Stdin defaults to `/dev/null`. A pipe is opened only when the caller
//! explicitly asks for one — either by passing a `stdin = "<string>"`
//! payload (pre-write + close) or `stdin_piped = true` (keep open for
//! `proc:write_stdin(bytes)`). `claude -p <prompt>` and similar children
//! that don't read stdin silently open it otherwise and print a spurious
//! "no stdin data received" warning after a few seconds.

use std::process::Stdio;
use std::sync::{Arc, Mutex as SyncMutex};

use mlua::{Function, Lua, RegistryKey, Table, UserData, UserDataMethods};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

/// Errors-as-data sentinel for `nefor.process.run` spawn failures. The
/// caller branches on `code` (any non-zero value) rather than handling
/// a Lua-level error — keeps "process couldn't be created" symmetric
/// with "process ran and exited non-zero".
const SPAWN_FAILURE_CODE: i32 = -1;

/// Lua work produced by an asynchronous runtime source. The broker is the
/// only consumer and therefore the only task which invokes these handlers.
pub enum RuntimeCallback {
    ProcessLine {
        handler: Arc<RegistryKey>,
        line: String,
        stream: &'static str,
    },
    ProcessExit {
        handler: Arc<RegistryKey>,
        code: i32,
    },
}

pub type RuntimeCallbackSender = mpsc::UnboundedSender<RuntimeCallback>;
pub type RuntimeCallbackReceiver = mpsc::UnboundedReceiver<RuntimeCallback>;
pub type RuntimeProcessRegistry = Arc<SyncMutex<Vec<watch::Sender<bool>>>>;

pub fn terminate_runtime_processes(registry: &RuntimeProcessRegistry) {
    let mut controls = lock_runtime_processes(registry);
    controls.retain(|control| control.send(true).is_ok());
}

pub fn active_runtime_processes(registry: &RuntimeProcessRegistry) -> usize {
    let mut controls = lock_runtime_processes(registry);
    controls.retain(|control| !control.is_closed());
    controls.len()
}

fn lock_runtime_processes(
    registry: &RuntimeProcessRegistry,
) -> std::sync::MutexGuard<'_, Vec<watch::Sender<bool>>> {
    match registry.lock() {
        Ok(controls) => controls,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Install `nefor.process.spawn` onto `nefor_tbl`.
pub fn install_process(
    lua: &Lua,
    nefor_tbl: &Table,
    runtime_callbacks: RuntimeCallbackSender,
    runtime_processes: RuntimeProcessRegistry,
) -> mlua::Result<()> {
    let process = lua.create_table()?;

    // This clone is used only while registering handler functions. Detached
    // tasks retain registry keys but never use the Lua handle itself.
    let spawn_lua = lua.clone();
    let spawn_fn = lua.create_function(move |_, opts: Table| {
        let runtime_callbacks = runtime_callbacks.clone();
        let runtime_processes = Arc::clone(&runtime_processes);
        let cmd_name: String = opts.get("cmd").map_err(|e| {
            mlua::Error::runtime(format!(
                "nefor.process.spawn: missing or invalid 'cmd' field: {e}"
            ))
        })?;
        if cmd_name.is_empty() {
            return Err(mlua::Error::runtime(
                "nefor.process.spawn: 'cmd' must be a non-empty string",
            ));
        }

        let args: Vec<String> = opts
            .get::<Option<Vec<String>>>("args")
            .unwrap_or(None)
            .unwrap_or_default();
        let cwd: Option<String> = opts.get::<Option<String>>("cwd").unwrap_or(None);
        let env_tbl: Option<Table> = opts.get::<Option<Table>>("env").unwrap_or(None);
        let stdin_string: Option<String> = opts.get::<Option<String>>("stdin").unwrap_or(None);
        let stdin_piped: bool = opts
            .get::<Option<bool>>("stdin_piped")
            .unwrap_or(None)
            .unwrap_or(false);

        let on_stdout = opts.get::<Option<Function>>("on_stdout").unwrap_or(None);
        let on_stderr = opts.get::<Option<Function>>("on_stderr").unwrap_or(None);
        let on_exit = opts.get::<Option<Function>>("on_exit").unwrap_or(None);

        // stdin policy, least-surprise for non-interactive children:
        //   - `stdin = "<string>"`      → piped, pre-write, close (signals EOF).
        //   - `stdin_piped = true`      → piped, kept open; caller writes via
        //                                  the returned handle's `write_stdin`.
        //   - neither                   → `null`. Without this default, children
        //     that don't read stdin (e.g. `claude -p "<prompt>"`) still see an
        //     open pipe and may wait/warn. Null makes stdin a closed /dev/null.
        let stdin_cfg = if stdin_string.is_some() || stdin_piped {
            Stdio::piped()
        } else {
            Stdio::null()
        };

        let mut cmd = Command::new(&cmd_name);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(stdin_cfg)
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        if let Some(env) = env_tbl {
            for pair in env.pairs::<String, String>() {
                let (k, v) = pair?;
                cmd.env(k, v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            mlua::Error::runtime(format!(
                "nefor.process.spawn: failed to spawn {cmd_name:?}: {e}"
            ))
        })?;

        // Stash handlers in the registry so our serializer task can reach
        // them without juggling `Function` handles across tokio tasks.
        let on_stdout_key = stash_fn(&spawn_lua, on_stdout)?;
        let on_stderr_key = stash_fn(&spawn_lua, on_stderr)?;
        let on_exit_key = stash_fn(&spawn_lua, on_exit)?;

        // Line-dispatch channel. Capacity is generous — most subprocesses
        // produce a manageable trickle of output and backpressure is fine.
        let (tx, mut rx) = mpsc::unbounded_channel::<DispatchMsg>();

        // stdout reader.
        let stdout_reader = child.stdout.take().map(|stdout| {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                loop {
                    match reader.next_line().await {
                        Ok(Some(line)) => {
                            if tx.send(DispatchMsg::Stdout(line)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(error = %e, "process stdout read error");
                            break;
                        }
                    }
                }
            })
        });

        // stderr reader.
        let stderr_reader = child.stderr.take().map(|stderr| {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                loop {
                    match reader.next_line().await {
                        Ok(Some(line)) => {
                            if tx.send(DispatchMsg::Stderr(line)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(error = %e, "process stderr read error");
                            break;
                        }
                    }
                }
            })
        });

        // Optional stdin pre-write: write `stdin_string` and drop stdin to
        // close the pipe. Callers who want interactive write-multiple use
        // the userdata's `write_stdin(bytes)` method instead (and should
        // omit the top-level `stdin` option).
        let shared_stdin: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(child.stdin.take()));
        if let Some(payload) = stdin_string {
            let stdin = Arc::clone(&shared_stdin);
            tokio::spawn(async move {
                let mut guard = stdin.lock().await;
                if let Some(s) = guard.as_mut() {
                    if let Err(e) = s.write_all(payload.as_bytes()).await {
                        tracing::warn!(error = %e, "process stdin pre-write failed");
                    }
                    // Drop to close the pipe and signal EOF to the child.
                }
                *guard = None;
            });
        }

        // Exit waiter — converts `child.wait().await` into a dispatch message.
        // We also provide `wait_done_rx` for the userdata's `wait()` method.
        let (exit_tx_user, exit_rx_user) = oneshot::channel::<i32>();
        let (terminate_tx, mut terminate_rx) = watch::channel(false);
        match runtime_processes.lock() {
            Ok(mut controls) => controls.push(terminate_tx.clone()),
            Err(poisoned) => poisoned.into_inner().push(terminate_tx.clone()),
        }
        let process_group = child.id();
        let tx_exit = tx.clone();
        tokio::spawn(async move {
            let waited = tokio::select! {
                status = child.wait() => status,
                changed = terminate_rx.changed() => {
                    if changed.is_ok() && *terminate_rx.borrow() {
                        terminate_process_tree(&mut child, process_group);
                    }
                    child.wait().await
                }
            };
            let code = match waited {
                Ok(status) => status.code().unwrap_or(-1),
                Err(e) => {
                    tracing::warn!(error = %e, "process wait failed");
                    -1
                }
            };

            // Child exit and pipe EOF are separate async observations. Publish
            // Exit only after both readers have drained every preceding line.
            if let Some(reader) = stdout_reader {
                let _ = reader.await;
            }
            if let Some(reader) = stderr_reader {
                let _ = reader.await;
            }

            let _ = exit_tx_user.send(code);
            let _ = tx_exit.send(DispatchMsg::Exit(code));
        });
        // Serialization task: preserve the process stream order while
        // forwarding handler invocations to the broker-owned callback queue.
        // Registry keys drop when this task and the queued callback release them.
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    DispatchMsg::Stdout(line) => {
                        if let Some(k) = &on_stdout_key {
                            let _ = runtime_callbacks.send(RuntimeCallback::ProcessLine {
                                handler: Arc::clone(k),
                                line,
                                stream: "on_stdout",
                            });
                        }
                    }
                    DispatchMsg::Stderr(line) => {
                        if let Some(k) = &on_stderr_key {
                            let _ = runtime_callbacks.send(RuntimeCallback::ProcessLine {
                                handler: Arc::clone(k),
                                line,
                                stream: "on_stderr",
                            });
                        }
                    }
                    DispatchMsg::Exit(code) => {
                        if let Some(k) = &on_exit_key {
                            let _ = runtime_callbacks.send(RuntimeCallback::ProcessExit {
                                handler: Arc::clone(k),
                                code,
                            });
                        }
                        // Exit is terminal — drain any remaining messages
                        // then stop.
                        break;
                    }
                }
            }
        });

        Ok(ProcessHandle {
            exit_rx: Arc::new(Mutex::new(Some(exit_rx_user))),
            stdin: shared_stdin,
            terminate: terminate_tx,
        })
    })?;
    process.set("spawn", spawn_fn)?;

    let run_fn = lua.create_function(|_, opts: Table| run_impl(opts))?;
    process.set("run", run_fn)?;

    nefor_tbl.set("process", process)?;
    Ok(())
}

fn terminate_process_tree(child: &mut tokio::process::Child, process_group: Option<u32>) {
    if terminate_descendants(process_group) {
        return;
    }
    let _ = child.start_kill();
}

fn terminate_descendants(process_group: Option<u32>) -> bool {
    #[cfg(unix)]
    if let Some(process_group) = process_group {
        unsafe {
            libc::killpg(process_group as libc::pid_t, libc::SIGKILL);
        }
        return true;
    }
    #[cfg(not(unix))]
    let _ = process_group;
    false
}

/// Synchronous subprocess invocation. Blocks the calling thread until
/// the child exits and returns a Lua table `{ code, stdout, stderr }`.
/// Spawn failures are folded into the same shape with `code = -1` and a
/// descriptive `stderr` — the caller branches on `code` rather than
/// catching a Lua error.
///
/// Uses `std::process::Command` (sync) deliberately: callers reach for
/// `process.run` from a plain sync Lua context (e.g. `pm.install` at
/// `init.lua` load time) where yielding into an async runtime is wrong.
///
/// Optional opts:
///   * `stdin` — string written to the child's stdin then closed. Required
///     for callers that read the payload off stdin (e.g. `da`).
fn run_impl(opts: Table) -> mlua::Result<RunOutcome> {
    let cmd_name: String = opts.get("cmd").map_err(|e| {
        mlua::Error::runtime(format!(
            "nefor.process.run: missing or invalid 'cmd' field: {e}"
        ))
    })?;
    if cmd_name.is_empty() {
        return Err(mlua::Error::runtime(
            "nefor.process.run: 'cmd' must be a non-empty string",
        ));
    }
    let args: Vec<String> = opts
        .get::<Option<Vec<String>>>("args")
        .unwrap_or(None)
        .unwrap_or_default();
    let cwd: Option<String> = opts.get::<Option<String>>("cwd").unwrap_or(None);
    let env_tbl: Option<Table> = opts.get::<Option<Table>>("env").unwrap_or(None);
    let stdin_data: Option<String> = opts.get::<Option<String>>("stdin").unwrap_or(None);

    let mut cmd = std::process::Command::new(&cmd_name);
    cmd.args(&args)
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if let Some(env) = env_tbl {
        for pair in env.pairs::<String, String>() {
            let (k, v) = pair?;
            cmd.env(k, v);
        }
    }

    // When stdin is requested we spawn + manually write so the child sees
    // its input before we wait_with_output it. `cmd.output()` is the
    // null-stdin fast path; both branches collapse to the same RunOutcome
    // shape so callers don't need to know which fired.
    let result = match stdin_data {
        Some(input) => match cmd.spawn() {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = std::io::Write::write_all(&mut stdin, input.as_bytes());
                    // Drop stdin to signal EOF; without this readers like
                    // `da` (read_to_string on stdin) block forever.
                    drop(stdin);
                }
                child.wait_with_output()
            }
            Err(e) => Err(e),
        },
        None => cmd.output(),
    };

    match result {
        Ok(out) => Ok(RunOutcome {
            code: out.status.code().unwrap_or(SPAWN_FAILURE_CODE),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Err(e) => Ok(RunOutcome {
            code: SPAWN_FAILURE_CODE,
            stdout: String::new(),
            stderr: format!("spawn failed: {e}"),
        }),
    }
}

struct RunOutcome {
    code: i32,
    stdout: String,
    stderr: String,
}

impl mlua::IntoLua for RunOutcome {
    fn into_lua(self, lua: &Lua) -> mlua::Result<mlua::Value> {
        let t = lua.create_table()?;
        t.set("code", self.code)?;
        t.set("stdout", self.stdout)?;
        t.set("stderr", self.stderr)?;
        Ok(mlua::Value::Table(t))
    }
}

enum DispatchMsg {
    Stdout(String),
    Stderr(String),
    Exit(i32),
}

fn stash_fn(lua: &Lua, f: Option<Function>) -> mlua::Result<Option<Arc<RegistryKey>>> {
    match f {
        Some(func) => {
            let key = lua.create_registry_value(func)?;
            Ok(Some(Arc::new(key)))
        }
        None => Ok(None),
    }
}

pub fn invoke_runtime_callback(lua: &Lua, callback: RuntimeCallback) {
    match callback {
        RuntimeCallback::ProcessLine {
            handler,
            line,
            stream,
        } => invoke_line_handler(lua, &handler, &line, stream),
        RuntimeCallback::ProcessExit { handler, code } => {
            invoke_exit_handler(lua, &handler, code);
        }
    }
}

fn invoke_line_handler(lua: &Lua, key: &RegistryKey, line: &str, which: &str) {
    let func: Function = match lua.registry_value(key) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, handler = which, "process handler missing from registry");
            return;
        }
    };
    if let Err(e) = func.call::<()>(line.to_owned()) {
        tracing::error!(error = %e, handler = which, "process handler raised");
    }
}

fn invoke_exit_handler(lua: &Lua, key: &RegistryKey, code: i32) {
    let func: Function = match lua.registry_value(key) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "process on_exit handler missing from registry");
            return;
        }
    };
    if let Err(e) = func.call::<()>(code) {
        tracing::error!(error = %e, "process on_exit handler raised");
    }
}

/// Lua-visible handle returned by `nefor.process.spawn`.
///
/// `wait()` yields until the child exits and returns its status code.
/// `kill()` is best-effort: on Unix it sends SIGKILL via the pid; on
/// Windows it's a no-op for MVP (callers wanting cross-platform signaling
/// can build on top later).
/// `write_stdin(bytes)` writes to the child's stdin if the pipe is still
/// open. Writing after exit is a no-op, not an error.
pub struct ProcessHandle {
    exit_rx: Arc<Mutex<Option<oneshot::Receiver<i32>>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    terminate: watch::Sender<bool>,
}

impl UserData for ProcessHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // wait() -> exit code. Yields the Lua coroutine until the child
        // exits; safe to call multiple times — subsequent calls after the
        // receiver is consumed return -1.
        methods.add_async_method("wait", |_, this, ()| async move {
            let mut guard = this.exit_rx.lock().await;
            match guard.take() {
                Some(rx) => match rx.await {
                    Ok(code) => Ok(code),
                    Err(_) => Ok(-1),
                },
                None => Ok(-1),
            }
        });

        // write_stdin(bytes) -> bool indicating whether the write was
        // accepted (stdin still open) vs dropped (already closed).
        methods.add_async_method("write_stdin", |_, this, data: String| async move {
            let mut guard = this.stdin.lock().await;
            match guard.as_mut() {
                Some(pipe) => match pipe.write_all(data.as_bytes()).await {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        tracing::warn!(error = %e, "process write_stdin failed");
                        Ok(false)
                    }
                },
                None => Ok(false),
            }
        });

        methods.add_async_method("kill", |_, this, ()| async move {
            Ok(signal_termination(&this.terminate))
        });
    }
}

fn signal_termination(terminate: &watch::Sender<bool>) -> bool {
    terminate.send(true).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn setup() -> (Lua, RuntimeCallbackReceiver) {
        let lua = Lua::new();
        let nefor = lua.create_table().unwrap();
        let (callback_tx, callback_rx) = mpsc::unbounded_channel();
        install_process(
            &lua,
            &nefor,
            callback_tx,
            Arc::new(SyncMutex::new(Vec::new())),
        )
        .unwrap();
        lua.globals().set("nefor", nefor).unwrap();
        (lua, callback_rx)
    }

    #[tokio::test]
    async fn echo_emits_stdout_line_and_zero_exit() {
        let (lua, mut callback_rx) = setup();

        let lines = Arc::new(StdMutex::new(Vec::<String>::new()));
        let exit = Arc::new(StdMutex::new(None::<i32>));

        let lines_c = Arc::clone(&lines);
        let on_stdout = lua
            .create_function(move |_, line: String| {
                lines_c.lock().unwrap().push(line);
                Ok(())
            })
            .unwrap();
        let exit_c = Arc::clone(&exit);
        let on_exit = lua
            .create_function(move |_, code: i32| {
                *exit_c.lock().unwrap() = Some(code);
                Ok(())
            })
            .unwrap();
        lua.globals().set("on_stdout", on_stdout).unwrap();
        lua.globals().set("on_exit", on_exit).unwrap();

        lua.load(
            r#"
            proc = nefor.process.spawn({
                cmd = "sh",
                args = { "-c", "echo hello" },
                on_stdout = on_stdout,
                on_exit = on_exit,
            })
            return proc:wait()
            "#,
        )
        .eval_async::<i32>()
        .await
        .expect("wait ok");

        // Let the serializer enqueue, then invoke callbacks as the broker would.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        while let Ok(callback) = callback_rx.try_recv() {
            invoke_runtime_callback(&lua, callback);
        }

        let lines = lines.lock().unwrap().clone();
        let exit = *exit.lock().unwrap();
        assert_eq!(lines, vec!["hello".to_string()]);
        assert_eq!(exit, Some(0));
    }

    #[tokio::test]
    async fn exit_callback_follows_all_short_lived_multiline_output() {
        type Observed = (Vec<String>, Vec<String>, Option<(i32, usize, usize)>);

        let (lua, mut callback_rx) = setup();
        let observed = Arc::new(StdMutex::new(Observed::default()));

        let stdout_observed = Arc::clone(&observed);
        let on_stdout = lua
            .create_function(move |_, line: String| {
                stdout_observed.lock().unwrap().0.push(line);
                Ok(())
            })
            .unwrap();
        let stderr_observed = Arc::clone(&observed);
        let on_stderr = lua
            .create_function(move |_, line: String| {
                stderr_observed.lock().unwrap().1.push(line);
                Ok(())
            })
            .unwrap();
        let exit_observed = Arc::clone(&observed);
        let on_exit = lua
            .create_function(move |_, code: i32| {
                let mut observed = exit_observed.lock().unwrap();
                observed.2 = Some((code, observed.0.len(), observed.1.len()));
                Ok(())
            })
            .unwrap();
        lua.globals().set("on_stdout", on_stdout).unwrap();
        lua.globals().set("on_stderr", on_stderr).unwrap();
        lua.globals().set("on_exit", on_exit).unwrap();

        lua.load(
            r#"
            proc = nefor.process.spawn({
                cmd = "sh",
                args = { "-c", [[
                    i=1
                    while [ "$i" -le 64 ]; do
                        printf 'out-%s\n' "$i"
                        printf 'err-%s\n' "$i" >&2
                        i=$((i + 1))
                    done
                ]] },
                on_stdout = on_stdout,
                on_stderr = on_stderr,
                on_exit = on_exit,
            })
            return proc:wait()
            "#,
        )
        .eval_async::<i32>()
        .await
        .expect("wait ok");

        while observed.lock().unwrap().2.is_none() {
            let callback =
                tokio::time::timeout(std::time::Duration::from_secs(1), callback_rx.recv())
                    .await
                    .expect("process callbacks stalled")
                    .expect("process callback channel closed");
            invoke_runtime_callback(&lua, callback);
        }

        let observed = observed.lock().unwrap();
        assert_eq!(observed.0.len(), 64);
        assert_eq!(observed.1.len(), 64);
        assert_eq!(observed.2, Some((0, 64, 64)));
        assert_eq!(observed.0.first().map(String::as_str), Some("out-1"));
        assert_eq!(observed.0.last().map(String::as_str), Some("out-64"));
        assert_eq!(observed.1.first().map(String::as_str), Some("err-1"));
        assert_eq!(observed.1.last().map(String::as_str), Some("err-64"));
    }

    #[tokio::test]
    async fn missing_cmd_errors() {
        let (lua, _callback_rx) = setup();
        let err = lua
            .load(r#"nefor.process.spawn({ cmd = "" })"#)
            .exec_async()
            .await
            .expect_err("empty cmd must error");
        assert!(err.to_string().contains("non-empty"));
    }

    #[tokio::test]
    async fn nonexistent_binary_errors() {
        let (lua, _callback_rx) = setup();
        let err = lua
            .load(r#"nefor.process.spawn({ cmd = "definitely-not-a-real-binary-xxxyyyzzz" })"#)
            .exec_async()
            .await
            .expect_err("missing binary must error");
        assert!(err.to_string().contains("failed to spawn"));
    }

    #[test]
    fn run_captures_stdout_and_zero_exit() {
        let (lua, _callback_rx) = setup();
        let (code, stdout): (i32, String) = lua
            .load(
                r#"
                local r = nefor.process.run({ cmd = "sh", args = { "-c", "echo hi" } })
                return r.code, r.stdout
                "#,
            )
            .eval()
            .expect("run ok");
        assert_eq!(code, 0);
        assert!(stdout.contains("hi"));
    }

    #[test]
    fn run_propagates_non_zero_exit_as_data() {
        let (lua, _callback_rx) = setup();
        let code: i32 = lua
            .load(
                r#"
                local r = nefor.process.run({ cmd = "sh", args = { "-c", "exit 7" } })
                return r.code
                "#,
            )
            .eval()
            .expect("run ok");
        assert_eq!(code, 7);
    }

    #[test]
    fn run_pipes_stdin_to_child() {
        // `cat` echoes stdin to stdout; if the stdin pipe + EOF wiring is
        // right, the captured stdout contains the input verbatim.
        let (lua, _callback_rx) = setup();
        let (code, stdout): (i32, String) = lua
            .load(
                r#"
                local r = nefor.process.run({
                  cmd   = "cat",
                  stdin = "hello-from-stdin",
                })
                return r.code, r.stdout
                "#,
            )
            .eval()
            .expect("run ok");
        assert_eq!(code, 0);
        assert_eq!(stdout, "hello-from-stdin");
    }

    #[test]
    fn run_spawn_failure_returns_data_not_error() {
        let (lua, _callback_rx) = setup();
        let (code, stderr): (i32, String) = lua
            .load(
                r#"
                local r = nefor.process.run({ cmd = "definitely-not-a-real-binary-xxxyyyzzz" })
                return r.code, r.stderr
                "#,
            )
            .eval()
            .expect("run ok — spawn failure must be data, not a Lua error");
        assert_eq!(code, -1);
        assert!(stderr.contains("spawn failed"), "got: {stderr}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_terminates_and_reaps_spawned_process_group() {
        let (lua, _callback_rx) = setup();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        lua.globals().set("script", script).unwrap();
        let (killed, code): (bool, i32) = lua
            .load(
                r#"
            local proc = nefor.process.spawn({ cmd = "sh", args = { "-c", script } })
            local delay = nefor.process.spawn({ cmd = "sh", args = { "-c", "sleep 0.1" } })
            delay:wait()
            local killed = proc:kill()
            return killed, proc:wait()
        "#,
            )
            .eval_async()
            .await
            .unwrap();
        assert!(killed);
        assert_ne!(code, 0);
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "descendant remained alive after kill"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runtime_teardown_terminates_unretained_process_tree() {
        let lua = Lua::new();
        let nefor = lua.create_table().unwrap();
        let (callback_tx, _callback_rx) = mpsc::unbounded_channel();
        let registry = Arc::new(SyncMutex::new(Vec::new()));
        install_process(&lua, &nefor, callback_tx, Arc::clone(&registry)).unwrap();
        lua.globals().set("nefor", nefor).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("runtime-descendant.pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        lua.globals().set("script", script).unwrap();
        lua.load(
            r#"
            nefor.process.spawn({ cmd = "sh", args = { "-c", script } })
            collectgarbage("collect")
        "#,
        )
        .exec()
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !pid_file.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(active_runtime_processes(&registry), 1);
        terminate_runtime_processes(&registry);
        while active_runtime_processes(&registry) != 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(active_runtime_processes(&registry), 0);
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "engine teardown left an unretained runtime descendant alive"
        );
    }
}
