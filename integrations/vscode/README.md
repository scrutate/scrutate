<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="../../docs/assets/banner-vscode.svg" alt="VS Code. How do I use the language server in VS Code?" width="880">
</p>

# Scrutate for VS Code

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [MCP server](../mcp/README.md) &nbsp;·&nbsp; Next: [Neovim](../neovim/README.md)

<!-- End of head. -->

The extension starts `scrutate lsp` as a language server for the open folder, so **Go to
Definition**, **Find All References**, **Go to Symbol in Workspace** and the **Outline** view work
in every language Scrutate indexes. The binary must be on `PATH`, or set `scrutate.path` in your
user settings (a machine setting: a repository's own settings cannot name the program to run).
The first start builds the index (`.scrutate/`); the server's log is in *Output → Scrutate*. On a
folder below the top of a repository, the server and the commands use the index at the top (the
directory `scrutate config --json` names), so results outside the folder open too and **Grep**
searches the whole repository. With several folders open, the server answers each one from its
own repository's index; the extension's own commands use the first folder.

Commands (Command Palette): **Scrutate: Search**, **Grep**, **Go to symbol definition**,
**Who calls this?**, **References**, **Update index**. Results open in a Quick Pick that jumps to
the location; **Update index** shows a message with what the update did.

Build: `npm install` here (`npm run check` type-checks `extension.js`), then
`npx @vscode/vsce package --no-rewrite-relative-links` and *Extensions → Install from VSIX*. vsce
refuses the SVG banner at the top of this README once it rewrites relative links, hence the flag.

Test: `SCRUTATE_BIN=/path/to/scrutate npm test` starts a real VS Code with the extension on a
small repository (`test/run.js`, with `@vscode/test-electron`) and checks the definition,
references, workspace symbol and outline providers and every command (`test/suite.js`), once on
the repository and once on its `src` folder; `test/root.test.js` first checks the root lookup in
plain Node. Without a display: `xvfb-run -a npm test`. It last passed in VS Code 1.145.3 on Linux
arm64 (2026-11-11), before the `src` folder run was added. `VSCODE_VERSION=1.85.0` picks the VS
Code to download (default `stable`). `SCRUTATE_VSIX=/path/to/scrutate.vsix node test/run.js`
installs the package with `--install-extension` and runs the same checks on the installed copy,
with the suite copied into it (VS Code gives a file the API object of the extension whose folder
holds it, so the suite's stubs reach the extension only from there); the suite asserts which copy
ran. The `vscode` job in `.github/workflows/ci.yml` runs both passes,
and checks the package's file list, on Linux, macOS and Windows with VS Code 1.85.0 (the
`engines` floor) and stable. The server is also tested end to end in
`crates/scrutate/tests/lsp.rs`.
