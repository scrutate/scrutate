# Agent-facing code search: what agents and harnesses expect (November 2026)

Research date: 2026-11-10. Every claim links to the primary source it came from. Versions and dates are given where the source states them. Context on the scrutate side comes from `crates/scrutate/src/mcp.rs` (14 tools, protocol versions `2024-11-05`, `2025-03-26`, `2025-06-18`, text-only results, no annotations, no server instructions, no progress, cancellation ignored).

Status on 2026-11-17: the scrutate side of this note is the code of its research date. Since then the MCP server has 19 tools (and `edit_symbol` with `serve --allow-edits`), accepts the handshake revisions `2024-11-05` to `2025-11-25` and the stateless `2026-07-28` (`server/discover`), sends tool annotations and server instructions, cuts every answer at `max_bytes` (24,000 bytes by default) with `offset` to continue, reports progress while a first index is built, stops a call on `notifications/cancelled`, sends `structuredContent` with an `outputSchema` per tool under `serve --structured`, and takes `response_format` (`concise` or `detailed`) on every tool but `edit_symbol` (`crates/scrutate/src/mcp.rs`). `read_code` also reads one whole definition by `symbol`, and the `path:start-end` location of a result is accepted wherever a tool takes a symbol (by `references` as `def`). Items 1 to 8 below are done. Item 9 stands as written. Of item 10, `bench/agent_eval.py` now records tokens, cost and tool calls per solved task with and without the server; no run of it is committed.

## What this means for scrutate

1. **Annotate every tool.** Set `readOnlyHint: true` on the 13 read tools and `openWorldHint: false` on all 14. Claude Code runs read-only calls in parallel and may repeat them after a dropped connection. Codex `writes` mode, VS Code and Gemini CLI plan-mode policies skip the approval prompt only for read-only tools. Today scrutate sends no annotations, so every client treats it as destructive and open-world. See [section 1](#1-model-context-protocol-current-state).
2. **Default every result to about 10k tokens or less, and say when it was cut.** Codex cuts each tool output to 10,000 tokens by dropping the middle. Claude Code warns at 10,000 tokens and moves results over 50,000 characters or 25,000 tokens into a file. Gemini CLI truncates at 40,000 characters. `references` (up to 2,000), `structural_search` (up to 5,000) and `read_code` (400 lines) can pass those limits. See [section 2](#2-token-and-output-limits-on-tool-results).
3. **Add paging and a footer with totals.** `grep_code`, `find_symbol` and `callers` use fixed caps (30, 20, 30) with no `offset` argument. The tools agents already use have `offset` or `head_limit` (Claude Code Grep, Zed `grep` at 20 per page, Gemini `total_max_matches`), and SWE-agent's ablation favors a capped summary that tells the agent to narrow the query. See [section 3](#3-how-leading-coding-agents-search-code) and [section 6](#6-measured-evidence).
4. **Ship `outputSchema` plus `structuredContent`, and keep the compact text block.** Codex Code Mode turns a tool's `outputSchema` into a TypeScript return type, so a script can use fields directly. The spec says to also return the JSON as text. The MCP roadmap says clients still differ on which form reaches the model, so the text block stays the main form. See [section 1](#1-model-context-protocol-current-state).
5. **Return server `instructions`.** With tool search on (the default), Claude Code loads only tool names and server instructions at session start, and its docs say instructions help Claude know when to search for a server's tools (4,096 character cap). Codex reads the instructions and asks that the first 512 characters stand alone. scrutate returns none. See [section 1](#1-model-context-protocol-current-state) and [section 2](#2-token-and-output-limits-on-tool-results).
6. **Plan the move to protocol `2026-07-28`, but keep the handshake.** The current spec drops `initialize` and adds a required `server/discover`, a required `resultType`, and `ttlMs`/`cacheScope` on `tools/list`. Claude Code probes stdio servers for the new revision by default. Codex defaults to the old handshake at `2025-06-18`. scrutate's error reply to an unknown method already makes clients fall back. It should also accept `2025-11-25`. See [section 1](#1-model-context-protocol-current-state).
7. **Report progress and honor cancellation during the first index build.** Codex fails a tool call after 60 seconds by default. Claude Code cancels a stdio call after 30 minutes without a response or progress notification, and moves it to the background after 2 minutes. On stdio the spec says cancellation must arrive as `notifications/cancelled`. scrutate ignores it while holding one mutex. See [section 1](#1-model-context-protocol-current-state) and [section 2](#2-token-and-output-limits-on-tool-results).
8. **Add symbol-scoped reads, a concise/detailed switch, and reusable ids.** Serena, probe, codanna, ast-grep's server and Sourcegraph all return whole symbols or capped ranges, not raw lines. codanna returns `symbol_id` handles and a guidance line, ast-grep has a text mode it says uses about 75% fewer tokens, and Serena shortens a result step by step before refusing it. Anthropic recommends a `response_format` enum and semantic ids over opaque ones. See [section 4](#4-agent-facing-code-search-and-code-intelligence-servers) and [section 5](#5-vendor-guidance-on-designing-tools-for-agents).
9. **Treat the CLI as a main agent surface, not a fallback.** On macOS and Linux, Claude Code leaves its Grep and Glob tools out by default and searches through Bash. Codex's bundled prompt says to use `rg`, and its current models run tools from inside a JavaScript `exec` cell. Serena v2 and Anthropic's code-execution post point the same way. OpenAI also advises that a custom search tool's output should not look like ripgrep output. So `scrutate grep` output should match `rg`, and ranked `search` output should look clearly different. See [section 3](#3-how-leading-coding-agents-search-code) and [section 5](#5-vendor-guidance-on-designing-tools-for-agents).
10. **Measure precision and tokens-to-success, not recall alone.** The 2026 evidence is mixed. Grep wins when the issue names the symbol, retrieval with low precision makes agents worse (BM25 at 0.375 precision did harm in one study), and structural or semantic retrieval helps on harder localization and with weaker models. scrutate's benchmarks should report tokens and tool calls per solved task. See [section 6](#6-measured-evidence).

## 1. Model Context Protocol, current state

### Revision

- The current revision is **`2026-07-28`** ([versioning page](https://modelcontextprotocol.io/specification/versioning)). It was released on 2026-07-28 ([release post](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/blog/content/posts/2026-07-28-spec-ga/index.md)). The one before it was `2025-11-25` ([changelog](https://modelcontextprotocol.io/specification/latest/changelog)).
- Main changes in `2026-07-28` ([changelog](https://modelcontextprotocol.io/specification/latest/changelog)):
  - There is no `initialize` handshake and no sessions. Every request carries its protocol version and client capabilities in `_meta`.
  - Servers must implement `server/discover`.
  - Every result has a required `resultType` field.
  - `ping`, `logging/setLevel` and `notifications/roots/list_changed` are gone.
  - Servers should return `tools/list` in a deterministic order.
  - List results must carry `ttlMs` and `cacheScope`.
  - `inputSchema` and `outputSchema` may use any JSON Schema 2020-12 keyword, and `structuredContent` may be any JSON value.
  - Tasks moved out of the core spec into an extension.
  - Server-to-client requests (roots, sampling, elicitation) now go through Multi Round-Trip Requests (`InputRequiredResult`).
- Roots, Sampling and Logging are **deprecated** as of `2026-07-28`. The spec suggests passing directories through tool parameters or server config instead of Roots, and logging to stderr on stdio ([deprecated registry](https://modelcontextprotocol.io/specification/2026-07-28/deprecated)).
- **Backward compatibility on stdio:** a dual-era client probes with `server/discover`. If it gets an error that is not a recognized modern error, or no reply, it falls back to `initialize`. The fallback must not depend on one specific error code ([stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio), [versioning and compatibility matrix](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning)). A legacy-only server such as scrutate today returns `-32601`, so it works with dual-era clients. It fails with modern-only clients.
- The roadmap of 2026-08-22 lists two open problems. A `tools/call` result can carry the same output in more than one form, and "a server developer today has no way to know which form a given client will put in front of the model". Large tool lists also cost context, and a "progressive discovery effort" has started ([roadmap post](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/blog/content/posts/2026-08-22-mcp-roadmap.md)).

### Features that matter to a local code-search server

| Feature | In the released spec (`2026-07-28`) | Source |
| --- | --- | --- |
| Tool annotations `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, `title` | Yes. Defaults are `false`, `true`, `false`, `true`, so a tool without annotations is assumed to write, destroy, not be idempotent, and reach the open world. Clients must treat annotations from untrusted servers as untrusted. | [schema.ts](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2026-07-28/schema.ts), [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools), [annotations post, 2026-03-16](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/blog/content/posts/2026-03-16-tool-annotations.md) |
| `outputSchema` and `structuredContent` | Yes. If `outputSchema` is given, the server must return conforming structured results. A tool with structured content should also return the serialized JSON in a text block. | [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools) |
| Resource links (`resource_link`) | Yes. A tool may return links to resources, which need not appear in `resources/list`. `ResourceLink` has an optional `size` "to display file sizes and estimate context window usage". | [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools), [schema](https://modelcontextprotocol.io/specification/2026-07-28/schema) |
| Pagination | Yes, for list methods (`tools/list`, `resources/list` and others) with an opaque `cursor`/`nextCursor`. There is no protocol-level pagination of a single tool result. That is up to tool arguments. | [pagination](https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/pagination) |
| Progress | Yes. The client puts a `progressToken` in `_meta`, and the server may send `notifications/progress` with an increasing `progress`, an optional `total` and an optional `message`. | [progress](https://modelcontextprotocol.io/specification/2026-07-28/basic/utilities/progress) |
| Cancellation | Yes. On stdio the client must send `notifications/cancelled`. The server should stop work and not reply. Implementations may reset a timeout on progress but should keep a maximum timeout. | [cancellation](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/basic/patterns/cancellation.mdx) |
| Long-running calls (Tasks) | Official extension `io.modelcontextprotocol/tasks`, not core. The server returns `resultType: "task"` with a `taskId`, the client polls `tasks/get`, and the client can call `tasks/cancel`. The server must not return a task to a client that did not declare the extension. | [Tasks extension](https://modelcontextprotocol.io/extensions/tasks/overview) |
| Elicitation | Yes (form and URL modes), now delivered through `InputRequiredResult`. | [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools), [changelog](https://modelcontextprotocol.io/specification/latest/changelog) |
| Roots | Deprecated in `2026-07-28`. | [roots](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/client/roots.mdx) |
| Transports | stdio (newline-delimited, nothing but MCP messages on stdout, logs may go to stderr) and Streamable HTTP. HTTP+SSE is deprecated. | [stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio), [deprecated registry](https://modelcontextprotocol.io/specification/2026-07-28/deprecated) |
| Tool names | Should be 1 to 128 characters, using only `A-Z a-z 0-9 _ - .`. | [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools) |
| Server-side state | No protocol session. The spec suggests a server mint "an explicit handle" and take it back as a tool argument, with its lifetime stated in the tool description and a clear error when it expires. | [tools, Stateful Tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools) |
| Tool list size, tool search | Not in the spec. Clients handle it: Claude Code (tool search), Codex (`deferred` surface, BM25 `tool_search`). The roadmap names it as open work. | See client rows below. |

### Client support

| Client | What its primary source says |
| --- | --- |
| Claude Code (v2.1.322, 2026-11-10) | stdio, HTTP (`streamable-http` alias), SSE (deprecated) and WebSocket ([MCP docs](https://code.claude.com/docs/en/mcp)). Probes stdio servers for `2026-07-28` by default since v2.1.292, 2026-10-06 ([changelog](https://code.claude.com/docs/en/changelog)). Opt out with `MCP_PROTOCOL_NEGOTIATION=legacy` ([env vars](https://code.claude.com/docs/en/env-vars)). Uses `readOnlyHint` to run calls in parallel with other read-only calls, and `readOnlyHint` or `idempotentHint` to repeat a call after a dropped connection ([MCP docs](https://code.claude.com/docs/en/mcp)). Tool search is on by default, so only tool names and server instructions load at start. A tool can set `_meta["anthropic/alwaysLoad"]` to load upfront ([MCP docs](https://code.claude.com/docs/en/mcp)). Supports `structuredContent` since v2.0.21, 2025-10-18 ([changelog](https://code.claude.com/docs/en/changelog)). Elicitation (form, URL), `roots/list` (launch directory plus `--add-dir`), `list_changed`, and Tasks on the new revision ([MCP docs](https://code.claude.com/docs/en/mcp), [extension matrix](https://modelcontextprotocol.io/extensions/client-matrix)). |
| Claude Desktop | The MCP extension matrix marks MCP Apps as supported and Tasks as not ([matrix source](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/extensions/client-matrix.mdx), updated 2026-11-03). I found no other first-party feature list (see Not verified). |
| Codex CLI (rust-v0.167.0, 2026-11-06) | stdio and Streamable HTTP. Reads server `instructions` and asks that "the first 512 characters" be self-contained ([Codex MCP docs](https://developers.openai.com/codex/mcp.md)). `startup_timeout_sec` defaults to 10 and `tool_timeout_sec` to 60. Per-tool `output_token_limit`. `default_tools_approval_mode = "writes"` "prompts for tools that aren't marked read-only" ([Codex MCP docs](https://developers.openai.com/codex/mcp.md)). `supports_parallel_tool_calls` defaults to `false` per server. Tool input schemas over 5,000 bytes get compacted ([config reference](https://developers.openai.com/codex/config-reference.md)). The protocol mode defaults to `Legacy`, which runs `initialize` at `2025-06-18`. A `2026-07-28` mode exists ([protocol_mode.rs](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/rmcp-client/src/protocol_mode.rs)). In Code Mode, the `outputSchema` becomes the TypeScript type `CallToolResult<...>` of the tool's `structuredContent` ([description.rs](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/code-mode-protocol/src/description.rs)). |
| Cursor | stdio, SSE and Streamable HTTP. Tools, Prompts, Resources, Roots, Elicitation and the MCP Apps extension are listed as supported ([Cursor MCP docs](https://cursor.com/docs/mcp)). Image results are attached to the chat (same page). Approval is the default, and Cursor 3.6+ has an "Auto-review" classifier ([Cursor help](https://cursor.com/help/customization/mcp)). |
| VS Code (GitHub Copilot agent mode) | stdio, Streamable HTTP and SSE ("legacy support"). Tools, Prompts, Resources, Elicitation, Sampling, Authentication, Server instructions, Roots, MCP Apps and icons. `title` is shown in chat. With `readOnlyHint`, VS Code does not ask for confirmation ([MCP developer guide](https://code.visualstudio.com/api/extension-guides/ai/mcp), page dated 11/4/2026). |
| Gemini CLI (v0.67.0, 2026-11-04) | Discovers tools, resources and prompts (prompts become slash commands). Handles `text`, `image`, `audio`, `resource` and `resource_link` content ([mcp-server.md](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/tools/mcp-server.md)). Renames tools to `mcp_{serverName}_{toolName}`, replaces disallowed characters and shortens names over 63 characters (same page). Default MCP request timeout 600,000 ms (same page). Policy rules can match `toolAnnotations = { readOnlyHint = true }`, and the docs show this used to auto-allow read-only MCP tools in Plan Mode ([plan-mode.md](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/cli/plan-mode.md)). The MCP tool adapter has no `structuredContent` handling ([mcp-tool.ts](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/packages/core/src/tools/mcp-tool.ts)). |
| Zed (v1.27.0, 2026-11-04) | "Zed currently supports MCP's Tools and Prompts features" and handles `notifications/tools/list_changed`. Local (command) and remote (URL) servers ([Zed MCP docs](https://zed.dev/docs/ai/mcp)). |

## 2. Token and output limits on tool results

| Harness | Limit | What happens to a larger result | Source |
| --- | --- | --- | --- |
| Claude Code, MCP tools | Warning above 10,000 tokens (fixed). Limit 25,000 tokens by default (`MAX_MCP_OUTPUT_TOKENS`). Separately, a text result over 50,000 characters. | Saved to a file in the session's `tool-results` directory and replaced by a message naming the path, so Claude reads the file when needed. A tool can raise the character threshold with `_meta["anthropic/maxResultSizeChars"]`, up to 500,000. Error results (`isError: true`) over about 11,000 characters keep the first 5,000 and last 5,000. HTTP responses over 16 MiB fail. | [MCP docs, output limits](https://code.claude.com/docs/en/mcp) |
| Claude Code, descriptions | Server instructions and upfront tool descriptions: 4,096 characters. Deferred tool descriptions: 16,384. | Cut off, and Claude sees only the text before the cut. Tool search reads only the part within the shorter limit. | [MCP docs, tool search for server authors](https://code.claude.com/docs/en/mcp) |
| Claude Code, timeouts | `MCP_TIMEOUT` (startup), `MCP_TOOL_TIMEOUT` (default about 28 hours), idle 30 minutes for stdio, auto-background after 2 minutes. | An idle call with no response and no progress notification fails. A long call becomes a background task. | [MCP docs](https://code.claude.com/docs/en/mcp) |
| Claude Code, built-in Read | 25,000 tokens per call. A whole file over 256 KB is refused. | Over the token limit: the first page plus a `PARTIAL view` notice. Over 256 KB: an error pointing at `offset`/`limit` or Grep. | [tools reference](https://code.claude.com/docs/en/tools-reference) |
| Claude Code, Bash | Inline up to about 30,000 characters (`bashOutputMaxChars` up to 128,000). | Past that: a file path plus a preview of up to 2,000 characters. A failing command gets a head-and-tail excerpt of about 10,000 characters. | [tools reference](https://code.claude.com/docs/en/tools-reference) |
| Codex CLI | 10,000 tokens per tool output for every model in the bundled `models.json` (`truncation_policy: tokens, 10000`) ([models.json](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/models-manager/models.json)). `tool_output_token_limit` changes it globally ([model_info.rs](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/models-manager/src/model_info.rs)), `tools.<tool>.output_token_limit` per MCP tool, plus a 20% serialization allowance ([Codex MCP docs](https://developers.openai.com/codex/mcp.md)). | The middle is removed, and a header reads `Warning: truncated output (original token count: N)` with the total line count. An oversized MCP result is reserialized into a single text preview with `structured_content: None`, so structured content is lost ([output-truncation lib.rs](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/utils/output-truncation/src/lib.rs)). Code Mode `exec` output defaults to 10,000 tokens ([description.rs](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/code-mode-protocol/src/description.rs)). |
| OpenAI guidance for Codex-style harnesses | "Limit to 10k tokens. You can cheaply approximate this by computing `num_bytes/4`." | Keep half the budget for the beginning and half for the end, and cut the middle. | [Codex prompting guide](https://developers.openai.com/cookbook/examples/gpt-5/codex_prompting_guide) |
| Gemini CLI | `tools.truncateToolOutputThreshold` 40,000 characters. `contextManagement.tools.distillation.maxOutputTokens` 10,000. Outputs over 20,000 tokens may be summarized by an LLM (`summarizationThresholdTokens`). | Truncated, or summarized above the threshold. | [configuration.md](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/reference/configuration.md) |
| Cursor | No number published on the pages I read. | Not stated (see Not verified). | [Cursor MCP docs](https://cursor.com/docs/mcp) |
| Zed | No MCP output limit published. The built-in `read_file` returns a symbol outline instead of content for files over 16,384 bytes. | Outline, or the first 1 KB when no outline exists. | [Zed MCP docs](https://zed.dev/docs/ai/mcp), [outline.rs](https://github.com/zed-industries/zed/blob/v1.27.0/crates/agent/src/outline.rs) |

## 3. How leading coding agents search code

- **Claude Code** (v2.1.322) ([tools reference](https://code.claude.com/docs/en/tools-reference)):
  - On macOS, Linux and WSL, Claude Code leaves `Glob` and `Grep` out of the default tool set. Claude uses `find` and `grep` through Bash, which run "embedded versions of `bfs` and `ugrep`". On Windows, Glob and Grep are default tools.
  - `Grep` is built on ripgrep. It has three modes: `files_with_matches` (the default), `content` (lines with file and line number) and `count` (per-file counts plus a total). It takes `glob`, `type`, `multiline`, `head_limit` and `offset`. An offset past the last match returns `No entries at this offset`. A pattern ripgrep rejects returns ripgrep's own error message.
  - `Glob` sorts by modification time and caps results at 100, with a truncation flag.
  - `Read` returns line-numbered content.
  - `LSP` (definitions, references, hover, document and workspace symbols, implementations, call hierarchy) works only with a code intelligence plugin installed.
  - No embedding index is described.
- **Codex CLI** (rust-v0.167.0):
  - Search runs through the shell. The bundled model prompt says: "When you search for text or files, you reach first for `rg` or `rg --files`" ([models.json](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/models-manager/models.json)).
  - Every model in `models.json` has `tool_mode: "code_mode_only"`. The prompt says to batch searches and reads in one `functions.exec` with `Promise.allSettled`.
  - Nested tools, MCP tools included, are called as `tools.<name>(...)`. Deferred tools are found with `tools.tool_search({query, limit: 8})`, which is BM25-ranked ([description.rs](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/code-mode-protocol/src/description.rs)).
  - The tool handlers directory has no grep, glob or read tool, only `unified_exec`, `apply_patch`, `tool_search`, `mcp`, `mcp_resource` and similar ([handlers dir](https://github.com/openai/codex/tree/rust-v0.167.0/codex-rs/core/src/tools/handlers)).
- **Cursor**:
  - "Instant Grep" is a local index that Cursor says "outperforms `ripgrep` on large codebases". Cursor says it "does not store embeddings of your codebase for search".
  - An Explore subagent runs parallel searches in its own context and returns a summary ([search docs](https://cursor.com/docs/agent/tools/search)).
  - Instant Grep shipped in Cursor 2.1 on 2025-11-21 ([changelog 2.1](https://cursor.com/changelog/2-1)). Cursor's post of 2026-03-23 describes the index: sparse n-grams over trigrams, a local postings file plus a memory-mapped lookup table, and index state tied to a git commit with the user's and agent's edits as a layer on top. It notes `rg` runs of more than 15 seconds in large monorepos ([fast regex search](https://cursor.com/blog/fast-regex-search)).
  - On 2025-11-06 Cursor reported a semantic search tool alongside grep ([semsearch post](https://cursor.com/blog/semsearch), see section 6). Its current search docs do not mention it.
- **Gemini CLI** (v0.67.0):
  - Tools are `glob`, `grep_search`, `list_directory`, `read_file` and `read_many_files` ([tools reference](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/reference/tools.md)).
  - `grep_search` takes `pattern`, `dir_path`, `include_pattern`, `exclude_pattern`, `names_only`, `case_sensitive`, `fixed_strings`, `context`, `after`, `before`, `no_ignore`, `max_matches_per_file` and `total_max_matches` (same page).
  - Output is grouped per file, with `L15:`-style line prefixes ([file-system.md](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/tools/file-system.md)).
  - `tools.useRipgrep` defaults to `true` ([configuration.md](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/reference/configuration.md)).
  - `glob` sorts newest first. `read_file` takes `offset`/`limit` ([file-system.md](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/tools/file-system.md)).
- **Aider**:
  - A repo map lists files with their key symbols and the important definition lines. Aider ranks them with "a graph ranking algorithm, computed on a graph where each source file is a node".
  - `--map-tokens` "defaults to 1k tokens", and Aider sizes the map dynamically ([repo map docs](https://aider.chat/docs/repomap.html)).
- **OpenHands** (software-agent-sdk v1.61.0, 2026-11-09):
  - The default preset is terminal, file editor and task tracker ([default.py](https://github.com/OpenHands/software-agent-sdk/blob/main/openhands-tools/openhands/tools/preset/default.py)).
  - An optional `grep` tool returns matching file paths sorted by modification time, "Only the first 100 results", with a `truncated` flag in its observation ([grep definition](https://github.com/OpenHands/software-agent-sdk/blob/main/openhands-tools/openhands/tools/grep/definition.py)).
- **SWE-agent** (v1.1.0):
  - Tools are `find_file`, `search_dir` and `search_file` ([config.yaml](https://github.com/SWE-agent/SWE-agent/blob/main/tools/search/config.yaml)).
  - `search_dir` prints per-file match counts. Over 100 files it prints "More than N files matched ... Please narrow your search." instead of the list ([search_dir](https://github.com/SWE-agent/SWE-agent/blob/main/tools/search/bin/search_dir)).
- **Zed agent** (v1.27.0):
  - Tools are `grep`, `find_path`, `read_file`, `list_directory` and `diagnostics` ([Zed tools docs](https://zed.dev/docs/ai/tools)).
  - `grep` uses the Rust `regex` crate. It returns 20 matches per page with `offset`, is case-insensitive by default, and shows the enclosing syntax node (up to 10 lines by default) or else 2 lines of context ([grep_tool.rs](https://github.com/zed-industries/zed/blob/v1.27.0/crates/agent/src/tools/grep_tool.rs)).
  - `find_path` returns 50 results per page ([find_path_tool.rs](https://github.com/zed-industries/zed/blob/v1.27.0/crates/agent/src/tools/find_path_tool.rs)).
  - `read_file` returns an outline for files over 16 KB ([outline.rs](https://github.com/zed-industries/zed/blob/v1.27.0/crates/agent/src/outline.rs)).

Common shape across these: line-numbered matches grouped by file, a files-only mode, a hard cap with an explicit "truncated, narrow the query" signal, and offset paging. Zed and scrutate both group a match under its enclosing function. Only Aider ranks results by default.

## 4. Agent-facing code search and code-intelligence servers

| Server | Tools | What it does beyond grep | Source |
| --- | --- | --- | --- |
| Serena (v2.1.0, 2026-10-19) | `find_symbol`, `get_symbols_overview`, `find_referencing_symbols`, `find_declaration`, `find_implementations`, `rename_symbol`, `replace_symbol_body`, `insert_after_symbol`, `search_for_pattern`, `read_file`, memory tools and more. A v2 option, `serena_repl`, runs Python against a facade object `s` (`s.lsp.find_symbol(...)`). | Symbol-scoped reads and edits through LSP (40+ languages) or JetBrains. The REPL keeps intermediate results out of context and offers "progressive disclosure" through `s.info(...)`. Answers are capped at `default_max_tool_answer_chars: 150000`. An oversized answer is replaced by progressively shorter versions, prefixed with "The answer is too long (N characters). You can adjust your query or raise the max_answer_chars parameter." | [README](https://github.com/oraios/serena), [tools docs](https://oraios.github.io/serena/01-about/035_tools.html), [config template](https://github.com/oraios/serena/blob/main/src/serena/resources/serena_config.template.yml), [text_utils.py](https://github.com/oraios/serena/blob/main/src/serena/util/text_utils.py) |
| ast-grep MCP | `dump_syntax_tree`, `test_match_code_rule`, `find_code`, `find_code_by_rule` | Structural matches returned as whole nodes (`path:10-15` plus the body). `max_results` (default unlimited). `output_format` is `"text"` (the default, "~75% fewer tokens") or `"json"`. A tool to test a rule before running it on the repo. | [README](https://github.com/ast-grep/ast-grep-mcp) |
| Sourcegraph MCP | `keyword_search`, `nls_search`, `read_file`, `list_files`, `find_references`, `go_to_definition`, `commit_search`, `diff_search`, `deepsearch`, `code_finder`, `evaluator` (Lua over results) and others | "All MCP tools implement result limits". `read_file` returns the first 200 lines of files over 128 KB with a notice. `find_references` defaults to 10. `diff_search` defaults to 20 (max 50). `list_repos` returns `hasMoreResults`. `compare_revisions` takes an `after` cursor. | [Sourcegraph MCP docs](https://sourcegraph.com/docs/api/mcp) |
| Zoekt | No MCP server in the repository. It is a trigram-indexed engine with a query language, and it ranks results "using a combination of code-related signals like whether the match is on a symbol" (ctags recommended). | Ranked results with symbol signal. | [README](https://github.com/sourcegraph/zoekt), [cmd dir](https://github.com/sourcegraph/zoekt/tree/main/cmd) |
| GitHub MCP server (v2.1.0, 2026-11-03) | `search_code` with `query`, `page`, `perPage` (max 100), `sort`, `order`, and `fields` to drop large per-result data | A field selector to shrink responses. `--toolsets`/`--tools` to limit the tool list. `--lockdown-mode` filters public content as a prompt-injection mitigation. | [README](https://github.com/github/github-mcp-server) |
| probe (probelabs/probe, which `buger/probe` resolves to; v0.6.0-rc345, 2026-11-06) | `search`, `query`, `extract`, `symbols`. Also an agent mode (`probe agent --mcp`). | Returns complete AST blocks ranked by BM25/TF-IDF/hybrid. `--max-tokens` budget. "Session-based dedup to avoid repeating context". `extract file#symbol` and `file:10-50`. Argues the LLM should write boolean queries (`AND`, `OR`, `+required`) instead of relying on embeddings. | [README](https://github.com/probelabs/probe) |
| codanna (v0.17.1, 2026-10-11) | `find_symbol`, `analyze_impact`, `semantic_search_with_context` and others. Also runs one-shot as `codanna mcp <tool>`. | One call returns signature, docs, callers, callees with call sites, and impact. Every result has a `symbol_id` "it can reuse to disambiguate name collisions" and a "guidance hint it can chain on". `--json` returns a structured envelope. | [README](https://github.com/bartolli/codanna) |
| Codebase-Memory (paper, 2026-03-28) | A tree-sitter knowledge graph exposed over MCP (call graph, impact, communities, 66 languages) | Measured results are in section 6. | [arXiv 2603.27277](https://arxiv.org/abs/2603.27277) |

Patterns that a plain grep tool lacks: token or character budgets that apply to the whole answer (probe, Serena, Sourcegraph), whole-symbol units instead of lines (Serena, ast-grep, probe, codanna), reusable ids for follow-up calls (codanna `symbol_id`), next-step guidance in the result (codanna), session dedup (probe), shortening instead of refusing (Serena), and a code or REPL surface that replaces many tools (Serena v2, probe's `execute_plan`).

## 5. Vendor guidance on designing tools for agents

**Anthropic, "Writing effective tools for agents" (2025-09-11)** ([post](https://www.anthropic.com/engineering/writing-tools-for-agents)):
- "More tools don't always lead to better outcomes." Build fewer, task-shaped tools. For example, `search_logs` should return only the relevant lines with context, not a `read_logs` tool. Consolidate steps that always happen together.
- Namespace related tools with prefixes, and test prefix against suffix naming on your own model.
- Return high-signal fields and avoid low-level ids such as `uuid`. Mapping ids to semantic names reduced hallucinations.
- Offer a `response_format` enum (`DETAILED`/`CONCISE`). In the post's example, concise used 72 tokens and detailed used 206.
- Use pagination, range selection, filtering and truncation "with sensible default parameter values". The post notes that Claude Code limits tool responses to 25,000 tokens by default. When truncating, steer the agent toward many small targeted searches.
- Error messages should say what to change, not return "opaque error codes or tracebacks".
- Name parameters without ambiguity (`user_id`, not `user`). Use annotations to flag open-world or destructive tools.

**Anthropic, "Code execution with MCP" (2025-11-04)** ([post](https://www.anthropic.com/engineering/code-execution-with-mcp)):
- Loading all tool definitions upfront, and passing every intermediate result through the model, costs tokens.
- The post presents servers as code modules the agent discovers on demand. One workflow dropped "from 150,000 tokens to 2,000 tokens".
- A search tool with a detail-level parameter gives progressive disclosure.
- The cost is a sandbox to run the agent's code.

**Anthropic, "Advanced tool use" (2025-11-24)** ([post](https://www.anthropic.com/engineering/advanced-tool-use)):
- Tool search with `defer_loading` cut context from about 77K to 8.7K tokens. MCP eval accuracy rose from 49% to 74% (Opus 4) and from 79.5% to 88.1% (Opus 4.5). The post says it is worth using above 10K tokens of definitions or 10 or more tools.
- Programmatic tool calling reduced average tokens from 43,588 to 27,297.
- Tool use examples raised accuracy on complex parameters from 72% to 90%.

**OpenAI, function calling guide** ([guide](https://developers.openai.com/api/docs/guides/function-calling)):
- Apply the "intern test", and "Use enums and object structure to prevent invalid states".
- "Don't make the model fill arguments you already know", and "Combine functions that are always called in sequence".
- "Aim for fewer than 20 functions available at the start of a turn". "Use tool search to defer large or infrequently used parts of your tool surface".
- "We recommend always enabling strict mode."
- Function output "should typically be a string".

**OpenAI, Codex prompting guide** ([guide](https://developers.openai.com/cookbook/examples/gpt-5/codex_prompting_guide)):
- Use the provided `apply_patch` and `shell` tools, and prefer `rg`.
- Limit tool output to 10k tokens, keeping the head and tail.
- For custom tools such as semantic search: "The model hasn't necessarily been post-trained to excel at these tools". It helps to "make the results look different from outputs the model is accustomed to seeing from other tools", for example "ripgrep results should look different from semantic search results to avoid the model collapsing into old habits".

**Google, Gemini API function calling best practices** ([docs](https://aistudio.google.com/docs/function-calling), redirected from ai.google.dev):
- "Use descriptive names without spaces or special characters."
- "Use specific types (integer, string, enum)."
- "Keep active set to 10-20 tools maximum."
- "Validate function calls before executing."

**Gemini CLI** adds the naming limits in section 1 (`mcp_{server}_{tool}`, 63 characters) ([mcp-server.md](https://github.com/google-gemini/gemini-cli/blob/v0.67.0/docs/tools/mcp-server.md)).

**When to prefer a CLI or code execution over many MCP tools:** Anthropic's code-execution post and Codex's `code_mode_only` models both favor scripts that call tools and filter results before the model sees them. Serena v2 adds a REPL for the same reasons: composition, context economy, and progressive disclosure ([Serena tools docs](https://oraios.github.io/serena/01-about/035_tools.html)). Claude Code's default search path on macOS and Linux is Bash ([tools reference](https://code.claude.com/docs/en/tools-reference)). codanna ships each MCP tool as a one-shot CLI command so skills can call it without MCP ([README](https://github.com/bartolli/codanna)).

## 6. Measured evidence

- **SWE-agent (Yang et al., arXiv 2405.15793, v3 2024-11-11), on SWE-bench Lite with GPT-4 Turbo** ([paper](https://arxiv.org/html/2405.15793v3)):
  - Summarized search (a capped list plus guidance to refine) resolved 18.0%. Iterative paging through matches resolved 12.0%. No search tool resolved 15.7%.
  - File viewer window: 100 lines resolved 18.0%, 30 lines 14.3%, the full file 12.7%.
  - The paper says agents with iterative search "tend to look through every match exhaustively".
- **Cursor, semantic search (2025-11-06)** ([post](https://cursor.com/blog/semsearch)):
  - Offline, on Cursor Context Bench: on average 12.5% higher answer accuracy with semantic search plus grep than with grep alone (6.5% to 23.5% by model).
  - Online A/B: +0.3% code retention overall and +2.6% on codebases with 1,000+ files.
  - First-party and not independently reproduced.
- **"Does a Language Server Save Tokens for Coding Agents?" (Xu, arXiv 2608.13568, 2026-06-29)** ([abstract](https://arxiv.org/abs/2608.13568)):
  - "The answer is conditional and usually negative."
  - When the task names the symbol, LSP costs 6% to 118% more tokens. Models pick grep for localization (0 to 6% semantic use).
  - "grep solves multi-file renames perfectly", while a location-only LSP misses a call site in about three quarters of them.
  - The authors recommend an adaptive router keyed on task class, model capability and lexical noise.
- **CodeGrep (Chen et al., arXiv 2608.05886, 2026-08-06)** ([abstract](https://arxiv.org/abs/2608.05886)):
  - An RL-trained retrieval agent feeding OpenHands on SWE-bench Verified (500 instances) resolved 27.0%, against 25.8% without retrieval. Resolved instances used 15% fewer rounds and 19% fewer tokens.
  - BM25 retrieval (precision 0.375) "degrades the agent". Jina (0.445) was neutral. CodeGrep (0.677) cut rollout cost.
  - The baseline used 631K tokens per resolved issue.
- **Agent Retrieval Bench (Qin and Xie, arXiv 2607.24882, 2026-07-27)** ([abstract](https://arxiv.org/abs/2607.24882)):
  - "No single retrieval family dominates."
  - Embeddings (Qwen3-Embedding) led on MRR and Recall@20, while RepoMap gave the best context yield at an 8K-token budget.
  - Logged agent trajectories missed every gold file on 27 to 35% of samples.
- **"Code Isn't Memory" (Bhola et al., arXiv 2606.22417, 2026-06-21)** ([abstract](https://arxiv.org/abs/2606.22417)):
  - A structural codebase index inside the agent (Claude Opus 4.7, SWE-PolyBench Verified and SWE-bench Pro) gave "a large localization gain and a statistically separated resolve gain", at lower cost per solve.
  - Across harnesses, it "does not regress against an agentic-grep baseline".
- **Small agents with semantic search (Lasbordes et al., arXiv 2610.05099, 2026-10-04)** ([abstract](https://arxiv.org/abs/2610.05099)):
  - Agents under 2B parameters with the ColGREP semantic tool beat grep-based agents on localization (SWE-bench Lite, Multi-SWE-bench Flash).
  - They used 29.1% fewer tokens and had 44.1% lower latency on CPU.
- **Codebase-Memory (Vogel et al., arXiv 2603.27277, 2026-03-28)** ([abstract](https://arxiv.org/abs/2603.27277)):
  - A tree-sitter graph over MCP reached 83% answer quality, against 92% for a file-exploration agent.
  - It used ten times fewer tokens and 2.1 times fewer tool calls.
- **Anthropic advanced tool use** (section 5): tool search raised MCP tool-selection accuracy. These are internal evals.

Reading across these: a capped, summarized result beats paging through every match. Grep is hard to beat when the query names the symbol. Ranked or structural retrieval pays off on harder localization, at small budgets, and with weaker models, but only above some precision. Token cost per solved task is the measure most of these studies report, and scrutate's benchmarks do not report it yet.

## Not verified

- **Cursor:** the MCP tool-output size limit and truncation rule. Whether the semantic search from the 2025-11-06 post still ships: the current search docs mention only Instant Grep and say Cursor does not store embeddings for search. The 2.1 changelog page needed a rendered fetch, and its HTML did not load with a plain request.
- **Claude Desktop:** no first-party list of MCP features beyond the community extension matrix.
- **VS Code:** a tool-count cap (often quoted as 128) does not appear on the pages I read. Its support for `structuredContent`, `resource_link`, progress and Tasks is not stated.
- **Zed:** the MCP protocol revisions it negotiates, and any output limit.
- **Gemini CLI:** whether it speaks `2026-07-28`. The only match in its docs is a release date in the changelog. Whether it sends progress tokens or cancellation.
- **Codex CLI:** whether it sends `progressToken` or `notifications/cancelled` to MCP servers (not checked in source).
- **Claude Code:** how `structuredContent` and `content` are combined before the model sees them. The docs do not say, and the changelog only says support exists (v2.0.21) and that images were kept when both are present (v2.1.128).
- **Amp:** I did not check Amp's search tools.
- **Sourcegraph MCP:** the page has no date or version.
- **Aider:** the repo map page does not name tree-sitter, so I left that out.
- **The 2026 arXiv papers** are preprints. I read only their abstracts, so the numbers above come from abstracts, not from the full text or reproduced runs.
- **The Cursor post and the Anthropic numbers** are first-party evals without public data.
- **The MCP extension matrix** is "maintained by the community". I read it from its source file (last commit 2026-11-03), because the rendered page dropped the check marks.
