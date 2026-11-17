<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-faq.svg" alt="Questions. What is scrutate, and what do people ask first?" width="880">
</p>

# Questions and answers

[Documentation](README.md) &nbsp;·&nbsp; Next: [Install](INSTALL.md)

<!-- End of head. -->

Short answers, each linked to the document with the evidence.

## What is scrutate?

An open-source code search tool for the terminal, editors and AI coding agents, in one Rust
binary. Engine A searches any folder at once with no index, like grep or ripgrep. Engine B indexes
a repository for ranked search by meaning or name, definitions, callers, references, and the
tests a git diff touches ([Architecture](ARCHITECTURE.md)).

## Can I try it without installing it?

Yes. The [playground](https://scrutate.github.io/playground/) runs scrutate in your browser,
compiled to WebAssembly ([Architecture](ARCHITECTURE.md#webassembly-wasm32-wasip1)): grep, ranked
search, definitions, callers and references. It searches scrutate's own source, a public GitHub
repository or a folder you open; a folder stays on your machine. It runs the newest scrutate
release, or, until the first release is published, the newest `main` commit that passed CI.

## How is it different from ripgrep?

Engine A is built on ripgrep's `grep-*` and `ignore` crates and agrees with ripgrep on 2,000
random corpora and patterns. It adds one spelling for every casing of an identifier (`--ident`),
matches within N edits (`--fuzzy`, `--fuzzy-regex`) and a trigram text index (`grep --index`).
Engine B has no ripgrep counterpart: it knows where each function is defined, what calls it and
what each file imports ([Competitive position](COMPETITIVE.md)).

## Is it faster than ripgrep?

In the recorded runs, yes. On the development Mac, Engine A was significantly faster than every
grep-class tool measured, ripgrep included, in every warm-cache and cold-cache benchmark, and in a
Linux virtual machine on the same Mac on every Linux-tree benchmark. That Mac is a laptop shared
with other work, so this is no publishable #1 claim. [Leaderboard](LEADERBOARD.md) has every
number with its 95% interval.

## Do I need to build an index?

No. `scrutate grep` needs no index, parser or background process. `scrutate index` builds Engine
B's index in `.scrutate/` at the repository root and updates it incrementally; `scrutate watch`
or `scrutate serve --watch` follows edits as they happen ([Command line](CLI.md)).

## Does my code leave my machine?

No. The binary makes no network connections and has no HTTP or DNS dependency, and its ranking
models are compiled in. [Security](SECURITY.md) has the threat model and the evidence.

## Which languages does it understand?

Engine A searches text in any language. Engine B parses the common languages with tree-sitter
grammars (more with the `extra-langs` build feature) and the rest with keyword rules.
[Languages](LANGUAGES.md) lists what the index finds in each, checked by tests. A `[[language]]`
table in `.scrutate.toml` adds your own language with its extensions, comments and definition
patterns ([CLI](CLI.md#custom-languages)).

## How do I give an AI coding agent scrutate's tools?

`scrutate setup claude` (or `codex`, `opencode`, `cursor`, `gemini`) writes an MCP server entry
into that agent's config file; `--uninstall` takes it out. The server is `scrutate serve`, MCP on
stdio; `--watch` keeps its index current. [MCP server](../integrations/mcp/README.md) lists the
tools and each client's config.

## Does it work in my editor?

Yes. `scrutate lsp` is a language server on stdio with definitions, references and symbols from
the index; VS Code and Neovim use it, and JetBrains IDEs use it through the LSP4IJ plugin and run
scrutate as external tools ([integrations/](../integrations/)).

## Which platforms does it run on?

All six are supported: macOS on Apple silicon and Intel, Linux on x86_64 and aarch64 (static musl
builds too), and Windows on x86_64 and ARM64. The binary and the wheels are built, installed and
tested on each; Intel macOS and Windows on GitHub's runners ([Install](INSTALL.md)).

## How do I install it?

From source for now: no release archive is published yet. [Install](INSTALL.md) has the build
command and the package definitions (Homebrew, Scoop, winget, `.deb`, `.rpm`) that wait for the
first release.

## Can I use it from Python?

Yes. The bindings stream Engine A's matches with the GIL released and open the index through
`scrutate.Repo` ([Python bindings](../crates/scrutate-py/README.md)).

## Is it production-ready?

Not yet declared. 0.1.0 is the first release; [Release readiness](RELEASE_READINESS.md) lists
what is and is not verified.

## Is it free to use?

Yes, under MIT OR Apache-2.0 at your option, for the code, the models and the documents. The
maintainer also offers commercial terms and paid support
([Sponsor and hire](../README.md#sponsor-and-hire)).

## What does the name mean?

*Scrutate* is an English verb from Latin *scrutari*, "to search carefully, examine thoroughly",
said SKROO-tayt. [Naming](NAMING.md) has the sources and the registry checks.
