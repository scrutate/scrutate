// claude-context's core API in one process for bench/yardstick.py (rivals/semantic.py): index the
// directory given, print {"ready": true}, then answer one JSON query per stdin line.
//   CLAUDE_CONTEXT_DIR=<dir with node_modules/@zilliz/claude-context-core> node claude_context.mjs DIR
import { createRequire } from "node:module";
import path from "node:path";
import readline from "node:readline";

const req = createRequire(path.join(process.env.CLAUDE_CONTEXT_DIR || process.cwd(), "node_modules", "noop.js"));
if (process.argv[2] === "--version") {
  console.log(req("@zilliz/claude-context-core/package.json").version);
  process.exit(0);
}
const { Context, OllamaEmbedding, MilvusVectorDatabase } = req("@zilliz/claude-context-core");
const root = path.resolve(process.argv[2]);
const ctx = new Context({
  embedding: new OllamaEmbedding({ model: process.env.EMBEDDING_MODEL, host: process.env.OLLAMA_HOST }),
  vectorDatabase: new MilvusVectorDatabase({ address: process.env.MILVUS_ADDRESS, token: process.env.MILVUS_TOKEN }),
});
try {
  await ctx.indexCodebase(root);
} catch (e) {
  console.log(JSON.stringify({ ready: false, error: String(e) }));
  process.exit(1);
}
console.log(JSON.stringify({ ready: true }));
for await (const line of readline.createInterface({ input: process.stdin })) {
  try {
    const q = JSON.parse(line);
    const found = await ctx.semanticSearch(root, q.query, q.limit || 10);
    console.log(JSON.stringify({ results: found.map(({ relativePath, startLine, endLine, score }) => ({ relativePath, startLine, endLine, score })) }));
  } catch (e) {
    console.log(JSON.stringify({ error: String(e) }));
  }
}
