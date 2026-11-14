//! Secret-safe answers: a tree with three planted secrets of each kind scrutate redacts, `.env`
//! files and private keys among them, asked through every MCP tool that returns text and through
//! the Grep hook. No secret's bytes may come back, and each redaction says its kind.

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
        add("generic-secret", "config.py", &|s| format!("client_secret = \"{s}\""), chars(s, ALNUM, 18));
        add("private-key", "keys.pem", &|s| s.to_string(), chars(s, ALNUM, 64));
    }
    out
}

/// The planted tree: code that uses the secrets, a `.env`, and three private keys.
fn tree() -> (PathBuf, Vec<(&'static str, String)>) {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets_{}", std::process::id()));
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
            f.push_str(&format!("{}\n{line}\n{}\n{}\n", armour("BEGIN"), &line[..48], armour("END")));
        } else {
            f.push_str(&line);
            f.push('\n');
        }
        secrets.push((kind, secret));
    }
    for (name, text) in files {
        fs::write(root.join(name), text).unwrap();
    }
    (fs::canonicalize(&root).unwrap(), secrets)
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
    let (root, secrets) = tree();
    assert!(secrets.len() >= 60, "{}", secrets.len());
    let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
    let settings = scrutate::mcp::Settings::new(None);
    let mut seen = String::new();
    // a page of 24,000 bytes, and pages of 700 that cut answers between a key's armour lines
    for size in [24_000, 700] {
        let mut call = |tool: &str, args: Value| {
            let mut offset = 0;
            loop {
                let mut a = args.clone();
                a["max_bytes"] = json!(size);
                a["offset"] = json!(offset);
                let c = scrutate::mcp::call_tool(&session, tool, &a, &settings, &Default::default());
                seen.push_str(&c.text);
                strings(&c.structured, &mut seen);
                let next = c.text.split("call again with offset=").nth(1).and_then(|r| r.split(|ch: char| !ch.is_ascii_digit()).next()?.parse().ok());
                match next {
                    Some(n) if n > offset => offset = n,
                    _ => break,
                }
            }
        };
        for file in ["config.py", ".env", "keys.pem"] {
            call("read_code", json!({"path": file, "start": 1, "end": 400}));
            call("outline", json!({"path": file}));
        }
        for pattern in [".", "=", "KEY", "-----"] {
            call("grep_code", json!({"pattern": pattern, "max_results": 1000}));
        }
        for query in ["secret token key password", "aws github slack stripe", "private key"] {
            call("search_code", json!({"query": query}));
        }
        for name in ["settings", "client_secret", "SETTINGS"] {
            call("find_symbol", json!({"name": name}));
            call("references", json!({"name": name}));
            call("callers", json!({"name": name}));
            call("callees", json!({"name": name}));
        }
        call("similar_code", json!({"target": "config.py:4"}));
        call("repo_map", json!({}));
        call("dead_code", json!({}));
        call("find_files", json!({"query": "env"}));
        // a key's body without its armour: a range that starts inside it, a search for its lines
        call("read_code", json!({"path": "keys.pem", "start": 2, "end": 3}));
        call("grep_code", json!({"pattern": "^[A-Za-z0-9]{40,}$", "max_results": 1000}));
        drop(call);
    }
    // the Grep hook, as Claude Code's Grep would ask for every line
    let hook = |env: &[(&str, &str)]| {
        let home = root.with_extension("home");
        let mut c = Command::new(BIN);
        c.args(["hook", "grep"]).current_dir(&root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        c.env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .env("CLAUDE_PROJECT_DIR", &root)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env("SCRUTATE_MAX_BYTES", "1000000");
        for (k, v) in env {
            c.env(k, v);
        }
        let input = json!({"session_id": "s", "hook_event_name": "PreToolUse", "tool_name": "Grep", "cwd": root.to_str().unwrap(), "tool_input": {"pattern": ".", "output_mode": "content", "-n": true}, "tool_use_id": "t1", "permission_mode": "default"});
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
    };
    let hooked = hook(&[]);
    assert!(hooked.contains("[redacted:"), "the hook answered: {hooked}");
    seen.push_str(&hooked);
    for (kind, secret) in &secrets {
        assert!(!seen.contains(secret.as_str()), "{kind}: {secret} came back");
    }
    // every kind is marked, in the text and in the structured form's counts
    for (kind, _) in &secrets {
        assert!(seen.contains(&format!("[redacted:{kind}]")), "{kind} is never marked");
    }
    assert!(seen.contains("secrets redacted: SCRUTATE_REDACT=off shows them"));
    // the documented opt-out returns raw text
    let raw = hook(&[("SCRUTATE_REDACT", "off")]);
    assert!(
        !raw.contains("[redacted:") && secrets.iter().filter(|(k, _)| *k != "env-secret" && *k != "database-password").all(|(_, s)| raw.contains(s.as_str())),
        "{raw}"
    );
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
