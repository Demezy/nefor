# Composition semantics for MAG-authored graphs

These are the runtime meanings behind Nefor's node combinators. They are not
workflow recipes: task-specific code chooses whichever composition preserves
the distinctions that matter.

## Fixed and dynamic multiplicity

`nefor.node.sequence` accepts a compile-time `List (Node I O)` and returns
`Node I (List O)`. The supplied node order defines result order even when
actors finish out of order. The empty list is the ordinary list identity and
produces `[]` after its input activation.

`nefor.dynamic.DynamicList O` is a different, runtime effect. A producer emits
indexed occurrences plus explicit completion. `nefor.dynamic.traverse-template`
instantiates one closed worker template per occurrence, and `nefor.dynamic.context` waits for
completion before presenting one ordered provider turn. No runtime-sized MAG
`List` value or conversion between the two universes exists. The shipped
`examples/nefor-agent/agentic-loop/dynamic-tasks.mag` exercises zero, invalid,
and reverse-completion cases.

## Products and sums

A product input such as `(A + B)` fires only after every occurrence arrives.
Slots bind to sender edges, so `(Finding + Finding)` from two producers keeps
the occurrences distinct. `fanout` and `parallel` construct common product
shapes.

A sum input such as `(A | B)` fires on either constructor. `choose` applies one
node to each arm. When two paths carry the same payload type but different
meanings, distinct nominal types such as `Approved` and `NeedChanges` keep that
reason visible to validation.

## Ordering without data

The kernel emits `mag.Unit` when an actor completes successfully. A Unit edge
therefore expresses sequencing without pretending that the downstream node
consumes the upstream result. `nefor.node.*>` is the standard keep-right
composition: it discards the left value, waits for successful completion, and
runs a `Node Unit O`.

## Errors as values

Agent results include `AgentError`; process nodes return `ProcessResult` with a
typed exit-or-signal termination. These are semantic values. A workflow may
route them, retry them, continue with partial evidence, or make them its final
business result.

The bounded retry gate is an ordinary node whose output distinguishes
`Continue T` from `Exhausted T`. Unhandled factory failures instead escalate to
`mag.run_failed`: execution escaped the typed business model. `kill` retires an
actor and voids late outputs; it is not a routeable semantic error.

## Cycles and absence

Cycles are ordinary graph topology. Their termination must be visible in typed
exits or in a finite runtime actor such as the retry gate; an unrestricted
feedback edge may never terminate.

There is no implicit "fire when X did not happen." Absence becomes positive
data produced by a timeout, failure, or another actor whose result can be
composed normally.
