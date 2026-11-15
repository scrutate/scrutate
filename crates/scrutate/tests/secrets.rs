//! Secret-safe answers: a tree with three planted secrets of each kind scrutate redacts, `.env`
//! files and private keys among them, asked through every MCP tool that returns text and through
//! the Grep hook. No secret's bytes may come back, and each redaction says its kind.

#[path = "support/canon.rs"]
mod canon;
#[path = "support/tag.rs"]
mod tag;
// only the Unix tests run rg here
#[cfg(unix)]
#[path = "support/rg.rs"]
mod rg;

use canon::canon;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use scrutate::session::{Every, Session};
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

/// Deterministic characters from `set`, `n` of them, for seed `seed`.
fn chars(seed: &mut u64, set: &str, n: usize) -> String {
    let set: Vec<char> = set.chars().collect();
    (0..n)
        .map(|_| {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            set[(*seed >> 33) as usize % set.len()]
        })
        .collect()
}

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const HEX: &str = "0123456789abcdef";

/// `text` in base64, as an HTTP Basic header carries it.
fn basic(text: &str) -> String {
    const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            out.push(if i <= chunk.len() { SET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// (kind, file, line holding the secret, the secret): three of each kind.
fn planted() -> Vec<(&'static str, &'static str, String, String)> {
    let mut seed = 7u64;
    let mut out = Vec::new();
    for _ in 0..3 {
        let s = &mut seed;
        let mut add = |kind: &'static str, file: &'static str, line: &dyn Fn(&str) -> String, secret: String| out.push((kind, file, line(&secret), secret));
        add("aws-access-key-id", "config.py", &|s| format!("AWS_ID = \"{s}\""), format!("AKIA{}", chars(s, "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 16)));
        add("aws-secret-access-key", "config.py", &|s| format!("aws_secret_access_key = {s}"), chars(s, &format!("{ALNUM}/+"), 40));
        add("github-token", "config.py", &|s| format!("GITHUB = \"{s}\""), format!("ghp_{}", chars(s, ALNUM, 36)));
        add("github-fine-grained-token", "config.py", &|s| format!("PAT = \"{s}\""), format!("github_pat_{}", chars(s, ALNUM, 82)));
        add("gitlab-token", "config.py", &|s| format!("GITLAB = \"{s}\""), format!("glpat-{}", chars(s, ALNUM, 20)));
        add(
            "slack-token",
            "config.py",
            &|s| format!("SLACK = \"{s}\""),
            format!("xoxb-{}-{}-{}", chars(s, "0123456789", 13), chars(s, "0123456789", 13), chars(s, ALNUM, 24)),
        );
        add(
            "slack-webhook",
            "config.py",
            &|s| format!("HOOK = \"https://hooks.slack.com/services/{s}\""),
            format!("T0{}/B0{}/{}", chars(s, ALNUM, 7), chars(s, ALNUM, 7), chars(s, ALNUM, 24)),
        );
        add("stripe-key", "config.py", &|s| format!("STRIPE = \"{s}\""), format!("sk_live_{}", chars(s, ALNUM, 24)));
        add("google-api-key", "config.py", &|s| format!("MAPS = \"{s}\""), format!("AIza{}", chars(s, ALNUM, 35)));
        add("openai-api-key", "config.py", &|s| format!("OPENAI = \"{s}\""), format!("sk-{}T3BlbkFJ{}", chars(s, ALNUM, 20), chars(s, ALNUM, 20)));
        add("anthropic-api-key", "config.py", &|s| format!("CLAUDE = \"{s}\""), format!("{}{}{}AA", "sk-ant-", "api03-", chars(s, ALNUM, 93)));
        add("npm-token", "config.py", &|s| format!("NPM = \"{s}\""), format!("npm_{}", chars(s, ALNUM, 36)));
        add("pypi-token", "config.py", &|s| format!("PYPI = \"{s}\""), format!("pypi-{}", chars(s, ALNUM, 90)));
        add("jwt", "config.py", &|s| format!("BEARER = \"{s}\""), format!("eyJ{}.eyJ{}.{}", chars(s, ALNUM, 20), chars(s, ALNUM, 20), chars(s, ALNUM, 24)));
        add("twilio-api-key", "config.py", &|s| format!("TWILIO = \"{s}\""), format!("SK{}", chars(s, HEX, 32)));
        add("sendgrid-api-key", "config.py", &|s| format!("SENDGRID = \"{s}\""), format!("SG.{}.{}", chars(s, ALNUM, 22), chars(s, ALNUM, 43)));
        add("mailgun-api-key", "config.py", &|s| format!("mailgun_key = \"{s}\""), format!("key-{}", chars(s, HEX, 32)));
        add("database-password", ".env", &|s| format!("DATABASE_URL=postgres://app:{s}@db.internal:5432/app"), chars(s, ALNUM, 14));
        add("env-secret", ".env", &|s| format!("SESSION_SECRET={s}"), format!("{}!{}", chars(s, ALNUM, 10), chars(s, ALNUM, 8)));
        add("password", "config.py", &|s| format!("db_password = \"{s}\""), format!("{}@{}!{}", chars(s, ALNUM, 6), chars(s, ALNUM, 5), chars(s, ALNUM, 4)));
        add("generic-secret", "settings.yml", &|s| format!("client_secret: {s}"), chars(s, ALNUM, 18));
        add(
            "url-password",
            "config.py",
            &|s| format!("REMOTE = \"https://deploy:{s}@git.example.com/app.git\""),
            format!("{}!{}", chars(s, ALNUM, 8), chars(s, ALNUM, 6)),
        );
        add("basic-auth", "config.py", &|s| format!("AUTH = \"Basic {s}\""), basic(&format!("deploy:{}", chars(s, ALNUM, 14))));
        add("bearer-token", "config.py", &|s| format!("HEADERS = {{\"Authorization\": \"Bearer {s}\"}}"), format!("{}7", chars(s, ALNUM, 31)));
        add("private-key", "keys.pem", &|s| s.to_string(), chars(s, ALNUM, 64));
        // typed, in Rust
        add("aws-secret-access-key", "conf.rs", &|s| format!("pub const aws_secret_access_key: &str = \"{s}\";"), chars(s, &format!("{ALNUM}/+"), 40));
        // a Rust constant, read whole and by its symbol (the concise form carries its line)
        add("github-token", "conf.rs", &|s| format!("pub const TOKEN: &str = \"{s}\";"), format!("ghp_{}", chars(s, ALNUM, 36)));
    }
    out
}

/// The planted tree: code that uses the secrets, a `.env`, and three private keys.
fn tree(test: &str) -> (PathBuf, Vec<(&'static str, String)>) {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_{test}_{}", std::process::id()));
    tag::claim(&root);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let mut files: std::collections::BTreeMap<&str, String> = Default::default();
    files.insert("config.py", "\"\"\"Settings.\"\"\"\n\n\ndef settings():\n    return SETTINGS\n\n\n".into());
    let mut secrets = Vec::new();
    for (kind, file, line, secret) in planted() {
        let f = files.entry(file).or_default();
        if kind == "private-key" {
            // the armour written out here, so no file holds a key's armour around a body
            let armour = |edge: &str| format!("-----{edge} RSA {}-----", "PRIVATE KEY");
            // a last line shorter than any rule for base64 alone: only the key's armour finds it
            let last = format!("{}==", &line[..22]);
            f.push_str(&format!("{}\n{line}\n{last}\n{}\n", armour("BEGIN"), armour("END")));
            secrets.push((kind, last));
        } else {
            f.push_str(&line);
            f.push('\n');
        }
        secrets.push((kind, secret));
    }
    for (name, text) in files {
        fs::write(root.join(name), text).unwrap();
    }
    // a file named with a token: a path that is a secret is one too
    let named = format!("ghp_{}", chars(&mut 99, ALNUM, 36));
    fs::create_dir_all(root.join("keys")).unwrap();
    fs::write(root.join(format!("keys/{named}.py")), "def rotate():\n    return 1\n").unwrap();
    secrets.push(("github-token", named));
    // armour in mixed case
    let armour = |edge: &str, what: &str| format!("-----{edge} {what}-----");
    let (body, last) = (chars(&mut 7, &format!("{ALNUM}+/"), 64), format!("{}==", chars(&mut 8, ALNUM, 22)));
    fs::write(root.join("mixed.pem"), format!("{}\n{body}\n{last}\n{}\n", armour("BEGIN", "Rsa Private Key"), armour("END", "Rsa Private Key"))).unwrap();
    secrets.extend([("private-key", body), ("private-key", last)]);
    // a key in UTF-16 with its byte-order mark, as the engines decode it
    let (body, last) = (chars(&mut 9, &format!("{ALNUM}+/"), 64), format!("{}==", chars(&mut 10, ALNUM, 22)));
    let text = format!("{}\n{body}\n{body}\n{last}\n{}\n", armour("BEGIN", "RSA PRIVATE KEY"), armour("END", "RSA PRIVATE KEY"));
    let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
    fs::write(root.join("utf16.pem"), utf16).unwrap();
    secrets.extend([("private-key", body), ("private-key", last)]);
    // a line longer than references shows whole: its window around `rotate` starts inside the value
    let value = format!("{}7", chars(&mut 77, ALNUM, 59));
    fs::write(root.join("long.py"), format!("client_secret = \"{value}\"; {}rotate(); {}\n", "_ = 0; ".repeat(18), "_ = 0; ".repeat(40))).unwrap();
    secrets.push(("password", value[value.len() - 16..].to_string()));
    // a Latin-1 file: a token on a line that is not UTF-8
    let latin1 = format!("{}{}", "ghp_", chars(&mut 66, ALNUM, 36));
    fs::write(root.join("latin1.py"), [format!("LEGACY = \"{latin1}\"  # caf").into_bytes(), b"\xe9\n".to_vec()].concat()).unwrap();
    secrets.push(("github-token", latin1));
    // a file that holds a marker's text and no secret: no redaction is counted for it
    fs::write(root.join("notes.py"), "NOTE = \"[redacted:github-token] is how answers show a token\"\n").unwrap();
    // as Claude Code has it: on Windows without the `\\?\` of canonicalize, which the hook declines
    (canon(&root), secrets)
}

/// Every string of an MCP answer: its text and its structured form.
fn strings(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

#[test]
fn no_planted_secret_comes_back_through_a_tool_or_the_hook_and_each_is_marked() {
    let (root, secrets) = tree("tools");
    assert!(secrets.len() >= 60, "{}", secrets.len());
    let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
    let settings = scrutate::mcp::Settings::new(None);
    let mut seen = String::new();
    // every tool that returns text, in both forms, at a page of 24,000 bytes and pages of 700 that
    // cut answers between a key's armour lines
    let calls: Vec<(&str, Value)> = {
        let mut v = Vec::new();
        for file in ["config.py", ".env", "keys.pem", "conf.rs"] {
            v.push(("read_code", json!({"path": file, "start": 1, "end": 400})));
            v.push(("outline", json!({"path": file})));
            v.push(("dependencies", json!({"path": file})));
            v.push(("dependents", json!({"target": file})));
        }
        // a key's body without its armour: a range that starts inside it, a search for its lines
        v.push(("read_code", json!({"path": "keys.pem", "start": 2, "end": 3})));
        v.push(("read_code", json!({"path": "mixed.pem"})));
        v.push(("read_code", json!({"path": "mixed.pem", "start": 3})));
        v.push(("grep_code", json!({"pattern": "==$", "max_results": 1000})));
        for pattern in [".", "=", "KEY", "-----", "^[A-Za-z0-9]{40,}$"] {
            v.push(("grep_code", json!({"pattern": pattern, "max_results": 1000})));
        }
        for query in ["secret token key password", "aws github slack stripe", "private key", "ghp rotate"] {
            v.push(("search_code", json!({"query": query})));
        }
        for name in ["settings", "client_secret", "SETTINGS", "rotate"] {
            v.push(("find_symbol", json!({"name": name})));
            v.push(("references", json!({"name": name})));
            v.push(("callers", json!({"name": name})));
            v.push(("callees", json!({"name": name})));
        }
        for symbol in ["TOKEN", "SETTINGS"] {
            v.push(("read_code", json!({"path": if symbol == "SETTINGS" { "config.py" } else { "conf.rs" }, "symbol": symbol})));
            v.push(("references", json!({"symbol": symbol})));
        }
        v.push(("structural_search", json!({"lang": "rust", "query": "(string_literal) @s"})));
        // a capture that holds the value alone, and one that holds the whole assignment
        v.push(("structural_search", json!({"lang": "python", "query": "(string) @s"})));
        v.push(("structural_search", json!({"lang": "python", "query": "(assignment) @a"})));
        v.push(("call_path", json!({"from": "settings", "to": "rotate"})));
        v.push(("similar_code", json!({"target": "config.py:4"})));
        v.push(("duplicates", json!({})));
        v.push(("impact", json!({})));
        v.push(("repo_map", json!({})));
        v.push(("dead_code", json!({})));
        v.push(("find_files", json!({"query": "env"})));
        v.push(("find_files", json!({"query": "rotate"})));
        v
    };
    let mut kinds_seen = std::collections::BTreeSet::new();
    for size in [24_000, 700] {
        for form in ["detailed", "concise"] {
            for (tool, args) in &calls {
                let mut offset = 0;
                loop {
                    let mut a = args.clone();
                    a["max_bytes"] = json!(size);
                    a["offset"] = json!(offset);
                    a["response_format"] = json!(form);
                    let c = scrutate::mcp::call_tool(&session, tool, &a, &settings, &Default::default());
                    let mut text = c.text.clone();
                    strings(&c.structured, &mut text);
                    for (kind, secret) in &secrets {
                        assert!(!text.contains(secret.as_str()), "{tool} {form} {size} offset {offset}: {kind} {secret} came back:\n{}", c.text);
                    }
                    kinds_seen.extend(secrets.iter().filter(|(k, _)| text.contains(&format!("[redacted:{k}]"))).map(|(k, _)| *k));
                    // the closing line counts redactions, never a marker's text a file holds
                    if *tool == "read_code" && args["path"] == "notes.py" {
                        assert!(!c.text.contains("secret redacted"), "{}", c.text);
                    }
                    seen.push_str(&text);
                    let next = c.text.split("call again with offset=").nth(1).and_then(|r| r.split(|ch: char| !ch.is_ascii_digit()).next()?.parse().ok());
                    match next {
                        Some(n) if n > offset => offset = n,
                        _ => break,
                    }
                }
            }
        }
    }
    let read = scrutate::mcp::call_tool(&session, "read_code", &json!({"path": "notes.py"}), &settings, &Default::default());
    assert!(read.text.contains("is how answers show a token") && !read.text.contains("secret redacted"), "{}", read.text);
    // the Grep hook, as Claude Code's Grep would ask for every line
    let hook = |env: &[(&str, &str)], ask: Value| {
        let home = root.with_extension("home");
        let mut c = Command::new(BIN);
        c.args(["hook", "grep"]).current_dir(&root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        c.env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .env("LOCALAPPDATA", home.join("state"))
            .env("CLAUDE_PROJECT_DIR", &root)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env("SCRUTATE_MAX_BYTES", "1000000");
        for (k, v) in env {
            c.env(k, v);
        }
        let input = json!({"session_id": "s", "hook_event_name": "PreToolUse", "tool_name": "Grep", "cwd": root.to_str().unwrap(), "tool_input": ask, "tool_use_id": "t1", "permission_mode": "default"});
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
    };
    let every_line = json!({"pattern": ".", "output_mode": "content", "-n": true});
    let hooked = hook(&[], every_line.clone());
    assert!(hooked.contains("[redacted:"), "the hook answered: {hooked}");
    seen.push_str(&hooked);
    // a key's body as context lines (`keys.pem-3-`), with and without numbers
    for ask in [
        json!({"pattern": "BEGIN", "output_mode": "content", "-A": 8}),
        json!({"pattern": "BEGIN", "output_mode": "content", "-C": 2}),
        json!({"pattern": "BEGIN", "output_mode": "content", "-A": 8, "-n": false}),
    ] {
        let answer = hook(&[], ask.clone());
        assert!(answer.contains("[redacted:private-key]"), "{ask}: {answer}");
        for (kind, secret) in &secrets {
            assert!(!answer.contains(secret.as_str()), "{ask}: {kind} {secret} came back: {answer}");
        }
        seen.push_str(&answer);
    }
    for (kind, secret) in &secrets {
        let at = seen.lines().filter(|l| l.contains(secret.as_str())).take(3).collect::<Vec<_>>().join("\n");
        assert!(!seen.contains(secret.as_str()), "{kind}: {secret} came back:\n{at}");
    }
    // every kind is marked
    for (kind, _) in &secrets {
        assert!(kinds_seen.contains(kind) || hooked.contains(&format!("[redacted:{kind}]")), "{kind} is never marked");
    }
    // `hook run`, the search a rewritten Bash command asks: grep's own lines, context and a key's
    // last line alone among them
    #[cfg(unix)]
    for ask in [
        &["grep", "-rn", "-e", ".", "."][..],
        &["grep", "-rn", "-A8", "BEGIN", "."],
        &["grep", "-rn", "-e", "==$", "."],
        &["grep", "-a", "-rHn", "-e", ".", "."],
        &["rg", "-a", "-n", "."],
    ] {
        if ask[0] == "rg" && !rg::have_rg() {
            continue;
        }
        let home = root.with_extension("home");
        let out = Command::new(BIN)
            .args(["hook", "run", "--"])
            .args(ask)
            .current_dir(&root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(text.contains("[redacted:"), "hook run grep {ask:?}: {text}");
        for (kind, secret) in &secrets {
            assert!(!text.contains(secret.as_str()), "hook run grep {ask:?}: {kind} {secret} came back");
        }
        seen.push_str(&text);
    }
    // the language server: its answers name symbols and the files that hold them
    {
        use std::io::{BufRead, BufReader, Read};
        let mut child = Command::new(BIN).arg("lsp").current_dir(&root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let (mut stdin, mut stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        let mut id = 0;
        let mut ask = |method: &str, params: Value| -> Value {
            id += 1;
            let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
            write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            stdin.flush().unwrap();
            loop {
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    assert!(stdout.read_line(&mut line).unwrap() > 0, "lsp closed its output");
                    if line.trim_end().is_empty() {
                        break;
                    }
                    if let Some(v) = line.trim_end().strip_prefix("Content-Length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut buf = vec![0u8; len];
                stdout.read_exact(&mut buf).unwrap();
                let m: Value = serde_json::from_slice(&buf).unwrap();
                if m["id"] == id {
                    return m;
                }
            }
        };
        let uri = |p: &Path| format!("file://{}", p.to_string_lossy().replace('\\', "/"));
        ask("initialize", json!({"processId": null, "rootUri": uri(&root), "capabilities": {}}));
        let mut answers = String::new();
        for (method, params) in [
            ("workspace/symbol", json!({"query": "rotate"})),
            ("workspace/symbol", json!({"query": "SETTINGS"})),
            ("textDocument/documentSymbol", json!({"textDocument": {"uri": uri(&root.join("config.py"))}})),
        ] {
            strings(&ask(method, params), &mut answers);
        }
        let _ = child.kill();
        let _ = child.wait();
        for (kind, secret) in &secrets {
            assert!(!answers.contains(secret.as_str()), "lsp: {kind} {secret} came back: {answers}");
        }
    }
    assert!(seen.contains("secrets redacted: SCRUTATE_REDACT=off shows them"));
    // the documented opt-out returns raw text
    let raw = hook(&[("SCRUTATE_REDACT", "off")], every_line);
    // the one marker's text a file holds (notes.py), and every secret as written
    assert_eq!(raw.matches("[redacted:").count(), 1, "{raw}");
    for (kind, secret) in &secrets {
        assert!(raw.contains(secret.as_str()), "{kind}: {secret} missing from the raw answer");
    }
    let _ = fs::remove_dir_all(&root);
}

/// With SCRUTATE_REDACT=on the CLI's `--json` stays one JSON document: the closing line goes to
/// stderr.
#[test]
fn the_cli_s_json_stays_whole_when_it_redacts() {
    let (root, secrets) = tree("cli");
    let out = Command::new(BIN).args(["read", "config.py", "--json"]).current_dir(&root).env("SCRUTATE_REDACT", "on").stdin(Stdio::null()).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    assert!(v.to_string().contains("[redacted:"), "{text}");
    assert!(!secrets.iter().any(|(_, s)| text.contains(s.as_str())));
    assert!(String::from_utf8_lossy(&out.stderr).contains("secrets redacted"));
    // and its text form, a file that is not UTF-8 among them
    for file in ["config.py", "latin1.py", ".env"] {
        let out = Command::new(BIN).args(["read", file]).current_dir(&root).env("SCRUTATE_REDACT", "on").stdin(Stdio::null()).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("[redacted:"), "read {file}: {text}");
        for (kind, secret) in &secrets {
            assert!(!text.contains(secret.as_str()), "read {file}: {kind} {secret} came back");
        }
    }
    let _ = fs::remove_dir_all(&root);
}

/// Marks the repository may hold, each with why: none today. A secret-shaped literal in a tracked
/// file is refused by GitHub's push protection and kept forever by a public repository, so tests
/// build their samples at run time.
const ALLOWED: &[(&str, &str)] = &[];

/// No tracked file of the repository holds anything the redactor takes for a secret (scrutate run
/// on itself), but what `ALLOWED` names.
#[test]
fn no_tracked_file_holds_a_secret() {
    let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Ok(out) = Command::new("git").args(["ls-files", "-z"]).current_dir(&top).stdin(Stdio::null()).output() else { return };
    if !out.status.success() {
        return; // not a git checkout (a published crate): nothing to read
    }
    let mut found = Vec::new();
    for rel in out.stdout.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let rel = String::from_utf8_lossy(rel).into_owned();
        let Ok(bytes) = fs::read(top.join(&rel)) else { continue };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut r = scrutate::secrets::Redactor::default();
        for (i, line) in text.lines().enumerate() {
            let (redacted, marks) = r.redact(line);
            for m in marks {
                if !ALLOWED.iter().any(|(path, _)| *path == rel) {
                    found.push(format!("{rel}:{}: {}: {}", i + 1, m.kind, redacted.trim()));
                }
            }
        }
    }
    assert!(found.is_empty(), "secret-shaped text in tracked files (build it at run time, or add it to ALLOWED with why):\n{}", found.join("\n"));
}

/// docs/SECURITY.md lists every detector with the rule it comes from.
#[test]
fn the_security_doc_lists_every_detector_and_its_source() {
    let doc = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/SECURITY.md")).unwrap();
    let detectors = scrutate::secrets::detectors();
    for (kind, source) in &detectors {
        assert!(doc.contains(&format!("| `{kind}` | {source} |")), "{kind} ({source}) is not in SECURITY.md's table");
    }
    // and the table names no other
    let section = &doc[doc.find("## Secrets in answers").unwrap()..];
    let rows = section.lines().skip(1).take_while(|l| !l.starts_with("## ")).filter(|l| l.starts_with("| `")).count();
    assert_eq!(rows, detectors.len(), "SECURITY.md's table has rows no detector has");
}

/// A key in a file over 1 MB is redacted as the whole file reads it: a page from its middle, its
/// short last line found alone by grep_code and by the Grep hook.
#[test]
fn a_key_over_a_megabyte_is_redacted_from_its_middle_and_by_its_last_line() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_big_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let armour = |edge: &str| format!("-----{edge} RSA {}-----", "PRIVATE KEY");
    let (body, last) = (chars(&mut 5, &format!("{ALNUM}+/"), 64), format!("{}==", chars(&mut 6, ALNUM, 22)));
    let mut big = format!("{}\n", armour("BEGIN"));
    for _ in 0..24_000 {
        big.push_str(&body);
        big.push('\n');
    }
    big.push_str(&format!("{last}\n{}\n", armour("END")));
    assert!(big.len() > 1_500_000);
    fs::write(root.join("big.pem"), big).unwrap();
    let root = canon(&root);
    let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
    let settings = scrutate::mcp::Settings::new(None);
    for (tool, args) in [
        ("read_code", json!({"path": "big.pem", "start": 4, "end": 6})),
        ("read_code", json!({"path": "big.pem", "start": 24_001, "end": 24_003})),
        ("grep_code", json!({"pattern": "==$"})),
    ] {
        let c = scrutate::mcp::call_tool(&session, tool, &args, &settings, &Default::default());
        let mut text = c.text.clone();
        strings(&c.structured, &mut text);
        assert!(!text.contains(&last) && !text.contains(&body) && text.contains("[redacted:"), "{tool} {args}: {}", c.text);
    }
    let home = root.with_extension("home");
    let input = json!({"session_id": "s", "hook_event_name": "PreToolUse", "tool_name": "Grep", "cwd": root.to_str().unwrap(), "tool_input": {"pattern": "==$", "output_mode": "content", "-n": true}, "tool_use_id": "t1", "permission_mode": "default"});
    let mut c = Command::new(BIN);
    c.args(["hook", "grep"]).current_dir(&root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    c.env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("LOCALAPPDATA", home.join("state"))
        .env("CLAUDE_PROJECT_DIR", &root)
        .env_remove("CLAUDE_CONFIG_DIR");
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let hooked = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    assert!(!hooked.contains(&last) && hooked.contains("[redacted:private-key]"), "the hook answered: {hooked}");
    let _ = fs::remove_dir_all(&root);
}

/// A key's body lines are known by their text once indexed, so `hook run` finds them whatever the
/// program writes before them: no line number (`grep -r`, `rg` piped), a colon in the file name,
/// context lines.
#[cfg(unix)]
#[test]
fn an_indexed_key_s_lines_are_found_by_their_text_through_hook_run() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_known_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("keys")).unwrap();
    let armour = |edge: &str| format!("-----{edge} RSA {}-----", "PRIVATE KEY");
    let mut lines = Vec::new();
    for (i, name) in ["keys/k.pem", "keys/a:1:b.pem", "top.pem"].into_iter().enumerate() {
        let seed = &mut (40 + i as u64);
        let mut body: Vec<String> = (0..4).map(|_| format!("M{}", chars(seed, &format!("{ALNUM}+/"), 63))).collect();
        body.push(format!("{}==", chars(seed, ALNUM, 22)));
        fs::write(root.join(name), format!("{}\n{}\n{}\n", armour("BEGIN"), body.join("\n"), armour("END"))).unwrap();
        lines.extend(body);
    }
    let root = canon(&root);
    let home = root.with_extension("home");
    let run = |args: &[&str]| {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .env("SCRUTATE_SEMANTIC", "none")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&["index"]);
    for ask in [
        &["grep", "-r", "-e", "==$", "."][..],
        &["grep", "-rn", "-e", "==$", "."],
        &["grep", "-rn", "-A8", "BEGIN", "."],
        &["rg", "==$", "keys"],
        &["rg", "-n", "==$"],
        &["rg", "-n", "-A8", "BEGIN"],
    ] {
        if ask[0] == "rg" && !rg::have_rg() {
            continue;
        }
        let mut args = vec!["hook", "run", "--"];
        args.extend_from_slice(ask);
        let text = run(&args);
        assert!(text.contains("[redacted:private-key]"), "hook run {ask:?}: {text}");
        for line in &lines {
            assert!(!text.contains(line.as_str()), "hook run {ask:?}: {line} came back:\n{text}");
        }
    }
    let _ = fs::remove_dir_all(&root);
}

/// A recursive grep or rg through `hook run` leaves scrutate's own index directory out: its
/// caches hold the repository's text in binary records, where a secret runs straight into the next
/// field's bytes and no detector can tell where it ends.
#[cfg(unix)]
#[test]
fn hook_run_searches_nothing_under_the_index_directory() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_index_dir_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("app.py"), "print('hello')\n").unwrap();
    let secret = chars(&mut 62, &format!("{ALNUM}/+"), 40);
    // a tsquery cache record: a capture's text, then the next capture's line as eight bytes, the
    // first one a digit (line 52)
    let mut record = b"\0\0\0assignment@\0\0\0aws_secret_access_key = ".to_vec();
    record.extend_from_slice(secret.as_bytes());
    record.extend_from_slice(b"4\0\0\0\0\0\0\0\x01\0\0\0a\n");
    for dir in [".scrutate/tsquery", "sub/.scrutate/tsquery"] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(root.join(dir).join("c.bin"), &record).unwrap();
    }
    let home = root.with_extension("home");
    let mut asks = vec![vec!["grep", "-a", "-rHn", "-e", ".", "."], vec!["grep", "-a", "-R", "aws", "."], vec!["grep", "-a", "-r", "aws", "sub"]];
    if rg::have_rg() {
        asks.push(vec!["rg", "-a", "-uuu", "-n", "aws"]);
        asks.push(vec!["rg", "-a", "--no-ignore", "--hidden", "aws", "sub"]);
    }
    for ask in asks {
        let out = Command::new(BIN)
            .args(["hook", "run", "--piped", "--"])
            .args(&ask)
            .current_dir(&root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.contains(&secret), "hook run {ask:?}: the cache's secret came back:\n{text}");
    }
    let _ = fs::remove_dir_all(&root);
}

/// A grep or rg through `hook run` that names a path inside scrutate's index directory, as written,
/// absolute, through `..` or through a link, is refused with a line that says why; a search for
/// the text `.scrutate` is not.
#[cfg(unix)]
#[test]
fn hook_run_refuses_a_named_path_inside_the_index_directory() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_index_named_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("app.py"), "# see .scrutate for the index\n").unwrap();
    let secret = chars(&mut 63, &format!("{ALNUM}/+"), 40);
    let mut record = b"\0\0\0assignment@\0\0\0aws_secret_access_key = ".to_vec();
    record.extend_from_slice(secret.as_bytes());
    record.extend_from_slice(b"4\0\0\0\0\0\0\0\x01\0\0\0a\n");
    for dir in [".scrutate/tsquery", "sub/.scrutate/tsquery"] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(root.join(dir).join("c.bin"), &record).unwrap();
    }
    std::os::unix::fs::symlink(root.join(".scrutate/tsquery/c.bin"), root.join("cache-link")).unwrap();
    std::os::unix::fs::symlink(root.join(".scrutate"), root.join("index-link")).unwrap();
    let root = canon(&root);
    let abs = root.join(".scrutate/tsquery/c.bin").to_string_lossy().into_owned();
    let home = root.with_extension("home");
    let run = |ask: &[&str]| {
        let out = Command::new(BIN)
            .args(["hook", "run", "--piped", "--"])
            .args(ask)
            .current_dir(root.join("sub"))
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let mut asks: Vec<Vec<&str>> = vec![
        vec!["grep", "-a", "aws", ".scrutate/tsquery/c.bin"],
        vec!["grep", "-a", "-H", "aws", "../sub/.scrutate/tsquery/c.bin"],
        vec!["grep", "-a", "aws", "../.scrutate/tsquery/c.bin"],
        vec!["grep", "-a", "aws", &abs],
        vec!["grep", "-a", "aws", "../cache-link"],
        vec!["grep", "-a", "-r", "aws", "../index-link"],
        vec!["grep", "-a", "-e", "aws", "--", ".scrutate/tsquery/c.bin"],
    ];
    // a directory named, a name in another case, and a walk that follows a link into it
    asks.push(vec!["grep", "-a", "-r", "aws", ".scrutate"]);
    asks.push(vec!["grep", "-a", "aws", "../.SCRUTATE/tsquery/c.bin"]);
    if rg::have_rg() {
        asks.push(vec!["rg", "-a", "aws", ".scrutate/tsquery/c.bin"]);
        asks.push(vec!["rg", "-a", "aws", ".scrutate"]);
        asks.push(vec!["rg", "-a", "aws", "../index-link"]);
        asks.push(vec!["rg", "-a", "-uuu", "aws", "../index-link"]);
    }
    for ask in &asks {
        let (code, out, err) = run(ask);
        assert!(!out.contains(&secret), "hook run {ask:?}: the cache's secret came back:\n{out}");
        assert!(code == Some(2) && err.contains("index directory"), "hook run {ask:?}: {code:?} {err}");
    }
    // a search that follows the link into the index directory met in the tree is refused; one
    // that does not follow it runs, and never reads it
    let mut walks: Vec<Vec<&str>> = vec![vec!["grep", "-a", "-R", "aws", ".."], vec!["grep", "-a", "--dereference-recursive", "aws", ".."]];
    if rg::have_rg() {
        walks.push(vec!["rg", "-a", "-L", "aws", ".."]);
    }
    for ask in &walks {
        let (code, out, err) = run(ask);
        assert!(!out.contains(&secret) && code == Some(2) && err.contains("index directory"), "hook run {ask:?}: {code:?} {out}\n{err}");
    }
    let mut runs: Vec<Vec<&str>> = vec![vec!["grep", "-a", "-r", "aws", ".."]];
    if rg::have_rg() {
        runs.push(vec!["rg", "-a", "aws", ".."]);
    }
    for ask in &runs {
        let (code, out, err) = run(ask);
        assert!(!out.contains(&secret) && code == Some(1), "hook run {ask:?}: {code:?} {out}\n{err}");
    }
    // the text `.scrutate` is a pattern, not a path
    let (code, out, _) = run(&["grep", "-rn", ".scrutate", ".."]);
    assert!(code == Some(0) && out.contains("app.py"), "{code:?} {out}");
    let _ = fs::remove_dir_all(&root);
}

/// Whatever path a search takes to an index file (a glob that names the directory again, a chain of
/// links, one through a directory outside the root, a hard link), `hook run` leaves its lines out,
/// and refuses a search whose output names no file when it would reach one; the Grep hook refuses
/// such a path rather than let Grep search it.
#[cfg(unix)]
#[test]
fn hook_run_leaves_out_every_index_file_a_search_reaches() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_index_ways_{}", std::process::id()));
    let far = root.with_extension("far");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&far);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("other")).unwrap();
    fs::create_dir_all(&far).unwrap();
    fs::write(root.join("src/app.py"), "print('hello')\n").unwrap();
    let secret = chars(&mut 64, &format!("{ALNUM}/+"), 40);
    let mut record = b"\0\0\0assignment@\0\0\0aws_secret_access_key = ".to_vec();
    record.extend_from_slice(secret.as_bytes());
    record.extend_from_slice(b"4\0\0\0\0\0\0\0\x01\0\0\0a\n");
    fs::create_dir_all(root.join(".scrutate/tsquery")).unwrap();
    fs::write(root.join(".scrutate/tsquery/c.bin"), &record).unwrap();
    use std::os::unix::fs::symlink;
    // src/out -> ../other, other/inner -> ../.scrutate; src/far -> a directory outside that links back
    symlink("../other", root.join("src/out")).unwrap();
    symlink("../.scrutate", root.join("other/inner")).unwrap();
    symlink(root.join(".scrutate"), far.join("back")).unwrap();
    symlink(&far, root.join("src/far")).unwrap();
    symlink(".scrutate", root.join("idx")).unwrap();
    let home = root.with_extension("home");
    let run = |ask: &[&str]| {
        let out = Command::new(BIN)
            .args(["hook", "run", "--piped", "--"])
            .args(ask)
            .current_dir(&root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let mut asks: Vec<Vec<&str>> = vec![vec!["grep", "-a", "-R", "aws", "src"]];
    if rg::have_rg() {
        // a glob of the user's that names the directory again, in every spelling
        asks.push(vec!["rg", "-a", "-g", ".scrutate", "-g", "*.bin", "aws", "."]);
        asks.push(vec!["rg", "-uuu", "-a", "-g", ".scrutate", "-g", "*.bin", "aws", "."]);
        asks.push(vec!["rg", "--hidden", "--no-ignore", "-a", "-g", ".scrutate", "-g", "*.bin", "aws", "."]);
        asks.push(vec!["rg", "-a", "--iglob", ".SCRUTATE", "--iglob", "*.bin", "aws", "."]);
        asks.push(vec!["rg", "-a", "-L", "aws", "src"]);
    }
    for ask in &asks {
        let (_, out, _) = run(ask);
        assert!(!out.contains(&secret), "hook run {ask:?}: the cache's secret came back:\n{out}");
    }
    // a hard link under another name is refused, with file names in the output or without
    fs::hard_link(root.join(".scrutate/tsquery/c.bin"), root.join("hard.bin")).unwrap();
    for ask in [&["grep", "-a", "-r", "aws", "."][..], &["grep", "-a", "-h", "-r", "aws", "."]] {
        let (code, out, err) = run(ask);
        assert!(code == Some(2) && !out.contains(&secret) && err.contains("index directory"), "{ask:?}: {code:?} {out}\n{err}");
    }
    let (code, out, _) = run(&["grep", "-a", "aws", "hard.bin"]);
    assert!(code == Some(2) && !out.contains(&secret), "{code:?} {out}");
    // the Grep hook refuses the path, a file or a link, and says why
    for path in [".scrutate/tsquery/c.bin", "idx", "hard.bin"] {
        let input = json!({"session_id": "s", "hook_event_name": "PreToolUse", "tool_name": "Grep", "cwd": root.to_str().unwrap(),
            "tool_input": {"pattern": "aws", "path": path, "output_mode": "content"}, "tool_use_id": "t1", "permission_mode": "default"});
        let mut c = Command::new(BIN)
            .args(["hook", "grep"])
            .current_dir(&root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let out = String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap();
        assert!(out.contains("\"deny\"") && out.contains("index directory") && !out.contains(&secret), "Grep {path}: {out}");
    }
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&far);
}

/// The shapes that defeated reading the output's file names: each is refused before the search
/// runs, or comes back clean, whatever the output looks like.
#[cfg(unix)]
#[test]
fn hook_run_refuses_a_search_that_can_reach_an_index_file_whatever_its_output_shape() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_index_shapes_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for d in [".git", "src", ".scrutate/tsquery", "sub/.scrutate/tsquery"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    let secret = chars(&mut 65, &format!("{ALNUM}/+"), 40);
    let mut record = b"\0\0\0assignment@\0\0\0aws_secret_access_key = ".to_vec();
    record.extend_from_slice(secret.as_bytes());
    record.extend_from_slice(b"4\0\0\0\0\0\0\0\x01\0\0\0a\n");
    for d in [".scrutate/tsquery", "sub/.scrutate/tsquery"] {
        fs::write(root.join(d).join("c.bin"), &record).unwrap();
    }
    fs::write(root.join("src/app.py"), "print('hello aws')\n").unwrap();
    let home = root.with_extension("home");
    let run = |ask: &[&str]| {
        let out = Command::new(BIN)
            .args(["hook", "run", "--piped", "--"])
            .args(ask)
            .current_dir(&root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let check = |what: &str, ask: &[&str]| {
        let (code, out, err) = run(ask);
        assert!(!out.contains(&secret) && code == Some(2) && err.contains("index directory"), "{what}: {ask:?}: {code:?} {out}\n{err}");
    };
    let rg = rg::have_rg();
    let cache = root.join(".scrutate/tsquery/c.bin");
    // a hard link at the root, under every output shape
    fs::hard_link(&cache, root.join("hard.bin")).unwrap();
    check("hard link", &["grep", "-a", "-r", "aws", "."]);
    check("hard link, colour", &["grep", "-a", "-r", "--color=always", "aws", "."]);
    check("hard link, names only", &["grep", "-a", "-r", "-l", "aws", "."]);
    if rg {
        for shape in [&["--heading"][..], &["-n", "--heading"], &["--field-match-separator=@"], &["--color", "always"], &["--json"]] {
            let mut ask = vec!["rg", "-a"];
            ask.extend_from_slice(shape);
            ask.extend_from_slice(&["aws", "."]);
            check("hard link", &ask);
        }
    }
    fs::remove_file(root.join("hard.bin")).unwrap();
    // names a prefix reader splits wrong: a file `x` beside `x-c.bin`, seventeen dashes
    fs::write(root.join("x"), "").unwrap();
    fs::hard_link(&cache, root.join("x-c.bin")).unwrap();
    check("x beside x-c.bin", &["grep", "-a", "-r", "aws", "."]);
    if rg {
        check("x beside x-c.bin", &["rg", "-a", "aws", "."]);
    }
    fs::remove_file(root.join("x")).unwrap();
    fs::remove_file(root.join("x-c.bin")).unwrap();
    fs::hard_link(&cache, root.join("a-b-c-d-e-f-g-h-i-j-k-l-m-n-o-p-q-r.bin")).unwrap();
    check("seventeen dashes", &["grep", "-a", "-r", "aws", "."]);
    fs::remove_file(root.join("a-b-c-d-e-f-g-h-i-j-k-l-m-n-o-p-q-r.bin")).unwrap();
    // a file `my` beside a link `my-idx` to the index directory
    fs::write(root.join("my"), "").unwrap();
    symlink(".scrutate", root.join("my-idx")).unwrap();
    check("my beside my-idx", &["grep", "-a", "-R", "aws", "."]);
    if rg {
        check("my beside my-idx", &["rg", "-a", "-L", "aws", "."]);
        check("my-idx, another path separator", &["rg", "-a", "-L", "--path-separator", "#", "aws", "."]);
    }
    fs::remove_file(root.join("my")).unwrap();
    fs::remove_file(root.join("my-idx")).unwrap();
    // a name that is not UTF-8, where rg's JSON gives `path.bytes`
    let odd = root.join(std::ffi::OsStr::from_bytes(b"n\xff.bin"));
    if fs::hard_link(&cache, &odd).is_ok() {
        check("a name that is not UTF-8", &["grep", "-a", "-r", "aws", "."]);
        if rg {
            check("a name that is not UTF-8", &["rg", "-a", "--json", "aws", "."]);
        }
        fs::remove_file(&odd).unwrap();
    }
    // a hard link at the root to a nested repository's index file
    fs::hard_link(root.join("sub/.scrutate/tsquery/c.bin"), root.join("subhard.bin")).unwrap();
    check("hard link to a nested index", &["grep", "-a", "-r", "aws", "."]);
    if rg {
        check("hard link to a nested index", &["rg", "-a", "aws", "."]);
    }
    fs::remove_file(root.join("subhard.bin")).unwrap();
    // a `--` that is a flag's value ends no flags: the `-r` after it still makes grep walk `.`
    fs::hard_link(&cache, root.join("hard.bin")).unwrap();
    check("-e --, then -r", &["grep", "-a", "-e", "--", "-r"]);
    fs::remove_file(root.join("hard.bin")).unwrap();
    // with none of those, the same searches run and find the source file
    let (code, out, _) = run(&["grep", "-a", "-r", "aws", "."]);
    assert!(code == Some(0) && out.contains("app.py") && !out.contains(&secret), "{code:?} {out}");
    let _ = fs::remove_dir_all(&root);
}

/// Every flag the allow-list lets `rg` and `grep` take, against every way a search can reach an
/// index file: each search that could reach one is refused before it runs, naming why, and the
/// record's secret is in neither stream; in a tree without one, no part of a planted key comes out
/// raw, whatever the pattern marks or replaces.
#[cfg(unix)]
#[test]
fn hook_run_holds_for_every_allowed_flag() {
    use scrutate::shellhook::{allowed_flags, Takes};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_every_flag_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let secret = chars(&mut 91, &format!("{ALNUM}/+"), 40);
    let key = format!("AKIA{}", chars(&mut 92, "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 16));
    let key_secret = chars(&mut 93, &format!("{ALNUM}/+"), 40);
    let mut record = b"\0\0\0assignment@\0\0\0aws_secret_access_key = ".to_vec();
    record.extend_from_slice(secret.as_bytes());
    record.extend_from_slice(b"4\0\0\0\0\0\0\0\x01\0\0\0a\n");
    let tree = |name: &str| {
        let r = base.join(name);
        for d in [".git", "src", ".scrutate/tsquery"] {
            fs::create_dir_all(r.join(d)).unwrap();
        }
        fs::write(r.join(".scrutate/tsquery/c.bin"), &record).unwrap();
        fs::write(r.join("src/app.py"), "print('hello aws')\n").unwrap();
        r
    };
    // (tree, the path searched, whether only a search that follows links met in the tree reaches
    // the record)
    let mut reach: Vec<(PathBuf, &str, bool)> = vec![(tree("dir"), ".scrutate", false)];
    let r = tree("link");
    symlink(".scrutate", r.join("idx")).unwrap();
    reach.push((r.clone(), ".", true));
    reach.push((r, "idx", false));
    let r = tree("chain");
    symlink(".scrutate", r.join("b")).unwrap();
    symlink("b", r.join("a")).unwrap();
    reach.push((r.clone(), ".", true));
    reach.push((r, "a", false));
    let r = tree("hard");
    fs::hard_link(r.join(".scrutate/tsquery/c.bin"), r.join("hard.bin")).unwrap();
    reach.push((r, ".", false));
    let r = tree("nested");
    fs::create_dir_all(r.join("sub/.scrutate/tsquery")).unwrap();
    fs::write(r.join("sub/.scrutate/tsquery/c.bin"), &record).unwrap();
    fs::hard_link(r.join("sub/.scrutate/tsquery/c.bin"), r.join("subhard.bin")).unwrap();
    reach.push((r, ".", false));
    for (i, name) in [&b"a-1-c.bin"[..], b"a:1:c.bin", b"a b.bin", b"n\xff.bin"].into_iter().enumerate() {
        let r = tree(&format!("name{i}"));
        if fs::hard_link(r.join(".scrutate/tsquery/c.bin"), r.join(std::ffi::OsStr::from_bytes(name))).is_ok() {
            reach.push((r, ".", false));
        }
    }
    let clean = tree("clean");
    fs::write(clean.join("conf.py"), format!("aws_access_key_id = \"{key}\"\naws_secret_access_key = \"{key_secret}\"\n")).unwrap();
    let patterns = base.join("patterns.txt");
    fs::write(&patterns, "aws\n").unwrap();
    let empty = base.join("empty.txt");
    fs::write(&empty, "").unwrap();
    let home = base.join("home");
    let run = |dir: &Path, ask: &[String]| {
        let out = Command::new(BIN)
            .args(["hook", "run", "--piped", "--"])
            .args(ask)
            .current_dir(dir)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let mut programs = vec!["grep"];
    if rg::have_rg() {
        programs.push("rg");
    }
    let (mut asked, mut ran, mut leaks) = (0, 0, Vec::new());
    for program in programs {
        for (flag, takes) in allowed_flags(program) {
            let value = match (program, flag.as_str()) {
                (_, "-e" | "--regexp") => "aws",
                (_, "-f" | "--file") => patterns.to_str().unwrap(),
                (_, "--exclude-from" | "--ignore-file") => empty.to_str().unwrap(),
                ("grep", "-d" | "--directories") => "recurse",
                ("grep", "-D" | "--devices") => "read",
                (_, "--binary-files") => "text",
                ("rg", "-E" | "--encoding") => "utf-8",
                (_, "--engine") => "default",
                (_, "--max-filesize") => "10M",
                ("rg", "-M") => "1000",
                ("rg", "-r") => "X",
                ("rg", "-t" | "--type") => "py",
                ("rg", "-T" | "--type-not") => "c",
                (_, "-g" | "--glob" | "--iglob" | "--include") => "*",
                (_, "--exclude" | "--exclude-dir") => "none",
                (_, "--label") => "stdin",
                (_, "--group-separator") => "~",
                (_, "--color" | "--colour") => "always",
                _ => "5",
            };
            let mut ask = vec![program.to_string()];
            match takes {
                Takes::No => ask.push(flag.clone()),
                Takes::Value => ask.extend([flag.clone(), value.to_string()]),
                Takes::Joined => ask.push(format!("{flag}={value}")),
            }
            let pattern_given = takes != Takes::No && matches!(flag.as_str(), "-e" | "--regexp" | "-f" | "--file");
            let follows = matches!((program, flag.as_str()), ("grep", "-R" | "--dereference-recursive") | ("rg", "-L" | "--follow"));
            let with = |pattern: &str, path: &str| {
                let mut ask = ask.clone();
                ask.extend((!pattern_given).then(|| pattern.to_string()));
                ask.push(path.into());
                ask
            };
            for (dir, path, by_following) in &reach {
                let ask = with("aws", path);
                let (code, out, err) = run(dir, &ask);
                asked += 1;
                assert!(!out.contains(&secret) && !err.contains(&secret), "{ask:?} in {dir:?}: the record's secret came back:\n{out}\n{err}");
                if follows || !by_following {
                    assert!(code == Some(2) && err.contains("index directory"), "{ask:?} in {dir:?}: not refused: {code:?} {out}\n{err}");
                }
            }
            // a pattern inside a key is where a flag that marks or replaces the match cuts it
            for pattern in ["aws", "AKIA", &key_secret[16..20]] {
                let ask = with(pattern, ".");
                let (code, out, err) = run(&clean, &ask);
                asked += 1;
                ran += usize::from(code == Some(0));
                let pieces = [&key[4..], &key_secret[..10], &key_secret[30..], &secret[..]];
                if let Some(line) = out.lines().chain(err.lines()).find(|l| pieces.iter().any(|p| l.contains(p))) {
                    leaks.push(format!("{ask:?}: {}", line.escape_debug()));
                }
                if pattern_given {
                    break;
                }
            }
        }
    }
    assert!(leaks.is_empty(), "a key came back raw:\n{}", leaks.join("\n"));
    // the table is not empty and most of its searches did run in the clean tree
    assert!(asked > 500 && ran * 2 > asked / (reach.len() + 3) * 3, "{asked} asked, {ran} ran");
    let _ = fs::remove_dir_all(&base);
}

/// A line that is not UTF-8 (a stray byte, or a long line's piece cut inside a character) has its
/// secrets redacted by `hook run` too.
#[cfg(unix)]
#[test]
fn hook_run_redacts_a_line_that_is_not_utf8() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_bytes_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let token = format!("{}{}", "ghp_", chars(&mut 60, ALNUM, 36));
    let mut stray = format!("GITHUB = \"{token}\"  # caf").into_bytes();
    stray.extend_from_slice(b"\xe9\n");
    fs::write(root.join("stray.py"), stray).unwrap();
    // 1 byte, then two-byte characters: the 1 MiB piece boundary falls inside one
    let long = format!("x{} GITHUB = \"{token}\"\n", "\u{e9}".repeat(600_000));
    fs::write(root.join("long.txt"), long).unwrap();
    // a file name that is not UTF-8 and holds a key, as find prints it
    use std::os::unix::ffi::OsStrExt;
    let key = format!("{}{}", "AKIA", chars(&mut 61, "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 16));
    fs::create_dir_all(root.join("n")).unwrap();
    // APFS refuses such a name: the case runs where the file system takes it
    let named = fs::write(root.join("n").join(std::ffi::OsStr::from_bytes(&[key.as_bytes(), b"-\xe9"].concat())), "").is_ok();
    let home = root.with_extension("home");
    let run = |args: &[&str], env: &[(&str, &str)]| {
        let mut c = Command::new(BIN);
        c.args(["hook", "run", "--piped", "--"])
            .args(args)
            .current_dir(&root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .stdin(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        String::from_utf8_lossy(&c.output().unwrap().stdout).into_owned()
    };
    for (args, env, secret, kind) in [
        // under a UTF-8 locale GNU grep calls the file binary and prints no line
        (&["grep", "-n", "GITHUB", "stray.py"][..], &[("LC_ALL", "C")][..], &token, "github-token"),
        (&["grep", "-n", "GITHUB", "long.txt"], &[], &token, "github-token"),
        (&["grep", "-a", "-rn", "GITHUB", "."], &[], &token, "github-token"),
        (&["grep", "-rn", "GITHUB", "."], &[("LC_ALL", "C")], &token, "github-token"),
        (&["find", "n", "-name", "AKIA*"], &[], &key, "aws-access-key-id"),
    ] {
        if args[0] == "find" && !named {
            continue;
        }
        let text = run(args, env);
        assert!(
            !text.contains(secret.as_str()) && text.contains(&format!("[redacted:{kind}]")),
            "{args:?} {env:?}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    let _ = fs::remove_dir_all(&root);
}
