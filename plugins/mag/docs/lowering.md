# Lowering — library data to a runtime artifact

Actor-specific MAG libraries construct ordinary values containing a runtime
factory identity, explicit concrete type arguments, parameters, and semantic
ports. Semantic MAG types remain separate from runtime wire tags: libraries
choose the wire protocol and generic functions preserve their typed relations.

MAG has no graph syntax or graph-specific lowering pass. It evaluates pure,
typed library code. The shipped Nefor libraries represent actors, ports,
routes, messages, rules, and result selection as nominal data whose semantic
fields contain opaque compiler descriptors. Graph validation delegates
compatibility and product coverage to the compiler rather than interpreting
descriptor maps in MAG, then marks the lowered value as the compilation result:

```lisp
(artifact modification-data)
```

The complete path is:

```text
namespaced modules
  -> ordinary typed actor values
  -> nefor.graph.Graph
  -> nefor.graph.validate
  -> Nefor-owned nefor.mag v1 program or delta envelope
  -> Artifact(opaque application value)
  -> runtime binding and defensive validation
```

The envelope schema lives in `mag/lib/nefor/mag.mag`; core MAG remains
schema-opaque. A program envelope contains an initial concrete modification and
an ordered list of operations. A delta envelope contains one concrete delta.
Version 1 defines exactly one operation, `InstantiateDeltaTemplate`, whose
closed expression vocabulary is Trigger, Capture, Field, IntToDecimalString,
and ConcatStrings. Its structural template names actor slots, local/existing
actor references, typed ports, routes and product positions, typed messages,
logical paths, scalar parameter bindings, and explicit actor-id relocation
metadata. It contains no executable MAG, generic AST, source, bytecode,
condition, nested operation, or generic object-construction facility.

## Authoring layer

`nefor.graph` defines graph data and composition functions. `nefor.actors` and
`nefor.shell` are ordinary libraries that return `nefor.graph.Node` values;
there are no compiler builtins named `agent`, `bash`, `graph`, `subgraph`, or
`sink`.

A typed port records two identities:

- `type`: a compiler-created `TypeTag<T>` witness, used by generic library
  composition and lowered to a complete canonical structural descriptor;
- `wire`: the runtime tag emitted or accepted by the implementation.

This lets an agent node expose `(CodeAudit | AgentError)` on the stable
`nefor.agent.Result` wire. The success type is declared with
`(type-tag CodeAudit)`; an undeclared or misspelled semantic type fails
compilation. Compatible edges route each selected constructor directly.
Resident rules use `nefor.actors.result-arm` to subscribe to one constructor
on the same actor and wire. The compiler neither knows what an LLM is nor
invents a coercion.

Actor-specific constructors are ordinary typed functions. They select a
factory identity, make generic arguments explicit as type descriptors, and
call `nefor.graph.actor` with parameters whose type their own signature owns.
Runtime registry contracts arrive through the domain-neutral typed
`host-input` boundary and are checked again by `nefor.graph.validate` before
lowering.

## Runtime artifact

`nefor.graph.lower` produces the raw graph-modification value consumed by the
kernel: actors, typed routes, typed initial messages, kills, rules, and
structural result metadata. Each explicit initial message retains its
destination descriptor as `semantic_type` even though the current factory
protocol still consumes `content.kind`. Lowering also gives every `Unit` actor
input with no incoming route and no explicit message exactly one typed
bootstrap message. Consequently any unfed `Node Unit T` is a source boundary;
the same node behind an incoming edge remains dependency-driven. A concrete
`output<T>` identity actor is the unique terminal, and the structural result
metadata selects that actor's output port.

The runtime binds each qualified factory identity to an implementation and
revalidates its concrete input/output contract as exact semantic-type/runtime-
wire pairs. Rust's `ConcreteType` relation is the single owner of semantic
edge compatibility and product coverage; Lua validates protocol wiring and
delegates non-trivial semantic checks to that host relation. The runtime
remains authoritative for
typed firing, sender-bound product slots, routing, lifecycle, failure handling,
and result completion.

Runtime expansion is authored as immutable operation data:

```text
typed trigger/captures -> closed expression references -> structural DeltaTemplate
  -> InstantiateDeltaTemplate -> ordered program operation
```

The current runtime still executes a resident named MAG function carried only
in the initial modification as a temporary staged seam. It is not part of the
canonical operation schema and new traversal authoring lowers the declarative
template alongside it. The next migration unit replaces that seam with direct
template materialization and then removes retained compiler environments,
`mag.eval`, and legacy rules.

A concrete delta has no result boundary or nested operations. Its routes may
target actors already live in the run; the runtime registry validates those
references against the combined live-plus-new inventory before applying
anything. Delta lowering applies the same bootstrap rule to newly introduced
actors, so an unfed `Unit` input starts once whether it was introduced in the
initial graph or by expansion.

## One-off command expressions

`mag-eval` wraps one node expression with a source, output, and the standard
artifact pipeline, so
the expression itself is concise:

```lisp
(nefor.process.exec "search" (as nefor.process.ProcessExecParams {:argv ["rg" "-n" "TODO" "src/"] :cwd nefor.process.cwd :timeout (nefor.contracts.no-timeout)}))
```

For a one-off pipeline, keep it inside the command node:

```lisp
(nefor.shell.script "search"
  (as nefor.shell.ShellScriptParams
    {:script (strip-margin """|rg -n 'TODO|FIXME' src/ |
                               |  sort""")
     :cwd "."
     :timeout (nefor.contracts.timeout-ms 30000)}))
```

Multi-node pipelines are full `.mag` programs: compose typed nodes through
`nefor.node`, connect the resulting boundary to an output, then pass a
`Graph -> Graph` function to `nefor.artifact.compile`. Each compilation
applies that function to `empty-graph` and validates the result for a fresh
run; it does not retrieve or mutate a stored graph.

## Module resolution

`core/types.mag` has identity `core.types` and is required as
`"core.types"`. Imports are transitive, definitions remain in their canonical
namespace, and each module evaluates once. `mag.load.module_roots` is the
complete ordered search path; ambiguous identities across roots are errors.
