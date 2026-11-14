<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="../../docs/assets/banner-jetbrains.svg" alt="JetBrains IDEs. How do I run scrutate from a JetBrains IDE?" width="880">
</p>

# Scrutate in JetBrains IDEs

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [Neovim](../neovim/README.md) &nbsp;·&nbsp; Next: [GitHub Actions](../github/README.md)

<!-- End of head. -->

0.1.0 has no dedicated plugin (IntelliJ Platform SDK, Gradle build). It ships an **External
Tools** definition, which every JetBrains IDE supports without a plugin:

1. Under *Settings → Tools → External Tools*, add the four tools below (the IDE has no import
   button there). The same definitions are in `External Tools.xml` next to this README: copied
   into the `tools` folder of the IDE's configuration directory, they are read at the IDE's next
   start (not tried in an IDE).
2. Assign keyboard shortcuts under *Settings → Keymap → External Tools*.

| Tool | Program | Arguments | Working directory |
|---|---|---|---|
| Scrutate: search | `scrutate` | `search -k 15 -- "$Prompt$"` | `$ProjectFileDir$` |
| Scrutate: grep | `scrutate` | `grep --sort -- "$Prompt$" "$ProjectFileDir$"` | `$ProjectFileDir$` |
| Scrutate: symbol | `scrutate` | `symbol -- "$SelectedText$"` | `$ProjectFileDir$` |
| Scrutate: callers | `scrutate` | `callers -- "$SelectedText$"` | `$ProjectFileDir$` |

The `--` keeps a query that starts with `-` from being read as an option. Output appears in the
Run tool window. The IDE links `path:line` only for an absolute path, so the grep tool is given
the project directory and its results are links. The other three answer from the index with
repository-relative paths, which the IDE does not link; the language server below navigates
those.

On Windows, put the binary's full path in **Program** (`C:\Users\me\.cargo\bin\scrutate.exe`)
and in the server command below. When the IDE does not find `scrutate` on its `PATH`, Windows
also looks in the IDE's own current folder, so an IDE started from inside a project
(`idea64.exe .`) would run a `scrutate.exe` that the repository holds.

## Language server through LSP4IJ

The [LSP4IJ](https://plugins.jetbrains.com/plugin/23257-lsp4ij) plugin (Red Hat, free) connects
any JetBrains IDE to a language server, so `scrutate lsp` gives Go to Declaration, Find Usages,
Go to Symbol and the Structure view from the Scrutate index.

1. Install LSP4IJ from *Settings → Plugins → Marketplace* and restart the IDE.
2. Open *Settings → Languages & Frameworks → Language Servers* and click **+**.
3. On the **Server** tab: name `Scrutate`, command `scrutate lsp` (use the full path to the
   binary if it is not on the IDE's `PATH`, and always on Windows, as above).
4. On the **Mappings** tab, under **File name patterns**, add the files to answer for, such as
   `*.py`, `*.rs`, `*.go`, `*.ts`, each with a language id (`python`, `rust`, ...). The server
   ignores the id; any value works.
5. Click **OK** and open a project file. The server takes the project directory from the IDE
   (`rootUri`); the first start builds `.scrutate/` and says so in the *Language Servers* tool
   window log.

The server answers definitions, references, workspace symbols and document symbols. It offers no
completion, hover or diagnostics, so the IDE keeps its own. Saving a file updates the index in the
background.

### The automated run

The `jetbrains` workflow (`.github/workflows/jetbrains.yml`) runs the test in `test/` on every
change under `integrations/jetbrains/` or to the language server (`crates/scrutate/src/lsp.rs`), and by
hand. It starts IntelliJ IDEA 2026.2.3 headless,
as the IntelliJ Platform's tests do, with LSP4IJ 0.21.0 from the Marketplace, and:

1. opens the fixture repository below as a project whose only content root it is, so LSP4IJ
   sends it as `rootUri`;
2. registers the server with the values steps 3 and 4 save (name `Scrutate`, command
   `<binary> lsp`, the mapping `*.py` / `python`), through the call the dialog's **OK** makes;
3. opens `src/app.py`, puts the caret on `make_point` in line 5 and asks for the targets of
   *Navigate → Declaration or Usages*. It passes when the one target comes from the language
   server and is `src/geo.py`, line 6.

The log names the IDE build and the LSP4IJ version. The test does not click through the Settings
dialog, since the platform's tests run without its Swing UI. To run it yourself, with Java 21 and
Gradle 9 (the first run downloads the IDE into Gradle's cache):

```sh
bash integrations/fixture.sh /tmp/scrutate-fixture
cd integrations/jetbrains/test
SCRUTATE_BIN=/path/to/scrutate SCRUTATE_FIXTURE=/tmp/scrutate-fixture gradle test
```

Keep the fixture outside any Git repository: the server takes the top of the repository around
it as the root.

### Recording a run by hand

For another IDE build, or when the workflow cannot run. On a named IDE build with LSP4IJ 0.21.0:

1. Make the test repository: `bash integrations/fixture.sh /tmp/scrutate-fixture` and
   `mkdir /tmp/scrutate-fixture/.git`, then open that folder as a project.
2. Follow steps 2 to 5 above exactly, with the mapping `*.py` / `python`.
3. In the *Language Servers* tool window, set the server's trace to *verbose*.
4. In `src/app.py`, put the caret on `make_point` in `return make_point().norm()` (line 5) and
   run *Navigate → Declaration or Usages*. The editor jumps to `src/geo.py`, line 6.
5. Save three things under `docs/evidence/`: the IDE and plugin versions (*Help → About*, copied as
   text), the LSP console text with `initialize`, `textDocument/definition` and its response, and
   a screen recording of the jump.

## MCP

The MCP server (`scrutate serve`) also works with the MCP settings of JetBrains AI Assistant and
Junie; see ../mcp/README.md.
