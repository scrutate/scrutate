# Integrations and install paths against their platforms' docs (November 2026)

Research date: 2026-11-11. Every claim about a platform links to the page it came from, with the version or date the page states. Where a page states no date, the table says so. Project file references are `path:line` at the working tree of 2026-11-11 (branch `release/0.1.0`). Nothing was run except reading files, `gzip -c` on the model file and `command -v` checks; no editor was opened and nothing was installed.

Status on 2026-11-17: the project side of this note is the tree of its research date. Since then `integrations/mcp/README.md` gives an entry per client and `scrutate setup <client>`; the Neovim plugin ships `lsp/scrutate.lua`; the JetBrains README adds the tools by hand with `$ProjectFileDir$`; the VS Code extension has `repository` and a `LICENSE`; the release builds an `abi3t` wheel for 3.15 and later and `pyproject.toml` has no `extension-module`; the model is `crates/scrutate/models/semantic.npz` (7.5 MB) and there is no `[patch]`; `packaging/README.md` names only the tap; `.pre-commit-hooks.yaml` exists with `language: unsupported`; the GitLab job has `merge_request_event` rules and writes a Code Quality report; the composite action has Windows paths and `integrations/github/README.md` shows how to use it; `tools/release_local.sh` writes the per-archive `.sha256` beside `SHA256SUMS`, and both the local and the CI archives hold `CHANGELOG.md`, `docs/` and `integrations/`; the winget manifests are 1.12.0.

## What this means for scrutate

Ordered by how badly a user would be hit.

1. **The "generic" MCP snippet is wrong for four of the six clients.** `integrations/mcp/README.md:30-33` gives one `mcpServers` JSON for "Cursor, Windsurf, JetBrains AI Assistant, ...". VS Code reads `servers`, not `mcpServers`. Codex reads TOML (`[mcp_servers.scrutate]`). Zed reads `context_servers`. Cursor marks `"type": "stdio"` as required. A user who pastes the snippet into VS Code, Codex or Zed gets no server and no error that points at the cause. Fix: one snippet per client, see [section 7](#7-mcp-client-configuration).
2. **Neovim users get no language server.** `integrations/neovim/lua/scrutate.lua` only shells out to the CLI. There is no `vim.lsp.config` / `vim.lsp.enable` setup and no `lsp/scrutate.lua`, so go to definition and references through `scrutate lsp` are not wired up, while VS Code and JetBrains users get them. Fix: ship `lsp/scrutate.lua` and document two lines, see [section 2](#2-neovim).
3. **The JetBrains External Tools steps do not match the IDE.** `integrations/jetbrains/README.md:6-7` says *External Tools → Import*. The JetBrains settings page lists only Add, Remove, Edit, Up, Down and Copy. The same README (`:17-18`) says `path:line` output is clickable, but the JetBrains docs say the tool "must output absolute file paths", and scrutate prints relative ones. See [section 3](#3-jetbrains-lsp4ij-and-external-tools).
4. **`vsce package` stops twice for a prompt.** `integrations/vscode/package.json` has no `repository` field and the extension folder has no `LICENSE` file. vsce asks "Do you want to continue? [y/N]" for each and aborts on anything but `y`, so a CI or scripted package fails. Fix: add `"repository"` and copy `LICENSE` into `integrations/vscode/`. See [section 1](#1-vs-code-extension).
5. **Free-threaded Python 3.15 has no wheel.** The release builds `cp39-abi3` and one `cp314t` wheel (`.github/workflows/release.yml:67-83`). PyO3 says the free-threaded build "cannot load `abi3` wheels". Python 3.15 is released, so a 3.15t user falls back to a source build. PyO3 0.30 and maturin 1.14+ can build an `abi3t` wheel for 3.15 and later. Also, `pyproject.toml:20` turns on `pyo3/extension-module`, which PyO3 0.30 marks deprecated. See [section 9](#9-python-bindings).
6. **The crates.io reason in the docs is right but incomplete.** The 10 MB limit is still the documented limit, and `models/semantic.npz` is 13,314,842 bytes after `gzip -9`. A second blocker is not mentioned: the build depends on `[patch.crates-io] ignore = { path = "vendor/ignore" }` (`Cargo.toml:25-26`), and `crates/scrutate/src/instant.rs:2005` calls `grow_threads`, one of the vendored changes. Cargo only reads `[patch]` at the workspace root, so a crate fetched from crates.io would build against the upstream `ignore`. See [section 8.6](#86-cargo-install).
7. **homebrew-core is not open to a new repository.** `packaging/README.md:56-58` says to open a PR "to a tap ... or to homebrew-core". Homebrew's acceptance policy says a repository under 30 days old is normally not eligible, and a self-submission needs 90 forks, 90 watchers or 225 stars. A tap is the only path for 0.1.0. The formula itself is fine. See [section 8.1](#81-homebrew).
8. **pre-commit `language: system` is a deprecated alias.** pre-commit 4.4.0 renamed it to `unsupported` and says the alias "will be removed in a future version" (`integrations/pre-commit/README.md:13`). There is no `.pre-commit-hooks.yaml` at the repository root, so `repo: https://github.com/crisnahine/scrutate` cannot work after publishing; only the `repo: local` form shown in the README does. See [section 4](#4-pre-commit).
9. **The GitLab template says "every merge request" but runs on branch pipelines.** `integrations/gitlab/.gitlab-ci.yml:1` claims merge requests, the job has no `rules`, and GitLab says a job needs `rules` matching `merge_request_event` to run in merge request pipelines. Its output is a plain artifact, so nothing shows in the merge request. See [section 5](#5-gitlab-and-bitbucket-templates).
10. **The composite action has no Windows path and no usage doc.** `.github/actions/scrutate/action.yml:17-23` exits "unsupported runner" on Windows though the release has Windows zips. It cannot be listed on the Marketplace from a subdirectory. It works when referenced as `crisnahine/scrutate/.github/actions/scrutate@v0.1.0`, but no file says so. See [section 6](#6-github-composite-action).
11. **`tools/release_local.sh` does not make what the templates download.** The CI release writes one `scrutate-<v>-<target>.sha256` per archive (`release.yml:50`), and GitLab, Bitbucket, the action and `docs/INSTALL.md:28-29` fetch that file. The local script writes only `SHA256SUMS` (`tools/release_local.sh:36`), and its archive also carries `CHANGELOG.md` and `integrations/` (`:24,27`), which the CI archive does not. A local dry run of the templates needs the `.sha256` made by hand. See [section 5](#5-gitlab-and-bitbucket-templates).
12. **winget schema 1.9.0 is old.** Microsoft Learn's manifest page (2026-09-14) uses 1.12.0, and winget-pkgs lists a 1.28.0 schema. Layout and fields are right. Whether winget-pkgs still accepts 1.9.0 is not stated. See [section 8.3](#83-winget).

Correct as checked: VS Code `engines`, activation, `main`, settings, client start on stdio and document selector; LSP4IJ server and mapping steps; Bitbucket anchors and artifacts; every release asset name the templates expect; the action's inputs and shells; the Claude Code command; the Homebrew formula's helpers; the Scoop manifest; the winget folder layout; the deb and rpm required fields; the wheel tags.

## 1. VS Code extension

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| `engines.vscode` | Required, "Cannot be `*`", a range of supported versions ([manifest reference](https://code.visualstudio.com/api/references/extension-manifest), dated 11/4/2026). Current VS Code is 1.145 of 2026-11-04 ([updates](https://code.visualstudio.com/updates)). | `^1.85.0`, `package.json:8`; `@types/vscode` 1.85.0, `:14` | Correct. `vscode-languageclient` 9.0.1 needs `^1.82.0` (`package-lock.json:119-121`), so 1.85 is enough. |
| Activation | `onStartupFinished` activates "some time after" start-up and "will not slow down VS Code startup". Commands contributed since 1.74.0 need no `onCommand` entry ([activation events](https://code.visualstudio.com/api/references/activation-events), 11/4/2026). | `["onStartupFinished"]`, `package.json:10` | Correct. The client starts in every window with a folder, which is what the README promises. |
| `main` | "The entry point to your extension" ([manifest](https://code.visualstudio.com/api/references/extension-manifest)). | `./extension.js`, `package.json:11`, plain CommonJS | Correct. |
| Settings | `contributes.configuration` with `properties` ([manifest](https://code.visualstudio.com/api/references/extension-manifest)). | `scrutate.path`, `scrutate.results`, `package.json:24-30` | Correct. A change to `scrutate.path` takes effect after a window reload, since the client reads it once (`extension.js:67`). |
| Client start | The guide imports from `vscode-languageclient/node`, calls `await client.start()` and disposes the client in `deactivate`. Its sample uses `^9.0.1` and `engines` `^1.100.0` ([language server guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide), 11/4/2026). | `new LanguageClient(id, name, { command, args: ["lsp"], options: { cwd } }, ...)`, `client.start()`, `client.stop()` in `deactivate`, `extension.js:5,68-74,141-143` | Correct. A `command` server option with no `transport` talks over stdio. The guide's own sample uses IPC with a Node module, so the stdio form is not shown on that page (see Not verified). |
| Document selector | The guide's form is `[{ scheme: 'file', language: 'plaintext' }]` ([guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide)). | `[{ scheme: "file" }]`, `extension.js:72` | Correct. A filter with only `scheme` matches every file on disk, which fits a server that answers for every language. Unsaved (`untitled`) buffers are not sent. |
| `vscode-languageclient` version | Latest client is 11.0.0 (release of 22 Oct, [releases](https://github.com/microsoft/vscode-languageserver-node/releases)). Its `package.json` requires `vscode ^1.91.0` and exports `./node` ([client package.json](https://raw.githubusercontent.com/microsoft/vscode-languageserver-node/main/client/package.json)). 10.0.0 changed `OutputChannel` to `LogOutputChannel`, the show-document middleware and handler parallelism. 11.0.0 changed `PipeTransport` and `SocketTransport`. Neither lists a change to the `LanguageClient` constructor or to `start`/`stop` ([README history](https://github.com/microsoft/vscode-languageserver-node/blob/main/README.md)). | Pinned 9.0.1, `package.json:13` | Correct and working. Moving to 11.0.0 needs `engines.vscode` `^1.91.0`. The code uses none of the APIs that changed. |
| `repository` | vsce logs "A 'repository' field is missing", then asks "Do you want to continue? [y/N]" and throws `Aborted` unless the answer is `y`. `--allow-missing-repository` skips it ([vsce `package.ts`](https://raw.githubusercontent.com/microsoft/vscode-vsce/main/src/package.ts)). | Missing in `package.json` | **Wrong.** Add `"repository": { "type": "git", "url": "https://github.com/crisnahine/scrutate" }`. With the field set, vsce also rewrites relative README links to the GitHub repo ([publishing](https://code.visualstudio.com/api/working-with-extensions/publishing-extension), 11/4/2026). |
| LICENSE | vsce logs "LICENSE, LICENSE.md, or LICENSE.txt not found" and asks the same question, unless `--skip-license` ([vsce `package.ts`](https://raw.githubusercontent.com/microsoft/vscode-vsce/main/src/package.ts)). The publishing page lists a root LICENSE as a tip. | No file in `integrations/vscode/`; `"license": "Apache-2.0"`, `package.json:7` | **Wrong.** Copy `LICENSE` into `integrations/vscode/` (vsce looks in the extension folder). |
| README | vsce throws only if README.md still holds the generator's template text ([vsce `package.ts`](https://raw.githubusercontent.com/microsoft/vscode-vsce/main/src/package.ts)). README images must be HTTPS and not SVG ([publishing](https://code.visualstudio.com/api/working-with-extensions/publishing-extension)). | `README.md`, no images | Correct. |
| Icon | Optional. If set: PNG, at least 128x128, not SVG ([publishing](https://code.visualstudio.com/api/working-with-extensions/publishing-extension)). | None | Correct (packages without one). The Marketplace page shows a blank icon. |
| `publisher` | Required in `package.json` ([publishing](https://code.visualstudio.com/api/working-with-extensions/publishing-extension)). | `"scrutate"`, `package.json:6` | Correct for `vsce package`. Publishing needs that publisher id to exist on the Marketplace (not checked). |
| `categories` | One of a fixed list that includes `Programming Languages` and `Other` ([manifest](https://code.visualstudio.com/api/references/extension-manifest)). | `["Other"]` | Correct. `Programming Languages` would be the better fit for a language server. |

Test on a Mac without touching the user's VS Code profile:

```sh
T=$(mktemp -d)
cp -R integrations/vscode "$T/ext-src" && cd "$T/ext-src"
npm ci                                         # into $T only
npm run check                                  # tsc type-check of extension.js
npx --yes @vscode/vsce package --no-dependencies=false </dev/null   # shows both prompts; aborts today
npx --yes @vscode/vsce ls                      # files that would go into the .vsix
code --extensions-dir "$T/exts" --user-data-dir "$T/data" --install-extension scrutate-0.1.0.vsix
code --extensions-dir "$T/exts" --user-data-dir "$T/data" --list-extensions --show-versions
```

Running the extension itself (activation, the client talking to `scrutate lsp`) needs a VS Code window, even with `@vscode/test-electron`. With the throwaway `--extensions-dir` and `--user-data-dir` above, it does not touch the user's profile, but it opens a window. The server side is already tested in `crates/scrutate/tests/lsp.rs`.

## 2. Neovim

Current stable is v0.12.6 of 2026-10-09; the previous minor line ended at v0.11.7 of 2026-03-28 ([releases](https://github.com/neovim/neovim/releases)). The pages at neovim.io/doc/user/ are built from the development branch (one entry is marked "Since: 0.13.0", [lua](https://neovim.io/doc/user/lua/), generated 2026-11-10), so the claims below are only the ones marked as older than 0.12.

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| LSP setup | `vim.lsp.config()` and `vim.lsp.enable()` are "Since: 0.11.0". A config has `cmd`, `filetypes`, `root_markers`, or lives in `lsp/<name>.lua` on `runtimepath`. `filetypes` is "`nil` for ALL filetypes" ([lsp](https://neovim.io/doc/user/lsp/)). `vim.lsp.start(config, opts)` is the lower-level call, with `root_dir` deciding client reuse. | Nothing. `lua/scrutate.lua` runs the CLI only. | **Missing.** Neovim users get no `scrutate lsp`. Fix below. |
| Layout | Files in `plugin/` run at start-up with no registration; modules in `lua/` load on `require`. A `setup()` that only sets options is preferred; a plugin that needs `setup()` before it works is a tradeoff the author should weigh ([lua-plugin](https://neovim.io/doc/user/lua-plugin/)). | Only `lua/scrutate.lua`; commands are created inside `setup()` (`:55-61`). README shows the lazy.nvim `config` call. | Correct as documented. A `plugin/scrutate.lua` that creates the four commands would make it work without `setup()`. |
| APIs used | `vim.fn.systemlist`, `vim.v.shell_error`, `vim.json.decode`, `vim.tbl_extend`, `vim.list_extend`, `vim.notify`, `nvim_create_user_command` | `lua/scrutate.lua:7-61` | Cannot be verified from documentation alone: the fetched pages give no "Since" tag for these. Run the headless test below on 0.12.6 and 0.11.7. |

Fix: add `integrations/neovim/lsp/scrutate.lua`, which Neovim 0.11+ finds on `runtimepath`:

```lua
return {
  cmd = { "scrutate", "lsp" },
  root_markers = { ".scrutate", ".git" },
}
```

and in the README: `vim.lsp.enable("scrutate")`. Without the file, the same in one call:

```lua
vim.lsp.config("scrutate", { cmd = { "scrutate", "lsp" }, root_markers = { ".scrutate", ".git" } })
vim.lsp.enable("scrutate")
```

`filetypes` is left out on purpose so it attaches to every filetype, as the VS Code client does.

Test without touching `~/.config/nvim` (Neovim is not installed here; the release tarball runs from a temp dir):

```sh
T=$(mktemp -d); cd "$T"
curl -sSfLO https://github.com/neovim/neovim/releases/download/v0.12.6/nvim-macos-arm64.tar.gz
tar xzf nvim-macos-arm64.tar.gz; NV="$T/nvim-macos-arm64/bin/nvim"
cd ~/Documents/Projects/scrutate
"$NV" --headless --clean --cmd "set rtp^=$PWD/integrations/neovim" \
  -c 'lua require("scrutate").setup()' -c 'ScrutateSymbol hits_json' \
  -c 'lua print(vim.inspect(vim.fn.getqflist()[1]))' -c 'qa!'
"$NV" --headless --clean --cmd "set rtp^=$PWD/integrations/neovim" \
  -c 'lua vim.lsp.enable("scrutate")' -c 'edit crates/scrutate/src/fmt.rs' -c 'sleep 5' \
  -c 'lua print(#vim.lsp.get_clients({ name = "scrutate" }))' -c 'qa!'
```

Repeat with the v0.11.7 tarball for the previous release. The asset name `nvim-macos-arm64.tar.gz` is from memory of past releases; check it on the releases page first.

## 3. JetBrains: LSP4IJ and External Tools

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Declaring a server | From the `[+]` at the top of the language server settings, or from the menu in the LSP console. Tabs: **Server** (name, command), **Mappings** (Language, File type, or File name pattern, each with an optional Language ID), Configuration, Debug, Installer, Workspace Folders ([UserDefinedLanguageServer.md](https://github.com/redhat-developer/lsp4ij/blob/main/docs/UserDefinedLanguageServer.md), no date). | `README.md:26-35`: `+`, Server tab name `Scrutate` and command `scrutate lsp`, Mappings tab with file name patterns and a language id | Correct. The menu path *Settings → Languages & Frameworks → Language Servers* (`:27`) is not on the LSP4IJ pages I read. |
| Template | A template directory holds `template.json` (`id`, `name`, `programArgs` with `default` and `windows`, `fileTypeMappings` of `{ fileType: { name, patterns }, languageId }`), and optionally `settings.json`, `initializationOptions.json`, `clientSettings.json`, `installer.json` ([template doc](https://github.com/redhat-developer/lsp4ij/blob/main/docs/UserDefinedLanguageServerTemplate.md)). "Import from custom template..." loads such a directory ([user-defined LS](https://github.com/redhat-developer/lsp4ij/blob/main/docs/UserDefinedLanguageServer.md)). | No template shipped | Correct (nothing to check). Shipping one would cut the manual steps to one import; example below. |
| External Tools import | The External Tools settings toolbar has Add, Remove, Edit, Up, Down and Copy ([External tools settings](https://www.jetbrains.com/help/idea/settings-tools-external-tools.html)). No import is described there or on [External tools](https://www.jetbrains.com/help/idea/configuring-third-party-tools.html). | `README.md:6-7`: "External Tools → Import the file `External Tools.xml`" | **Wrong.** The user finds no Import button. Fix: drop the import step and keep the by-hand table, or say where the XML goes in the IDE config directory once that is checked (see Not verified). |
| Clickable output | Output filters use `$FILE_PATH$:$LINE$`, and "the tool must output absolute file paths" ([External tools settings](https://www.jetbrains.com/help/idea/settings-tools-external-tools.html)). | Filters in `External Tools.xml:8-12` and others; `README.md:17-18` says links are clickable; `search`, `symbol`, `callers` print repository-relative paths | **Wrong** for relative output. Fix: say links work only with absolute paths, or pass `$ProjectFileDir$` as the path argument where a command takes one (`grep ... "$Prompt$" "$ProjectFileDir$"`). |

A template that would match the README's steps (`template.json`):

```json
{
  "id": "scrutate",
  "name": "Scrutate",
  "programArgs": { "default": "scrutate lsp", "windows": "scrutate.exe lsp" },
  "fileTypeMappings": [
    { "fileType": { "name": "Scrutate files", "patterns": ["*.py", "*.rs", "*.go", "*.ts", "*.js", "*.java"] }, "languageId": "scrutate" }
  ]
}
```

Test: none of this runs headless. LSP4IJ needs a running JetBrains IDE with the plugin installed. A throwaway IDE config is possible with `idea.config.path` and `idea.system.path` set in a custom `idea.properties` (not checked against JetBrains docs). The External Tools XML can only be checked for well-formedness here: `python3 -I -c 'import xml.dom.minidom,sys; xml.dom.minidom.parse(sys.argv[1])' "integrations/jetbrains/External Tools.xml"`.

## 4. pre-commit

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Manifest for `repo:` | A hook repository "must contain a `.pre-commit-hooks.yaml` file". Required keys `id`, `name`, `entry`, `language`; `pass_filenames` defaults to `true`, `types` to `[file]` ([pre-commit.com](https://pre-commit.com/), quick start shows 4.7.0). | No `.pre-commit-hooks.yaml` in the repository | Correct for what is documented (only `repo: local`). After publishing, `repo: https://github.com/crisnahine/scrutate` fails until the manifest exists. |
| `language` | `unsupported` is "new in 4.4.0: previously `language: system`. the alias will be removed in a future version". Same for `unsupported_script` (was `script`). `rust` hooks are "installed via `cargo install --bins`" and the repo must have a `Cargo.toml` that builds a binary ([pre-commit.com](https://pre-commit.com/)). | `language: system`, `integrations/pre-commit/README.md:13` | **Wrong (deprecated).** Use `language: unsupported` and add `minimum_pre_commit_version: "4.4.0"`. `rust` would compile the whole tool on each user's first run, and the repository root is a virtual workspace (`Cargo.toml:1-5`), so `cargo install --bins` at the root has no package to install. Whether pre-commit's rust support handles a workspace member is not stated. |
| `repo: local` | "A local hook must define id, name, language, entry, and files / types" ([pre-commit.com](https://pre-commit.com/)). | `id`, `name`, `entry`, `language`, `pass_filenames: false`; no `files`/`types` | Correct: `types` has a default. |
| Hook logic | n/a | `entry: bash -c 'scrutate index >/dev/null && ! scrutate dupes ... | grep -q "\"kind\": \"exact\""'` | Works with today's pretty JSON (`crates/scrutate/src/cli.rs:815`). If `scrutate dupes` fails, `grep -q` finds nothing and `!` turns it into a pass. |
| `rev` | pre-commit "assumes that the value of rev is an immutable ref (such as a tag or SHA)"; branches are "not supported" ([pre-commit.com](https://pre-commit.com/)). | n/a | Applies once a manifest exists. |

If a manifest is added at the root, this shape follows the page:

```yaml
- id: scrutate-dupes
  name: scrutate duplicate functions
  entry: bash -c 'scrutate index >/dev/null && ! scrutate dupes --json --min-tokens 80 | grep -q "\"kind\": \"exact\""'
  language: unsupported
  pass_filenames: false
  minimum_pre_commit_version: "4.4.0"
```

Test before publishing (pre-commit is not installed here; a venv keeps it out of the system):

```sh
T=$(mktemp -d); python3 -m venv "$T/v"; "$T/v/bin/pip" install -q pre-commit
cd "$T" && git init -q demo && cd demo && echo 'def a(): return 1' > a.py && git add a.py
# paste the YAML block from integrations/pre-commit/README.md into .pre-commit-config.yaml, then:
git add .pre-commit-config.yaml && "$T/v/bin/pre-commit" run --all-files
# with a root .pre-commit-hooks.yaml committed in the scrutate repo:
"$T/v/bin/pre-commit" try-repo ~/Documents/Projects/scrutate scrutate-dupes --all-files
```

`try-repo` takes "a local path on disk" and uses `HEAD` unless `--ref` is given ([pre-commit.com](https://pre-commit.com/)), so it needs the manifest committed, not only in the working tree.

## 5. GitLab and Bitbucket templates

### GitLab

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| `include` | `include:remote` takes "A public URL accessible by an HTTP/HTTPS `GET` request", with no authentication ([CI/CD YAML reference](https://docs.gitlab.com/ci/yaml/), no page date). Included files merge in order and the main file wins on overlap; arrays are replaced, not merged ([includes](https://docs.gitlab.com/ci/yaml/includes/)). | Comment: "Copy into your repository's .gitlab-ci.yml or `include:` it", `.gitlab-ci.yml:2` | Correct. The include line would be `include: { remote: "https://raw.githubusercontent.com/crisnahine/scrutate/v0.1.0/integrations/gitlab/.gitlab-ci.yml" }`. Its global `variables:` block merges into the including pipeline. |
| Components | A component project needs a top-level `templates/` directory with `spec: inputs:`, and "You can only reference components in the same GitLab instance as your project". Catalog listing needs a semantic-version release made by the `release` keyword ([components](https://docs.gitlab.com/ci/components/), history up to GitLab 19.0). | Not a component | Correct to not claim one. A component would need a mirror project on gitlab.com. |
| Stages | Default stages are `.pre`, `build`, `test`, `deploy`, `.post` ([YAML reference](https://docs.gitlab.com/ci/yaml/)). | `stage: test`, `:7` | Correct, unless the including project defines `stages` without `test`. |
| `before_script` | "The combined scripts execute together in a single shell" ([YAML reference](https://docs.gitlab.com/ci/yaml/)). | `export PATH` in `before_script`, used in `script`, `:14-16` | Correct. |
| Merge requests | A job runs in merge request pipelines only with `rules` (or `workflow:rules`) matching `$CI_PIPELINE_SOURCE == "merge_request_event"` ([merge request pipelines](https://docs.gitlab.com/ci/pipelines/merge_request_pipelines/)). | No `rules`; comment says "on every merge request", `:1` | **Wrong** wording: it runs in branch pipelines. Fix the comment, or add `rules: [{ if: '$CI_PIPELINE_SOURCE == "merge_request_event"' }]`. |
| Code quality | A Code Quality artifact is "a single JSON array" of objects with `description`, `check_name`, `fingerprint`, `location.path`, `location.lines.begin`, `severity` (`info` to `blocker`). Results show in the merge request Reports tab on every tier, in the Changes view on Ultimate ([code quality](https://docs.gitlab.com/ci/testing/code_quality/)). | Plain `artifacts:paths`, `:19-21` | Correct as a download. Nothing shows in the merge request. A `scrutate dupes` to Code Quality conversion would be new work. |
| `expire_in` | The reference shows `expire_in: 1 week` as an example ([YAML reference](https://docs.gitlab.com/ci/yaml/)). | `1 week`, `:21` | Correct. |

### Bitbucket

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Reused step | A step under `definitions: steps:` with an anchor, reused by `- step: *name` ([YAML anchors](https://support.atlassian.com/bitbucket-cloud/docs/yaml-anchors/)). | `bitbucket-pipelines.yml:3-5,18-20` | Correct. |
| Artifacts | "A list of glob patterns", relative to `BITBUCKET_CLONE_DIR`, kept 14 days, downloadable from the Artifacts tab ([pipeline artifacts](https://support.atlassian.com/bitbucket-cloud/docs/use-artifacts-in-steps/)). | `- scrutate-dupes.json`, `:16-17` | Correct. |
| Sharing | `definitions: imports:` shares whole pipelines and is a Premium feature; step definitions are shared through anchors only inside one file ([YAML sharing](https://support.atlassian.com/bitbucket-cloud/docs/yaml-sharing/), from search results). | Copy-only template | Correct. Bitbucket cannot include a file from GitHub. |

The Bitbucket configuration reference page did not load (see Not verified), so variable persistence between script lines is from the shared step behavior, not a quote.

### Release asset names

| Expected by | Name | Produced by `release.yml`? | By `release_local.sh`? |
|---|---|---|---|
| GitLab `:10-13`, Bitbucket `:9-12` | `scrutate-0.1.0-x86_64-unknown-linux-musl.tar.gz` | Yes (`:20`, `:44,49`) | Only on an x86_64 musl host; never on this Mac |
| GitLab, Bitbucket | `scrutate-0.1.0-x86_64-unknown-linux-musl.sha256` | Yes (`:50`; `sha256sum *` expands before the redirect creates the file, so it lists only the archive) | **No**, only `SHA256SUMS` (`:36`) |
| Action `action.yml:18-21` | `scrutate-<v>-{x86_64,aarch64}-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, each `.tar.gz` and `.sha256` | Yes (`:20,22,23,24`) | Host target only, no `.sha256` |
| `docs/INSTALL.md:26-29` | `scrutate-0.1.0-x86_64-unknown-linux-gnu.tar.gz` and `.sha256` | Yes (`:19`) | As above |
| Scoop, winget | `scrutate-0.1.0-{x86_64,aarch64}-pc-windows-msvc.zip`, folder of the same name inside | Yes (`:25-26,49`; `7z a dist/$name.zip $name` keeps the folder) | No |
| Scoop autoupdate | `SHA256SUMS` | Yes (`:109`) | Yes, different contents |
| Archive layout | top folder `scrutate-<v>-<target>/` with the binary | Yes (`:45-46`) | Yes, plus `CHANGELOG.md` and `integrations/` |

The URL form `https://github.com/<owner>/<repo>/releases/download/<tag>/<file>` matches the GitHub form for a tag; the docs page I read shows only the `releases/latest/download/` form ([linking to releases](https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases)).

Test the templates on a Mac with a local server standing in for GitHub (needs a Linux musl archive, for example from `tools/linux_x86_cross_verify.sh` output or a CI artifact, and Docker through colima):

```sh
D=$(mktemp -d); mkdir -p "$D/crisnahine/scrutate/releases/download/v0.1.0"
cp scrutate-0.1.0-x86_64-unknown-linux-musl.tar.gz "$D/crisnahine/scrutate/releases/download/v0.1.0/"
(cd "$D/crisnahine/scrutate/releases/download/v0.1.0" && shasum -a 256 *.tar.gz > scrutate-0.1.0-x86_64-unknown-linux-musl.sha256)
python3 -m http.server 8765 --directory "$D" &
sed 's#https://github.com#http://host.docker.internal:8765#' integrations/gitlab/.gitlab-ci.yml > "$D/gl.yml"
docker run --rm --platform linux/amd64 -v "$PWD":/src -w /src debian:bookworm-slim sh -c '<before_script and script lines from $D/gl.yml>'
```

GitLab's own pipeline editor lint and a real merge request pipeline need a GitLab project, and the `include:remote` URL needs the public GitHub file, so those wait for publishing. The same holds for Bitbucket.

## 6. GitHub composite action

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Metadata | `name`, `description`, `runs` required; each input needs `description`; composite needs `runs.using: composite`; `shell` is "Required if `run` is set"; composite actions get no `INPUT_*` variables and use the `inputs` context ([metadata syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/metadata-syntax)). | `action.yml:1-31` | Correct. |
| Marketplace | "Each repository must contain a single action metadata file (`action.yml` or `action.yaml`) at the root"; actions in sub-folders "will not be automatically listed in the marketplace"; the repository must be public ([publish in Marketplace](https://docs.github.com/en/actions/how-tos/create-and-publish-actions/publish-in-github-marketplace)). | `.github/actions/scrutate/action.yml` | Cannot be listed. A Marketplace listing needs a separate repository (for example `crisnahine/scrutate-action`) with `action.yml` at its root and `branding`. |
| Reference | `{owner}/{repo}/{path}@{ref}` is "A subdirectory in a public GitHub repository at a specific branch, ref, or SHA" ([workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)). | Not documented anywhere in the repository | Works as `uses: crisnahine/scrutate/.github/actions/scrutate@v0.1.0` with `with: { version: v0.1.0, args: "dupes --json" }`. Add that to a README. |
| Shell per OS | `bash` on Windows is the one from Git for Windows; the Windows default is `pwsh` ([workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)). | `shell: bash`; `case` has no Windows branch, `:17-23` | Runs on Linux and macOS. Windows runners stop with "unsupported runner", though Windows zips are released. Add `MINGW*|MSYS*` branches that fetch the `.zip` and unzip it. |
| Inputs in `run` | Expressions in `run:` "are evaluated and then substituted" before the shell runs, which "can make it vulnerable to shell command injection"; composite authors should "consider whether your code might execute untrusted input" ([script injections](https://docs.github.com/en/actions/concepts/security/script-injections)). | `${{ inputs.version }}`, `${{ inputs.args }}` inline, `:16,31` | Works. The values come from the workflow author, so the risk is low. Passing them through `env:` follows the docs' advice. |
| `sha256sum` on macOS | Not listed in the macOS 15 arm64 runner image readme (image 20261104.0514.1, [runner-images](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-arm64-Readme.md)). | Used at `:27`, and by `release.yml:50` on both macOS jobs | Cannot be verified from documentation alone. This Mac (macOS 27.2.1) has `/sbin/sha256sum`. If a runner lacks it, both the action on macOS and the macOS release jobs fail. `shasum -a 256 -c` is a safe swap. |

Test: the action cannot run on GitHub before the repository exists. Its shell body can be run by hand against the local server from section 5, with `GITHUB_PATH=$(mktemp)` set and the URL rewritten. Third-party runners such as `act` are not covered by GitHub's docs.

## 7. MCP client configuration

Project sources: `integrations/mcp/README.md:25-33`. `docs/CLI.md` section `serve` (lines 152 to 188) gives no client snippet.

| Client | What its source says | Project | Verdict |
|---|---|---|---|
| Claude Code | `claude mcp add [options] <name> -- <command> [args...]`; `--` separates Claude's options from the server command; default scope `local`; project scope writes `.mcp.json` with `mcpServers`, and an entry with no `type` is stdio ([Claude Code MCP](https://code.claude.com/docs/en/mcp), no page date). | `claude mcp add scrutate -- scrutate --root /path/to/repo serve`, `:27` | Correct. |
| Codex CLI | `~/.codex/config.toml`, or `.codex/config.toml` in trusted projects; table `[mcp_servers.<server-name>]` with `command` (required), `args`, `env`, `cwd`, `startup_timeout_sec` (default 10), `tool_timeout_sec` (default 60); `codex mcp add <name> -- <command>` ([Codex MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli), redirected from developers.openai.com/codex/mcp, no date). | Covered only by the "generic" JSON | **Wrong**: Codex does not read JSON. |
| Cursor | `.cursor/mcp.json` or `~/.cursor/mcp.json`, key `mcpServers`; stdio fields `type` (marked required, `"stdio"`), `command`, `args`, `env`, `envFile`; `${workspaceFolder}` is resolved ([Cursor MCP](https://cursor.com/docs/context/mcp), no date). | Generic JSON without `type` | **Wrong** by the docs: `type` is listed as required. |
| VS Code | `.vscode/mcp.json` or the user `mcp.json`; "It defines servers in a top-level `servers` object"; stdio needs `type`; predefined variables work; `code --add-mcp '{"name":...,"type":"stdio","command":...,"args":[...]}'` ([MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration), 11/4/2026). A portable `.mcp.json` with `mcpServers` is also read by Copilot sessions ([MCP servers](https://code.visualstudio.com/docs/copilot/customization/mcp-servers), 11/4/2026). | Generic `mcpServers` JSON | **Wrong** for `.vscode/mcp.json`. |
| Gemini CLI | `~/.gemini/settings.json` or `.gemini/settings.json`, key `mcpServers`, fields `command`, `args`, `env`, `cwd`, `timeout` (ms, default 10 minutes), `trust`; `gemini mcp add [options] <name> <commandOrUrl> [args...]`, default scope project ([Gemini CLI MCP](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md), no date). | Generic JSON | Correct shape. Not named in the README. |
| Zed | `settings.json`, key `context_servers`, entries `{ "command": "...", "args": [...], "env": {} }` ([Zed MCP](https://zed.dev/docs/ai/mcp), mentions Zed v0.224.0). | Generic JSON | **Wrong**: Zed does not read `mcpServers`. |

Correct snippets (replace `/path/to/repo`; VS Code and Cursor can use the workspace variable instead):

Claude Code (shell, or `.mcp.json` in the repository):
```sh
claude mcp add scrutate -- scrutate --root /path/to/repo serve
```
```json
{ "mcpServers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"] } } }
```

Codex CLI (`~/.codex/config.toml`):
```toml
[mcp_servers.scrutate]
command = "scrutate"
args = ["--root", "/path/to/repo", "serve"]
```
or `codex mcp add scrutate -- scrutate --root /path/to/repo serve`.

Cursor (`.cursor/mcp.json`):
```json
{ "mcpServers": { "scrutate": { "type": "stdio", "command": "scrutate", "args": ["--root", "${workspaceFolder}", "serve"] } } }
```

VS Code (`.vscode/mcp.json`):
```json
{ "servers": { "scrutate": { "type": "stdio", "command": "scrutate", "args": ["--root", "${workspaceFolder}", "serve"] } } }
```
or `code --add-mcp '{"name":"scrutate","type":"stdio","command":"scrutate","args":["--root","/path/to/repo","serve"]}'`.

Gemini CLI (`.gemini/settings.json`):
```json
{ "mcpServers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"] } } }
```

Zed (`settings.json`):
```json
{ "context_servers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"], "env": {} } } }
```

Codex's default `tool_timeout_sec` of 60 is above the 20 second wait scrutate uses while the first index builds (`docs/CLI.md`, `serve`), so a first call answers "still being built" and does not time out.

Test without touching a user profile:

```sh
T=$(mktemp -d); cd "$T"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | scrutate --root ~/Documents/Projects/scrutate serve
claude mcp add --scope project scrutate -- scrutate --root ~/Documents/Projects/scrutate serve   # writes $T/.mcp.json only
claude mcp get scrutate
code --user-data-dir "$T/data" --extensions-dir "$T/exts" --add-mcp '{"name":"scrutate","type":"stdio","command":"scrutate","args":["serve"]}'
python3 -I -c 'import json,sys; json.load(open(sys.argv[1]))' "$T/.mcp.json"
```

Whether each client actually starts the server from its file needs the client running; Cursor and Zed have no headless check in their docs.

## 8. Install paths

### 8.1 Homebrew

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Cargo build | Use the helpers directly: `system "cargo", "install", *std_cargo_args` ([Formula Cookbook](https://docs.brew.sh/Formula-Cookbook), no date). `std_cargo_args(root: prefix, path: ".", features: nil)` ([Formula API](https://docs.brew.sh/rubydoc/Formula.html)). | `std_cargo_args(path: "crates/scrutate")`, `scrutate.rb:13`; `depends_on "rust" => :build`, `:10` | Correct. |
| Completions | `generate_completions_from_executable` with `shell_parameter_format` nil "Defaults to plainly passing the shell": `foo completions bash`, `zsh`, `fish` ([Formula API](https://docs.brew.sh/rubydoc/Formula.html)). | `generate_completions_from_executable(bin/"scrutate", "completions")`, `:14` | Correct. |
| `head` | Git URLs "must always specify `branch:`" ([Cookbook](https://docs.brew.sh/Formula-Cookbook)). | `branch: "main"`, `:8` | Correct. |
| `license` | Required in homebrew-core, SPDX id ([Cookbook](https://docs.brew.sh/Formula-Cookbook)). | `"Apache-2.0"`, `:7` | Correct. |
| `test do` | No user input, tests basic function, runs in a temp dir ([Cookbook](https://docs.brew.sh/Formula-Cookbook)). | version check and a `grep`, `:17-21` | Correct. |
| Source archive | Install steps cannot use "an unversioned, unchecksummed archive"; upstream needs an immutable tag or release ([Acceptable Formulae](https://docs.brew.sh/Acceptable-Formulae)). | Tag archive `v0.1.0.tar.gz`, zero `sha256` placeholder, `:4-6` | Correct once the hash is filled. `models/*.npz` and `vendor/ignore` are tracked in git, so the tag archive has what `include_bytes!` and `[patch]` need. |
| Tap | Name the repo `homebrew-<name>`; formulae in `Formula/`; `brew install user/repository/formula`; `brew tap-new` makes a local tap ([taps](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap)). | `packaging/README.md:56-58`: `crisnahine/homebrew-tap` | Correct. |
| homebrew-core | "A code repository less than 30 days old is normally not eligible"; self-submission needs "at least 90 forks, 90 watchers or 225 stars" ([Package Acceptance Policy](https://docs.brew.sh/Package-Acceptance-Policy)). | `packaging/README.md:57-58` offers homebrew-core | **Wrong** for 0.1.0. Say tap only. |

Test (macOS is blocked here by the Xcode version, per `packaging/README.md:23-25`; the Linux container run already passed):

```sh
git archive --prefix=scrutate-0.1.0/ -o /tmp/scrutate-0.1.0.tar.gz HEAD
docker run --rm -v /tmp:/host homebrew/brew sh -c 'brew tap-new --no-git local/test && \
  sed -e "s#https://github.com/crisnahine/scrutate/archive/refs/tags/v0.1.0.tar.gz#file:///host/scrutate-0.1.0.tar.gz#" \
      -e "s#0\{64\}#$(sha256sum /host/scrutate-0.1.0.tar.gz | cut -d" " -f1)#" /host/scrutate.rb > $(brew --repo local/test)/Formula/scrutate.rb && \
  brew install --build-from-source local/test/scrutate && brew test local/test/scrutate && brew audit --strict local/test/scrutate'
```

(Copy `packaging/homebrew/scrutate.rb` to `/tmp/scrutate.rb` first.) `brew audit --online` and a real tap install need the public tag.

### 8.2 Scoop

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Required fields | `version`, `description`, `homepage`, `license` ([App Manifests](https://github.com/ScoopInstaller/Scoop/wiki/App-Manifests)). | All four, `scrutate.json:2-5` | Correct. |
| Architectures | `32bit`, `64bit`, `arm64`, each with `url`, `hash`, `extract_dir` and others; hashes are SHA256 by default ([App Manifests](https://github.com/ScoopInstaller/Scoop/wiki/App-Manifests)). | `64bit`, `arm64`, `:6-17` | Correct. |
| `bin` | String or array of executables ([App Manifests](https://github.com/ScoopInstaller/Scoop/wiki/App-Manifests)). | `"scrutate.exe"`, `:18` | Correct. |
| `checkver: github` | Uses the homepage as the repository; tag matched by `\/releases\/tag\/(?:v|V)?([\d.]+)`, so a leading `v` is dropped ([Autoupdate](https://github.com/ScoopInstaller/Scoop/wiki/App-Manifest-Autoupdate)). | `:19`, homepage is the GitHub repo | Correct. |
| `hash.url` | Default regex finds `<hash> <space> ...$basename` in a checksum file; `$baseurl` is the URL without the file name ([Autoupdate](https://github.com/ScoopInstaller/Scoop/wiki/App-Manifest-Autoupdate)). | `$baseurl/SHA256SUMS`, `:31-33` | Correct: `SHA256SUMS` lines are `sha256sum` output. |

Test: install cannot be tested on a Mac. `packaging/validate_manifests.py` checks the schema. A real install needs Windows; Scoop on Windows can install from a local manifest path once the zips exist somewhere it can download.

### 8.3 winget

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Layout | First folder is the lowercase first letter of the PackageIdentifier, then each identifier segment (case kept), then the version ([Authoring](https://github.com/microsoft/winget-pkgs/blob/master/doc/Authoring.md)). | `manifests/c/crisnahine/Scrutate/0.1.0/` | Correct. |
| Files | At least version, defaultLocale and installer files ([Create your package manifest](https://learn.microsoft.com/en-us/windows/package-manager/package/manifest), ms.date 2026-09-14). | The three files | Correct. |
| Schema version | Learn's examples use `ManifestVersion: 1.12.0`. winget-pkgs has schema folders up to 1.28.0 and points to 1.28.0 ([schema folder](https://github.com/microsoft/winget-pkgs/tree/master/doc/manifest/schema), [Authoring](https://github.com/microsoft/winget-pkgs/blob/master/doc/Authoring.md)). | `1.9.0` in all three files | Old. Whether winget-pkgs still accepts it is not stated. Regenerate with `wingetcreate` at release time. |
| Portable zip | `NestedInstallerType` and `NestedInstallerFiles` are not described on the pages I read. | `InstallerType: zip`, `NestedInstallerType: portable`, `RelativeFilePath` with the folder, `PortableCommandAlias` | Cannot be verified from the fetched docs; the file passes the 1.9.0 JSON schema (`packaging/README.md:34-36`). |

Test: `winget validate` runs only on Windows. Nothing installs before the zips are public, except from a Windows machine with `winget install --manifest <dir>` and the `InstallerUrl` pointed at a local server.

### 8.4 Debian and RPM

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Debian control | Mandatory: Package, Version, Architecture, Maintainer, Description; Maintainer is "name ... then the email address inside angle brackets" ([Debian Policy 4.7.4.1, control fields](https://www.debian.org/doc/debian-policy/ch-controlfields.html)). | Generated by cargo-deb; `maintainer` at `crates/scrutate/Cargo.toml:15` | Correct format. |
| Debian copyright | Every package must have `/usr/share/doc/PACKAGE/copyright`, not compressed and not a symlink; for Apache-2.0 it should point to `/usr/share/common-licenses` ([Policy 4.7.4.1, docs](https://www.debian.org/doc/debian-policy/ch-docs.html)). | Assets ship `LICENSE` and `README.md` to `/usr/share/doc/scrutate/`, `Cargo.toml:18-22` | Cannot be verified from Debian docs alone: cargo-deb writes a `copyright` file, which `dpkg-deb -c` shows. A separate LICENSE there is allowed, not needed. |
| Binary path | `/usr/bin` | `usr/bin/`, `Cargo.toml:19`; `/usr/bin/scrutate`, `:27` | Correct. |
| RPM preamble | Name, Version, Release, Summary, License are required; SPDX ids "recommended"; `%license` marks license files ([rpm-spec(5), RPM 6.1.90, 2026-10-19](https://rpm-software-management.github.io/rpm/man/rpm-spec.5)). | `summary`, licence from the workspace `Apache-2.0`; LICENSE to `/usr/share/licenses/scrutate/` with `doc = true`, `Cargo.toml:24-30` | Correct fields. The LICENSE is marked as doc; RPM has a license marker for it. Low impact. |
| In the release | n/a | Not built by `release.yml` (`packaging/README.md:64-66`) | A release has no `.deb` or `.rpm` today. |

Test (already done for arm64; x86_64 not built):

```sh
docker run --rm -v "$PWD":/src -w /src rust:1-slim-bookworm sh packaging/linux/build-packages.sh
docker run --rm -v "$PWD/dist":/d debian:bookworm-slim sh -c 'dpkg-deb -I /d/*.deb; dpkg-deb -c /d/*.deb | grep -E "usr/bin|copyright"'
docker run --rm -v "$PWD/dist":/d fedora sh -c 'rpm -qip /d/*.rpm | grep -E "^(Name|Version|Release|License|Summary)"; rpm -qlp /d/*.rpm'
```

This build is heavy (a full release compile); run it when benchmarks are not running.

### 8.5 pip install of the wheels

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| manylinux | `manylinux_x_y_arch` works on any distro with glibc x.y or newer ([platform compatibility tags](https://packaging.python.org/en/latest/specifications/platform-compatibility-tags/), latest history entry March 2024). | `manylinux: 2_34` for Linux, `release.yml:83` | Correct: glibc 2.34 or newer. Distros with older glibc get no wheel. |
| macOS | `macosx_x_y_arch`; for macOS 11 and later the tag is `macosx_x_0_arch` ([tags spec](https://packaging.python.org/en/latest/specifications/platform-compatibility-tags/)). | `MACOSX_DEPLOYMENT_TARGET=13.0`, `release.yml:62`, so `macosx_13_0_arm64` | Correct. |
| abi3 | "The CPython stable ABI is `abi3`" ([tags spec](https://packaging.python.org/en/latest/specifications/platform-compatibility-tags/)). | `cp39-abi3` | Correct. |

Test without a system install: `python3 -m venv "$T/v" && "$T/v/bin/pip" debug --verbose | grep -E 'cp39-abi3-macosx_13_0_arm64|abi3-macosx'` shows whether this pip accepts the tag; then `"$T/v/bin/pip" install --no-deps dist/scrutate-*.whl`. For Linux: `docker run --rm python:3.12-slim pip debug --verbose | grep manylinux_2_34_aarch64`.

### 8.6 cargo install

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Size | "crates.io currently has a 10MB size limit on the `.crate` file" ([Publishing on crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html)). | `docs/INSTALL.md:84-85`, `packaging/README.md:46-51`: `semantic.npz` 14.6 MB, 13.3 MB after gzip | Correct. Measured today: 13,314,842 bytes with `gzip -9`. |
| Files outside the crate | Only files in the package are packed; check with `cargo package --list` ([Publishing](https://doc.rust-lang.org/cargo/reference/publishing.html)). | `include_bytes!` of `../../models/*.npz`, `crates/scrutate/src/lib.rs:40-41` | Correct, as `packaging/README.md:47-49` says. |
| `[patch]` | "Cargo only looks at the patch settings in the `Cargo.toml` manifest at the root of the workspace" ([Overriding dependencies](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html)). | `[patch.crates-io] ignore = { path = "vendor/ignore" }`, `Cargo.toml:25-26`; `wb.grow_threads(...)`, `instant.rs:2005` | **Incomplete.** A published crate would resolve `ignore` from crates.io without the four changes. Whether upstream `ignore` has `grow_threads` was not checked; the workspace comment says it is one of the vendored changes. Add this to both docs. |

Test (heavy, run outside benchmark windows): `cargo package -p scrutate --list`, then read `target/package/scrutate-0.1.0/Cargo.toml` for a `[patch]` section, then `cargo publish -p scrutate --dry-run`.

## 9. Python bindings

| Item | What the source says | Project | Verdict |
|---|---|---|---|
| Free-threaded | "The free-threaded build of CPython cannot load `abi3` wheels." With only `abi3`, PyO3 builds a version-specific extension on a non-GIL interpreter. PEP 803 adds `abi3t` for free-threaded 3.15 and later; `abi3` plus `abi3t` gives `abi3` for 3.14 and older and `abi3t` for 3.15 and newer; maturin 1.14 or later builds `abi3t` wheels ([PyO3 0.30.1 building and distribution](https://pyo3.rs/latest/building-and-distribution.html)). maturin 1.14.0: "Support pyo3 abi3t features on Python3.15 and PyO3 0.29" ([maturin changelog](https://www.maturin.rs/changelog.html), latest 1.16.0). Python 3.15.0 final was scheduled for 2026-10-09 and later bugfix releases are listed ([PEP 790](https://peps.python.org/pep-0790/)). | `abi3-py39`, `crates/scrutate-py/Cargo.toml:17`; one `cp314t` build, `release.yml:72,82`; `docs/INSTALL.md:59-60` | **Gap.** Free-threaded 3.15 users get no wheel. Add the `abi3t` feature and a 3.15t build. |
| `extension-module` | "Deprecated, users should remove this feature and upgrade to `maturin >= 1.9.4` or `setuptools-rust >= 1.12`" ([PyO3 features](https://pyo3.rs/latest/features.html)). | `features = ["pyo3/extension-module"]`, `pyproject.toml:20`; `requires = ["maturin>=1.5,<2"]`, `:2` | **Wrong (deprecated).** Remove the feature and raise the floor to `maturin>=1.9.4` (1.14 for `abi3t`). |
| `license` | The table subkeys "were deprecated by PEP 639 in favor of the string value" ([pyproject.toml spec](https://packaging.python.org/en/latest/specifications/pyproject-toml/)). maturin has full PEP 639 support since 1.9.0 ([changelog](https://www.maturin.rs/changelog.html)). | `license = { text = "Apache-2.0" }`, `pyproject.toml:10` | Deprecated form. Use `license = "Apache-2.0"`. |

The `README.md:86-94` usage matches `crates/scrutate-py/README.md`; no documentation contradicts it.

Test: `T=$(mktemp -d); python3 -m venv "$T/v"; "$T/v/bin/pip" install maturin; "$T/v/bin/maturin" build --release -m crates/scrutate-py/Cargo.toml -o "$T/dist"` prints a warning while `extension-module` is set (a full release build, so not during benchmarks). The 3.15t path needs a free-threaded 3.15 interpreter, which is not on this machine.

## What can only be tested after publishing

- Anything fetched from `github.com/crisnahine/scrutate`: the release URLs in GitLab, Bitbucket, the action, Scoop, winget and INSTALL; the Homebrew tag archive and `brew audit --online`; GitLab `include:remote`; `pre-commit` with `repo:` and a `rev`; `gh attestation verify`.
- Marketplace and registry steps: `vsce publish` (needs the publisher id), the winget-pkgs PR checks, a Scoop bucket, a Homebrew tap clone.
- Hosted CI: a real GitLab merge request pipeline, a Bitbucket pipeline, the action on GitHub runners, and `release.yml` itself (macOS `sha256sum`, Windows Git Bash `sha256sum`, `7z`).
- Windows installs (Scoop, winget, the action on Windows) need a Windows machine, published or not.

## Not verified

- **vscode-languageclient on stdio:** that a `command` server option without `transport` uses stdio. The VS Code guide only shows the IPC form, and I did not read the client's `node/main.ts`.
- **VS Code Marketplace publisher** `scrutate`: whether it exists or is free.
- **Neovim:** "Since" tags for `vim.json.decode`, `vim.fn.systemlist` with a list, `vim.tbl_extend`, `vim.list_extend`, `vim.notify`; whether `workspace_required` defaults to true in 0.12 (the docs say attachment depends on a root unless `workspace_required=false`); the macOS release asset name `nvim-macos-arm64.tar.gz`. The neovim.io pages are built from the development branch, not 0.12.6.
- **LSP4IJ:** the settings path *Languages & Frameworks → Language Servers* named in the README.
- **JetBrains:** where External Tools XML files live in the IDE config directory, and whether an IDE picks up a copied file; `idea.properties` paths for a throwaway config.
- **pre-commit:** whether `language: unsupported` is allowed in a remote hook repository (the page talks about it only for local hooks), and how `language: rust` handles a workspace member.
- **Bitbucket:** the configuration reference page returned only its title, so the full key list and variable persistence between script lines are not quoted. The YAML sharing details come from search snippets, not a read page.
- **GitHub:** the `releases/download/<tag>/<file>` URL form (only `releases/latest/download/` is on the linking page); whether `sha256sum` exists on `macos-latest`, `macos-15-intel` and in Git Bash on Windows runners.
- **Claude Code:** a variable for the project directory in `.mcp.json` (only `${CLAUDE_PLUGIN_ROOT}` for plugins is shown), and the page date.
- **MCP clients not in the brief:** Windsurf and JetBrains AI Assistant, which the README names next to the generic snippet.
- **winget:** whether winget-pkgs still accepts manifest schema 1.9.0, and the `NestedInstallerType: portable` fields, which the fetched pages do not describe.
- **cargo-deb and cargo-generate-rpm:** how they fill the Debian `copyright` file and the RPM `License` and `Release` tags (their docs are outside the source list).
- **crates.io:** how to ask for a larger size limit; whether upstream `ignore` has `WalkBuilder::grow_threads`; that the packaged manifest drops the workspace `[patch]`.
- **pip:** the first pip version that accepts `manylinux_2_34` tags.
- **Python 3.15:** PEP 790 as fetched lists 3.15.2 released 2026-10-26, which is unusually soon after 3.15.0; I took only "3.15 is released" from it.
