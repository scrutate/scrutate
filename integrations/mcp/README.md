<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="../../docs/assets/banner-mcp.svg" alt="MCP server. How do I give an agent scrutate's tools?" width="880">
</p>

# Scrutate as an MCP server

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [Command line](../../docs/CLI.md) &nbsp;·&nbsp; Next: [VS Code](../vscode/README.md)

<!-- End of head. -->

`scrutate serve` speaks the Model Context Protocol over stdio (JSON-RPC 2.0): the handshake
revisions 2024-11-05 / 2025-03-26 / 2025-06-18 / 2025-11-25, and revision 2026-07-28, which has
no handshake (`server/discover`, a protocol version in every request's `_meta`). Tools: `search_code`, `grep_code`,
`find_symbol`, `callers`, `callees`, `call_path`, `repo_map`, `read_code`, `outline`, `dependencies`, `dependents`,
`references`, `similar_code`, `duplicates`, `structural_search`, `impact`, `find_files`, `dead_code`, `reindex`.
`serve --allow-edits` lists one more, `edit_symbol`, the one tool that writes source files
([edits](../../docs/CLI.md#serve---allow-edits-and-edit_symbol)).

The index refreshes itself before a call (scans at most a second apart by default, or
`--refresh-interval S`). A call without a `progressToken` waits up to 20 seconds for a first
index build; one with a token waits for the build and reports progress. Every tool
except `reindex` and `edit_symbol` is marked read-only and local, so clients that ask before a
write do not ask. Answers are cut at whole items to 24,000 bytes by default (`max_bytes`,
`offset`, or `serve --max-bytes` for another default), and every tool but `edit_symbol` takes
`response_format: "concise"`.
`read_code path=...` reads lines and `read_code symbol=...` a whole definition; `grep_code`
searches the files the index holds (not `.git/` or `node_modules/`); `reindex` with `force: true`
reads every file again. Two switches matter to a client:

- `serve --short-tools` (`SCRUTATE_MCP_SHORT_TOOLS=1`): tool descriptions of a few words, for a
  client that sends the tool list to its model every session.
- `serve --structured`: `structuredContent` and an `outputSchema` on every tool; a large answer
  pages there too, with the items of the text page and `next_offset`. Leave it off for Claude
  Code, which would show its model the JSON in place of the text with the snippets; turn it on
  for a client whose scripts read tool results (Codex in Code Mode).

A call that sends a `progressToken` hears the files read so far against the total while the
index is built, `reindex` runs or `structural_search` reads a large tree.

Every tool, argument, limit and protocol detail is in the
[CLI reference](../../docs/CLI.md#serve-mcp).

## Client configuration

`scrutate setup` writes the entry into the agent's own file and keeps every other line, byte for
byte, in every file it writes itself. Claude Code's user file is the exception when `claude` is
on `PATH`: the entry goes through `claude mcp add-json` and `claude mcp remove`, and Claude Code
writes that whole file itself.

```sh
scrutate setup claude      # ~/.claude.json, through `claude mcp add-json` when claude is installed
scrutate setup codex       # $CODEX_HOME/config.toml: [mcp_servers.scrutate]
scrutate setup opencode    # ~/.config/opencode/opencode.json (or .jsonc)
scrutate setup cursor      # ~/.cursor/mcp.json
scrutate setup gemini      # ~/.gemini/settings.json
scrutate setup cursor --project            # the repository's .cursor/mcp.json (.mcp.json for Claude Code)
scrutate setup claude --uninstall          # takes out what setup added (hook included)
```

The entry runs `scrutate` by its absolute path with `serve`, so it serves the repository the
agent starts in; a `--project` entry names its repository with `--root`. `--dry-run` shows the
change, `--hook` also answers Claude Code's Grep with scrutate's search, and `--instructions` adds
a short block to `CLAUDE.md`, `AGENTS.md` or `GEMINI.md`
([details](../../docs/CLI.md#setup-and-the-grep-hook)). Gemini CLI starts a stdio server only in
a folder you trust.

Entries by hand (replace `/path/to/repo`; Cursor and VS Code fill in `${workspaceFolder}`
themselves). Give `serve` `--root DIR` (or `SCRUTATE_ROOT`) or start it inside a repository: in
the home directory or a filesystem root with no index, it stops with exit code 2.

Claude Code (`.mcp.json` in the repository):
```json
{ "mcpServers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"] } } }
```

Codex CLI (`~/.codex/config.toml`):
```toml
[mcp_servers.scrutate]
command = "scrutate"
args = ["--root", "/path/to/repo", "serve"]
```

VS Code (`.vscode/mcp.json`; the key is `servers`):
```json
{ "servers": { "scrutate": { "type": "stdio", "command": "scrutate", "args": ["--root", "${workspaceFolder}", "serve"] } } }
```

Zed (`settings.json`; the key is `context_servers`):
```json
{ "context_servers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"], "env": {} } } }
```

Tested in a running session: Claude Code. Checked with each client's own listing after
`scrutate setup` in a scratch home (2026-11-13): Claude Code 2.1.323 (`claude mcp list`:
Connected), Codex CLI 0.155.1 (`codex mcp list`: enabled), cursor-agent 2026.10.22 (`mcp list`:
ready), opencode 1.19.5 and 2.2.7 (`opencode mcp list`: connected) and Gemini CLI 0.68.0 (`gemini
mcp list`: Connected in a trusted folder). VS Code (`code --add-mcp` writes the `servers` entry
above) was checked with its own command; Zed follows its documentation as of November 2026 and
was not started here.

The server stays on the machine: it writes only `.scrutate/`, the frecency file of `find_files`
under `$XDG_STATE_HOME/scrutate/` (else `~/.local/state/scrutate/`, on Windows
`%LOCALAPPDATA%\scrutate\`) and, with `--allow-edits`, the definitions `edit_symbol` is asked to
apply. It opens no network connections and runs no repository code.
