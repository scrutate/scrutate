# Changelog

All notable changes to Scrutate. The format follows Keep a Changelog; versions follow SemVer.
Versioned interfaces: the CLI, the JSONL `schema` number, the MCP tool names and the Python API.
Past 1.0, an incompatible change to one bumps the major version and is listed under "Breaking".
The on-disk index is a cache with its own `schema_version`: a new version rebuilds an
incompatible index by itself, and that is never breaking.

## [Unreleased]

### Added
- Vendored code ranks below the project's own in `search` and the MCP `search_code`, and is never
  left out: `vendor/`, `third_party/`, `node_modules/`, `Pods/`, `Carthage/`, minified files and
  their kin, a subset of GitHub linguist's `vendor.yml`. A name only vendored code defines,
  searched for as itself, keeps its place. Every JSON item whose `path` is vendored code, in
  any answer, has `"vendored": true`
  ([CLI](docs/CLI.md#engine-b)).
- `SCRUTATE_COUNT_KEY=N` keeps a count of big searches apart from the one the user's other
  searches join; the tests of that count use it, so another suite's searches cannot change what
  they see ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `scrutate review --base REV [--head REV]` reports a pull request: `impact` between the two
  commits and the duplicates the head adds, as a PR comment (`--format markdown`), JSON or a
  GitLab Code Quality report, which the GitLab template saves as the job's `codequality` report
  ([CLI](docs/CLI.md)).
- The GitHub Action's `comment: true` posts that report as one pull request comment and edits it
  on each push, with only the `pull-requests: write` permission and never failing the job;
  `args` is now optional.
- Every MCP tool takes `response_format`: `detailed` (the default) or `concise`, which keeps every
  location and name and drops code, snippets and explanations; on this repository a concise
  answer is 5% to 44% of the detailed one ([CLI](docs/CLI.md)). The Python `Repo.call` reads it
  too; `batch` checks it.
- `serve --short-tools` (or `SCRUTATE_MCP_SHORT_TOOLS=1`) sends a short tool list with the same
  tools, arguments and checks: 5,614 bytes against 16,647 (13,968 before `response_format`).
- `scrutate setup <claude|codex|opencode|cursor|gemini>` writes an MCP server entry that runs this
  binary into the agent's user file (or with `--project` the repository's), editing it in place;
  `--uninstall` restores it byte for byte, `--dry-run` writes nothing, `--instructions` adds a
  block to `CLAUDE.md`, `AGENTS.md` or `GEMINI.md`. Checked with Claude Code, Codex,
  cursor-agent, opencode and Gemini CLI ([CLI](docs/CLI.md)).
- `scrutate setup claude --hook` installs a `PreToolUse` hook that answers Claude Code's Grep with
  Engine A's matches, bounded as an MCP answer is, and `scrutate hook stats` shows the bytes it
  saved. A plain `--uninstall` removes the hook too. It works on Windows, with Grep's paths there
  (`src\a.rs`).
- `scrutate tsquery LANG QUERY [PATHS] --rewrite TEMPLATE` rewrites each match from a template of
  its captures and prints a unified diff; `--write` writes the files. No MCP tool
  ([CLI](docs/CLI.md)).
- A `[[language]]` table in `.scrutate.toml` or your configuration file adds a language without a
  build: extensions, comment marks and one regular expression per definition kind. Its files get
  symbols, outlines, documentation and ranked hits, and `grep --type` and `languages` know it; a
  bad table is one error with the file and line, exit 2 ([LANGUAGES](docs/LANGUAGES.md)).
- A configuration file that does not parse is one line naming the file and the line.
- docs/FAQ.md answers the first questions about scrutate, with links to the evidence;
  `CITATION.cff` gives GitHub's "Cite this repository".
- The website https://scrutate.github.io (repository `scrutate/scrutate.github.io`), with a
  playground that runs scrutate compiled to WebAssembly in the browser; the README's top links
  and `CITATION.cff`'s `url` point to it. The README opens with its tagline and badges, then a
  "Try it in your browser" image, where a hand presses the button that opens the playground, and
  its top links; the terminal demo image is gone, since the hero already shows real searches and
  the playground runs them live.
- `.github/FUNDING.yml` shows GitHub's Sponsor button (GitHub Sponsors and Buy Me a Coffee), and
  the README's "Sponsor and hire" section links both.
- A security workflow runs `cargo deny check` (`deny.toml`: advisories, the licence list,
  crates.io only), `zizmor --persona pedantic` on `.github`, and a check that each file's count
  of `unsafe` matches the inventory in docs/SECURITY.md, on every push to main, every pull request
  and once a week ([SECURITY](docs/SECURITY.md#controls-and-evidence)).
- Every fuzz target runs for 45 minutes once a week in CI, from the corpus the last run kept; a
  crash fails the run and is uploaded ([fuzz](fuzz/README.md)).
- The release workflow takes a `dry_run` input: a manual run with it runs every job, `ci.yml`
  included, attests every file, and makes, checks and deletes a draft release
  `rehearsal-<run id>`, so the tag path is tested before the first tag
  ([Releasing](docs/RELEASING.md#7-what-a-published-release-adds)).
- The release workflow attaches the VS Code extension (`.vsix`) to the release and attests it,
  and uploads the wheels to TestPyPI and PyPI and the extension to the VS Code Marketplace and
  Open VSX, each by trusted publishing or token and each only once the owner sets it up
  ([Releasing](docs/RELEASING.md#7-what-a-published-release-adds)).
- A release carries a `.deb` and an `.rpm` for x86_64 and aarch64 Linux, built from the tested
  release binary, listed in `SHA256SUMS` and attested ([Install](docs/INSTALL.md#from-a-release-archive)).
- `packaging/homebrew/tap/` and `packaging/scoop/bucket/` hold the files of the Homebrew tap and
  the Scoop bucket repositories, with their test, bottle, autobump and Excavator workflows;
  INSTALL.md shows `brew trust --tap scrutate/tap`, which Homebrew 7 needs
  ([packaging](packaging/README.md)).
- The `jetbrains` workflow runs the language server in a headless IntelliJ IDEA 2026.2.3 with
  LSP4IJ 0.21.0 from the Marketplace: it registers `scrutate lsp` with the values the Language
  Servers settings page saves and checks that Go to Declaration on the fixture repository opens
  `src/geo.py` at line 6. It runs when the JetBrains files change and by hand
  ([JetBrains](integrations/jetbrains/README.md#the-automated-run)).

- `[index] max_files` and `max_bytes` in the configuration file (`SCRUTATE_INDEX_MAX_FILES`,
  `SCRUTATE_INDEX_MAX_BYTES`) cap how many files, and bytes of them, each index holds; an update
  that would pass a cap stops with exit 2 and leaves the last index as it was. No cap by default
  ([CLI](docs/CLI.md)).
- `refs`, `callers`, `callees`, `deps`, `rdeps` and the language server resolve C and C++
  (`#include` to repository files, namespaces and `::`, members of the class around and its
  bases, a header's declaration as the definition it declares) and C# (namespaces, `using`,
  aliases, typed locals, members, partial classes) ([CLI](docs/CLI.md#refs)).
- Java fields and record components are symbols of a new kind, `field`, and enum and interface
  constants are constants: `refs` ties a field read, `this.f`, `obj.f`, `C.F`, a static import
  and a record accessor `p.x()` to them, where it listed `field or key`
  ([CLI](docs/CLI.md#refs)). An index made before reads every file again.
- `bench/agent_eval.py` runs Claude Code on 20 tasks over three pinned corpora with and without
  the MCP server, and records per run whether the answer checks out, tokens, cost, tool calls and
  tool errors. Each run is paid, so it starts only with `--yes-spend`; no round has run yet
  ([Quality](docs/QUALITY.md#agent-eval)).
- `bench/yardstick.py` measures each workload against its rivals: grep tools, zoekt and livegrep,
  universal-ctags, ast-grep, rust-analyzer, pyright, clangd and gopls, and on the QUALITY queries
  (`--quality N`) ck, grepai and claude-context with local embeddings, with each tool's MRR@10
  and hit@5. It checks every answer before it times one, reports a wrong answer instead of a
  time, and records each rival's version. `bench/competitors/install_rivals.sh` installs every
  rival pinned by version and checksum; `yardstick.Dockerfile` and `claude-context.compose.yml`
  run them in containers. CI runs `--smoke` on a small tree in Python, Rust, C and Go
  ([Releasing](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).

### Changed
- The `scrutate` crate is ready for crates.io: it holds its models and data and nothing else
  (7.5 MiB compressed), and `cargo publish --dry-run -p scrutate-ignore -p scrutate` verifies both
  crates; publishing waits for the release ([INSTALL](docs/INSTALL.md)).
- The bundled semantic model stores its word tables as int8 with one scale per row: 7.6 MB in
  place of 14.6 MB, the binary is that much smaller, and ranking quality holds (MRR@10 0.5873 to
  0.5874). Its revision is 2, so an index makes its vectors again on the next update; a model
  file named by `SCRUTATE_SEMANTIC` must be in the new format ([QUALITY](docs/QUALITY.md)).
- The vendored walker is its own crate, `scrutate-ignore` (a modified copy of `ignore` 0.4.34,
  `Unlicense OR MIT`, its eleven changes listed in `vendor/ignore/README.md`), and the workspace
  `[patch]` is gone, so a crate built from crates.io walks with the same code
  ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- The macOS benchmark chain waits for the new binary to start at its settled speed
  (`bench/wait_aged.py`, at most an hour) in place of a fixed hour
  ([RELEASING](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- `bench/leaderboard.py` stops a row at 20 runs once every verdict holds at alpha 0.001
  (Haybittle-Peto boundary); replayed over every recorded run it changed no verdict. On by
  default, off with `--no-stop-early`; R-14.10 now defines a #1 over at least 20 runs per tool
  ([RELEASING](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- The release binaries for glibc Linux (x86_64 and aarch64) lay out the functions a small search
  runs side by side, as the macOS one does: a second link with rust-lld and the order file under
  that build's own names ([Releasing](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- With `serve --structured`, a large answer pages in `structuredContent` too: each page carries
  the JSON of the items its text page shows, with `offset`, `next_offset` and `total`, in place
  of a `null` result ([CLI](docs/CLI.md#serve-mcp)).
- An MCP call with a `progressToken` hears the files read so far against the total, while the
  first index is built and while `reindex`, a large scan or `structural_search` runs, at most
  twice a second; it no longer hears the seconds waited. A cancelled `structural_search` stops
  reading files ([CLI](docs/CLI.md#serve-mcp)).
- The crate builds and runs on `wasm32-wasip1`, which starts no thread: the thread rules take
  one thread, and Engine A, a parallel walk of the vendored `ignore`, `index`, `search`, `symbol`
  and `callers` run on the calling thread. `serve`, `lsp` and `tsquery` fail there with a message
  that says they need threads, `watch` says wasi has no file events (`notify` is not built
  there), and the Grep hook lets Grep answer. Files the system cannot map are read into memory,
  and the text index updates without its lock, which wasi does not have. The vendored `ignore`
  gains two changes for it, eleven in all. A CI job checks the build
  ([ARCHITECTURE](docs/ARCHITECTURE.md)). Native builds are unchanged.
- INSTALL.md's attestation check pins the release workflow and the tag
  (`--signer-workflow`, `--source-ref`): with `--owner` alone any workflow of the organization
  passed. It says what "signed" means and how to run the binaries, which carry no Apple or
  Windows code signature ([Install](docs/INSTALL.md#unsigned-binaries-on-macos-and-windows)).
- A release waits for `security.yml` on the tagged commit as well as `ci.yml`: a crate under a
  licence outside `deny.toml`, a git dependency, a zizmor finding or an unlisted `unsafe` now
  stops it. `ci.yml` no longer runs `cargo audit`; `cargo deny` checks the same advisories.
- PCRE2 is the `pcre2` cargo feature, on by default; a build without it refuses `-P`
  ([CLI](docs/CLI.md)).
- The repository is now github.com/scrutate/scrutate, under the `scrutate` organization; the old
  address redirects, and the install commands, Action, CI templates, packages and
  `CITATION.cff` name the new one.
- With reduced motion on, the README and document images stop only what travels or grows; before,
  they showed only slow cross-fades.
- Ranked search folds Japanese shinjitai with traditional and simplified Han, so `関係` finds
  `關係` and `关系` and the reverse (OpenCC ver.1.4.2 `JPShinjitaiCharacters.txt`, Apache-2.0).
  An older index reads every file again ([ARCHITECTURE](docs/ARCHITECTURE.md#tokens-textrs)).
- `watch` on macOS follows FSEvents with file-level events, loaded with `dlopen` only by `watch`,
  so a save updates that file alone with no walk of the tree: 0.24 to 0.31 s from the save to the
  update on a tree of 20,000 directories (release build, the 200 ms burst wait included), where
  the timer took 11 to 14 s (debug build). The timer stays as the fallback when FSEvents cannot
  load ([CLI](docs/CLI.md#watch)).
- The `notify` crate is 9.0.0-rc.8, pinned (it needs Rust 1.91, the existing minimum).
- Three tool errors say how to fix the call: `missing required argument name` (was `missing
  name`, and `missing to` for `call_path`), `missing path or symbol` names what each takes, and
  `path outside repo` says to give a path relative to the root. `batch` reports the same text
  ([CLI](docs/CLI.md#serve-mcp)).

### Fixed
- `tsquery --rewrite --write` on Windows reads and writes through the root it opened and each
  directory opened from it, refusing a junction or link at every name: a directory swapped for a
  junction after the plan, the root included, sent the write outside `--root`
  ([Security](docs/SECURITY.md#the-structural-rewrite)).
- `callees` of PHP code that writes `new self()`, `new static()` or `new parent()` lists the
  class, where it listed `self`, `static` or `parent` as an external name
  ([CLI](docs/CLI.md#engine-b)).
- The language server's references under an import alias span the alias as the line writes it:
  `h()` after `from util import helper as h` is 1 character wide, where it took the width of
  `helper` and ran past the end of a short line ([CLI](docs/CLI.md#lsp)).
- `callers` lists a call the resolver could not place, tied by its name only (`tx.foo.update()`
  on an untyped parameter), as undecided also when no other definition has the name, where
  `callers update` listed it as decided and `callers ProviderProfile.update` as undecided;
  `impact` marks such callers undecided too ([CLI](docs/CLI.md#engine-b)).
- A name a Python class body binds, `h = 5` beside `from util import helper as h` (also in an
  `if`, a tuple or a `for` target), is a local of the class body once bound, where `refs helper`
  listed it as a use of the import; `h = h` still reads the import on its right side, as does a
  comprehension
  ([CLI](docs/CLI.md#engine-b)).
- `callers`, `callees` and `refs` follow a PHP alias written in another case: `PF()` after
  `use function Lib\phpfn as pf` is a call of `phpfn`, and `t::make()` after `use Lib\Tool as T`
  a use of `Tool`, where callers and refs missed them and callees said `external: PF` ([CLI](docs/CLI.md#engine-b)).
- `refs` through an import alias lists only uses of the aliased definition: a local of the
  alias's name (`const g = () => 2; g()` beside `import { jsfn as g }`) no longer adds its own
  group to `refs jsfn` ([CLI](docs/CLI.md#engine-b)).
- A Java class of the default package (a file with no `package` line) is looked for in its own
  directory only: `Util.help()` in `ex/b/Main.java` is `ex/b/Util.java`'s, where every
  default-package directory was one package and it could go to `ex/a/Util.java`; with no
  `Util.java` in the directory, `refs` and `callees` both answer `external`
  ([CLI](docs/CLI.md#engine-b)).
- A call on a PHP static property, `User::$current->save()`, goes through the property's declared
  type to `User::save`, and `current` there is `field or key`, where `save` was `receiver` and
  `current` a local variable ([CLI](docs/CLI.md#engine-b)).
- `$this->word()` in a PHP trait that has no `word` is `receiver` in `refs`, since the class using
  the trait decides, where it said `not found` ([CLI](docs/CLI.md#engine-b)).
- `this` in a JavaScript method under a quoted key (`"get": function () {}`, `"del"() {}`) is
  the object literal, so `this.put()` there is `api.put`, where it was `receiver`
  ([CLI](docs/CLI.md#engine-b)).
- `callers`, `callees`, `impact` and `refs` follow an import alias through a package that
  re-exports the name: `from pkg import helper as h` with `pkg/__init__.py` importing `helper`
  from `.util`, where the alias was looked for only in imports naming `helper` or `util`
  ([CLI](docs/CLI.md#engine-b)).
- A PHP variable a closure takes with `function () use ($u)`, or `use (&$u)`, has the type it
  has around the closure, so `$u->save()` inside is `User::save`, where it was `receiver`
  ([CLI](docs/CLI.md#engine-b)).
- `const x = require('./n').x`, `const { later } = await import('./lazy')` and TypeScript
  `import m = require('./n')` bind their names to the module's definitions, where `refs` read
  them as local variables or receivers ([CLI](docs/CLI.md#engine-b)).
- A JavaScript or TypeScript `super()` in a subclass constructor is a call of the base class:
  `callers Base` and `impact` list `Kid.constructor`, `refs Base` lists the `super()`, and
  `callees Kid.constructor` gives `Base` where it said `external: super` ([CLI](docs/CLI.md#engine-b)).
- A field of a Java anonymous class is what a use of its name inside the class reads, where a
  local of that name in the method around it hid the field and the field had no references
  ([CLI](docs/CLI.md#engine-b)).
- `Outer.this.tick()` in a Java inner class is a call of `Outer.tick`, where it was `receiver`
  and `dead` left `Outer.tick` undecided ([CLI](docs/CLI.md#engine-b)).
- A local variable of a Java instance initializer block (`{ int count = 3; count++; }` in a
  class or enum body) is a local there, where it and its uses were read as the field of that name
  ([CLI](docs/CLI.md#engine-b)).
- PHP class, interface, trait, enum, method, namespace and `use` alias names, and `self::`,
  `static::` and `parent::`, match in any case,
  as PHP matches them: `s::SLUG()` after `use App\Util\Str as S`, `\app\util\str::slug()` and
  `$x->Slug()` are calls of `Str::slug` in `refs`, `callers` and `dead`, where they were
  `external` or not found; variables, properties and constants still compare as written
  ([CLI](docs/CLI.md#engine-b)).
- `refs` finds a Go method promoted through an embedded struct field: `v.Hello()` on a `Wrap`
  that embeds `Base` (or `*Base`, `base.Base` from another package, also two levels down) is
  `Base.Hello`, where it said `not found` ([CLI](docs/CLI.md#engine-b)).
- `refs` ties a call of a Rust trait's default method on a type that implements the trait
  without overriding it (`s.hello()` after `impl Greet for S {}`, also `impl Greet for inner::S`
  and `impl super::Greet for S` inside `mod inner { }`) to that method, where it said
  `not found` ([CLI](docs/CLI.md#engine-b)).
- A Rust `use` of an item of an inline `mod other { }` reaches it: `use other::work` and the
  calls `work()` after it were `external` ([CLI](docs/CLI.md#engine-b)).
- `refs` lists a use under an import alias also in a file whose import never spells the
  definition's name: `h()` after `const h = require("./m")` or `import h from "./m"`, which
  `callers` already found ([CLI](docs/CLI.md#engine-b)).
- The name of a JavaScript generator expression (`function* load() {}`) or class expression
  (`class Box {}` as a value) is its own inside it, where uses there went to an outer `load` or
  `Box`; a generator expression is also a scope for its parameters and locals
  ([CLI](docs/CLI.md#engine-b)).
- The name of a JavaScript named function expression is its own inside it: in `var h = function
  load() { return load(); }` the inner call no longer goes to an outer `function load`, which
  `callers load` listed `h` for ([CLI](docs/CLI.md#engine-b)).
- `refs` ties a JavaScript or TypeScript shorthand property, the `helper` in `{ helper }` or
  `module.exports = { helper }`, to the `helper` it reads, where it listed it as `field or key`.
- A Rust path inside a macro's arguments resolves as the same path outside a macro: with `use
  other::work`, the `util::work()` in `println!("{}", util::work())` or `vec![util::work()]` was
  read as the bare `work` and tied to `other::work` ([CLI](docs/CLI.md#engine-b)).
- `callers` and `callees` follow a CommonJS `module.exports = helper` or `module.exports =
  function named() {}` to the call `h()` of `const h = require("./m")`, and an ES default import
  `import h from "./m"` to the definition it reaches, also through a file that re-exports it
  with `const helper = require("./m"); module.exports = helper`: they matched the local name
  to the imported one, so `h` reached nothing ([CLI](docs/CLI.md#engine-b)).
- `dead` counts a Ruby writer (`def color=`) as used only where it is called (`x.color = 1`,
  `send(:color=, 1)`): reading `@color` kept a writer nothing calls out of the dead
  ([CLI](docs/CLI.md#engine-b)).
- A code index segment write that is stopped or fails removes its half-written directory, which
  stayed in `.scrutate` until a later write took its name ([Architecture](docs/ARCHITECTURE.md#storage-indexrs-lexrs)).
- A path that names the repository's root from a directory below it (`dead ..`, `map ..` from
  `repo/pkg`) is the whole repository: it was an error, "no such file or directory in the
  repository" ([CLI](docs/CLI.md#engine-b)).
- `grep -c -U` counts as ripgrep does when a match ends at the end of its lines (`b|\n$`,
  `a|b\n\B`, `-w a|b\n`): `$`, `\B` and word edges saw the end of the matched lines as the end
  of the text, and counted a match too many ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- The language server answers requests queued behind one that waited out the first build at
  once, so `shutdown` behind four of them comes after one 20-second wait, not four
  ([CLI](docs/CLI.md#lsp)).
- A `serve --watch` tool call no longer waits for the server's own update of a saved file, which
  may itself wait for another process's write lock: it answers from the last committed index at
  once, where it waited until the lock was free (37 s for a lock held 40 s)
  ([CLI](docs/CLI.md#the-index-directory)).
- Ctrl-C ends a wait for the text index's lock that another process holds, under `watch` and
  `grep --index --update` (exit 130): the wait went on up to 10 minutes
  ([CLI](docs/CLI.md#watch)).
- Ctrl-C stops a code index update while a large segment's vectors are clustered (`watch`, a
  cancelled `reindex`): it waited for the clustering to end, two minutes in a debug build of a
  20,000-file tree ([CLI](docs/CLI.md#watch)).
- A path that leaves the repository and comes back by its parts (`outline ../../repo/pkg/a.py`
  from `repo/pkg`, an MCP `path` of `../repo/pkg`) names the repository's file or directory:
  `outline` said the file had no symbols, `dead_code` and `repo_map` found nothing, and
  `grep_code` named files by the detour ([CLI](docs/CLI.md#answers-from-the-index-one-definition-for-every-front-end)).
- `setup --uninstall` keeps text added after the scrutate block on its own line: with a file that
  had no last line break, it joined that text to the line above the block
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- A `grep` option given twice takes its last value, as in ripgrep (`-m1 -m2`, `-i -i`,
  `-C1 -C3`): it was an error ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- The Arabic letter mark (U+061C) in a path or name is shown as `\u{61c}`, as the other bidi
  marks are: it was printed raw ([CLI](docs/CLI.md#conventions)).
- `grep --index` skips the file standard output writes to, as the walk does: with an earlier
  search's output in the text index, `grep --index foo > out.txt` searched its own output and grew
  it until the disk filled ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `dead` finds a use of a definition named with its owner (Pascal's `procedure TFoo.Tidy`, used
  as `o.Tidy`) and of a Ruby writer (`def label=`, used as `v.label = "x"`) by its own name: both
  were called dead while code used them. A method's declaration in its class and its body no
  longer count as uses of each other ([CLI](docs/CLI.md#engine-b)).
- Two `Repo`s of the Python module with different custom languages each work under their own
  root's tables: an update, a tool call (`impact` parses files itself) and `grep(types=)` could use
  the tables another `Repo` loaded last, keep chunks parsed with them or call a custom type
  unknown ([CLI](docs/CLI.md#custom-languages)).
- `grep -t cpp` takes `.h` files, as ripgrep's `cpp` type does, `-t objc` takes `.h` and `.m`, and `-t matlab` and
  `-t puppet` find `.m` and `.pp` files, where they found none: a type now includes the extensions its language shares with another and is told apart in by
  content ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `tsquery --rewrite --write` on Linux and macOS reads and writes through the root as it was
  opened when the plan started and each directory opened from it, refusing links: a directory
  swapped for a link between the read and the write, the root included, sent the write and its
  temporary file outside `--root`
  ([Security](docs/SECURITY.md#the-structural-rewrite)).
- The keyword tier (OCaml and F# without a grammar, unknown languages) names `let rec fact` as
  `fact`, finds `let inline square` and `let private helper`, no longer takes `let mutable x` for a
  function named `mutable`, and keeps a function named after a modifier word (`let data x`,
  `proc inner()`), and takes only OCaml's and F#'s own words after `let` for modifiers
  (`let data x y` binds `data`, `let inline (+.)` gives no definition named `inline`); the
  content revision is 8 so an older index reads its files again
  ([Architecture](docs/ARCHITECTURE.md#storage-indexrs-lexrs)).
- `grep -z` on a compressed file named on the command line whose text has a NUL reports a
  binary match, as ripgrep does, where it reported nothing and exit 1
  ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- A custom language's extension is refused when a built-in language claims a file name that
  ends with it (`txt` for `CMakeLists.txt`): such a table took those files from the built-in
  language ([CLI](docs/CLI.md#custom-languages)).
- `check` reports a segment whose vectors are gone or not as wide as the model's, and an IVF row
  out of range, where it said OK or stopped with a bare error; a search finds a segment of the
  wrong width damaged and rebuilds it, where it panicked ([CLI](docs/CLI.md#engine-b)).
- A `SCRUTATE_SEMANTIC` model whose tables do not agree in size is refused with an error: it
  panicked while indexing, and every file stayed skipped after the model was fixed
  ([CLI](docs/CLI.md#environment-variables)).
- `watch` on a disk that ignores case (macOS's default) walks the tree after a rename that
  changes only the case of a name: the text index kept the old name beside the new one
  ([CLI](docs/CLI.md#watch)).
- A concise tool answer too long for `max_bytes` is the detailed page, which says how to go on:
  it was cut in the middle of an item and lost the line with the next offset, so items between
  pages were never shown ([CLI](docs/CLI.md#serve-mcp)).
- The language server counts columns as the editor does in a file with a byte order mark, so
  definition on its first line and its document symbols land on the name, and a definition that
  starts on the first or last line of another (`} function c() {`) is its sibling in the
  document symbols, not inside it
  ([CLI](docs/CLI.md#lsp)).
- `grep --json` spans and colour leave out the empty matches ripgrep leaves out: one right where
  a match ended and one past the line's end (`\s*$` on `foo  ` gives one span, not three), and an
  empty match inside a character stays empty instead of covering the character, at ripgrep's byte
  offset when the line is valid UTF-8 (`x*` on `é€x`: 0, 1, 2, 3, 4).
- Ctrl-C stops a `watch` update at its next batch of files, the first update of a large tree
  included, where it ran on to its end; a second Ctrl-C ends any command at once
  ([CLI](docs/CLI.md#watch)).
- A C++ header with a Doxygen `@class` (or `@interface`, `@protocol`) in a comment is parsed as
  C++: only a line that starts with an Objective-C keyword makes a `.h` file Objective-C; an index
  reads its files again.
- Text answers show bidi marks, embeddings, overrides and isolates and the line and paragraph
  separators in a path as escapes (`\u{202e}`), as they show control characters: a file named
  `bidi<U+202E>yp.txt` displayed as `biditxt.py` ([CLI](docs/CLI.md#serve-mcp)).
- `files` and `tsquery` with a `--root` that does not exist or is a file stop with "no such
  directory" or "not a directory" and exit 2, as the other commands do; they answered "no file
  matches" with exit 1.
- The definition patterns of every `[[language]]` table share one regex size budget, so a
  repository's `.scrutate.toml` with many large patterns is refused at once with the pattern's
  line; each pattern had the whole budget, and ten took 340 MB and three seconds in every command,
  `serve` and `lsp` included ([CLI](docs/CLI.md#custom-languages)).
- The index's write-ahead log stays small while readers overlap: after an update it is cut to
  nothing once readers on older commits finish (waited for about a second, else by the next
  update), and a cut log keeps at most 4 MB of its file. Under four MCP servers searching and a
  `watch` editing every second for 10 minutes, the log peaked at 1.29 MB, where it reached 5.88
  MB ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A Free Pascal unit in a `.pp` file that starts with compiler directives (`{$mode objfpc}{$H+}`,
  the Lazarus default) is parsed as Pascal, not as one Puppet module; an index reads its files
  again.
- The resolver takes a file's language as the parser does, from its text where the extension is
  shared: an Objective-C `.h` header no longer goes through the C rules, which could not place its
  calls and left `dead` asking about them in place of a C file's call.
- A `serve` reader left idle lets go of segments a write replaced, so their disk space is freed
  on Unix and they can be removed on Windows, where idle readers kept them open until reused.
- `setup claude` replacing an old entry through `claude mcp` adds the old one back when the new
  `add-json` fails, where the user was left with no scrutate server
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- `serve` reads a message of at most 256 MiB, as `lsp` does: a longer line, or one with no end,
  is dropped as it is read and answered with a parse error, where it was held in memory whole
  ([CLI](docs/CLI.md#serve-mcp)).
- `callers` and `refs` through a C# or C++ type whose bases form a wide diamond take linear time:
  each base type is read once per depth, as for Java, where 9 levels of 6 interfaces took 35 s for
  one call.
- `setup` makes its temporary file with the permissions of the file it replaces before writing to
  it: a `config.toml` only its owner may read was readable by others while it was written
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- A language server request during the first build waits for it up to 20 seconds, then answers
  -32801 "the index is still being built": it waited for the whole build on the one thread that
  reads requests, so `shutdown` and cancellations queued behind it ([CLI](docs/CLI.md#lsp)).
- Each search, tool call and language server request reads the code index from one commit: a
  `watch` that committed between its reads could drop a hit whose rows it had already picked
  ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A failed write of `grep`'s output other than a closed pipe (a full disk, a file size limit)
  is an error with exit code 2, as in ripgrep: it stopped the search and exited 0 or 1 without a
  word.
- `grep foo . > out.txt` skips `out.txt`, as ripgrep does: it searched its own output and grew
  it until the disk or a file size limit stopped it ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- A new segment's files and directory are flushed to disk before the database names them, and
  the text index's before its manifest does, so power lost after an update can no longer leave
  an index naming segments of empty or zeroed files that pass their shape checks; an update of 20 files takes about 60 ms longer on macOS
  ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A Pascal statement that is a name alone (`DoWork;`, `Obj.Tidy;`) is a call edge, so `callers`,
  `callees` and `impact` see a procedure called without parentheses; an index reads its files
  again ([CLI](docs/CLI.md#engine-b)).
- `refs` of a name a Fortran, Pascal or Ada definition or a PHP function has finds its uses in any
  case in those languages' files: `refs DoWork` missed `dowork()`. Fortran needs a build with
  `--features extra-langs` ([CLI](docs/CLI.md#engine-b)).
- `grep --fuzzy-regex` reports where a match starts: the reversed pattern is run back from its
  end. The column, the `--json` spans and the colour took a lower bound (`zzz a12b` gave column 1
  for `a[0-9]*b`) ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- An internal error advises `scrutate index --force` only in a process that opened an index:
  `grep` without `--index`, `hook`, `setup` and the other commands without one now only ask for a
  report; with `--index` the text index counts.
- The MCP tools `repo_map` and `dead_code` take an absolute directory under the root, which
  gave no files or "no such file", and `repo_map` and `map` refuse a path that is not there;
  `grep_code` reads a path whose name holds a backslash (`x\ty`) as written when no file has the
  escape's reading. The tools also take a path spelled through the root as the server was given
  it (`--root <dir>/link`, macOS's `/tmp`). The command line, the language server and the tools
  turn paths into repository paths in one place ([CLI](docs/CLI.md#serve-mcp)).
- `index`, `watch`, `serve --watch` and `reindex` wait for another writer for up to 10 minutes,
  or until cancelled, and so do the first open of a new index, the rebuild of an index of an
  older schema and the merge of segments after an update: they gave up with "database is locked"
  after 30 s. Ctrl-C ends `watch` during such a wait. A `watch` burst whose update failed is tried
  again with a walk of the tree, with the next burst or after a pause of 1 s doubling up to 30 s,
  so its files are indexed with no further change ([CLI](docs/CLI.md#watch)).
- An open of the index that finds another process writing no longer takes the lock for damage:
  it replaced the database and deleted that process's work.
- `grep -c -U` no longer counts the empty match after each matched group's line end, at the end
  of a file with no final line end, or right where a match before it ended, as ripgrep: `^$` on
  `a`, a blank line and `b` counts 1, not 2, and `b?$` on the same text with no final line end 3,
  not 4.
- `grep -A` and `-B` win over `-C` in either order, as in ripgrep: `-C1 -A3` showed one line
  after each match, not three ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `grep -i` with a class that matches nothing (`ab[^\s\S]`) finds nothing and exits 1, as
  ripgrep does; it stopped with an internal error.
- `dead`, `callers` and `callees` match a Fortran, Pascal or Ada name, and a PHP function outside a class,
  in any case, as those languages do: `call compute_total(y)` is a call of `Compute_Total`, and
  `FormatPrice(3)` of `formatPrice`, also in a Pascal unit of a `.pp` file, whose language its
  text decides; an Ada `Main` is an entry point. Fortran needs a build with `--features
  extra-langs` ([CLI](docs/CLI.md#engine-b)).
- `dead` no longer lists Ruby methods that Ruby calls itself (`method_missing`,
  `respond_to_missing?`, `coerce`, `to_str`, `inherited` and their kin) or operator methods
  (`<=>`) ([CLI](docs/CLI.md#engine-b)).
- `callers`, `callees`, `impact` and `refs` follow an import alias: with `from util import helper
  as h` or `const { lone: l } = require("./u")`, a call `h()` was no caller of `helper`, so
  `impact` found no callers and no tests ([CLI](docs/CLI.md#engine-b)).
- Several first opens of a new linked work tree at once (an editor starting its language server
  and MCP server) no longer delete each other's copied segments: the losing open removed the
  segments the winner's database listed, and `check` found the new index damaged.
- `setup --instructions` and `--uninstall` take a scrutate marker only as a whole line: a
  sentence quoting `<!-- scrutate:begin -->` got a second block, and uninstall then cut from that
  sentence to the block's end. A begin line with no end line stops setup, and `--uninstall`
  with or without `--instructions`, with no file changed
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- On Windows an index update that a virus scanner or the search indexer briefly blocked failed with
  "Access is denied" or "being used by another process" when it renamed a new segment or the text
  index manifest into place; the rename is now tried again for up to about two seconds.
- On Windows the indexes keep each file's status change time, as on Unix: a file edited back to
  its old size and modification time is read again by the next update
  instead of answering from its old chunks until `index --force`. The walk reads the change times
  with one more listing per directory ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- `watch` on Windows walks the tree when a burst overflows the watch buffer, now 64 KiB: before,
  its files waited for a later change 30 s after the last walk ([CLI](docs/CLI.md#watch)).
- `watch` on Linux checks the directories that got no inotify watch on a timer, so a change there
  is indexed within 1 to 30 s, and the burst no longer walks the tree while one has none; the
  message names `fs.inotify.max_user_watches` when that limit is the cause
  ([CLI](docs/CLI.md#watch)).
- A one-line file of 8,000 nested functions indexes in 0.13 s with a 44 MB peak, from 3.1 s and
  542 MB: the definitions of a file share a text budget of 1 MiB plus 8 bytes per byte of the
  file, past which each keeps its first line. Normal files keep at most 5.1 times their size, so
  they index as before ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- On Unix `.scrutate/` is made with mode 0700, and an existing one is set to it by the next
  write: under umask 022 it was readable by every user, the text of files made private with
  `chmod 600` included ([SECURITY](docs/SECURITY.md)).
- In Rust, a call on the result of a method of a standard or outside type goes through a table
  of standard methods: `v.first().unwrap().ok()` on a `Vec<Report>` is `Report.ok`, and
  `file.metadata().ok()` is external, so `callers` and `dead` decide more; a value built by
  `Type::new()` with its type arguments unwritten stays undecided ([CLI](docs/CLI.md#refs)).
- A plain name in a Rust macro's arguments, outside the standard expression macros such as
  `println!` and `vec!`, is `macro argument` and undecided, no longer tied by name to a
  definition ([CLI](docs/CLI.md#refs)).
- A Verilog task call without parentheses (`h;`) and a VHDL entity or component instance are
  call edges, and `a(i)` of a VHDL signal, variable, constant or port the file declares is not;
  an index reads its files again ([CLI](docs/CLI.md#engine-b)).
- The Grep hook reads the `managed-settings.d` drop-ins and lets Grep run wherever managed policy
  it does not read exists, so an administrator's read rule is never bypassed
  ([SECURITY](docs/SECURITY.md)).
- `path` takes a function named like its file (`def main` in `main.py`) over the file's module
  chunk, as `callers` and `callees` do.
- A `tsquery` predicate outside every pattern's parentheses is an error: it matched every node,
  so `--rewrite --write` would have rewritten all of them.
- `grep -t TYPE` (and `-t !TYPE`, `SCRUTATE_GREP_TYPES`, the configuration file's `types`)
  matches after the ignore rules, as ripgrep's `--type` does: a gitignored file of the type is no
  longer searched, and `-g '*.txt' -t py` searches only the `.txt` files.
- VS Code's symbol, callers and references commands ask for the word under the cursor without
  what comes before a dot when the name with its dots finds nothing, as the language server
  does: on `norm` in `p.norm()` they asked only for `p.norm` and found nothing. R's `is.ready` is
  still asked whole, and a word is alphabetic characters and digits of any script, as the server
  reads one: on `ß` in `größe(2)` they found no word, and on `हिंदी` they asked only for `ह`, as a
  vowel sign is alphabetic but not a letter.
- The GitHub Action keeps a backslash in `args` as written: `grep -c \bTODO\b .` searched for
  `bTODOb` and `--root C:\repo` named `C:repo`, so a gate could pass on the wrong search.
- Python `grep(sort=True)` gives the CLI's `--sort` order (`a/b` before `a-b`), and with
  `max_results=N` the first N matches in it, the same on every run: it capped the search at
  whichever N matches came first and sorted those ([bindings](crates/scrutate-py/README.md)).

## [0.1.0] - 2026-11-11

First release. How each part works is in [ARCHITECTURE](docs/ARCHITECTURE.md), the flags and
outputs in [CLI](docs/CLI.md), the measurements in [BENCHMARKS](docs/BENCHMARKS.md) and
[LEADERBOARD](docs/LEADERBOARD.md).

### Engine A: universal instant search
- Literal, regex (linear time, size-limited), identifier (all casings, digit/acronym aware),
  fuzzy/approximate (Myers bit-parallel, pigeonhole prefilter), multiline, inverted, whole-word,
  smart-case matching; context lines; `-c`, `-l`, `--max-count`, `--max-results`.
- Parallel ignore-aware traversal (`.gitignore`, `.ignore`, global excludes, hidden, symlinks,
  depth and size limits, type globs); per-file batched streaming; Ctrl-C and broken-pipe
  cancellation; deterministic `--sort`; JSONL output (schema 1); exit codes 0/1/2/130.
- Differentially tested against ripgrep on random corpora and patterns: zero disagreements.
- Text not on disk can be searched (`Paths::Memory`), with the output a file of the same bytes
  gives; one module (`source.rs`) reads every kind of source.
- One scheduler with one module of thread rules runs every walk and file list; a list of more
  than 256 files takes its share of the searches running on macOS, and any search adds threads
  when its reads come from the disk.
- `-e PATTERN` (repeatable) for several patterns (`-e '->'` works); each line of a multi-line
  match gets its own line number, and in colour its path.
- `grep -i` and `grep -F -i` with a literal use 37 to 50% less CPU on one thread (a 740 MB file:
  100.6 to 63.3 ms of user time for `pm_resume`, 96.2 to 48.4 ms for `KÖNIG`).
- Search threads format plain output into per-file buffers; lines over 64 KB are written
  uncopied, their UTF-8 checked 256 KB at a time.
- Ctrl-C is handled through `signal()` / `SetConsoleCtrlHandler` directly, saving the `ctrlc`
  crate's start-up cost on macOS.
- Gitignore lines of three common shapes (`*.log`, `**/a/b`, `/build*`) match without building a
  regex, which cost more than a small repository's whole walk.
- Plain output arriving within a millisecond of other output waits in a 64 KB buffer and leaves
  in one write: `return` in the Linux tree (88,000 files) went from 302 ms to 190 ms on Linux
  (ripgrep 319 ms), 675 ms to 650 ms on macOS.
- Start-up on macOS on Apple silicon is 1.9 ms (a Rust hello-world 1.5 ms): system functions a
  search never calls are bound lazily, and a grep-only argument parser skips clap.
- Faster start: a one-file `scrutate grep` uses 2 M fewer instructions, 131 fewer page reclaims
  and 2.2 MB less memory; `-F` with one literal skips the regex engine, and the Apple silicon
  executable is linked with an order file (`crates/scrutate/startup.order`, from
  `tools/order_file.py`).
- macOS builds need macOS 13 or newer (chained fixups).
- A `.gitignore` used on many paths compiles its globs into one set: the Linux tree in a Linux VM,
  144 ms to 120 ms (ripgrep: 135 ms).
- Smart case counts only letters that stand for themselves (`-S '\Sfoo'` is case-insensitive, as
  in ripgrep) and folds only a pattern with a literal and no uppercase letter; of `-i` and `-S`
  the later wins, and `SCRUTATE_GREP_SMART_CASE` applies when neither is given. `-c` with
  `--max-count` stops at the limit; `-w` with `--fuzzy` or `--fuzzy-regex` is an error.
- A file of 64 MB or more is searched on every search thread as ranges of whole lines, output in
  file order.
- A regex whose matches all end in one of a few literals (`\w+_(suspend|resume)\(`) finds
  candidate lines by those literals.
- Small roots are walked on the calling thread and a one-file search starts no thread: 21.1 M to
  19.2 M instructions (GNU grep in the C locale: 20.8 M).
- A large file with no line breaks is read into one buffer, each search thread filling its own
  part: 100 MB in one line in 4.0 ms on five threads, 12.6 ms on one; `^(a|aa)+$` over it and two
  more files, 15.3 ms to 11.0 ms (ugrep 16.5 ms).
- `-P` / `--pcre2`: PCRE2 syntax (look-around, backreferences), stopped by PCRE2's match limit
  on exponential patterns.
- `-z` / `--search-zip`: zip and tar members (`archive.zip{member}`) and compressed files, with a
  64 MiB cap on one expanded line and escaped member names ([SECURITY](docs/SECURITY.md)).
- `--fuzzy` patterns up to 4096 characters, an edit being one character (`naïve` is one edit from
  `naive`); `grep --index --fuzzy` agrees with the walk. `--fuzzy-regex[=N]`: a regex within N
  edits.
- `grep --index --update`: refresh the text index (changed files only), then search it.
- Files that could not be searched, or not to the end, are named in warnings.
- `^` and `$` are line anchors in every mode, as in ripgrep: `^(a|aa)+$` over ten million one-byte
  lines took 198 ms, now 8 ms.
- A regex whose every match starts a line with one of a few literals looks only at those line
  starts: 52 ms to 18 ms.
- On macOS a many-file search runs one thread per performance core, and a walk whose reads come
  from the disk adds threads up to 24: the Linux tree with a cold cache, 2.9 s on 5 threads,
  1.4 s this way, ripgrep 1.8 s.
- `scrutate grep` uses less memory on big trees (peak resident memory: the ripgrep tree 9.9 MB to
  5.9 MB, py-big 12.6 MB to 8.2 MB, the Linux tree 14.3 MB to 9.6 MB; ripgrep: 7.1, 8.5 and
  14.2 MB).
- On macOS, concurrent searches of one user share the threads, since the kernel serves about four
  file-reading threads at full speed; `SCRUTATE_GREP_DEBUG=1` shows the count.
- On Linux a tree read from the disk gets up to 64 threads: the Linux tree cold in a 5-CPU VM,
  1.22 s on 5 threads, 0.37 s this way (git grep 1.23 s, ugrep 0.64 s); 231 files cold: 5.8 ms
  (ripgrep 7.5 ms).
- Paths print as given, as in grep and ripgrep (`./a.txt` for `.`); with one file as the only
  path, lines leave its name out; `-H` and `-I` choose.
- On Unix a backslash in a file name stays in the printed path; only Windows turns `\` into `/`
  and prints `\\?\C:\x` as `C:/x`.
- On Linux, files and directories on `/proc`, `/sys` and the other kernel pseudo filesystems are
  read to the end.
- A `-g` glob is taken whole, commas included (`-g '*.{txt,log}'`); `SCRUTATE_GREP_GLOBS` and
  the configuration file's `globs` split at commas outside braces.
- With `-A`, `-B` or `-C`, a line `--` separates groups, as in ripgrep.
- `-c` with `-U` counts as ripgrep does; context lines of CRLF files drop the `\r`.
- A pattern or option value that is not UTF-8 is an error (a path may be any bytes).
- `ignore` 0.4.34 is vendored (`vendor/ignore`) with nine changes, listed in the workspace
  `Cargo.toml`; an idle walker thread first waits 10 µs instead of 1 ms (a 598-file package:
  10.7 ms to 8.1 ms).
- `--max-results N` stops at exactly N matching lines; `-m N` is a per-file limit.
- A search path that does not exist is a warning and exit code 2; the other paths are searched.
- With no path, a pipe or file on standard input is searched (`-` names it; label `<stdin>`).
- A file with a NUL byte is searched as ripgrep does: named on the command line, line output
  prints `binary file matches (found "\0" byte around offset N)` (a `binary` record with
  `--json`). A walked file with a NUL byte is left out of `-c` and `-l`.
- `-c` and `-l` give one record per path given, as ripgrep does.
- `--sort` prints in the order of `rg --sort path`, also with `--max-results N`.
- `-n` is accepted and `-N` drops line numbers; `-j`, `SCRUTATE_THREADS` and `threads` stop at
  256.
- `grep`, the code index, `watch` and `tsquery` read `.rgignore` files, as ripgrep does.
- `--type-list` and `--version` need no pattern; a bad `.scrutate.toml` no longer stops
  `--help`, `--version`, `config` and `completions`.
- On Windows a file that falls back to the stream reader is searched from its first line.
- Colour or `--json` output of a file with millions of matches holds megabytes (one 20 MB file
  with 2 million matches: 341 MB, now 12 MB).
- In `--json`, `spans` and `column` count bytes of the printed `text`, as do the Python bindings'
  `Match.spans` and `Match.column`.
- On Unix a non-UTF-8 file name prints as its own bytes in piped output, and `--json` adds
  `path_bytes`; control characters in names print as escapes ([SECURITY](docs/SECURITY.md)).
- With `-l`, `--stats` says `N of M files match` and the JSON summary gives `"matches": null`.
- The MCP `grep_code` tool and `grep --symbols` end a file whose search a NUL byte stopped with a
  `note:` line (it was printed as line 18446744073709551615, or dropped), and `refs` and
  `references` say so too (`notes` in JSON).
- A cancelled `grep_code` or `references` call stops its Engine A search between files.
- `references` and `refs` search the files `grep_code` searches: hidden files included,
  `node_modules` and the like left out.
- Engine A has one way in, `instant::search`; the Python bindings' `grep` no longer yields a
  binary-file note as a `Match`.

### Engine B: code intelligence
- `scrutate map [PATH]` and the MCP tool `repo_map`: the files that define code, ranked by
  PageRank over calls and imports, each with its 5 most called definitions.
  `scrutate path FROM TO [--depth N]` and the MCP tool `call_path`: the shortest call chain
  between two definitions; no chain exits 1 ([CLI](docs/CLI.md)).
- `scrutate dead [PATH]` and the MCP tool `dead_code` list the definitions nothing in the
  repository uses, leaving out each language's entry points (`main`, tests, `#[no_mangle]`,
  Python `__x__` names, exports and the like); a name some line holds that the resolver ties to
  nothing is listed as undecided, with the reason.
- GDScript (`.gd`) has a grammar in the default build (`tree-sitter-gdscript` 6.1.0, MIT, 0.27 MB
  of the binary): functions, classes, signals, enums, constants and variables, `##` comments as
  documentation, a file with `class_name Player` as that class, and calls (`f()`, `obj.f()`,
  `super.f()`) for `callers` and `callees`; `grep --type gdscript` searches `.gd` files
  ([LANGUAGES](docs/LANGUAGES.md)).
- `scrutate impact [--base REV] [--staged] [--depth N]` and the MCP tool `impact`: from the git
  diff to the definitions it adds, edits and deletes, their callers with distances (3 steps by
  default) and the test files to run; a bad revision is exit code 2 ([CLI](docs/CLI.md)).
- `scrutate files QUERY [--limit N]` and the MCP tool `find_files`: the repository's paths ranked
  for a query that may be misspelt (`srch.rs` finds `search.rs`), then by frecency; no index
  needed, about 8 ms on this repository.
- Windows: a damaged index is rebuilt while another handle holds its files (the rebuild could
  fail on a segment it had deleted), and `grep_code` and textual references print paths from the
  root, not absolute paths.
- A linked work tree no longer takes a segment its main checkout removed during the copy, seen
  once on a Linux runner as an update that reported success over a broken index.
- `callers` lists callers in path and line order; an index updated through renames answered 63
  of 120 `callers` questions differently from a fresh one.
- `callers` asks the resolver about every call site (it stopped at the 400 likeliest), and
  `callers` and `callees` keep what they learned under `.scrutate/answers/` while nothing they
  read changes: `callers size` on Guava costs 1.4 s of CPU on a first run and 0.43 s after (the
  capped answer cost 0.6 s). The bindings' callers list has no `unchecked` any more.
- What parsing makes of a file is an artifact kept by content (`artifacts.rs`), so a parsed and a
  stored file give the same index and vectors for another model need no parse. Schema 14 rebuilds
  older indexes once. The store adds 7.5% to `.scrutate/` here and 9.5% on Guava; a fresh build
  costs 18% less on Guava ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- The `tsquery` cache names files by their paths from the root, so a copied tree answers from
  the first tree's cache.
- A look for changes walks the tree once for both indexes, and `grep_code` and `references` read
  the text index for the files it holds as they are now: a rare word on 12,001 files, 405 ms of
  CPU to 90 ms; resolved `refs` of a rare name on 12,000 files, 0.54 s to 0.05 s.
- One session module (`session.rs`) holds a repository open for the MCP server, the language
  server, the bindings and `watch`, with reads side by side on up to 8 readers: eight
  `references` calls sent together no longer queue ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- The resolver is one walker plus a rule set per language (`resolve/`); no answer changed.
- `refs`, `callers`, `callees`, `deps`, `rdeps` and the language server resolve PHP
  (`resolve/php.rs`): namespaces and `use`, classes, traits, `$x->m()` through the type of `$x`,
  `self::`, `static::`, `parent::` and `C::m()` ([CLI](docs/CLI.md)).
- `search` and the ranked part of `find` use a running `serve` session too, with the same bytes
  and exit code as a fresh process in about half the time or less.
- The models are trained on rows the engine writes (`bench/gen_features.py`, `bench/train_semantic.py`);
  on six packages the rows equal the reference's bit for bit (7,836 queries, 1,171,940
  candidates).
- The benchmark builders (`bench/make_bench.py`, `bench/make_bench_code.py`, `bench/quality.py
  --make`) and the ranker's trainer read the engine too: the hidden `views` command. Before the
  reference engine was removed they wrote the same files byte for byte on 13
  packages (1,877 files, 12,039 queries) and the same ranker ([QUALITY](docs/QUALITY.md)).
- `batch --features` names the features (`feature_names`); no feature, score or ranking changed.
- The index keeps its tables to itself: other modules ask `Index`; answers are unchanged.
- The language server's go to definition names the definition the resolver picks: on
  `item.total()` after `item = Order()`, with a class `Item` in the file, it answered
  `Item.total` and now `Order.total`.
- One module decides what a name refers to (`names.rs`); a `callers` request reads each file once
  (450 call sites in one file: 33 ms CPU to 19 ms).
- The models are one set (`models.rs`) loaded on first use: `symbol` on this crate's index takes
  6.8 ms CPU (25.8 ms before), a ranked search 20.5 ms (30.2 ms).
- A change of vector model remakes the vectors without rebuilding the index: to
  `SCRUTATE_SEMANTIC=none` 22 ms CPU instead of 507 ms, and back 1.39 s instead of 3.47 s here,
  2.90 s instead of 8.34 s on Guava. A ranker whose features differ makes `search` fail saying
  so; before, it was silently dropped.
- Incremental index (SQLite + memory-mapped segments), content hashing, atomic updates,
  compaction, GC, corruption detection and self-healing rebuild, schema/model versioning,
  multi-process safety, `check` command.
- 61 tree-sitter grammars in the default build and 13 more with `--features extra-langs`, each
  verified by a snapshot sample (writing the optional ones' samples fixed the D, LLVM IR,
  AutoHotkey, VHDL and OCaml rules); keyword heuristics for grammar-less languages, windows for
  everything else; imports and call edges ([LANGUAGES](docs/LANGUAGES.md)).
- Text index for repeated grep-style queries (`scrutate index --text`, `grep --index`): trigram
  postings whose output equals the walk's; incremental and safe for concurrent readers.
- UTF-16 files with a byte-order mark are transcoded and indexed (they were skipped as binary).
- Text index format 2 adds position masks to the postings (the Linux tree: `kernel_thread` 1,781
  candidate files to 164; from 4% behind Zoekt to 5% ahead). The index is 409 MB instead of
  148 MB.
- Chunk line numbers follow grep's line counting (`\n` only), found by fuzzing. Index schema 12.
- Ranked search: field-weighted BM25 + exact lookup + semantic vectors (IVF) + LightGBM
  LambdaRank ranker; bit-identical with the Python reference implementation.
- The Rust engine and the Python reference cluster a large segment (20,000 chunks or more)
  bit for bit the same, checked by `bench/parity.py` and `bench/parity_repos.sh`.
- The ranker is trained with documented types as answers too, and a feature tells a name typed
  with its case (`BurstFiles`) from the same words in another case: a type is found from its
  documentation with MRR 0.478 (0.017 before), functions lose 0.031 ([QUALITY](docs/QUALITY.md)).
  `bench/retrain_ranker.sh` refuses a ranker that does worse; `bench/eval_sealed.sh` keeps a
  sealed hold-out.
- Ranked search reads every line of an indexed file: code outside definitions is in 50-line
  windows, and a definition starts at its attribute lines (`#[get("/users")]` above a handler is
  found with it).
- Lock files with an ordinary extension (`package-lock.json`, `pnpm-lock.yaml` and the like) are
  left out of the index like `*.lock`; `grep` still searches them ([CLI](docs/CLI.md)).
- Less memory in `grep -j1` (141 MB to 103 MB for a 100 MB line), and faster (the Linux tree
  2.44 s to 2.36 s, py-big 132 ms to 126 ms); the default mode on one 740 MB file holds 6.1 MB,
  down from 14.3 MB.
- `refs`, `deps` and `rdeps`:
  - `rdeps` of a module with thousands of importers stays small (3.8 GB to 103 MB for 5,000
    importers).
  - A relative import that climbs above the root (`../../lib` from `src/a.js`, `from ...x` in
    `src/b.py`) resolves to no file.
  - `refs` places uses right after a byte-order mark or an invalid UTF-8 byte.
  - A file past 2,000 levels of nesting is listed as too deep to read, where `refs` overflowed the
    stack.
  - The resolver no longer panics on a file that starts with a byte-order mark and holds a NUL
    byte.
  - `refs` through a typed variable takes linear time: 5,000 uses from 23 to 32 s to under
    0.25 s.
  - A Java class in front of a call (`Util.foo()`) is a use of the class: every static call on a
    class was "not found".
  - Rust binaries, tests, examples, benches and `build.rs` are crate roots (`crate::` in
    `src/bin/tool.rs` starts at that file).
  - A variable's type comes from the declaration in scope at the use.
  - A Java diamond of interfaces no longer takes exponential time (over 100 s to 0.04 s at 57
    interfaces).
  - A tsconfig `extends` written as a list is read.
  - An inherited method is the one the method resolution order reaches first, up to 8 levels:
    `class D(B, C)` calls `C.m` where it named `A.m`, and a chain of more than 3 classes no
    longer answers not found.
  - A variable inside a closure has the type of its enclosing declaration
    (`xs.map(x => p.norm())` calls `Point.norm`), unless the closure binds the name
    (`xs.map(p => p.norm())`, a bare `p => ...`).
  - A Python base class from outside the repository keeps its place in the method resolution
    order: `class T(unittest.TestCase, Helper)` no longer calls `Helper.setUp` for
    `self.setUp()`, which stays undecided because `TestCase` may define it (`object` is not
    counted).
  - tsconfig `paths` and `${configDir}` are read as TypeScript 5.5 reads them.
  - `refs` and `callers` stay fast where many blocks bind one name (2,000 blocks: 11 to 13 s
    before, 0.14 s).
  - `refs` counts every use on a line (it stopped at 64); `--max-results N` counts uses.
  - `refs` in one large function takes linear time: 4,000 calls from 28 s to 0.12 s.
  - A use on a line longer than 400 bytes shows the part around it: 16 MB of output to under
    1 MB.
- `grep -z` reads at most 64 KiB of a tar member's long name: a 306 KB archive with a 300 MiB
  name took 1.27 GB and takes 5.6 MB.
- Context output on several threads no longer runs one file's lines into another's.
- A named pipe, `<(...)` or `/dev/stdin` given as a path is searched, and a link to a directory
  is walked without an `Is a directory` warning.
- `grep -z` finds the members after an old-GNU sparse member (`tar --sparse`); they were lost.
- A large file of very long lines is streamed: 100 MB of 4 MB lines peaked at 103 MB, now 15 to
  18 MB.
- A case-insensitive search for a long literal no longer slows on near misses (lines of 79 `=`
  against a pattern of 80: 1.2 s, now 0.01 s).
- A large one-line file searched for a `-F` literal peaks under 6 MB instead of 103 MB.
- With `-e`, a first path that is not UTF-8 is searched as a path.
- A home directory kept in git is no longer indexed from a folder below it when `HOME` is a
  symbolic link.
- `--stats` and the JSON summary of a search stopped by `--max-results` count only printed
  matches.
- MCP tool calls no longer read a file outside the repository through a linked folder
  ([SECURITY](docs/SECURITY.md)).
- `serve` and `lsp` open an index another process rebuilt on their next call; before, the two
  processes rebuilt in turn.
- A panic in the first index build no longer leaves `serve` answering "still building" for good.
- `index` keeps an update and exits 0 when the segment merge after it fails.
- The text index survives more: a file shortened while read, a manifest cut by a crash, many
  small updates, and an edit that keeps size and modification time (`cp -p`, `rsync -t`,
  `touch -r`); its format is 3 ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A full index build of a C, Go or Java tree no longer keeps every syntax tree in memory: 40 MB
  of C in 600 files took 2.1 GB and takes 0.56 GB.
- Haskell type synonyms, Puppet functions, LLVM named types and OCaml classes are definitions.
- Deep nesting no longer costs more than linear time: a 1 MB Perl file of nested `sub` lines
  0.83 s to 0.2 s of CPU, 16,000 nested PHP includes 20 s to 0.06 s, declarations inside C `#if`
  blocks, and a call chain such as `a()()()` or `x.a().a()` (a 256 KB file, 22 s to 0.25 s). A
  C++ `operator` call such as `a.operator()()()` no longer stores names as long as the chain.
- A damaged segment found during a merge rebuilds the index; `serve --watch` and `watch` keep
  watching after an update panics.

- A tool call path that leaves the repository is refused before anything is looked at, and an
  absolute path elsewhere (`/a.py`) no longer names the repository's file of that name. The
  language server reads no file behind a link that leaves the root and works on a Windows share or
  in WSL (`file://wsl.localhost/...`); MCP `callees` says how many outside names it left out past
  50 ([SECURITY](docs/SECURITY.md)).
- `read_code` gives one answer for a path outside the repository whether or not a file is there.
  The Python bindings read a path spelling (`./pkg/a.py`) and a location (`pkg/a.py:3-9`) as the
  MCP tools do.
- An older scrutate no longer rebuilds an index a newer one built: it reads it when only the
  parsing rules are newer and otherwise refuses it with exit 2; `scrutate index --force`
  rebuilds it.
- A documentation comment whose lines carry no comment mark (a GNU style C block, Lua
  `--[[ ]]`, Ruby `=begin` and the like) is in its definition's documentation only, and without a
  grammar a language's own comment marks are read ([LANGUAGES](docs/LANGUAGES.md)).
- The language server goes to the definition a receiver call names, not the plain namesake:
  `o.valid?()` to `valid?`, `self.save!` to `save!`, `M.ok?(x)` to `ok?`, `Main.update!(x)` to
  `update!`, `step' y` to `step'`, and `is` in R `is.ready(x)` finds `is.ready`. References of
  `valid` no longer list `valid?`.
- `follow = true` in a repository's own `.scrutate.toml` is ignored with a warning, so a clone
  cannot make `scrutate grep` leave it through a link ([SECURITY](docs/SECURITY.md)).
- A name typed with its punctuation is found by that name first: `search 'valid?'`,
  `search Get-Greeting`, `search 'list->vector'`, `search 'Rec.valid?'`. More test file names are
  told (`button.test.jsx`, `parser_test.rs`, `parser_spec.rb`, `WidgetTest.kt`, a directory
  `MyApp.Tests`). `similar` takes a window's location.
- `callers` lists calls written outside every definition, and `callees FILE:LINE` answers for
  such a line.
- Elixir `@doc false` leaves a function undocumented, PowerShell help in a function body and an
  Erlang `-doc` attribute are documentation, and `this["k"] = function` is the member `k`.
- A Go type on its own line (`type Server struct`) takes the comment above it, and a Go type alias
  (`type Celsius = float64`) is a definition.
- `symbol`, `callers`, `callees`, `refs`, `outline`, `deps`, `rdeps`, `similar`, `dupes`
  (exact + MinHash/LSH near-duplicates), `tsquery` (tree-sitter structural search),
  cross-repository `search --also`.
- `refs` resolves each occurrence to its definition for Python, JavaScript/TypeScript, Rust, Go
  and Java; `--def` keeps one definition and `--textual` gives every whole-word occurrence.
- Java in `refs`, `callers`, `callees` and the language server: packages, imports, members,
  nested classes, records, method references, and overloads picked by the argument count
  ([CLI](docs/CLI.md)).
- `callers` of an override lists a call through its base type as undecided, a `super` call is a
  decided call of the nearest base method (also `super(C, self)`), TypeScript follows `extends`,
  and a bare Python import finds the module beside the importing file (`from helpers import x`
  in `tests/`).
- A JavaScript or TypeScript import of a workspace package by its `package.json` name resolves to
  the package's file through `exports`, `module`, `main` or `index`; a `.d.ts` beside its
  implementation no longer takes the import ([CLI](docs/CLI.md)).
- Imports are recorded as their statements write them, in more languages; a Java star import is
  one `a.b.*` record that `deps` resolves to the package's files.
- In a language the resolver does not read, a call without a receiver is a call of the
  definition in its own file (`callees` listed the index's first three namesakes), and a MATLAB `.m` file is no longer in the C family.
- A search that meets a damaged segment array rebuilds the index and answers; it panicked.
- `callers` of a constructor lists the calls of its class (`Point(1)` for `Point.__init__`,
  `new Widget()`, `Invoice.new`); it printed "(no results)". `refs` gives the reason
  `not resolved in this language` for a language with a grammar and no resolver rules.
- A call is recorded under its definition's name (a trailing `?` or `!`, a prime, `r#`):
  `callers valid?` and `callers save!` printed "(no results)", and a Haskell `step'` was indexed
  as `step`.
- The comment above a definition is its documentation in more languages, and survives what
  stands between (Rust attributes after `/// doc`: 172 items of this repository had lost theirs;
  decorators, `@spec` and the like) ([LANGUAGES](docs/LANGUAGES.md)).
- More forms are definitions: a function assigned to a name, member or table field in Lua,
  JavaScript and TypeScript (`exports.a.b = function`, a call through
  `const app = require("./app")` resolves to it), Julia methods on another module's function
  (`Base.:(==)`), R6 methods (`R6::R6Class`) and the C++ call operator
  ([LANGUAGES](docs/LANGUAGES.md)).
- `search` looks a one-word query up as a name: `search find` and `search get` printed
  "(no results)".
- A Java file counts as a test by `Test.java` or `Tests.java` with the capital (`Contest.java`
  and `Latest.java` were ranked as tests), and a Django `tests.py` is a test file.
- `languages --json` lists every kind of definition the index gives a language, and a Racket
  `define-syntax-rule` is a macro.
- `refs` finds a name in its composed and decomposed Unicode spelling.
- `tsquery` caches each file's captures in `.scrutate/tsquery/`: a repeated query on the Linux
  tree, about 25 s then 1.6 s.
- Semantic vectors stored as int8 (schema 13): the index about a fifth smaller, quality within
  0.001 MRR.
- `scrutate lsp`: a language server for editors; the VS Code extension starts it, JetBrains uses
  it through LSP4IJ.
- `scrutate watch` and `serve --watch` keep the indexes fresh as files change.
- A long-running index reparses an edited file incrementally for the 21 grammars without an
  external scanner: a 66 KB Go file, 5.5 ms to 2.0 ms per edit.
- Ranker v8: better than v7 on held-out code in 56 of 58 cells, never worse beyond noise
  ([QUALITY](docs/QUALITY.md)).
- Ranked search finds words of every script, scripts without spaces as character pairs; names
  compare in NFC (new dependency `unicode-normalization`), and Traditional and simplified Han
  find each other (a Unihan 18.0.0 table).
- Constants are symbols of kind `constant` in many languages (TypeScript `static
  readonly` fields, JavaScript `static` fields in capitals, Java `static final` and the like):
  `symbol`, `outline`, `read_code` and `refs` find them, ranked search never returns them.
- A method is named by its owner's bare type (`Box.put` for `Box<T>::put`); a dotted Lua or Elixir
  name is split off its owner (`MyApp.Accounts.User` is `User`); `Account:deposit` in Lua is
  `Account.deposit`; an object literal's functions are methods under what holds it.
- New definitions in many languages, among them Rust `mod`, `type` and `macro_rules!`, C++
  methods declared in a class, JavaScript `module.exports.X`, TypeScript `declare function`, and
  C and C++ prototypes; a `.h` file in C++ or Objective-C is parsed with that grammar
  ([LANGUAGES](docs/LANGUAGES.md)).
- Calls inside Rust macro arguments and turbofish calls (`f::<T>(x)`) are call edges.
- Calls are recorded in more languages and forms (PHP `?->`, Lisp names whole), and a Scheme
  `(define (f x) ...)` head is not a call ([CLI](docs/CLI.md)).
- Chunk text is cut at 32 KiB, and many sibling definitions or deep nesting index in linear time
  (20,000 one-line JavaScript functions: 58 s to 0.8 s; 60,000 nested Lisp calls: 19 s to 0.3 s);
  HTML and XML are no longer parsed.
- Inside a git repository the ignore files decide for `build`, `dist`, `target` and `venv`;
  `\,` keeps a comma in `SCRUTATE_INDEX_EXCLUDE_DIRS`.
- The heuristic tier reads OCaml `let f x =` as a function; Kconfig and `.txt` files get no
  keyword symbols.
- The model signature is a hash of the model file, so a retrained model rebuilds the vectors.
- A read never waits for another process's writer: it answers from the last committed index. A
  `serve` call waits up to 20 seconds for a first build.
- More kinds of damage are rebuilt, a writer stopped before its commit no longer blocks later
  ones, and a read-only index is answered as it is ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- On Unix a change of status time alone (a `chmod`) is seen.
- A `.scrutate` that is or holds a symbolic link is refused, and nothing is written or deleted
  outside the repository ([SECURITY](docs/SECURITY.md)).
- `refs`, `callers` and `callees` infer a receiver's type from return types, fields, constructors
  and more, and a call matches a definition only in its own language family and by the name as
  written (`Ok(1)` is not a call of `ok`) ([CLI](docs/CLI.md)).
- Name lookup in `symbol`, `callers`, `callees`, `similar`, `refs` and `read_code` takes the name
  as typed first, then `Type::method` and generic owners, and orders namesakes; the C++
  `operator->` and `operator*` are found by name, and an explicit destructor or operator call
  (`a.~Shape()`, `a.operator==(b)`, `p->operator()(3)`) is recorded, so `callers ~Shape` and
  `callers operator==` find it ([CLI](docs/CLI.md)).
- A location (`path:start-end`) names exactly one definition in `symbol`, `callers` and
  `callees`.
- `callers`, `callees`, `symbol`, `dupes`, `tsquery`, `read_code symbol`, `duplicates` and
  `structural_search` say when they stopped at their limit and which argument raises it.
- `tsquery` prints paths as `grep PATH...` does and names a missing path in a warning, exit code
  2; `dupes` lists groups in one fixed order.
- Imports: Go through `go.mod`, an `#include "util.h"` beside the including file; config files
  are read only as regular files inside the repository of at most 1 MB.
- A name binding hides an outer one only in its own scope; `tsquery` decodes non-UTF-8 files,
  skips files over 1 MB and refuses an empty query.
- `dupes`: `exact` means the same body once whitespace is squeezed; `--threshold 1` prints the
  exact groups only.
- `grep --symbols` with a path argument finds the file by its path under the root.
- `deps` and `rdeps` resolve imports as `refs` does (Python `from . import`, Rust, Go,
  TypeScript, Java), and `rdeps` counts the unresolved imports that may name the file.
- An index built with older parsing rules rereads every file once (`content_rev` in `meta`).
- An update can be cancelled and rolls back, also when it panics.
- A one-line file with 8,000 nested functions peaks at 542 MB to index (462 MB with
  `SCRUTATE_SEMANTIC=none`), down from about 1.1 GB (0.75 GB).
- `similar` on a constant says constants are not compared, and `similar -k 0` returns nothing
  instead of panicking.
- A backslash in a Unix file name stays in `tsquery`, `read_code` and the language server.
- Names with control characters print as escapes in the index commands, and read back as an
  argument.
- The Python reference indexes the engine's file set and cuts the same chunks; the parity check
  fails on any difference.
- One module, `stores.rs`, keeps every store under `.scrutate/` current; `reindex` and the
  bindings' `index()` now also update the text index when there is one.
- `tsquery` keys its cache by the code index's content hash, so a warm run reads no unchanged
  file.
- Every front end runs the same operations through `Repo`.
- Breaking, the JSON of the index commands (schema 2): `symbol`, `callers`, `callees`, `refs`,
  `outline`, `deps`, `rdeps`, `similar`, `dupes`, `read`, `search` and the ranked `find` print
  `{"schema": 2, "result": ...}` with `stopped_at` when the answer stopped at its limit; they
  printed the answer alone. `refs --json` and the `grep_code` result lose their own
  `"schema": 1`; a `batch` record and the MCP structured result say `"schema": 2`. Engine A's
  JSON Lines, the error records, `tsquery --json`, `check`, `stats`, `languages` and `config`
  keep schema 1 ([CLI](docs/CLI.md)).
- `scrutate read PATH [--start N] [--end N]` and `read --symbol NAME`: the MCP `read_code` tool
  on the command line.
- The MCP `grep_code` tool takes `mode` (`regex`, `literal`, `identifier`, `fuzzy`,
  `fuzzy_regex`, `pcre2`, with `max_edits`), `case` (`sensitive`, `insensitive`, `smart`),
  `word`, `path` and `globs`; it was a case-sensitive regex.
- `grep --max-bytes N` (`SCRUTATE_GREP_MAX_BYTES`, `[grep] max_bytes`) bounds a grep's text to
  whole lines with the `--offset` that continues (on this repository `grep -n "fn " .` printed
  285 KB; with `--max-bytes 4000`, 4,000 bytes or fewer). A paged answer that filled its limit
  exactly no longer prints one byte more.
- `find` lists the lines of its Engine A search in path order.
- The Python bindings gain `Repo.references`, `Repo.call`, `undecided` on the callers list, and
  `GrepIter.stats()["notes"]`.

### Product
- The `tsquery` cache and the kept answers are read only as regular files within their size
  limit, never through a symbolic link ([SECURITY](docs/SECURITY.md)).
- `scrutate --root DIR grep ...` reads grep's own arguments and shows grep's help. CI builds every
  fuzz target before replaying the inputs that once crashed one.
- The structural search numbers against ast-grep were measured with scrutate's capture cache
  filled, so scrutate parsed no file; the documents and the chart now say so, and the benchmark
  harness runs `tsquery --no-cache` from now on.
- In a linked git work tree (`git worktree add`), the first lookup copies the main checkout's
  index and parses only the files that differ: 0.06 s of user CPU (0.2 s with system time) on
  this repository, where a build costs 4.3 s. Renames, copies and a branch switch and back no
  longer parse files the index has seen; nothing of the main checkout is written.
- An index command uses the index a running `scrutate serve` holds open on the same root, through
  `.scrutate/session.sock` (Unix, owner only), with the bytes and exit code a fresh process
  prints; it never starts a server. `SCRUTATE_NO_SESSION=1` turns this off;
  `SCRUTATE_SESSION_DEBUG=1` says why a call did not use one ([CLI](docs/CLI.md)).
- The reference package lost its command line, MCP server and agent (`scrutate_ref/cli.py`,
  `mcp_server.py`, `agent.py`, `python -m scrutate_ref`, the `scrutate-ref` script and the `mcp`,
  `agent` and `all` extras): nothing used them. The binary is the product.
- Ranking quality has its own guard: `bench/quality.py` runs 1,183 committed queries on a frozen
  copy of this repository and fails below the floor in `bench/quality/floor.json`; a CI step and
  the release gates run it ([QUALITY](docs/QUALITY.md)).
- The bit-for-bit Python mirror of the engine ended: `python/scrutate_ref` no longer parses,
  indexes or ranks (`text.py`, `parse.py`, `index.py`, `lex.py`, `search.py`, `pretrain.py` are
  gone, with the tree-sitter grammars and `snowballstemmer`), and `bench/parity.py` and
  `bench/parity_repos.sh` are gone. The mirror covered 7 of the 73 grammars and doubled every
  change; the quality floor and `tests/ranking_pins.rs` watch ranking now. The package keeps
  `rank.py` and `semantic.py`; commit `8a4f4b4` is the last with `bench/eval_transformers.py` and
  `bench/ivf_recall.py`.
- `[index] exclude_dirs` is read from the `.scrutate.toml` at the index root (or
  `SCRUTATE_INDEX_EXCLUDE_DIRS`), so every front end indexes the same files; the file may be a
  link inside the root (at most 1,000,000 bytes), and a name with a backslash, which made
  `grep_code` fail, excludes nothing. A capped `--json` answer says so on standard
  error, `grep --index` from an ignored directory exits 2, and `dupes --min-tokens` at the
  largest value no longer wraps.
- Contributions are accepted under a contributor licence agreement (`CLA.md`), and the name and
  the logo have a trademark policy (`TRADEMARKS.md`).
- The licence is `MIT OR Apache-2.0` at the user's choice (it was Apache-2.0 alone): `LICENSE` is
  now `LICENSE-APACHE` beside a new `LICENSE-MIT`, and every package and
  `THIRD_PARTY_NOTICES.txt` say so; `vendor/ignore` keeps `Unlicense OR MIT`. The README has a
  "Sponsor and hire" section.
- Unified `find`, MCP server (19 tools), configuration files with environment-variable
  precedence, shell completions, progress reporting; errors as JSON records with `--json` or
  `SCRUTATE_DIAGNOSTICS=json`.
- On/off environment variables are off for `0`, `false`, `no`, `off` or empty; `NO_COLOR` turns
  colour off for any non-empty value.
- One definition of every index operation for the CLI, the MCP server, `batch` and the Python
  bindings (`ops.rs`); `batch` runs any operation by name.
- For AI agents: every MCP tool but `reindex` is read-only; `grep_code` groups matches by
  enclosing symbol; `references` resolves each use; an answer is at most 24,000 bytes by default
  (`max_bytes`) with the `offset` that continues it, as `--max-bytes` (`SCRUTATE_MAX_BYTES`) and
  `--offset` on the CLI; a first index build blocks a tool call for at most 20 seconds
  ([CLI](docs/CLI.md)).
- A capped `grep_code` or `references` answer holds the first matches in path order.
- `read_code` by path keeps at most 1 MB of line text, refuses a file with a NUL byte in its first
  8 KB, and says `more lines follow` when a large file was not read to its end.
- Commands run in a subdirectory use the index and `.scrutate.toml` above it, up to the top of
  the repository, so a nested clone or submodule does not use the outer index; `search --also`
  prints another repository's results by the path given ([CLI](docs/CLI.md)).
- Editors opened on a folder below the repository top use the index at the top, through
  `scrutate config --json`; the language server built a second, partial index there.
- In the home directory or at a filesystem root with no index, a command that would build one
  stops with exit code 2 and names `--root` and `scrutate index`.
- `languages` lists the extensions a language shares with another (`extensions_by_content`).
- `symbol`, `callers` and `callees` take `--limit`; `check --json` counts the indexed files and
  the constants; `batch` reports a line that is not JSON and runs the rest.
- `reindex` takes `force`. `grep_code` searches the files the index holds.
- JSON-RPC answers as the specification says (-32700, -32600, -32601, -32602), and notifications
  are never answered.
- MCP revision 2026-07-28 beside the handshake revisions; `serve --structured` gives every tool
  an `outputSchema`; `notifications/cancelled` stops a call, and a `progressToken` gets progress
  during the first build ([CLI](docs/CLI.md)).
- `read_code` reads a whole definition with `symbol`, and the location at the start of every
  result (`path:start-end`) is accepted wherever a tool takes a symbol.
- The index recovers from a corrupt database in every front end that stays open, not only the
  CLI.
- `refs` resolves a method call on a variable through its declared type (`x: T`, `x = T()`,
  `new T()`, `T::new()`, `T {}`, `&T{}`), and finds a Go method by its receiver type.
- TypeScript and JavaScript imports through `paths` or `baseUrl` resolve for `refs`, `deps` and
  `rdeps`.
- Smart case sees a letter written by its code point (`\x41`).
- `watch` and `serve --watch` no longer stop on Linux and Windows when `.git/info/exclude` has
  patterns.
- `-U` with `--fuzzy` or `--fuzzy-regex` is an error.
- An MCP answer checks the files it cites with one `stat` each and reindexes a changed one alone;
  without `--refresh-interval` the server scans at most a second apart.
- At the end of its input the server answers the calls in flight first; an `initialize` without a
  known `protocolVersion` is answered with 2025-11-25.
- `read_code` honours an explicit `end` and says where the rest starts.
- A tool argument of the wrong type is an error naming it; `search_code` takes at most 10
  `alt_queries`.
- `watch` and `serve --watch` set the watch before the first update, use one inotify watch per
  directory on Linux (2 instead of 7,012 on a tree with an ignored `node_modules`), and update a
  burst of saved files without a walk on Linux and Windows ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- `find` takes a question with a space and a trailing `?` as words, and goes to ranked search
  when a regex-looking query finds nothing.
- Text files check out with LF line ends on every platform (`.gitattributes`).
- Toolchain pinned to Rust 1.99.1 (`rust-version` 1.91).
- Python bindings (PyO3 0.30): three wheels per platform (`cp39-abi3`, `cp314t`,
  `cp315-abi3.abi3t`), the GIL released (in `stats()` too), notices in `dist-info/licenses`,
  Ctrl-C within about 100 ms, and results as a `scrutate.Results` list with `stopped_at`
  ([bindings](crates/scrutate-py/README.md)).
- Integrations: one MCP configuration per client, GitLab and Bitbucket templates, a pre-commit
  hook that works through `repo:`, the GitHub Action on Windows runners, Neovim's
  `lsp/scrutate.lua` (so `vim.lsp.enable('scrutate')` is the whole setup), and VS Code and Neovim
  that never run a `scrutate.exe` from the opened folder.
- `release.yml` refuses a tag whose version differs from the packages' (`tools/check_version.py`)
  and runs all of `ci.yml` on the tagged commit first; every action is pinned to a commit; the
  glibc archives need at most glibc 2.34.
- The GitHub Action puts scrutate's exit code in its `exit-code` output and fails only on an
  error (2).
- Benchmark suite (`bench/leaderboard.py`, `bench/indexed.py`) against grep, ripgrep, ack, ag,
  git grep, ugrep, ast-grep, Zoekt and livegrep, every answer checked, with a regression gate
  (`SCRUTATE_BASE_BIN`, `leaderboard_regression.py --within`) ([BENCHMARKS](docs/BENCHMARKS.md)).
  `benchmarks.yml` runs it on the small corpora and counts every benchmark's instructions under
  cachegrind (`bench/instructions.py`, past 1% fails), behind one `benchmark gate` check.
- Random checks in CI: `bench/fuzz_grep.py` and `bench/fuzz_incremental.py`, 20 seconds each.
- Packaging: Homebrew, Scoop and winget (schema 1.12.0), `.deb` and `.rpm`, each carrying
  `THIRD_PARTY_NOTICES.txt`; `docs/dashboard.html` charts the benchmark runs.
- Fuzz targets (`fuzz/`): the `.npy` reader (three defects fixed), the text index, the
  edit-distance matcher and every parser tier.
