# Headless frontend

`libs.cli` is a noninteractive frontend over the same agentic-loop, providers,
conversation-manager and sessions used by the starter TUI. It starts no TUI
process, requires no terminal and never reads stdin. It does not run a second
agent loop or infer request completion from tool names or graph activity.

## Starter usage

```sh
nefor run --frontend cli --prompt "summarize this codebase"
nefor run --frontend cli --format json --prompt "answer this question"
nefor run --frontend cli --resume SESSION --prompt "continue the investigation"
nefor run --frontend tui --resume SESSION
```

The default frontend remains `tui`. Both frontends accept `--prompt`,
`--resume SESSION` (`--session` is an alias), and the existing
`--mode safe|auto|yolo` / `--yolo` controls. Contradictory session selectors
are usage errors. CLI requires a nonempty explicit prompt, even on resume;
it has no implicit stdin input or REPL. Permission defaults are unchanged.
An unresolved permission popup or review is denied through its original
correlation and fails the request instead of waiting for an absent human.

## Output and completion

- Text stdout contains only the final whole-request answer plus a newline.
  It does not stream acknowledgements, tools, reasoning or replayed history.
- `--format json` prints one object with `session_id`, `request_id`, `status`
  (`success`, `error`, `interrupted`), `answer`, and optional structured `error`.
- Stderr prints `session_id: SESSION` once after activation and useful errors.
- Exit codes: success `0`; runtime, readiness, resume, or persistence failure
  `1`; invalid invocation `2`; interrupted request `130`.

The loop owns whole-request completion, including detached MAG work,
terminal deliveries and lead continuations. CLI consumes the correlated
`agentic_loop.request_completed` event, then asks the sessions actor for a
flush barrier before printing and shutting down. Resident services and
unrelated work are not request obligations.

Session replay reconstructs canonical conversation facts and MAG provenance;
it does not re-execute old prompts or recover interrupted execution after a
process crash. Current-version sessions can be continued by either frontend.
Frontend display catalogs and descriptors are not persisted.

## Composition interface

`require("libs.startup").parse(nefor.runtime.argv)` returns `frontend`,
`session_id`, `prompt`, `mode`, and `format`. `apply_mode(options, loop)`
applies only an explicit mode override.

A composition configures and registers shared providers, sessions, the
conversation manager, loop, tool gate and workflow once. It then either
spawns its chat surface or registers CLI:

```lua
require("libs.cli").start {
  prompt = startup.prompt,
  format = startup.format,
  readiness = composition_readiness,
}
```

The readiness table supplies `required_plugins`, `required_tools`,
`tool_sources`, and optional `timeout_ms`. Exclude `chat-surface` for CLI.
Register replay consumers and frontend observers before calling
`sessions.init(startup.session_id)`; replay may begin synchronously.
Apply explicit startup mode after session initialization. The starter
composition is the executable ordering example.

Both frontends submit `chat.input.submit` with a stable `submission_id`.
A fresh session's empty context is ready without creating conversation facts:
the first canonical submit opens persistence before the loop creates and seeds
its root. Resume instead waits for full replay and its correlated context query.
Direct `agentic_loop.submit` is not a frontend persistence boundary.

Invalid startup arguments must exit during composition loading, before spawning
processes. The starter uses `os.exit(2)` there: the broker shutdown sink is not
yet installed during `init.lua`. After startup the frontend uses cooperative
`nefor.engine.shutdown`. SIGINT is first reported as
`engine.interrupt_requested`; required-process termination is reported as
`engine.plugin_process_terminated`. For accepted CLI work, either fact starts
request cancellation while surviving plugins remain live. The CLI waits for the
canonical MAG result, correlated whole-request completion, and session flush
before it prints and asks the engine to tear down processes. If MAG itself
dies, lead-workflow marks its live handles unknown and request lifecycle emits a
correlated `mag_authority_lost` completion after releasing obligations. This is
not a `mag.run_result`: only MAG can author that canonical execution terminal.

The development-only `plugin agentic-cli` spelling calls the same `start`
implementation. Its one positional prompt is translated to `--prompt`;
otherwise it accepts the shared startup options. The former REPL,
`stream-json`, file-prepend and model-switching parser are removed. The
shipped starter needs no virtual plugin for headless operation.
