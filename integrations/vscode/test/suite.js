// Runs inside VS Code (test/run.js): the language server through VS Code's own providers, then
// every command with its pickers answered by the test. The folder may be below the repository top.
const assert = require("assert");
const fs = require("fs");
const path = require("path");
const vscode = require("vscode");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Ask until the server (and its first index) answers. */
async function until(what, f) {
  for (let i = 0; i < 150; i++) {
    const v = await f();
    if (v && v.length) return v;
    await sleep(200);
  }
  throw new Error(`no answer: ${what}`);
}

exports.run = async function () {
  // which copy runs: this folder, or the package installed from the .vsix
  const ext = vscode.extensions.getExtension("scrutate.scrutate");
  assert.ok(ext, "the extension is loaded");
  const real = (p) => {
    const r = fs.realpathSync.native(p);
    return process.platform === "win32" ? r.toLowerCase() : r;
  };
  const under = process.env.SCRUTATE_TEST_EXTENSION_UNDER;
  if (under) assert.ok(real(ext.extensionPath).startsWith(real(under)), `${ext.extensionPath} is not under ${under}`);
  console.log(`extension path: ${ext.extensionPath}`);

  const folder = vscode.workspace.workspaceFolders[0].uri;
  const top = process.env.SCRUTATE_TEST_TOP || folder.fsPath;
  const root = vscode.Uri.file(top);
  const app = vscode.Uri.joinPath(root, "src/app.py");
  const geo = vscode.Uri.joinPath(root, "src/geo.py");
  const doc = await vscode.workspace.openTextDocument(app);
  await vscode.window.showTextDocument(doc);
  const onCall = new vscode.Position(4, 13); // `make_point` in `return make_point().norm()`
  const onTheCall = async () => {
    const editor = await vscode.window.showTextDocument(doc);
    editor.selection = new vscode.Selection(onCall, onCall);
  };

  const defs = await until("definition", () => vscode.commands.executeCommand("vscode.executeDefinitionProvider", app, onCall));
  assert.strictEqual(defs.length, 1);
  assert.ok((defs[0].uri || defs[0].targetUri).path.endsWith("/src/geo.py"));
  assert.strictEqual((defs[0].range || defs[0].targetRange).start.line, 5);

  const refs = await vscode.commands.executeCommand("vscode.executeReferenceProvider", app, onCall);
  assert.ok(refs.some((r) => r.uri.path.endsWith("/src/app.py") && r.range.start.line === 4), JSON.stringify(refs));
  assert.ok(refs.some((r) => r.uri.path.endsWith("/src/geo.py") && r.range.start.line === 11), JSON.stringify(refs));

  const symbols = await vscode.commands.executeCommand("vscode.executeWorkspaceSymbolProvider", "make_po");
  assert.ok(symbols.some((s) => s.name === "make_point"), JSON.stringify(symbols));

  const outline = await vscode.commands.executeCommand("vscode.executeDocumentSymbolProvider", geo);
  assert.deepStrictEqual(outline.map((s) => s.name), ["Point", "make_point", "use_it"]);
  assert.strictEqual(outline[0].children[0].name, "norm");

  // the commands: the test types the query and takes the pick it wants
  const { showInputBox, showQuickPick, showInformationMessage } = vscode.window;
  let typed, want, shown, said;
  vscode.window.showInputBox = async () => typed;
  vscode.window.showQuickPick = async (items) => {
    shown = items;
    return items.find(want) || items[0];
  };
  vscode.window.showInformationMessage = async (message) => {
    said = message;
  };
  const at = () => {
    const e = vscode.window.activeTextEditor;
    return `${path.relative(top, e.document.uri.fsPath).split(path.sep).join("/")}:${e.selection.active.line + 1}`;
  };
  try {
    typed = "build the origin point";
    want = () => false;
    await vscode.commands.executeCommand("scrutate.search");
    assert.strictEqual(at(), "src/geo.py:6", "search opens the first hit");

    typed = "def use_it";
    await vscode.commands.executeCommand("scrutate.grep");
    assert.strictEqual(at(), "src/geo.py:11", "grep opens the match");

    typed = "-?def main";
    await vscode.commands.executeCommand("scrutate.grep");
    assert.strictEqual(at(), "src/app.py:4", "a pattern that starts with - is not read as an option");

    await onTheCall();
    await vscode.commands.executeCommand("scrutate.symbol");
    assert.strictEqual(at(), "src/geo.py:6", "symbol takes the word under the cursor");

    const onNorm = new vscode.Position(12, 14); // `norm` in `return p.norm()`
    (await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(geo))).selection = new vscode.Selection(onNorm, onNorm);
    await vscode.commands.executeCommand("scrutate.symbol");
    assert.strictEqual(at(), "src/geo.py:2", "the word under the cursor stops at a dot");

    // an R name keeps its dots: the dotted run is asked first
    const rFile = vscode.Uri.joinPath(root, "src/r.R");
    fs.writeFileSync(rFile.fsPath, "is.ready <- function(x) x\nuse <- function() is.ready(1)\n");
    // a query answers from the last committed index; this waits for the server's update to commit
    await vscode.commands.executeCommand("scrutate.index");
    const onReady = new vscode.Position(1, 22); // `ready` in `is.ready(1)`
    (await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(rFile))).selection = new vscode.Selection(onReady, onReady);
    await vscode.commands.executeCommand("scrutate.symbol");
    assert.strictEqual(at(), "src/r.R:1", "a name with dots is asked whole first");

    // a word is alphabetic characters and digits of any script, as the server reads one
    const uFile = vscode.Uri.joinPath(root, "src/umlaut.py");
    fs.writeFileSync(uFile.fsPath, "def größe(x):\n    return x\n\n\ndef nutze():\n    return größe(2)\n");
    await vscode.commands.executeCommand("scrutate.index");
    const onEszett = new vscode.Position(5, 14); // `ß` in `return größe(2)`
    (await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uFile))).selection = new vscode.Selection(onEszett, onEszett);
    await vscode.commands.executeCommand("scrutate.symbol");
    assert.strictEqual(at(), "src/umlaut.py:1", "a word with letters outside ASCII");

    // vowel signs are Alphabetic but not letters (\p{L}); the server keeps them in the word
    const hFile = vscode.Uri.joinPath(root, "src/hindi.py");
    fs.writeFileSync(hFile.fsPath, "def हिंदी(x):\n    return x\n\n\ndef नया():\n    return हिंदी(2)\n");
    await vscode.commands.executeCommand("scrutate.index");
    const onHa = new vscode.Position(5, 11); // `ह` in `return हिंदी(2)`
    (await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(hFile))).selection = new vscode.Selection(onHa, onHa);
    await vscode.commands.executeCommand("scrutate.symbol");
    assert.strictEqual(at(), "src/hindi.py:1", "a word with vowel signs");

    await onTheCall();
    want = (item) => item.description.startsWith("src/geo.py:");
    await vscode.commands.executeCommand("scrutate.callers");
    assert.strictEqual(at(), "src/geo.py:11", "callers lists use_it");

    await onTheCall();
    want = (item) => item.description.startsWith("src/geo.py:12");
    await vscode.commands.executeCommand("scrutate.refs");
    assert.ok(shown.some((i) => i.label === "[def] make_point"), JSON.stringify(shown));
    assert.ok(shown.some((i) => i.description.startsWith("src/app.py:5")), JSON.stringify(shown));
    assert.strictEqual(at(), "src/geo.py:12", "refs opens the reference picked");

    await vscode.commands.executeCommand("scrutate.index");
    assert.ok(/^Scrutate: /.test(said), said);

    typed = "zzzz_no_such_thing_qqqq";
    said = undefined;
    await vscode.commands.executeCommand("scrutate.grep");
    assert.strictEqual(said, "Scrutate: no matches");

    assert.ok(fs.existsSync(path.join(top, ".scrutate")), "the index is at the top");
    // VS Code lowercases the drive letter of fsPath on Windows
    const same = (a, b) => (process.platform === "win32" ? a.toLowerCase() === b.toLowerCase() : a === b);
    if (!same(folder.fsPath, top)) assert.ok(!fs.existsSync(path.join(folder.fsPath, ".scrutate")), "no index in the folder");
  } finally {
    Object.assign(vscode.window, { showInputBox, showQuickPick, showInformationMessage });
  }
};
