# MAG authoring reference

MAG is a small, typed, expression-oriented language. Programs compose immutable graph values from namespaced libraries. This reference covers the supported author-facing layer; runtime implementation structures are intentionally not public API.

See [Orchestrating MAG](orchestrating.md) for lead tools, [Patterns](../../../plugins/mag/docs/patterns.md) for common topologies, and [Errors](errors.md) for diagnostics.

## Modules and data

Load modules only with literal requires:

```lisp
(require "nefor.actors")
(require "agents")
(require "nefor.agents")
(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")
```

Declare nominal records and algebraic types with `type`:

```lisp
(type Finding {:path String :summary String})
(type Decision (adt [Finding Finding] [AgentError nefor.contracts.AgentError]))
(type Pair (+ Finding Finding))
```

`A | B` is a one-of union. `A + B` is an all-of product. Product occurrences matter: `T + T` requires two matching incoming edges from distinct senders.

Eliminate a sum with an exhaustive `match`. Each arm names one nominal
constructor, binds its payload at that constructor's concrete type, and
produces the same result type:

```lisp
(let describe
  (fn [[decision Decision]] -> String
    (match decision
      [Finding finding (get finding "summary")]
      [nefor.contracts.AgentError failure (get failure "last_output")])))
```

An arm has the shape `[Constructor binding expression]`; a generic constructor
is written as a type application such as `[(Some String) present ...]`.
Missing, repeated, foreign, or non-nominal arms are rejected while checking.
Evaluation selects the arm from constructor evidence retained by MAG, never
from a user-authored string field. Named and generic aliases of sums are
unfolded for exhaustiveness.

Ordinary strings interpret `\n`, `\t`, `\\`, and `\"`. Triple-quoted strings
are raw and may span lines; quotes, `$`, and backslashes inside them have no
special meaning. `strip-margin` follows Scala's margin convention, removing
leading whitespace through `|` while preserving line breaks:

```lisp
(let script
  (strip-margin """|set -e
                    |echo 'export PATH="$HOME/.local/bin:$PATH"'
                    |find . \( -name '*.mag' -o -name '*.md' \)"""))
(let command (replace script "\n" " "))
```

## Bindings and lexical blocks

MAG has one binding form:

```lisp
(let prefix "hello")
(let message (str prefix " world"))
```

A source file and every function body are lexical blocks. Each direct
`(let name value)` declaration adds one immutable typed binding to that block;
`let` is not valid inside an argument, collection, or `if` branch. Extract a
helper function when a nested expression needs several declarations.

Peer declarations are mutually visible, so functions can call later functions
and form mutually recursive families. Strict values remain eager: acyclic
forward references are scheduled automatically, while an eager initialization
cycle is rejected. A name denotes typed overloads, so the same spelling may be
used for different semantic types but not twice for the same type.

## A complete graph

```lisp
(require "nefor.actors")
(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")

(let start (nefor.graph.source "task"
        (type-tag nefor.contracts.Task)
        (as nefor.contracts.Task {:prompt "Inspect the repository."})))
(let worker (nefor.agents.with-tools agents.resolve-model agents.standard "worker"
        "Inspect the repository and report the result."
        nefor.actors.read-only-tools
        (type-tag nefor.contracts.Task)
        (type-tag nefor.contracts.TextAnswer)
        2))
(let result (nefor.graph.output "result"
        (type-tag (core.types.Result nefor.contracts.AgentError nefor.contracts.TextAnswer))))
(nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start worker)
         (nefor.graph.edge worker result)])))
```

An authored program is a pure `Graph -> Graph` function. `nefor.artifact.compile` applies it to `nefor.graph.empty-graph`, validates the complete topology, and prepares a fresh run. Build one flat edge list; compose edge families with `concat` and `map` rather than nested lists.

### Sources and output

`nefor.graph.source<T>` captures and emits a value checked against `T`. It is the only node allowed to have no incoming edge.

`nefor.graph.output<T>` is a concrete `T -> T` identity node and the result boundary. A graph must contain exactly one, it must be terminal, and every ordinary node must be reachable from a source and able to reach it. `nefor.graph.output-for` derives the compatible type from a preceding node.

### Semantic types and runtime wires

A port stores compiler-checked semantic evidence and a runtime protocol wire.
Public constructors derive the latter internally:

- Any input type other than `nefor.contracts.ProviderInput` starts a fresh typed
  user turn.
- `nefor.contracts.ProviderInput` is the nominal continuation type and passes an
  already-built provider turn through unchanged.

MAG programs provide only `(type-tag T)` and connect compatible typed ports.
They do not name, invent, or construct the runtime wire protocol. Prefer public
constructors such as `source`, `agent`, `output`, `command`, and
`worktree.create`.

## Agents

`nefor.actors.agent` has semantic type:

```text
I -> (O | nefor.contracts.AgentError)
```

The whole union must be handled by a compatible downstream node or terminal output. `AgentError` preserves `last_output` and classifies the reason as provider failure or structured-output validation failure. The structured agent automatically asks the model to correct invalid output up to `:max-corrections`; `0` means only the initial attempt. Exhaustion emits `AgentError` as data—it is not a successful `O` and should be routed deliberately.

`:tools` is the agent's capability boundary. Use `nefor.actors.read-only-tools`, `nefor.actors.general-tools`, or an explicit list. A tool call not in the invocation allowlist is rejected even if the tool exists globally. `:da-policy` configures command policy; it does not replace the runtime approval gate.

If a downstream reviewer can work with partial failed output, accept the full union as input. Otherwise route success and error into separate ordinary nodes with `nefor.node.choose`.

## Edges, products, unions, and joins

`nefor.graph.edge` connects compatible nodes. A producer may fan out through several edges; a consumer may fan in through several edges.

- A single input fires for each matching arrival.
- A union input `A | B` fires when either variant arrives.
- A product input `A + B` fires only after every product occurrence is filled. This is the all-of join mechanism.
- Multiple edges from one producer express fan-out.
- A `Unit` dependency edge expresses ordering without transferring domain data.
- Cycles are legal if all nodes remain source-reachable and output-reachable.

There is no graph mutation API. `graph`, `add-edges`, and `remove-edges` are total pure set operations over the graph being authored.

## Process and shell nodes

Use structured `process.exec` when one executable plus arguments expresses the
operation:

```lisp
(require "nefor.process")

(nefor.process.exec
  "search"
  (as nefor.process.ProcessExecParams
    {:argv ["rg" "-n" "TODO" "src/"]
     :cwd nefor.process.cwd
     :timeout (nefor.contracts.no-timeout)}))
```

`argv` is passed directly to process spawn: no shell is inserted, so operators
such as `|`, `>`, glob expansion, and shell built-ins are ordinary arguments.
For shell syntax, use explicit POSIX `shell.script`, which lowers to
`["/bin/sh", "-c", script]`:

```lisp
(require "nefor.shell")

(nefor.shell.script
  "bounded-search"
  (as nefor.shell.ShellScriptParams
    {:script (strip-margin """|rg -n 'TODO|FIXME' src/
                               |  | sort""")
     :cwd "."
     :timeout (nefor.contracts.timeout-ms 30000)}))
```

POSIX shell does not imply Bash. When Bash semantics are required, invoke it
explicitly with `process.exec`, for example `:argv ["/bin/bash" "-lc" "set -o
pipefail; command"]`.

Both nodes require a non-empty `cwd`; relative paths resolve from the MAG host's
inherited working directory, exposed as `nefor.process.cwd` (`"."`). They accept
a `Unit` input for no stdin or `nefor.contracts.Text` to pass upstream
`content` as stdin. The output is `ProcessResult`, containing separate `stdout`,
`stderr`, and a nominal `ProcessExited {code}` or `ProcessSignaled {signal}`
termination value. Use exhaustive `match` to distinguish the two constructors;
authored MAG never compares process-termination strings. Nonzero exit is result
data, not a compilation failure.

Timeouts are mandatory and explicit. `(nefor.contracts.no-timeout)` is
unbounded; use it only when waiting indefinitely is intentional.
`(nefor.contracts.timeout-ms N)` sets a positive wall-clock bound. A process
that never exits keeps its run nonterminal, so an awaited run also waits
indefinitely. The current API has no `bash`, `BashOptions`,
`command-with-options`, or `pipe-command` compatibility surface.

## Human approvals

`nefor.actors.approval-gate` branches a `TextAnswer` into nominal `Approved` and `Rejected` results. Use it when human judgment is part of the graph's meaning. It is distinct from lead `write-review`, which authorizes execution of a write-capable orchestration plan before launch. See [Orchestrating MAG](orchestrating.md#author-and-launch-a-program).

## Runtime expansion

Most workflows should be fully static. When runtime data determines cardinality,
use the node-oriented `DynamicList`/`nefor.dynamic.traverse-template` boundary. It authors
an immutable `InstantiateDeltaTemplate` operation subscribed to a typed output.
Fixed worker lists use `nefor.node.sequence`.

Version 1 evaluates only the closed Trigger, Capture, Field,
IntToDecimalString, and ConcatStrings expression forms while materializing a
structural delta template. There is no general runtime expression language or
post-compilation MAG function application. Operations are program metadata, not
graph edges, and do not give actors authority to alter the graph. See
[MAG composition semantics](../../../plugins/mag/docs/patterns.md).

## Worktrees

`nefor.worktree.create` and `nefor.worktree.open` are explicit, typed workflow nodes:

- `create` requires absolute repository and worktree paths plus branch and base. It creates only a fresh branch/worktree and refuses to adopt an existing path or local branch.
- `open` validates an existing repository/path/branch triple and never creates or changes it.

Successful worktrees outlive the MAG run. The public capability intentionally has no merge, removal, inventory, or cleanup operation. Route the returned `Worktree` into agents that need the isolated path, and keep integration or cleanup outside the graph unless an explicit capability owns it.
