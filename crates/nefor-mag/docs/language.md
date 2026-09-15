# MAG authoring reference

MAG is a small, typed, expression-oriented language. Programs compose immutable graph values from namespaced libraries. This reference covers the supported author-facing layer; runtime implementation structures are intentionally not public API.

See [Orchestrating MAG](orchestrating.md) for lead tools, [Patterns](../../../plugins/mag/docs/patterns.md) for common topologies, and [Errors](errors.md) for diagnostics.

## Modules and data

Import modules by identity. The empty selector imports the module while definitions remain in their canonical namespace:

```mag
import nefor.actors.{}
import agents.{}
import nefor.agents.{}
import nefor.artifact.{}
import nefor.contracts.{}
import nefor.graph.{}
```

Imports are declarations, not runtime expressions. A `.mag` suffix selects this syntax and a `.magl` suffix selects the legacy Lisp parser; explicit entry overrides may select either frontend without changing imported modules' suffix selection.

Declare nominal records and algebraic data types with `type`:

```mag
type Finding {path: String, summary: String}
type Decision = Finding(Finding) | AgentError(nefor.contracts.AgentError)
```

Algebraic alternatives are owned by their declared ADT. Construct one through the owner, such as `Decision.Finding(finding)` or `Decision.AgentError(failure)`. `(A, B)` is an anonymous all-of product type, not a nominal declaration. Product occurrences matter: `(T, T)` requires two matching incoming edges from distinct senders.

Eliminate an ADT with an exhaustive `match`. Each case names one constructor, binds its payload at that constructor's concrete type, and produces the same result type:

```mag
let describe: fn(Decision) -> String = |decision| => match decision {
  case Finding(finding) => get(finding, "summary"),
  case AgentError(failure) => canonical(get(failure, "last_output")),
}
```

A case has the shape `case Constructor(binding) => expression`. Missing, repeated, foreign, or non-nominal cases are rejected while checking. Evaluation selects the case from constructor evidence retained by MAG, never from a user-authored string field. Generic ADT instantiations retain their owner and constructor identities during exhaustiveness checking. Generic binder names must be unique within one binder list; nested scopes may reuse names.

Ordinary strings interpret `\n`, `\t`, `\\`, and `\"`. Triple-quoted strings are raw and may span lines; quotes, `$`, and backslashes inside them have no special meaning. <code>`strip-margin`</code> follows Scala's margin convention, removing leading whitespace through `|` while preserving line breaks:

```mag
let script = `strip-margin`("""|set -e
                               |echo 'export PATH="$HOME/.local/bin:$PATH"'
                               |find . \( -name '*.mag' -o -name '*.md' \)""")
let command = replace(script, "\n", " ")
```

## Native data collections and equality

`[...]` constructs the single native `List` representation. Parenthesized comma-separated values construct a product such as `(A, B, ...)`. A nominal record value uses `Type {field: value}`; a generic record uses `Type<A, B> {field: value}`. Braced fields without a nominal type name are blocks, not anonymous record values. Use `Map<K, V>` for homogeneous dynamically keyed data.

Native `Map<K, V>` and `Set<T>` values have no literal syntax. Import `core.map.{}` or `core.set.{}` and construct them through those ordinary modules. Map/Set lookup, membership, count, and insertion do not reveal storage order; no ordered fold or enumeration API is exposed. Inserting an equal existing key or member is an evaluation error. Map and Set equality is extensional and unordered.

`=` performs exact recursive equality over concrete data. It preserves nominal ADT ownership and constructor identity, compares products/lists positionally, and compares Float bit patterns. Functions, type witnesses/descriptors, packed compiler values, artifacts, and data containing such opaque behavior are rejected statically. Generic functions that use equality carry this requirement to each instantiation.

Artifact serialization is deterministic but does not make collection order observable. String-keyed maps become canonically keyed JSON objects; other maps and sets use reserved `$mag` envelopes whose entries are sorted only while serializing.

## Bindings and lexical blocks

Bindings use `let`, with an optional type annotation:

```mag
let prefix = "hello"
let message: String = str(prefix, " world")
```

A source file and every braced expression block are lexical blocks. Each direct `let name = value` declaration adds one immutable typed binding to that block; a block's final expression is its value. `let` is not valid as an ordinary argument, list item, or unbraced branch. Use a block or extract a helper function when an expression needs several declarations.

Peer declarations are mutually visible, so functions can call later functions and form mutually recursive families. Strict values remain eager: acyclic forward references are scheduled automatically, while an eager initialization cycle is rejected. A name denotes typed overloads, so the same spelling may be used for different semantic types but not twice for the same type.

Function types use `fn(A, B) -> C`; function values use `|a, b| => expression`. Generic parameters follow the binding name:

```mag
type Entry<K, V> {key: K, value: V}
let entry<K, V>: fn(K, V) -> Entry<K, V> = |key, value| =>
  Entry<K, V> {key: key, value: value}
```

Names containing punctuation or reserved words are enclosed in backticks, for example <code>nefor.node.`>>>`</code> or a local <code>`max-retries`</code> binding.

## A complete graph

```mag
import core.types.{}
import agents.{}
import nefor.actors.{}
import nefor.agents.{}
import nefor.artifact.{}
import nefor.contracts.{}
import nefor.graph.{}

let start = nefor.actors.`task-source`("task", "Inspect the repository.")
let worker = nefor.agents.`with-tools`(
  agents.`resolve-model`,
  agents.standard,
  "worker",
  "Inspect the repository and report the result.",
  nefor.actors.`read-only-tools`,
  type_tag<nefor.contracts.Task>(),
  type_tag<nefor.contracts.TextAnswer>(),
  2,
)
let result = nefor.graph.output(
  "result",
  type_tag<core.types.Result<nefor.contracts.AgentError, nefor.contracts.TextAnswer>>(),
)

nefor.artifact.compile((|graph| => nefor.graph.`add-edges`(graph, [
  nefor.graph.edge(start, worker),
  nefor.graph.edge(worker, result),
])): fn(nefor.graph.Graph) -> nefor.graph.Graph)
```

An authored program is a pure `Graph -> Graph` function. `nefor.artifact.compile` applies it to <code>nefor.graph.`empty-graph`</code>, validates the complete topology, and prepares a fresh run. Build one flat edge list; compose edge families with `concat` and `map` rather than nested lists.

### Sources and output

`nefor.graph.source<T>` captures and emits a value checked against `T`. More generally, any exposed node whose input accepts `Unit` may be an unfed root and receives one automatic activation; feeding it through an edge suppresses that activation.

`nefor.graph.output<T>` is a concrete `T -> T` identity node and the result boundary. A graph must contain exactly one, it must be terminal, and every ordinary node must be reachable from a root and able to reach it. <code>nefor.graph.`output-for`</code> derives the compatible type from a preceding node.

### Semantic types and runtime wires

A port stores compiler-checked semantic evidence and a runtime protocol wire. Public constructors derive the latter internally:

- Any input type other than `nefor.contracts.ProviderInput` starts a fresh typed user turn.
- `nefor.contracts.ProviderInput` is the nominal continuation type and passes an already-built provider turn through unchanged.

MAG programs provide only `type_tag<T>()` and connect compatible typed ports. They do not name, invent, or construct the runtime wire protocol. Prefer public constructors such as `source`, `agent`, `output`, `process.exec`, and `worktree.create`.

## Agents

`nefor.actors.agent` has semantic type:

```text
I -> core.types.Result<nefor.contracts.AgentError, O>
```

The whole result must be handled by a compatible downstream node or terminal output. `AgentError` preserves `last_output` and classifies the reason as provider failure or structured-output validation failure. The structured agent automatically asks the model to correct invalid output up to `max-corrections`; `0` means only the initial attempt. Exhaustion emits `AgentError` as data—it is not a successful `O` and should be routed deliberately.

`tools` is the agent's capability boundary. Use <code>nefor.actors.`read-only-tools`</code>, <code>nefor.actors.`general-tools`</code>, or an explicit list. A tool call not in the invocation allowlist is rejected even if the tool exists globally. `da-policy` configures command policy; it does not replace the runtime approval gate.

If a downstream reviewer can work with partial failed output, accept the full result as input. Otherwise bind the successful continuation with `nefor.node.>=>`, consume the full `Result`, or use an explicit Result unpack/repack node. `nefor.node.choose` is the corresponding branching combinator for `core.types.Either<A, B>`.

## Edges, products, ADTs, and joins

`nefor.graph.edge` connects compatible nodes. A producer may fan out through several edges; a consumer may fan in through several edges.

- A single input fires for each matching arrival.
- A nominal ADT input is one owner type and fires for each complete owner value; constructors are unpacked only by explicit branching nodes.
- A product input `(A, B)` fires only after every product occurrence is filled. This is the all-of join mechanism.
- Multiple edges from one producer express fan-out.
- A `Unit` dependency edge expresses ordering without transferring domain data.
- Cycles are legal if all nodes remain source-reachable and output-reachable.

There is no graph mutation API. `graph`, <code>`add-edges`</code>, and <code>`remove-edges`</code> are total pure set operations over the graph being authored.

## Process and shell nodes

Use structured `process.exec` when one executable plus arguments expresses the operation:

```mag
import nefor.contracts.{}
import nefor.process.{}

nefor.process.exec("search", nefor.process.ProcessExecParams {
  argv: ["rg", "-n", "TODO", "src/"],
  cwd: nefor.process.cwd,
  timeout: nefor.contracts.`no-timeout`(),
})
```

`argv` is passed directly to process spawn: no shell is inserted, so operators such as `|`, `>`, glob expansion, and shell built-ins are ordinary arguments. For shell syntax, use explicit POSIX `shell.script`, which lowers to `["/bin/sh", "-c", script]`:

```mag
import nefor.contracts.{}
import nefor.shell.{}

nefor.shell.script("bounded-search", nefor.shell.ShellScriptParams {
  script: `strip-margin`("""|rg -n 'TODO|FIXME' src/
                            |  | sort"""),
  cwd: ".",
  timeout: nefor.contracts.`timeout-ms`(30000),
})
```

POSIX shell does not imply Bash. When Bash semantics are required, invoke it explicitly with `process.exec`, for example `argv: ["/bin/bash", "-lc", "set -o pipefail; command"]`.

Both nodes require a non-empty `cwd`; relative paths resolve from the MAG host's inherited working directory, exposed as `nefor.process.cwd` (`"."`). They accept `Unit`; an unfed node receives one automatic activation, while an incoming `Unit` edge makes it dependency-driven. The output is `ProcessResult`, containing separate `stdout`, `stderr`, and a nominal `ProcessExited` or `ProcessSignaled` termination value. Use exhaustive `match` to distinguish the two constructors; authored MAG never compares process-termination strings. Nonzero exit is result data, not a compilation failure.

Timeouts are mandatory and explicit. <code>nefor.contracts.`no-timeout`()</code> is unbounded; use it only when waiting indefinitely is intentional. <code>nefor.contracts.`timeout-ms`(N)</code> sets a positive wall-clock bound. A process that never exits keeps its run nonterminal, so an awaited run also waits indefinitely. The current API has no `bash`, `BashOptions`, `command-with-options`, or `pipe-command` compatibility surface.

## Human approvals

<code>nefor.actors.`approval-gate`</code> branches a `TextAnswer` into nominal `Approved` and `Rejected` results. Use it when human judgment is part of the graph's meaning. It is distinct from lead `write-review`, which authorizes execution of a write-capable orchestration plan before launch. See [Orchestrating MAG](orchestrating.md#author-and-launch-a-program).

## Runtime expansion

Most workflows should be fully static. When runtime data determines cardinality, a producer exposes the indexed-items-plus-completion `DynamicList<T>` protocol. Consumers retain that same nominal boundary: <code>nefor.dynamic.`traverse-template`</code> materializes one worker per item, while `nefor.dynamic.context` buffers through completion and activates once with the ordered list. The operation or factory owns this interpretation; the compiler grants no privileges from type-name spelling. Fixed worker lists use `nefor.node.sequence`.

Version 1 evaluates only the closed Trigger, Capture, Field, IntToDecimalString, and ConcatStrings expression forms while materializing a structural delta template. There is no general runtime expression language or post-compilation MAG function application. Operations are program metadata, not graph edges, and do not give actors authority to alter the graph. See [MAG composition semantics](../../../plugins/mag/docs/patterns.md).

## Worktrees

`nefor.worktree.create` and `nefor.worktree.open` are explicit, typed workflow nodes:

- `create` requires absolute repository and worktree paths plus branch and base. It creates only a fresh branch/worktree and refuses to adopt an existing path or local branch.
- `open` validates an existing repository/path/branch triple and never creates or changes it.

Successful worktrees outlive the MAG run. The public capability intentionally has no merge, removal, inventory, or cleanup operation. Route the returned `Worktree` into agents that need the isolated path, and keep integration or cleanup outside the graph unless an explicit capability owns it.
