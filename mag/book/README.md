# The MAG Book

Start with [Reasoners Is All You Need](<Reasoners Is All You Need.md>) for the
small composition model used throughout the Nefor chapters: anything with a
typed context input and output can be composed as a reasoner, and a graph of
reasoners is itself a reasoner.

The book then has two parts:

- [Core MAG](<01. core/README.md>) introduces the strictly typed, pure functional
  language on its own: values, types, functions, modules, immutable file inputs,
  and artifacts.
- [MAG inside Nefor](<02. nefor/README.md>) is a live application of the language. It
  shows how Nefor's MAG libraries construct typed graphs, compose high-level
  nodes, and produce artifacts for the actor runtime.

Nefor is what we use MAG for today, not a boundary on what MAG can be used for.
Future applications can sit beside it without changing the core language path.

Complete examples live in
[`mag/examples`](../examples/).
