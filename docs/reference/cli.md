# CLI reference

The engine parses global options and forwards composition arguments to Lua.

## Engine grammar

```text
nefor [GLOBAL_OPTIONS]
nefor [GLOBAL_OPTIONS] run [CONFIG_ARG ...]
nefor [GLOBAL_OPTIONS] plugin
nefor [GLOBAL_OPTIONS] plugin <NAME> [PLUGIN_ARG ...]
```

| Option | Meaning |
| --- | --- |
| `--config <DIR>` | Configuration directory containing `init.lua`. |
| `--data-dir <DIR>` | Writable runtime data root. |
| `--log-file <PATH>` | Exact aggregate log file path. |
| `-h`, `--help` | Engine help. |
| `-V`, `--version` | Build version. |

Bare `nefor` and `nefor run` start serve mode. Only `run` forwards trailing
arguments to Lua as `nefor.runtime.argv`. Every token after `run`, including a
hyphenated token, belongs to the composition. Put engine options before `run`.

```sh
nefor --config ./my-config run --resume SESSION --mode safe
```

`nefor --resume SESSION` is invalid: session selection is not an engine option.

## Forwarded composition arguments

Arguments after `run` are opaque strings owned by the selected `init.lua`.
The engine imposes no syntax or meaning on them.

The shipped agent starter defaults to `tui` and accepts a startup-selected
noninteractive frontend over the same providers, loop, and sessions:

```sh
nefor run --frontend tui --prompt "question"
nefor run --frontend cli --prompt "question"
nefor run --frontend cli --resume SESSION --prompt "follow-up" --format json
nefor run --frontend tui --resume SESSION
```

CLI requires a nonempty `--prompt`; there is no implicit stdin read or REPL.
`--resume` is the documented session selector (`--session` remains an alias).
Contradictory selectors are usage errors. `--mode safe|auto|yolo` and `--yolo`
retain their normal permission meaning. CLI never automatically approves
interactive requests.

CLI `--format text|json` defaults to text. Text stdout is the final whole-request
answer plus a newline, not intermediate acknowledgments or replay. JSON returns
`session_id`, `request_id`, `status`, `answer`, and optional structured `error`.
The activated session ID and diagnostics go to stderr. Exit codes are success 0,
runtime/readiness/resume/persistence failure 1, usage 2, and interruption 130.
See the [frontend contract](../../lua/libs/cli/README.md) for composition and
completion semantics.

## Virtual plugin CLIs

`nefor plugin` lists `cli` entries registered while loading the selected config.
`nefor plugin <NAME> ...` calls that Lua entry and forwards the remaining argv.
Clap consumes a standalone `--`; because the engine intercepts outer help, a
virtual CLI that needs its own help may require:

```sh
nefor plugin <name> -- --help
```

### Development-only `agentic-cli`

The repository's `cli-config/` registers `agentic-cli` for deterministic
experiments. It is not part of the shipped starter, release archive, or
Homebrew distribution. Normal headless execution uses `run --frontend cli`.
The virtual entry delegates to the same [frontend](../../lua/libs/cli/README.md).

## Directory resolution

Configuration:

1. `--config`
2. `NEFOR_CONFIG_DIR`
3. `$XDG_CONFIG_HOME/nefor`, otherwise `~/.config/nefor`

Data:

1. `--data-dir`
2. `NEFOR_DATA_DIR`
3. `$XDG_DATA_HOME/nefor`, otherwise `~/.local/share/nefor`

Aggregate logging destination:

1. non-empty `NEFOR_LOG_STDERR` selects stderr;
2. `--log-file` selects that exact path;
3. `NEFOR_LOG_FILE` selects that exact path;
4. `<data-root>/logs/nefor.log`.

Explicit file paths are not relocated under the data root. The engine creates
parent directories and exits with a diagnostic naming the selected path if it
cannot initialize logging.

Plugin executable and immutable runtime roots are selected by Lua distribution
helpers, not by the engine. The engine has no plugin-directory flag, discovery,
inventory, or installation-provenance concept; it executes the exact command
arrays registered by Lua. The engine exports resolved `NEFOR_CONFIG_DIR` and
`NEFOR_DATA_DIR` before composition and subprocess spawn.

## Version output

`nefor --version` reports the build version without starting the composition.
