// Scrutate for VS Code: `scrutate lsp` as a language client, plus commands over the CLI's JSON
// output. No bundler: `npm install`, then `npx @vscode/vsce package` and install the .vsix.
const vscode = require("vscode");
const { execFile } = require("child_process");
const { LanguageClient } = require("vscode-languageclient/node");
const { findRoot, fileOf, resolveBinary } = require("./root");

/** @type {LanguageClient | undefined} */
let client;

function folder() {
  const f = vscode.workspace.workspaceFolders;
  return f && f.length ? f[0].uri.fsPath : undefined;
}

function binary() {
  const setting = vscode.workspace.getConfiguration("scrutate").get("path", "scrutate");
  const bin = resolveBinary(setting);
  if (!bin) throw new Error(`${setting} was not found on PATH: install scrutate, or set scrutate.path to its full path`);
  return bin;
}

/** The command's output, and the root its paths are from. @returns {Promise<{out: string, root: string}>} */
async function run(args) {
  const bin = binary();
  const dir = folder();
  // without a folder the command would run in the editor's own directory (index could index $HOME)
  if (!dir) throw new Error("open a folder first");
  // the repository top, which may be above the folder: index answers and grep paths are both from it
  const cwd = await findRoot(bin, dir);
  return new Promise((resolve, reject) => {
    execFile(bin, args, { cwd, maxBuffer: 64 * 1024 * 1024 }, (err, stdout, stderr) => {
      // exit 1 = no results, which is not an error
      if (err && err.code !== 1) return reject(new Error(stderr || err.message));
      resolve({ out: stdout, root: cwd });
    });
  });
}

async function open(root, path, line) {
  const uri = vscode.Uri.file(fileOf(root, path));
  const doc = await vscode.workspace.openTextDocument(uri);
  const ed = await vscode.window.showTextDocument(doc);
  const pos = new vscode.Position(Math.max(0, line - 1), 0);
  ed.selection = new vscode.Selection(pos, pos);
  ed.revealRange(new vscode.Range(pos, pos), vscode.TextEditorRevealType.InCenter);
}

/** @param {{out: string, root: string}} r @param {string} placeHolder */
async function pickHits(r, placeHolder) {
  /** @type {any[]} */
  const hits = r.out ? JSON.parse(r.out).result : [];
  if (!hits.length) return vscode.window.showInformationMessage("Scrutate: no results");
  const items = hits.map((h) => ({
    label: `${h.kind ? "[" + h.kind + "] " : ""}${h.qualname || h.name || ""}`,
    description: `${h.path}:${h.start}`,
    detail: h.why ? Object.entries(h.why).map(([k, v]) => `${k} ${v}`).join("  ") : undefined,
    hit: h,
  }));
  const pick = await vscode.window.showQuickPick(items, { placeHolder, matchOnDescription: true });
  if (pick) await open(r.root, pick.hit.path, pick.hit.start);
}

/** As the language server: the run with dots first (R's `is.ready`), then the word (`norm` in `p.norm()`). */
function wordsUnderCursor() {
  const ed = vscode.window.activeTextEditor;
  if (!ed) return [];
  const ranges = [/[\p{Alphabetic}\p{N}_][\p{Alphabetic}\p{N}_.]*/u, /[\p{Alphabetic}\p{N}_]+/u].map((re) => ed.document.getWordRangeAtPosition(ed.selection.active, re));
  return [...new Set(ranges.filter(Boolean).map((r) => ed.document.getText(r)))];
}

/** The answer of `scrutate CMD --json -- NAME` for the first name that has one, else for the last. */
async function firstAnswer(cmd, names) {
  let r, name;
  for (name of names) {
    r = await run([cmd, "--json", "--", name]);
    const res = r.out ? JSON.parse(r.out).result : [];
    if (Array.isArray(res) ? res.length : (res.definitions || []).length || (res.unresolved || []).length) break;
  }
  return { r, name };
}

async function withErrors(f) {
  try {
    await f();
  } catch (e) {
    vscode.window.showErrorMessage(`Scrutate: ${e.message}`);
  }
}

function startClient() {
  const cwd = folder();
  if (!cwd) return;
  let bin;
  try {
    bin = binary();
  } catch (e) {
    vscode.window.showErrorMessage(`Scrutate: ${e.message}`);
    return;
  }
  // the server finds the repository top above the folder itself
  client = new LanguageClient(
    "scrutate",
    "Scrutate",
    { command: bin, args: ["lsp"], options: { cwd } },
    { documentSelector: [{ scheme: "file" }] }
  );
  client.start().catch((e) => vscode.window.showErrorMessage(`Scrutate: language server did not start: ${e.message}`));
}

function activate(context) {
  startClient();
  const k =() => vscode.workspace.getConfiguration("scrutate").get("results", 15);
  context.subscriptions.push(
    vscode.commands.registerCommand("scrutate.search", () =>
      withErrors(async () => {
        const q = await vscode.window.showInputBox({ prompt: "Scrutate search (meaning, name or identifier)" });
        if (!q) return;
        await pickHits(await run(["search", "-k", String(k()), "--json", "--", q]), q);
      })
    ),
    vscode.commands.registerCommand("scrutate.grep", () =>
      withErrors(async () => {
        const q = await vscode.window.showInputBox({ prompt: "Scrutate grep (regex; instant, no index)" });
        if (!q) return;
        const { out, root } = await run(["grep", "--json", "--max-results", "500", "--sort", "--", q, "."]);
        const items = out
          .split("\n")
          .filter((l) => l.trim())
          .map((l) => JSON.parse(l))
          .filter((r) => r.type === "match")
          .map((r) => ({ label: `${r.path}:${r.line}`, description: r.text.trim(), r }));
        if (!items.length) return vscode.window.showInformationMessage("Scrutate: no matches");
        const pick = await vscode.window.showQuickPick(items, { matchOnDescription: true });
        if (pick) await open(root, pick.r.path, pick.r.line);
      })
    ),
    vscode.commands.registerCommand("scrutate.symbol", () =>
      withErrors(async () => {
        const words = wordsUnderCursor();
        if (!words.length) words.push(await vscode.window.showInputBox({ prompt: "Symbol (Class.method works)" }));
        if (!words[0]) return;
        const { r, name } = await firstAnswer("symbol", words);
        await pickHits(r, `definitions of ${name}`);
      })
    ),
    vscode.commands.registerCommand("scrutate.callers", () =>
      withErrors(async () => {
        const words = wordsUnderCursor();
        if (!words.length) words.push(await vscode.window.showInputBox({ prompt: "Function name" }));
        if (!words[0]) return;
        const { r, name } = await firstAnswer("callers", words);
        await pickHits(r, `callers of ${name}`);
      })
    ),
    vscode.commands.registerCommand("scrutate.refs", () =>
      withErrors(async () => {
        const words = wordsUnderCursor();
        if (!words.length) return;
        const { out, root } = (await firstAnswer("refs", words)).r;
        const r = out ? JSON.parse(out).result : {};
        const items = [];
        for (const d of r.definitions || []) {
          items.push({ label: `[def] ${d.qualname}`, description: `${d.path}:${d.start}`, path: d.path, line: d.start });
          for (const o of d.references || []) {
            if (o.via !== "definition") items.push({ label: d.qualname, description: `${o.path}:${o.line}  ${o.text.trim()}`, path: o.path, line: o.line });
          }
        }
        for (const o of r.unresolved || []) items.push({ label: `(unresolved: ${o.reason})`, description: `${o.path}:${o.line}  ${o.text.trim()}`, path: o.path, line: o.line });
        if (!items.length) return vscode.window.showInformationMessage("Scrutate: no references");
        const pick = await vscode.window.showQuickPick(items, { matchOnDescription: true });
        if (pick) await open(root, pick.path, pick.line);
      })
    ),
    vscode.commands.registerCommand("scrutate.index", () =>
      withErrors(async () => {
        const { out } = await run(["index"]);
        vscode.window.showInformationMessage(`Scrutate: ${out.trim()}`);
      })
    )
  );
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
