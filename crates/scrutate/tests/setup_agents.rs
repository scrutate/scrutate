//! `scrutate setup <agent>` in scratch homes: one entry per agent, a second run that changes
//! nothing, `--uninstall` back to the same bytes, comments kept, a file that does not parse left
//! alone, `--dry-run` that writes nothing, the project files, the hook and the instruction block;
//! the command written answers MCP, and each installed agent's own listing sees the server.
//! Every program runs with HOME, CODEX_HOME and the XDG directories in the build's temporary
//! directory, so no real configuration is read or written.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
const AGENTS: [&str; 5] = ["claude", "codex", "opencode", "cursor", "gemini"];

struct Scratch {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("setup_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        for d in ["home", "codex", "config", "state", "data", "repo/.git", "repo/sub"] {
            fs::create_dir_all(base.join(d)).unwrap();
        }
        let base = fs::canonicalize(base).unwrap();
        fs::write(base.join("repo/m.py"), "def needle():\n    return 1\n").unwrap();
        Scratch { home: base.join("home"), repo: base.join("repo"), base }
    }

    /// `program` with the scratch homes, no variable of the Claude Code running these tests, and
    /// `path` as its PATH.
    fn command(&self, program: &str, path: &str) -> Command {
        // Windows reads the home from USERPROFILE and app data from APPDATA and LOCALAPPDATA
        let vars = [
            ("HOME", self.home.clone()),
            ("USERPROFILE", self.home.clone()),
            ("APPDATA", self.base.join("data")),
            ("LOCALAPPDATA", self.base.join("state")),
            ("CODEX_HOME", self.base.join("codex")),
            ("XDG_CONFIG_HOME", self.base.join("config")),
            ("XDG_STATE_HOME", self.base.join("state")),
            ("XDG_DATA_HOME", self.base.join("data")),
        ];
        // both sides canonical: on Windows that adds the verbatim prefix to one of them only
        let tmp = fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap();
        for (_, v) in &vars {
            assert!(v.starts_with(&tmp), "{} is not a scratch directory", v.display());
        }
        let mut c = Command::new(program);
        for (k, _) in std::env::vars() {
            if k.starts_with("CLAUDE")
                || k.starts_with("SCRUTATE_")
                || k.starts_with("CURSOR")
                || k.starts_with("CODEX")
                || k.starts_with("GEMINI")
                || k.starts_with("OPENCODE")
            {
                c.env_remove(k);
            }
        }
        c.envs(vars).env("PATH", path).current_dir(&self.repo).stdin(Stdio::null());
        c
    }

    fn setup(&self, args: &[&str]) -> (i32, String, String) {
        self.setup_in(&self.repo, args, "/usr/bin:/bin")
    }

    fn setup_in(&self, dir: &Path, args: &[&str], path: &str) -> (i32, String, String) {
        let out = self.command(BIN, path).arg("setup").args(args).current_dir(dir).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    /// The user's file of `agent`.
    fn user_file(&self, agent: &str) -> PathBuf {
        match agent {
            "claude" => self.home.join(".claude.json"),
            "codex" => self.base.join("codex/config.toml"),
            "opencode" => self.base.join("config/opencode/opencode.json"),
            "cursor" => self.home.join(".cursor/mcp.json"),
            _ => self.home.join(".gemini/settings.json"),
        }
    }

    fn write(&self, p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The binary's path as setup writes it: canonical, without Windows' verbatim prefix.
fn bin() -> String {
    fs::canonicalize(BIN).unwrap().to_str().unwrap().trim_start_matches(r"\\?\").to_string()
}

/// The command line a file of `agent` holds for the scrutate server.
fn command_line(agent: &str, text: &str) -> Vec<String> {
    let strs = |v: &Value| v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect::<Vec<_>>();
    match agent {
        "codex" => {
            let t: toml::Table = text.parse().unwrap();
            let e = &t["mcp_servers"]["scrutate"];
            let mut v = vec![e["command"].as_str().unwrap().to_string()];
            v.extend(e["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()));
            v
        }
        "opencode" => {
            let v: Value = serde_json::from_str(text).unwrap();
            let e = v.pointer("/mcp/scrutate").or_else(|| v.pointer("/mcp/servers/scrutate")).unwrap();
            strs(&e["command"])
        }
        _ => {
            let v: Value = serde_json::from_str(text).unwrap();
            let e = &v["mcpServers"]["scrutate"];
            let mut c = vec![e["command"].as_str().unwrap().to_string()];
            c.extend(strs(&e["args"]));
            c
        }
    }
}

#[test]
fn each_agent_gets_one_entry_and_uninstall_takes_it_back() {
    let s = Scratch::new("user");
    for agent in AGENTS {
        let file = s.user_file(agent);
        let (code, out, err) = s.setup(&[agent]);
        assert_eq!(code, 0, "{agent}: {err}");
        assert!(out.contains(&file.display().to_string()), "{agent}: {out}");
        let first = fs::read_to_string(&file).unwrap();
        assert_eq!(command_line(agent, &first), vec![bin(), "serve".into()], "{agent}");
        let (code, out, _) = s.setup(&[agent]);
        assert_eq!(code, 0);
        assert!(out.contains("already has"), "{agent}: {out}");
        assert_eq!(fs::read_to_string(&file).unwrap(), first, "{agent}: a second run changes nothing");
        let (code, out, err) = s.setup(&[agent, "--uninstall"]);
        assert_eq!(code, 0, "{agent}: {err}");
        assert!(out.contains("removed"), "{agent}: {out}");
        assert!(!file.exists(), "{agent}: the file setup made is gone");
    }
    let claude: Value = {
        s.setup(&["claude"]);
        serde_json::from_str(&fs::read_to_string(s.user_file("claude")).unwrap()).unwrap()
    };
    assert_eq!(claude["mcpServers"]["scrutate"]["type"], "stdio");
    s.setup(&["opencode"]);
    let oc: Value = serde_json::from_str(&fs::read_to_string(s.user_file("opencode")).unwrap()).unwrap();
    assert_eq!(oc["mcp"]["scrutate"]["type"], "local");
    assert_eq!(oc["mcp"]["scrutate"]["enabled"], true);
}

#[test]
fn every_other_byte_and_comment_stays_and_uninstall_restores_the_file() {
    let s = Scratch::new("keep");
    let files: [(&str, PathBuf, &str); 5] = [
        ("claude", s.user_file("claude"), "{\n  \"numStartups\": 3,\n  \"mcpServers\": {\n    \"other\": {\n      \"type\": \"stdio\",\n      \"command\": \"x\",\n      \"args\": []\n    }\n  },\n  \"theme\": \"dark\"\n}"),
        ("codex", s.user_file("codex"), "# my settings\nmodel = \"o4\" # the model\n\n[mcp_servers.other]\n# a server of mine\ncommand = \"y\"\nargs = [\"z\"]\n\n[profiles.fast]\nmodel = \"mini\"\n"),
        ("opencode", s.base.join("config/opencode/opencode.jsonc"), "// opencode settings\n{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  /* mine */\n  \"mcp\": {\n    \"local-one\": {\"type\": \"local\", \"command\": [\"a\"], \"enabled\": false}, // keep\n  },\n}\n"),
        ("cursor", s.user_file("cursor"), "{\"mcpServers\": {}}"),
        ("gemini", s.user_file("gemini"), "{\n\t\"theme\": \"GitHub\",\n\t\"general\": {\"vimMode\": true}\n}\n"),
    ];
    for (agent, file, before) in &files {
        s.write(file, before);
        let (code, _, err) = s.setup(&[agent]);
        assert_eq!(code, 0, "{agent}: {err}");
        let after = fs::read_to_string(file).unwrap();
        for comment in ["# my settings", "# the model", "# a server of mine", "// opencode settings", "/* mine */", "// keep"] {
            assert_eq!(before.contains(comment), after.contains(comment), "{agent}: {comment}");
        }
        if *agent == "opencode" {
            assert!(after.contains(&format!("\"command\": [\n        {},\n        \"serve\"\n      ]", Value::from(bin()))), "{after}");
        } else {
            assert_eq!(command_line(agent, &after), vec![bin(), "serve".into()], "{agent}");
        }
        let (code, _, _) = s.setup(&[agent]);
        assert_eq!(code, 0);
        assert_eq!(&fs::read_to_string(file).unwrap(), &after, "{agent}: one entry");
        let (code, _, err) = s.setup(&[agent, "--uninstall"]);
        assert_eq!(code, 0, "{agent}: {err}");
        assert_eq!(&fs::read_to_string(file).unwrap(), before, "{agent}: the bytes before setup");
        let (code, out, _) = s.setup(&[agent, "--uninstall"]);
        assert_eq!(code, 0);
        assert!(out.contains("has no"), "{agent}: {out}");
    }
}

#[test]
fn opencode_keeps_the_shape_its_file_already_uses() {
    let s = Scratch::new("ocv2");
    let file = s.user_file("opencode");
    let before = "{\n  \"mcp\": {\n    \"servers\": {\n      \"mine\": {\"type\": \"local\", \"command\": [\"m\"]}\n    }\n  }\n}\n";
    s.write(&file, before);
    assert_eq!(s.setup(&["opencode"]).0, 0);
    let v: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert!(v.pointer("/mcp/scrutate").is_none());
    assert_eq!(v["mcp"]["servers"]["scrutate"]["command"][1], "serve");
    assert!(v["mcp"]["servers"]["scrutate"].get("enabled").is_none());
    assert_eq!(s.setup(&["opencode", "--uninstall"]).0, 0);
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn a_file_that_does_not_parse_is_left_alone_and_a_dry_run_writes_nothing() {
    let s = Scratch::new("bad");
    for (agent, text) in [("cursor", "{\"mcpServers\": {,}"), ("codex", "model = [unclosed\n"), ("gemini", "not json"), ("opencode", "{\"mcp\": 3}")] {
        let file = s.user_file(agent);
        s.write(&file, text);
        let (code, _, err) = s.setup(&[agent]);
        assert_eq!(code, 2, "{agent}");
        assert!(err.contains(&file.display().to_string()) && (err.contains("does not parse") || err.contains("not an object")), "{agent}: {err}");
        assert_eq!(fs::read_to_string(&file).unwrap(), text, "{agent}");
        fs::remove_file(&file).unwrap();
    }
    let s = Scratch::new("dry");
    for agent in AGENTS {
        let (code, out, err) = s.setup(&[agent, "--dry-run", "--instructions"]);
        assert_eq!(code, 0, "{agent}: {err}");
        assert!(out.contains("would add") && out.contains(&s.user_file(agent).display().to_string()) && out.contains("  + "), "{agent}: {out}");
    }
    assert_eq!(fs::read_dir(&s.home).unwrap().count(), 0, "nothing written in the home");
    assert_eq!(fs::read_dir(s.base.join("codex")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(s.base.join("config")).unwrap().count(), 0);
}

#[test]
fn project_files_name_the_repository_and_codex_has_none() {
    let s = Scratch::new("project");
    let sub = s.repo.join("sub");
    let root = s.repo.to_str().unwrap().trim_start_matches(r"\\?\").to_string();
    for (agent, rel) in [("claude", ".mcp.json"), ("opencode", "opencode.json"), ("cursor", ".cursor/mcp.json"), ("gemini", ".gemini/settings.json")] {
        let (code, _, err) = s.setup_in(&sub, &[agent, "--project"], "/usr/bin:/bin");
        assert_eq!(code, 0, "{agent}: {err}");
        let text = fs::read_to_string(s.repo.join(rel)).unwrap();
        assert_eq!(command_line(agent, &text), vec![bin(), "--root".into(), root.clone(), "serve".into()], "{agent}");
        assert_eq!(s.setup_in(&sub, &[agent, "--project", "--uninstall"], "/usr/bin:/bin").0, 0);
        assert!(!s.repo.join(rel).exists(), "{agent}");
    }
    let (code, _, err) = s.setup(&["codex", "--project"]);
    assert_eq!(code, 2);
    assert!(err.contains("Codex has no project configuration file"), "{err}");
    assert_eq!(fs::read_dir(&s.home).unwrap().count(), 0, "a project setup writes nothing in the home");
}

#[test]
fn the_hook_and_the_instructions_come_only_when_asked_and_go_with_uninstall() {
    let s = Scratch::new("hook");
    let settings = s.home.join(".claude/settings.json");
    let claude_md = s.home.join(".claude/CLAUDE.md");
    assert_eq!(s.setup(&["claude"]).0, 0);
    assert!(!settings.exists() && !claude_md.exists(), "no hook and no instructions unless asked");
    let before_settings = "{\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"matcher\": \"Bash\",\n        \"hooks\": [{\"type\": \"command\", \"command\": \"echo\"}]\n      }\n    ]\n  },\n  \"model\": \"opus\"\n}\n";
    let before_md = "# My rules\n\nBe brief.";
    s.write(&settings, before_settings);
    s.write(&claude_md, before_md);
    let (code, out, err) = s.setup(&["claude", "--hook", "--instructions"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Grep hook"), "{out}");
    let v: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    let ours = &v["hooks"]["PreToolUse"][1];
    assert_eq!(ours["matcher"], "Grep");
    assert_eq!(ours["hooks"][0]["type"], "command");
    // on Windows a path both Git Bash and PowerShell run as written (the runners' has no space)
    let run = if cfg!(windows) { bin().replace('\\', "/") } else { bin() };
    assert_eq!(ours["hooks"][0]["command"], format!("{run} hook grep"));
    assert_eq!(ours["hooks"][0]["timeout"], 10);
    let md = fs::read_to_string(&claude_md).unwrap();
    assert!(md.starts_with(before_md) && md.contains("<!-- scrutate:begin -->") && md.contains("grep_code"), "{md}");
    let (code, _, _) = s.setup(&["claude", "--hook", "--instructions"]);
    assert_eq!(code, 0);
    assert_eq!(fs::read_to_string(&claude_md).unwrap(), md, "one block");
    assert_eq!(serde_json::from_str::<Value>(&fs::read_to_string(&settings).unwrap()).unwrap()["hooks"]["PreToolUse"].as_array().unwrap().len(), 2, "one hook");
    let (code, _, err) = s.setup(&["claude", "--uninstall", "--hook", "--instructions"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), before_settings);
    assert_eq!(fs::read_to_string(&claude_md).unwrap(), before_md);
    assert!(!s.user_file("claude").exists());

    // a fresh home: the files setup made are deleted again
    fs::remove_file(&settings).unwrap();
    fs::remove_file(&claude_md).unwrap();
    assert_eq!(s.setup(&["claude", "--project", "--hook", "--instructions"]).0, 0);
    assert!(s.repo.join(".claude/settings.json").is_file() && s.repo.join("CLAUDE.md").is_file());
    assert_eq!(s.setup(&["claude", "--project", "--hook", "--instructions", "--uninstall"]).0, 0);
    assert!(!s.repo.join(".claude/settings.json").exists() && !s.repo.join("CLAUDE.md").exists() && !s.repo.join(".mcp.json").exists());

    for agent in ["codex", "opencode", "cursor", "gemini"] {
        let (code, out, _) = s.setup(&[agent, "--hook", "--dry-run"]);
        assert_eq!(code, 0);
        assert!(out.contains("no hook was installed"), "{agent}: {out}");
    }
    let (_, out, _) = s.setup(&["gemini", "--dry-run"]);
    assert!(out.contains("trust"), "{out}");
}

#[test]
fn a_plain_uninstall_takes_out_the_server_the_hook_and_the_block() {
    let s = Scratch::new("plainuninstall");
    let settings = s.home.join(".claude/settings.json");
    let claude_md = s.home.join(".claude/CLAUDE.md");
    let before_settings = "{\n  \"model\": \"opus\"\n}\n";
    let before_md = "# My rules\n";
    s.write(&settings, before_settings);
    s.write(&claude_md, before_md);
    assert_eq!(s.setup(&["claude", "--hook", "--instructions"]).0, 0);
    let (code, out, _) = s.setup(&["claude", "--uninstall", "--dry-run"]);
    assert_eq!(code, 0);
    for part in ["mcpServers.scrutate", "Grep hook", "scrutate block"] {
        assert!(out.contains(part), "{part}: {out}");
    }
    assert!(fs::read_to_string(&settings).unwrap().contains("hook grep"), "a dry run writes nothing");
    let (code, _, err) = s.setup(&["claude", "--uninstall"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), before_settings);
    assert_eq!(fs::read_to_string(&claude_md).unwrap(), before_md);
    assert!(!s.user_file("claude").exists());

    // a hook of a scrutate binary that has moved since goes too, and the project files setup made
    let moved = "{\n  \"hooks\": {\n    \"PreToolUse\": [{\"matcher\": \"Grep\", \"hooks\": [{\"type\": \"command\", \"command\": \"/old/place/scrutate hook grep\", \"timeout\": 10}]}]\n  }\n}\n";
    s.write(&settings, moved);
    assert_eq!(s.setup(&["claude", "--uninstall"]).0, 0);
    assert_eq!(fs::read_to_string(&settings).unwrap(), "{\n  \"hooks\": {\n    \"PreToolUse\": []\n  }\n}\n", "only the hook");
    assert_eq!(s.setup(&["claude", "--project", "--hook", "--instructions"]).0, 0);
    assert_eq!(s.setup(&["claude", "--project", "--uninstall"]).0, 0);
    assert!(!s.repo.join(".claude/settings.json").exists() && !s.repo.join("CLAUDE.md").exists() && !s.repo.join(".mcp.json").exists());
}

#[test]
fn a_marker_quoted_in_prose_is_not_the_block() {
    let s = Scratch::new("markerprose");
    let claude_md = s.home.join(".claude/CLAUDE.md");
    let before_md = "The setup writes `<!-- scrutate:begin -->` markers.\n\n- rule one\n- rule two\n";
    s.write(&claude_md, before_md);
    let (code, _, err) = s.setup(&["claude", "--instructions"]);
    assert_eq!(code, 0, "{err}");
    let md = fs::read_to_string(&claude_md).unwrap();
    assert!(md.starts_with(before_md) && md.matches("<!-- scrutate:end -->").count() == 1, "{md}");
    let (code, _, err) = s.setup(&["claude", "--uninstall", "--instructions"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(fs::read_to_string(&claude_md).unwrap(), before_md);

    // a begin line with no end line is left alone
    let broken = "# Rules\n<!-- scrutate:begin -->\nmine\n";
    s.write(&claude_md, broken);
    let (code, _, err) = s.setup(&["claude", "--instructions"]);
    assert_ne!(code, 0);
    assert!(err.contains("scrutate:end"), "{err}");
    assert_eq!(fs::read_to_string(&claude_md).unwrap(), broken);
}

#[test]
fn a_block_with_crlf_line_ends_is_the_block() {
    let s = Scratch::new("markercrlf");
    let claude_md = s.home.join(".claude/CLAUDE.md");
    let before_md = "# Rules\r\n\r\nBe brief.\r\n";
    s.write(&claude_md, before_md);
    assert_eq!(s.setup(&["claude", "--instructions"]).0, 0);
    // an editor or git's autocrlf turns every line end into CRLF
    let crlf = fs::read_to_string(&claude_md).unwrap().replace("\r\n", "\n").replace('\n', "\r\n");
    s.write(&claude_md, &crlf);
    let (code, _, err) = s.setup(&["claude", "--instructions"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(fs::read_to_string(&claude_md).unwrap().matches("<!-- scrutate:begin -->").count(), 1, "one block");
    let (code, _, err) = s.setup(&["claude", "--uninstall", "--instructions"]);
    assert_eq!(code, 0, "{err}");
    let after = fs::read_to_string(&claude_md).unwrap();
    assert!(!after.contains("scrutate:"), "{after:?}");
    assert!(after.starts_with(before_md.trim_end()), "{after:?}");
}

/// Claude Code's own `claude mcp` replaces an entry by remove, then add: when the add fails, the
/// old entry is added back, so the user is not left with none.
#[cfg(unix)]
#[test]
fn a_failed_replace_through_claude_mcp_puts_the_old_entry_back() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("claudeundo");
    let old = r#"{"type":"stdio","command":"/old/place/scrutate","args":["serve"]}"#;
    s.write(&s.user_file("claude"), &format!("{{\n  \"mcpServers\": {{\n    \"scrutate\": {old}\n  }}\n}}\n"));
    let bin = s.base.join("bin");
    let log = s.base.join("claude.log");
    // a claude whose add-json fails for anything but the old entry
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$2 $4\" in\n  'add-json '*/old/place/*) exit 0 ;;\n  add-json*) echo 'refused' >&2; exit 1 ;;\nesac\nexit 0\n",
        log.display()
    );
    s.write(&bin.join("claude"), &script);
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let (code, _, err) = s.setup_in(&s.repo, &["claude"], &format!("{}:/usr/bin:/bin", bin.display()));
    assert_ne!(code, 0, "{err}");
    let calls: Vec<String> = fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[0].starts_with("mcp remove scrutate") && calls[1].starts_with("mcp add-json scrutate"), "{calls:?}");
    assert!(calls[2].starts_with("mcp add-json scrutate") && calls[2].contains("/old/place/scrutate"), "the old entry goes back: {calls:?}");
    assert!(err.contains("put back"), "{err}");
}

/// Sends `initialize` and `tools/list` to `argv` run in `dir` and returns the tool names.
fn mcp_tools(argv: &[String], dir: &Path, s: &Scratch) -> Vec<String> {
    let mut child = s
        .command(&argv[0], "/usr/bin:/bin")
        .args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"1"}}}}}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
    drop(stdin);
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    child.wait().unwrap();
    let replies: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(replies.iter().any(|r| r["id"] == 1 && r["result"]["serverInfo"]["name"].is_string()), "{out}");
    replies.iter().find(|r| r["id"] == 2).unwrap()["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn the_command_line_written_answers_initialize_and_tools_list() {
    let s = Scratch::new("serve");
    for agent in AGENTS {
        assert_eq!(s.setup(&[agent]).0, 0);
        let argv = command_line(agent, &fs::read_to_string(s.user_file(agent)).unwrap());
        let tools = mcp_tools(&argv, &s.repo, &s);
        assert!(tools.contains(&"grep_code".to_string()) && tools.contains(&"search_code".to_string()), "{agent}: {tools:?}");
    }
    assert_eq!(s.setup_in(&s.repo, &["cursor", "--project"], "/usr/bin:/bin").0, 0);
    let argv = command_line("cursor", &fs::read_to_string(s.repo.join(".cursor/mcp.json")).unwrap());
    // a project's entry names its repository, so it serves it from anywhere
    assert!(mcp_tools(&argv, &s.home, &s).contains(&"grep_code".to_string()));
}

/// The output of `argv`, or `None` when it did not finish within `limit`.
fn run_for(c: &mut Command, limit: Duration) -> Option<String> {
    let mut child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let start = Instant::now();
    loop {
        if child.try_wait().ok()?.is_some() {
            let mut out = String::new();
            child.stdout.take()?.read_to_string(&mut out).ok()?;
            let mut err = String::new();
            child.stderr.take()?.read_to_string(&mut err).ok()?;
            return Some(out + &err);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The installed program `name`, by the PATH of the tests, with its directory as PATH (a Node
/// program finds `node` there).
fn installed(name: &str) -> Option<(String, String)> {
    let env_name = format!("SCRUTATE_TEST_{}", name.to_uppercase().replace('-', "_"));
    let found = std::env::var_os(&env_name)
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file()));
    match found {
        Some(p) => {
            let dir = p.parent().unwrap().to_str().unwrap().to_string();
            let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).find(|d| d.join("node").is_file());
            let node = node.map(|d| format!("{}:", d.display())).unwrap_or_default();
            Some((p.to_str().unwrap().to_string(), format!("{dir}:{node}/usr/bin:/bin")))
        }
        None => {
            eprintln!("skipped: {name} is not installed (or set {env_name})");
            None
        }
    }
}

#[test]
fn each_installed_agent_lists_the_server() {
    let s = Scratch::new("listing");
    let limit = Duration::from_secs(90);
    if let Some((claude, path)) = installed("claude") {
        let (code, out, err) = s.setup_in(&s.repo, &["claude"], &path);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("claude mcp add-json scrutate"), "setup runs claude's own command: {out}");
        let listed = run_for(s.command(&claude, &path).args(["mcp", "list"]), limit).expect("claude mcp list finishes");
        assert!(listed.lines().any(|l| l.starts_with("scrutate:") && l.contains("Connected")), "{listed}");
        let (code, _, err) = s.setup_in(&s.repo, &["claude"], &path);
        assert_eq!(code, 0, "{err}");
        let (code, _, err) = s.setup_in(&s.repo, &["claude", "--uninstall"], &path);
        assert_eq!(code, 0, "{err}");
        let listed = run_for(s.command(&claude, &path).args(["mcp", "list"]), limit).unwrap();
        assert!(!listed.contains("scrutate:"), "{listed}");
    }
    if let Some((codex, path)) = installed("codex") {
        assert_eq!(s.setup(&["codex"]).0, 0);
        let listed = run_for(s.command(&codex, &path).args(["mcp", "list"]), limit).expect("codex mcp list finishes");
        assert!(listed.lines().any(|l| l.starts_with("scrutate") && l.contains(&bin()) && l.contains("enabled")), "{listed}");
    }
    if let Some((cursor, path)) = installed("cursor-agent") {
        assert_eq!(s.setup(&["cursor"]).0, 0);
        let listed = run_for(s.command(&cursor, &path).args(["mcp", "list"]), limit).expect("cursor-agent mcp list finishes");
        assert!(listed.lines().any(|l| l.contains("scrutate") && l.contains("ready")), "{listed}");
    }
    if let Some((opencode, path)) = installed("opencode") {
        assert_eq!(s.setup(&["opencode"]).0, 0);
        // opencode 2 lists through a background service, which answers before it has read the
        // configuration for a moment after it starts
        let mut listed = run_for(s.command(&opencode, &path).args(["mcp", "list"]), limit);
        for _ in 0..10 {
            if !listed.as_deref().is_some_and(|l| l.contains("No MCP servers configured")) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
            listed = run_for(s.command(&opencode, &path).args(["mcp", "list"]), limit);
        }
        let _ = run_for(s.command(&opencode, &path).args(["service", "stop"]), Duration::from_secs(20));
        let listed = listed.expect("opencode mcp list finishes");
        assert!(listed.contains("scrutate") && listed.contains("connected"), "{listed}");
    }
    if let Some((gemini, path)) = installed("gemini") {
        assert_eq!(s.setup(&["gemini"]).0, 0);
        // trust is the user's to give: the test gives it for its scratch folder
        s.write(&s.home.join(".gemini/trustedFolders.json"), &format!("{{\"{}\": \"TRUST_FOLDER\"}}", s.repo.display()));
        let listed = run_for(s.command(&gemini, &path).args(["mcp", "list"]), limit).expect("gemini mcp list finishes");
        assert!(listed.contains("scrutate") && listed.contains("Connected"), "{listed}");
    }
}

/// Text added after the scrutate block stays on a line of its own when the block is taken out:
/// only a block at the end of the file takes the line break setup put before it.
#[test]
fn uninstall_keeps_text_added_after_the_block_on_its_own_line() {
    let s = Scratch::new("after_block");
    let claude_md = s.home.join(".claude/CLAUDE.md");
    // with and without a line break at the end of the user's text, LF and CRLF
    for before in ["# Notes", "# Notes\n", "# Notes\r\n"] {
        s.write(&claude_md, before);
        assert_eq!(s.setup(&["claude", "--instructions"]).0, 0);
        let md = fs::read_to_string(&claude_md).unwrap();
        s.write(&claude_md, &format!("{md}- rule two\n"));
        let (code, _, err) = s.setup(&["claude", "--uninstall", "--instructions"]);
        assert_eq!(code, 0, "{err}");
        let want = if before.ends_with('\n') { format!("{before}- rule two\n") } else { format!("{before}\n- rule two\n") };
        assert_eq!(fs::read_to_string(&claude_md).unwrap(), want, "{before:?}");
    }
}
