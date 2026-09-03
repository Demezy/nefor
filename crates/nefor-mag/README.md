# MAG

MAG stands for **Meta-Algebraic Grammar**: a grammar for constructing typed
algebraic descriptions at the meta level, before anything interprets them. MAG
is a strictly typed, pure functional language that compiles an immutable source
snapshot into one artifact.

```sh
mag compile main.mag --source-dir .
```

## Rust API

`CompilerSession` accepts explicit `CompileRequest` values for in-memory entry
source and `LoadRequest` values for file-backed entry programs. Both requests
carry the source directory, host inputs, module roots, and compiler options.
The session is currently cold-only: every call creates independent compiler
state, every load owns a distinct resident program, and session telemetry
accounts for requests without implying a cache. Existing free `compile_*` and
`load_*` functions remain available and use the same cold implementation.

## Documentation

- [The MAG Book](../../mag/book/README.md)
- [Language and authoring reference](docs/language.md)
- [Compiler errors and recovery](docs/errors.md)
- [Compilation and orchestration](docs/orchestrating.md)
