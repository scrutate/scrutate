# 14. Language servers and compilers are not part of default resolution

Status: accepted, 2026-11-17.

## Context

The mission asks to evaluate compiler front ends and language servers again where they would beat tree-sitter, for example in reference resolution. `refs`, `callers`, `callees`, `deps` and the language server's definition request all go through one resolver (`crates/scrutate/src/resolve/`, `names.rs`). It reads scopes, imports, declared and constructed types, declared return types, field types, base classes, Go receivers and embedded fields, Java arity, C includes by path suffix, and a table of Rust std method signatures. Where it cannot type a value it answers `receiver` and `callers` lists the site as undecided. It decides nothing it cannot show from the source.

A type checker decides what that resolver leaves open: a method on a value whose type is inferred, trait and interface dispatch, overloads by argument type, generics, macro and template expansion. A probe with `refs` on 10 common method names left 38% of the method-shaped occurrences undecided in this repository's Rust source and 16% in its Python benchmark scripts.

Every tool that closes that gap needs more than the files:

- rust-analyzer runs `cargo metadata`, build scripts (`cargo check`) and proc macros by default (rust-analyzer manual, configuration: `cargo.buildScripts.enable`, `procMacro.enable`).
- pyright runs a Python interpreter to find search paths (pyright docs, import-resolution.md).
- gopls calls `go list` through go/packages (gopls design doc); the go command downloads modules, can switch toolchains, and cgo runs pkg-config (cmd/go, go.dev/doc/toolchain, cmd/cgo).
- clangd needs the build's flags, in practice a compile_commands.json from the build, and `--query-driver` runs a compiler that may be checked in with the project (clangd docs, installation and system headers).
- tsserver loads the language service plugins tsconfig names (TypeScript tsconfig reference, `plugins`).
- JDT LS imports Gradle projects, and Gradle evaluates the project's settings and build scripts (Gradle docs, build lifecycle).
- C# servers load projects through an MSBuild design-time build that runs targets, including the repository's Directory.Build files (dotnet/project-system design-time-builds.md, MSBuild build process overview).

The mission forbids executing repository code during searching or indexing, and asks for search and indexing that start at once on any tree.

## Decision

1. Default search, indexing and resolution stay tree-sitter plus the resolver. No language server or compiler is started by `grep`, `index`, `refs`, `callers`, the MCP server or `scrutate lsp`.
2. Precise answers may come later only as an optional source that settles occurrences the resolver marks undecided. It never overrides a decided answer, and it is used only for files whose content hash matches what it indexed.
3. The first such source to consider is a SCIP index the user's CI already produced, read as data: scrutate executes nothing.
4. A live server backend is allowed only behind an explicit option, and only in a mode that runs no repository code and makes no network calls: gopls with `GOTOOLCHAIN=local`, `GOPROXY=off` and `CGO_ENABLED=0`; pyright with an interpreter outside the repository; a pinned tsserver with plugins off. rust-analyzer with build scripts or proc macros, JDT LS with a Gradle import, MSBuild-loaded C# servers and clangd with `--query-driver` are out.
5. Nothing in 2 to 4 is built before `bench/yardstick.py` has a hard-case W5 query set per language (a method on a call's result, trait or interface dispatch, overloads, macros or templates) and a run shows the server deciding more of it at equal precision.

## Consequences

- Answers stay fast to start, the same on any machine, and safe on an untrusted tree. The cost is the undecided share above, which `callers` and `dead` already report as undecided.
- The language server's own job stays editor navigation from the index. An editor that wants checker-precise answers keeps its language's own server beside `scrutate lsp`.
- The next step is measurement: the W5 hard-case query sets and one yardstick run against rust-analyzer, pyright, gopls and clangd, with the result file committed.
