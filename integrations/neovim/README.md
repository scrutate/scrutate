<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="../../docs/assets/banner-neovim.svg" alt="Neovim. How do I use scrutate from Neovim?" width="880">
</p>

# Scrutate for Neovim

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [VS Code](../vscode/README.md) &nbsp;·&nbsp; Next: [JetBrains IDEs](../jetbrains/README.md)

<!-- End of head. -->

```lua
-- lazy.nvim
{ dir = "/path/to/scrutate/integrations/neovim", config = function() require("scrutate").setup() end }
```

Needs Neovim 0.10 or newer (the plugin runs the CLI through `vim.system`). On Windows the plugin
looks `scrutate` up on PATH itself, in absolute entries only, never in the opened repository;
`setup({ bin = [[C:\path\to\scrutate.exe]] })` names it outright.

Commands: `:Scrutate <query>` (ranked search), `:ScrutateGrep <regex>`, `:ScrutateSymbol [name]`,
`:ScrutateCallers [name]` (default: word under cursor), with results in the quickfix list. Each
runs at the top of the repository holding Neovim's current directory, whose index scrutate uses
(`scrutate config --json` names it), so from a subdirectory every result opens and
`:ScrutateGrep` searches the whole repository.

The language server (definitions, references, symbols) needs Neovim 0.11 or newer. The plugin
ships `lsp/scrutate.lua`, so with the plugin loaded one line turns it on:

```lua
vim.lsp.enable("scrutate")
```

Without the plugin, or to name another binary:

```lua
vim.lsp.config("scrutate", { cmd = { "/path/to/scrutate", "lsp" }, root_markers = { ".scrutate", ".git" } })
vim.lsp.enable("scrutate")
```

Test: in a repository made by `integrations/fixture.sh`,
`nvim --headless -u NONE -l integrations/neovim/test/run.lua /path/to/scrutate` runs
`:ScrutateSymbol` before any index, every command once the index is built, `:Scrutate` after an
edit, a go-to-definition through the language server, and the commands again from a subdirectory (the test makes a `.git` directory at the top). It last
passed on Neovim 0.12.6 (2026-11-11). The `neovim` job in `.github/workflows/ci.yml` runs it on
Linux, macOS and Windows with Neovim 0.10.4, 0.11.7 and 0.12.6; on 0.10 it skips the language
server check, since `vim.lsp.config` is new in 0.11.
