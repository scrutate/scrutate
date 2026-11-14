//! `scrutate setup <agent>`: one MCP server entry that runs this binary, written into an agent's
//! configuration (and, when asked, the Grep hook and a block of instructions), edited in place so
//! that every other byte of the file stays and `--uninstall` takes back exactly what was added.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// The agents `setup` knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    Claude,
    Codex,
    Opencode,
    Cursor,
    Gemini,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// the agent's file in the repository of the current directory, not the user's
    pub project: bool,
    pub uninstall: bool,
    pub dry_run: bool,
    /// also the Grep hook (Claude Code only); `uninstall` takes it out whether asked or not
    pub hook: bool,
    /// also a block in the agent's instruction file; `uninstall` takes it out whether asked or not
    pub instructions: bool,
}

/// The name of the server entry and of the markers around the instruction block.
pub const NAME: &str = "scrutate";
const BEGIN: &str = "<!-- scrutate:begin -->";
const END: &str = "<!-- scrutate:end -->";
const HOOK_TIMEOUT_S: u64 = 10;

/// Runs `setup` and prints what it did (or, with `dry_run`, what it would do). Nothing is written
/// unless every file it would change parses.
pub fn run(agent: Agent, o: Options) -> Result<i32> {
    let env = Env::from_process()?;
    let steps = plan(agent, o, &env)?;
    let mut out = String::new();
    for s in &steps {
        if o.dry_run {
            s.preview(&mut out);
        } else {
            s.apply(&mut out)?;
        }
    }
    print!("{out}");
    Ok(0)
}

/// Where the agents keep their files, and what the entry runs.
struct Env {
    home: PathBuf,
    claude_dir: PathBuf,
    claude_json: PathBuf,
    codex_home: PathBuf,
    config_home: PathBuf,
    /// the top of the repository the current directory is in (the directory with `.git`), else it
    top: PathBuf,
    bin: String,
    claude: Option<PathBuf>,
}

impl Env {
    fn from_process() -> Result<Env> {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).context("no home directory: set HOME")?;
        let claude_config = var("CLAUDE_CONFIG_DIR");
        let cwd = std::env::current_dir().context("no current directory")?;
        let top = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(&cwd).to_path_buf();
        let exe = std::env::current_exe().context("cannot find this program's path")?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let bin = exe.to_str().context("this program's path is not UTF-8")?.trim_start_matches(r"\\?\").to_string();
        Ok(Env {
            claude_dir: claude_config.clone().unwrap_or_else(|| home.join(".claude")),
            claude_json: claude_config.map_or_else(|| home.join(".claude.json"), |d| d.join(".claude.json")),
            codex_home: var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            config_home: var("XDG_CONFIG_HOME").filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config")),
            top: std::fs::canonicalize(&top).map(|p| PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))).unwrap_or(top),
            claude: on_path("claude"),
            home,
            bin,
        })
    }
}

/// The first `name` on `PATH` that is a file.
fn on_path(name: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) { ["exe", "cmd", "bat"].iter().map(|e| format!("{name}.{e}")).collect() } else { vec![name.to_string()] };
    std::env::split_paths(&std::env::var_os("PATH")?).flat_map(|d| names.iter().map(move |n| d.join(n)).collect::<Vec<_>>()).find(|p| p.is_file())
}

/// One thing `setup` does.
enum Step {
    /// `path` goes from `before` to `after` (`None`: no file)
    File {
        path: PathBuf,
        before: Option<String>,
        after: Option<String>,
        what: String,
    },
    /// the agent's own command, and the one that undoes the step before it when this one fails
    Run {
        argv: Vec<String>,
        what: String,
        undo: Option<Vec<String>>,
    },
    Note(String),
}

impl Step {
    fn preview(&self, out: &mut String) {
        match self {
            Step::File { path, before, after, what } => {
                let _ = writeln!(out, "would {what}: {}", path.display());
                diff(out, before.as_deref().unwrap_or(""), after.as_deref().unwrap_or(""));
                if after.is_none() {
                    let _ = writeln!(out, "  (the file is deleted: nothing else is left in it)");
                }
            }
            Step::Run { argv, what, .. } => {
                let _ = writeln!(out, "would {what}: {}", shown_command(argv));
            }
            Step::Note(n) => {
                let _ = writeln!(out, "note: {n}");
            }
        }
    }

    fn apply(&self, out: &mut String) -> Result<()> {
        match self {
            Step::File { path, before, after, what } => {
                let now = read(path)?;
                if now != *before {
                    bail!("{} changed while setup ran: nothing written to it, run setup again", path.display());
                }
                match after {
                    Some(text) => write_atomic(path, text)?,
                    None => std::fs::remove_file(path).with_context(|| format!("delete {}", path.display()))?,
                }
                let _ = writeln!(out, "{what}: {}", path.display());
            }
            Step::Run { argv, what, undo } => {
                let run = |argv: &[String]| -> Result<()> {
                    let o = std::process::Command::new(&argv[0])
                        .args(&argv[1..])
                        .stdin(std::process::Stdio::null())
                        .output()
                        .with_context(|| format!("run {}", argv[0]))?;
                    if !o.status.success() {
                        bail!("{} failed: {}", shown_command(argv), String::from_utf8_lossy(&o.stderr).trim());
                    }
                    Ok(())
                };
                if let Err(e) = run(argv) {
                    let Some(undo) = undo else { return Err(e) };
                    return Err(match run(undo) {
                        Ok(()) => e.context("the old entry was put back"),
                        Err(u) => e.context(format!("the old entry could not be put back ({u:#})")),
                    });
                }
                let _ = writeln!(out, "{what}: {}", shown_command(argv));
            }
            Step::Note(n) => {
                let _ = writeln!(out, "note: {n}");
            }
        }
        Ok(())
    }
}

fn shown_command(argv: &[String]) -> String {
    argv.iter().map(|a| shell_word(a)).collect::<Vec<_>>().join(" ")
}

/// `s` as one word of a shell: a POSIX shell, or on Windows cmd, PowerShell or Git Bash, where
/// double quotes hold a path in all three.
fn shell_word(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{s}\"")
    } else if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-:=+,@%".contains(&b)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The lines that differ between `a` and `b`, as `- ` and `+ ` lines.
fn diff(out: &mut String, a: &str, b: &str) {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    for (sign, lines) in [('-', &a[pre..a.len() - suf]), ('+', &b[pre..b.len() - suf])] {
        for line in lines {
            let _ = writeln!(out, "  {sign} {line}");
        }
    }
}

fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(String::from_utf8(b).map_err(|_| anyhow!("{} is not UTF-8: left as it is", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Writes `text` to a temporary file beside `path` and renames it over `path`, keeping its
/// permissions; a link is followed, so the file it names is replaced and the link stays.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.scrutate-{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = create_tmp(&tmp, &target)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("write {}", target.display()))
}

/// A new file at `tmp` with the permissions of `target` from the start, so a file only its owner
/// may read (a config holding a token) is never readable by others while it is written.
fn create_tmp(tmp: &Path, target: &Path) -> std::io::Result<std::fs::File> {
    let _ = std::fs::remove_file(tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    let old = std::fs::metadata(target).ok().map(|m| m.permissions());
    #[cfg(unix)]
    if let Some(p) = &old {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        o.mode(p.mode() & 0o7777);
    }
    let f = o.open(tmp)?;
    if let Some(p) = old {
        std::fs::set_permissions(tmp, p)?;
    }
    Ok(f)
}

// ------------------------------------------------------------------------- what each agent gets

/// The arguments of the entry: the user's entry serves the repository the agent runs in, a
/// project's entry names its repository.
fn serve_args(o: Options, env: &Env) -> Vec<String> {
    if o.project {
        vec!["--root".into(), env.top.to_string_lossy().into_owned(), "serve".into()]
    } else {
        vec!["serve".into()]
    }
}

fn strings(v: &[String]) -> J {
    J::A(v.iter().cloned().map(J::S).collect())
}

fn plan(agent: Agent, o: Options, env: &Env) -> Result<Vec<Step>> {
    let args = serve_args(o, env);
    let bin = env.bin.clone();
    let mut steps = Vec::new();
    match agent {
        Agent::Claude => {
            let entry = obj(vec![("type", J::S("stdio".into())), ("command", J::S(bin.clone())), ("args", strings(&args))]);
            match (&env.claude, o.project) {
                (Some(claude), false) => steps.extend(claude_program(claude, &env.claude_json, &entry, o)?),
                _ => {
                    let path = if o.project { env.top.join(".mcp.json") } else { env.claude_json.clone() };
                    steps.extend(json_entry(&path, &["mcpServers", NAME], &entry, o)?);
                }
            }
            if o.project && !o.uninstall {
                steps.push(Step::Note("Claude Code asks once before it starts a server of a project's .mcp.json".into()));
            }
            if o.hook || o.uninstall {
                let settings = if o.project { env.top.join(".claude").join("settings.json") } else { env.claude_dir.join("settings.json") };
                steps.extend(asked(o.hook, claude_hook(&settings, &bin, o)?));
            }
        }
        Agent::Codex => {
            if o.project {
                bail!("Codex has no project configuration file: run `scrutate setup codex` for the user's {}", env.codex_home.join("config.toml").display());
            }
            steps.extend(toml_entry(&env.codex_home.join("config.toml"), &bin, &args, o)?);
            if o.hook {
                steps.push(Step::Note("Codex has PreToolUse hooks but searches by running rg in its shell, not through a search tool of its own; scrutate's hook answers Claude Code's Grep only, so no hook was installed".into()));
            }
        }
        Agent::Opencode => {
            let dir = if o.project { env.top.clone() } else { env.config_home.join("opencode") };
            let path = ["opencode.jsonc", "opencode.json"].iter().map(|n| dir.join(n)).find(|p| p.is_file()).unwrap_or_else(|| dir.join("opencode.json"));
            let text = read(&path)?;
            let v2 = match text.as_deref().filter(|t| !t.trim().is_empty()) {
                Some(t) => parse(t)
                    .map(|n| n.value(t))
                    .map_err(|e| anyhow!("{} does not parse ({e}): left as it is", path.display()))?
                    .pointer("/mcp/servers")
                    .is_some_and(Value::is_object),
                None => false,
            };
            let command = J::A(std::iter::once(J::S(bin.clone())).chain(args.iter().cloned().map(J::S)).collect());
            let (keys, entry): (&[&str], J) = if v2 {
                (&["mcp", "servers", NAME], obj(vec![("type", J::S("local".into())), ("command", command)]))
            } else {
                (&["mcp", NAME], obj(vec![("type", J::S("local".into())), ("command", command), ("enabled", J::B(true))]))
            };
            steps.extend(json_entry(&path, keys, &entry, o)?);
            if o.hook {
                steps.push(Step::Note("opencode's hooks are JavaScript plugins (tool.execute.before), not commands; scrutate's hook answers Claude Code's Grep only, so no hook was installed".into()));
            }
        }
        Agent::Cursor => {
            let path = if o.project { env.top.join(".cursor").join("mcp.json") } else { env.home.join(".cursor").join("mcp.json") };
            let entry = obj(vec![("command", J::S(bin.clone())), ("args", strings(&args))]);
            steps.extend(json_entry(&path, &["mcpServers", NAME], &entry, o)?);
            if o.hook {
                steps.push(Step::Note("cursor-agent has a preToolUse hook, but its search tool takes other arguments and answers in another form; scrutate's hook answers Claude Code's Grep only, so no hook was installed".into()));
            }
        }
        Agent::Gemini => {
            let path = if o.project { env.top.join(".gemini").join("settings.json") } else { env.home.join(".gemini").join("settings.json") };
            let entry = obj(vec![("command", J::S(bin.clone())), ("args", strings(&args))]);
            steps.extend(json_entry(&path, &["mcpServers", NAME], &entry, o)?);
            if !o.uninstall {
                steps.push(Step::Note(
                    "Gemini CLI starts a stdio server only in a folder it trusts (`gemini trust`, or ~/.gemini/trustedFolders.json); setup leaves trust to you"
                        .into(),
                ));
            }
            if o.hook {
                steps.push(Step::Note("Gemini CLI has a BeforeTool hook, but its search tool takes other arguments and answers in another form; scrutate's hook answers Claude Code's Grep only, so no hook was installed".into()));
            }
        }
    }
    // an uninstall takes out everything setup adds, so a removed binary leaves no hook behind
    if o.instructions || o.uninstall {
        let file = match (agent, o.project) {
            (Agent::Claude, false) => Some(env.claude_dir.join("CLAUDE.md")),
            (Agent::Claude, true) => Some(env.top.join("CLAUDE.md")),
            (Agent::Codex, _) => Some(env.codex_home.join("AGENTS.md")),
            (Agent::Opencode, false) => Some(env.config_home.join("opencode").join("AGENTS.md")),
            (Agent::Opencode | Agent::Cursor, true) => Some(env.top.join("AGENTS.md")),
            (Agent::Cursor, false) => None,
            (Agent::Gemini, false) => Some(env.home.join(".gemini").join("GEMINI.md")),
            (Agent::Gemini, true) => Some(env.top.join("GEMINI.md")),
        };
        match file {
            Some(f) => steps.extend(asked(o.instructions, block_entry(&f, o)?)),
            None if !o.instructions => {}
            None => steps.push(Step::Note(
                "Cursor keeps a user's rules in its settings, not in a file: no instructions were written (with --project they go into AGENTS.md)".into(),
            )),
        }
    }
    Ok(steps)
}

/// `steps`, without the notes (`has no scrutate hook`) of a part the command line did not name.
fn asked(named: bool, steps: Vec<Step>) -> Vec<Step> {
    steps.into_iter().filter(|s| named || !matches!(s, Step::Note(_))).collect()
}

/// The user scope of Claude Code through its own `claude mcp`, which many running Claude Code
/// processes share `~/.claude.json` with.
fn claude_program(claude: &Path, file: &Path, entry: &J, o: Options) -> Result<Vec<Step>> {
    let current = match read(file)? {
        Some(t) if !t.trim().is_empty() => parse(&t)
            .map(|n| n.value(&t))
            .map_err(|e| anyhow!("{} does not parse ({e}): left as it is", file.display()))?
            .pointer(&format!("/mcpServers/{NAME}"))
            .cloned(),
        _ => None,
    };
    let claude = claude.to_string_lossy().into_owned();
    let remove = Step::Run {
        argv: vec![claude.clone(), "mcp".into(), "remove".into(), NAME.into(), "--scope".into(), "user".into()],
        what: "removed the scrutate server".into(),
        undo: None,
    };
    let add_json = |v: &Value| vec![claude.clone(), "mcp".into(), "add-json".into(), NAME.into(), v.to_string(), "--scope".into(), "user".into()];
    let add = |undo: Option<Vec<String>>| Step::Run { argv: add_json(&entry.value()), what: "added the scrutate server".into(), undo };
    Ok(match (o.uninstall, current) {
        (true, Some(_)) => vec![remove],
        (true, None) => vec![Step::Note(format!("{} has no scrutate server", file.display()))],
        (false, Some(v)) if v == entry.value() => vec![Step::Note(format!("{} already has the scrutate server", file.display()))],
        // a replace is a remove, then an add: when the add fails, the old entry goes back
        (false, Some(old)) => vec![remove, add(Some(add_json(&old)))],
        (false, None) => vec![add(None)],
    })
}

fn file_step(path: &Path, before: Option<String>, after: Option<String>, what: &str) -> Step {
    Step::File { path: path.to_path_buf(), before, after, what: what.into() }
}

/// The member at `keys` of a JSON (or JSONC) file set to `entry`, or removed.
fn json_entry(path: &Path, keys: &[&str], entry: &J, o: Options) -> Result<Vec<Step>> {
    let before = read(path)?;
    let refused = |e: anyhow::Error| anyhow!("{} does not parse ({e:#}): left as it is", path.display());
    let label = keys.join(".");
    if o.uninstall {
        return Ok(match before.as_deref().filter(|t| !t.trim().is_empty()) {
            None => vec![Step::Note(format!("{} has no {label}", path.display()))],
            Some(t) => match unset(t, keys).map_err(refused)? {
                None => vec![Step::Note(format!("{} has no {label}", path.display()))],
                Some(after) => vec![file_step(path, before.clone(), after, &format!("removed {label} from"))],
            },
        });
    }
    let after = set(before.as_deref().filter(|t| !t.trim().is_empty()), keys, entry).map_err(refused)?;
    Ok(match after {
        None => vec![Step::Note(format!("{} already has {label}", path.display()))],
        Some(a) => vec![file_step(path, before, Some(a), &format!("added {label} to"))],
    })
}

/// The hook entry of Claude Code's settings: a `PreToolUse` group with matcher `Grep`.
fn claude_hook(path: &Path, bin: &str, o: Options) -> Result<Vec<Step>> {
    let mut hook = vec![("type", J::S("command".into()))];
    hook.extend(hook_run(bin, cfg!(windows)));
    hook.push(("timeout", J::N(HOOK_TIMEOUT_S)));
    let group = obj(vec![("matcher", J::S("Grep".into())), ("hooks", J::A(vec![obj(hook)]))]);
    let before = read(path)?;
    let refused = |e: anyhow::Error| anyhow!("{} does not parse ({e:#}): left as it is", path.display());
    let keys = ["hooks", "PreToolUse"];
    let text = before.as_deref().filter(|t| !t.trim().is_empty());
    if o.uninstall {
        let after = match text {
            Some(t) => pull(t, &keys, is_our_hook).map_err(refused)?,
            None => None,
        };
        return Ok(match after {
            None => vec![Step::Note(format!("{} has no scrutate hook", path.display()))],
            Some(a) => vec![file_step(path, before.clone(), a, "removed the scrutate Grep hook from")],
        });
    }
    Ok(match push(text, &keys, &group, is_our_hook).map_err(refused)? {
        None => vec![Step::Note(format!("{} already has the scrutate Grep hook", path.display()))],
        Some(a) => vec![file_step(path, before, Some(a), "added the scrutate Grep hook to")],
    })
}

/// How Claude Code runs the hook. On Windows it runs a command in Git Bash, or in PowerShell
/// when there is no Git Bash (`Sn=e.shell??ij()`): a path with `/` and no character either shell
/// reads runs the same in both; another path goes as the program and its arguments, which
/// Claude Code starts with no shell (`B$e(wo[0],wo[1],...)`).
fn hook_run(bin: &str, windows: bool) -> Vec<(&'static str, J)> {
    if !windows {
        return vec![("command", J::S(format!("{} hook grep", shell_word(bin))))];
    }
    if bin.chars().all(|c| c.is_alphanumeric() || r"\/._-:".contains(c)) {
        vec![("command", J::S(format!("{} hook grep", bin.replace('\\', "/"))))]
    } else {
        vec![("command", J::S(bin.into())), ("args", strings(&["hook".into(), "grep".into()]))]
    }
}

fn is_our_hook(v: &Value) -> bool {
    let ours = |h: &Value| {
        let command = h.get("command").and_then(Value::as_str).unwrap_or("");
        match h.get("args") {
            None => command.ends_with(" hook grep") && command.contains("scrutate"),
            Some(a) => command.contains("scrutate") && *a == serde_json::json!(["hook", "grep"]),
        }
    };
    v.get("matcher").and_then(Value::as_str) == Some("Grep") && v.get("hooks").and_then(Value::as_array).is_some_and(|h| h.len() == 1 && ours(&h[0]))
}

/// `[mcp_servers.scrutate]` of Codex's `config.toml`, appended as one table, or removed.
fn toml_entry(path: &Path, bin: &str, args: &[String], o: Options) -> Result<Vec<Step>> {
    let before = read(path)?;
    let old: toml::Table = match before.as_deref() {
        Some(t) => t.parse().map_err(|e| anyhow!("{} does not parse ({e}): left as it is", path.display()))?,
        None => toml::Table::new(),
    };
    let current = old.get("mcp_servers").and_then(|t| t.get(NAME)).cloned();
    if o.uninstall {
        let Some(text) = before.as_deref().filter(|_| current.is_some()) else {
            return Ok(vec![Step::Note(format!("{} has no mcp_servers.scrutate", path.display()))]);
        };
        let after = toml_unset(text, &old).with_context(|| format!("{}", path.display()))?;
        return Ok(vec![file_step(path, before.clone(), (!after.is_empty()).then_some(after), "removed mcp_servers.scrutate from")]);
    }
    let block = format!("[mcp_servers.{NAME}]\ncommand = {}\nargs = [{}]\n", toml_str(bin), args.iter().map(|a| toml_str(a)).collect::<Vec<_>>().join(", "));
    let want: toml::Table = block.parse().expect("the block setup writes");
    let want_entry = want["mcp_servers"][NAME].clone();
    if current.as_ref() == Some(&want_entry) {
        return Ok(vec![Step::Note(format!("{} already has mcp_servers.scrutate", path.display()))]);
    }
    let base = match (&current, before.as_deref()) {
        (Some(_), Some(t)) => toml_unset(t, &old).with_context(|| format!("{}", path.display()))?,
        (_, t) => t.unwrap_or("").to_string(),
    };
    let after = if base.is_empty() { block } else { format!("{base}\n{block}") };
    let mut expected = old.clone();
    expected
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("mcp_servers is not a table")?
        .insert(NAME.into(), want_entry);
    if after.parse::<toml::Table>().ok() != Some(expected) {
        bail!("{}: the scrutate table cannot be added as a table of its own here (an inline mcp_servers?): edit it by hand", path.display());
    }
    Ok(vec![file_step(path, before, Some(after), "added mcp_servers.scrutate to")])
}

/// A TOML basic string.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `text` without its `[mcp_servers.scrutate]` table (and its subtables), and the newline setup
/// put before it.
fn toml_unset(text: &str, old: &toml::Table) -> Result<String> {
    let header = format!("[mcp_servers.{NAME}]");
    let sub = format!("[mcp_servers.{NAME}.");
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1).filter(|&i| i < text.len()));
    let line = |i: usize| text[starts[i]..starts.get(i + 1).copied().unwrap_or(text.len())].trim();
    let first = (0..starts.len()).find(|&i| line(i) == header).ok_or_else(|| anyhow!("mcp_servers.scrutate is not a table of its own: edit it by hand"))?;
    let mut last = first;
    for i in first + 1..starts.len() {
        let l = line(i);
        if l.starts_with('[') && !l.starts_with(&sub) {
            break;
        }
        last = i;
    }
    // blank lines and comments before the next table belong to it
    while last > first && (line(last).is_empty() || line(last).starts_with('#')) {
        last -= 1;
    }
    let end = starts.get(last + 1).copied().unwrap_or(text.len());
    let start = starts[first].saturating_sub(usize::from(starts[first] > 0));
    let after = format!("{}{}", &text[..start], &text[end..]);
    let mut expected = old.clone();
    if let Some(t) = expected.get_mut("mcp_servers").and_then(toml::Value::as_table_mut) {
        t.remove(NAME);
    }
    let empty = |t: &mut toml::Table| {
        if t.get("mcp_servers").and_then(toml::Value::as_table).is_some_and(toml::Table::is_empty) {
            t.remove("mcp_servers");
        }
    };
    empty(&mut expected);
    let mut got: toml::Table = after.parse().map_err(|_| anyhow!("mcp_servers.scrutate cannot be cut out cleanly: edit it by hand"))?;
    empty(&mut got);
    if got != expected {
        bail!("mcp_servers.scrutate cannot be cut out cleanly: edit it by hand");
    }
    Ok(after)
}

/// The instruction block, appended to the agent's instruction file, or removed.
fn block_entry(path: &Path, o: Options) -> Result<Vec<Step>> {
    let block = format!(
        "{BEGIN}\nUse the scrutate MCP tools to search code: `search_code` for a question in words or a name, `grep_code` for an exact pattern, `find_symbol` for a definition, `references`, `callers` and `callees` to follow a name, and `outline` and `read_code` to read a file in pieces instead of whole.\n{END}\n"
    );
    let before = read(path)?;
    let text = before.as_deref().unwrap_or("");
    // a marker counts only as a whole line, ended by LF or CRLF: a sentence quoting one is the user's text
    let eol = |i: usize| if text[i..].starts_with("\r\n") { 2 } else { usize::from(text[i..].starts_with('\n')) };
    let line = |from: usize, m: &str| {
        text[from..]
            .match_indices(m)
            .map(|(i, _)| from + i)
            .find(|&i| (i == 0 || text.as_bytes()[i - 1] == b'\n') && (i + m.len() == text.len() || eol(i + m.len()) > 0))
    };
    let found = match line(0, BEGIN) {
        Some(b) => match line(b, END) {
            Some(e) => Some((b, e + END.len() + eol(e + END.len()))),
            None => bail!("{} has a {BEGIN} line with no {END} line: edit it by hand", path.display()),
        },
        None => None,
    };
    if o.uninstall {
        let Some((b, e)) = found else {
            return Ok(vec![Step::Note(format!("{} has no scrutate block", path.display()))]);
        };
        // setup put a line break before the block; with text after the block it goes only when
        // the text before it already ended in one, else the two lines would join
        let start = if e < text.len() && !text[..b.saturating_sub(1)].ends_with('\n') {
            b
        } else if text[..b].ends_with("\r\n") {
            b - 2
        } else {
            b.saturating_sub(usize::from(text[..b].ends_with('\n')))
        };
        let after = format!("{}{}", &text[..start], &text[e..]);
        return Ok(vec![file_step(path, before.clone(), (!after.is_empty()).then_some(after), "removed the scrutate block from")]);
    }
    let after = match found {
        Some((b, e)) if text[b..e] == block => return Ok(vec![Step::Note(format!("{} already has the scrutate block", path.display()))]),
        Some((b, e)) => format!("{}{block}{}", &text[..b], &text[e..]),
        None if text.is_empty() => block,
        None => format!("{text}\n{block}"),
    };
    Ok(vec![file_step(path, before, Some(after), "added the scrutate block to")])
}

// ------------------------------------------------------------------------- JSON values written

/// A JSON value with its keys in the order they are written.
#[derive(Clone)]
enum J {
    S(String),
    B(bool),
    N(u64),
    A(Vec<J>),
    O(Vec<(String, J)>),
}

fn obj(pairs: Vec<(&str, J)>) -> J {
    J::O(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

impl J {
    fn value(&self) -> Value {
        match self {
            J::S(s) => Value::String(s.clone()),
            J::B(b) => Value::Bool(*b),
            J::N(n) => Value::from(*n),
            J::A(a) => Value::Array(a.iter().map(J::value).collect()),
            J::O(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), v.value())).collect()),
        }
    }

    /// Pretty, `unit` per level, the first line at the caller's position and the rest at `depth`.
    fn render(&self, unit: &str, depth: usize) -> String {
        let pad = |d: usize| unit.repeat(d);
        match self {
            J::A(a) if !a.is_empty() => {
                format!("[\n{}\n{}]", a.iter().map(|v| format!("{}{}", pad(depth + 1), v.render(unit, depth + 1))).collect::<Vec<_>>().join(",\n"), pad(depth))
            }
            J::O(o) if !o.is_empty() => format!(
                "{{\n{}\n{}}}",
                o.iter().map(|(k, v)| format!("{}{}: {}", pad(depth + 1), Value::from(k.as_str()), v.render(unit, depth + 1))).collect::<Vec<_>>().join(",\n"),
                pad(depth)
            ),
            other => other.value().to_string(),
        }
    }
}

// ------------------------------------------------------------------------- JSONC text edits

/// A value of a JSON text, with comments and trailing commas allowed (JSONC), by byte span.
#[derive(Debug)]
struct Node {
    start: usize,
    end: usize,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    /// members: the key's start, the key, the value
    Object(Vec<(usize, String, Node)>),
    Array(Vec<Node>),
    Scalar,
}

impl Node {
    fn value(&self, text: &str) -> Value {
        match &self.kind {
            Kind::Object(m) => Value::Object(m.iter().map(|(_, k, v)| (k.clone(), v.value(text))).collect()),
            Kind::Array(a) => Value::Array(a.iter().map(|v| v.value(text)).collect()),
            Kind::Scalar => serde_json::from_str(&text[self.start..self.end]).unwrap_or(Value::Null),
        }
    }

    fn member(&self, key: &str) -> Option<(usize, &Node)> {
        match &self.kind {
            // the last of two equal keys is the one a reader keeps
            Kind::Object(m) => m.iter().rposition(|(_, k, _)| k == key).map(|i| (i, &m[i].2)),
            _ => None,
        }
    }

    /// Where each entry starts (a member's key, an item) and ends.
    fn entries(&self) -> Vec<(usize, usize)> {
        match &self.kind {
            Kind::Object(m) => m.iter().map(|(s, _, v)| (*s, v.end)).collect(),
            Kind::Array(a) => a.iter().map(|v| (v.start, v.end)).collect(),
            Kind::Scalar => Vec::new(),
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

fn parse(text: &str) -> Result<Node> {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    p.ws()?;
    let n = p.value(0)?;
    p.ws()?;
    if p.i != p.s.len() {
        bail!("unexpected text at byte {}", p.i);
    }
    Ok(n)
}

impl Parser<'_> {
    fn ws(&mut self) -> Result<()> {
        loop {
            match self.s.get(self.i) {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.i += 1,
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'/') => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                Some(b'/') if self.s.get(self.i + 1) == Some(&b'*') => {
                    let close = self.s[self.i + 2..].windows(2).position(|w| w == b"*/").ok_or_else(|| anyhow!("unclosed comment at byte {}", self.i))?;
                    self.i += close + 4;
                }
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node> {
        if depth > 256 {
            bail!("nested too deeply");
        }
        let start = self.i;
        let kind = match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                loop {
                    self.ws()?;
                    if self.s.get(self.i) == Some(&b'}') {
                        break;
                    }
                    let key_start = self.i;
                    let key = match self.string()? {
                        Value::String(k) => k,
                        _ => unreachable!(),
                    };
                    self.ws()?;
                    self.expect(b':')?;
                    self.ws()?;
                    let v = self.value(depth + 1)?;
                    members.push((key_start, key, v));
                    self.ws()?;
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => break,
                        _ => bail!("expected , or }} at byte {}", self.i),
                    }
                }
                self.i += 1;
                Kind::Object(members)
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws()?;
                    if self.s.get(self.i) == Some(&b']') {
                        break;
                    }
                    items.push(self.value(depth + 1)?);
                    self.ws()?;
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => break,
                        _ => bail!("expected , or ] at byte {}", self.i),
                    }
                }
                self.i += 1;
                Kind::Array(items)
            }
            Some(b'"') => {
                self.string()?;
                Kind::Scalar
            }
            Some(_) => {
                let n = self.s[self.i..].iter().position(|b| !(b.is_ascii_alphanumeric() || b"+-.".contains(b))).unwrap_or(self.s.len() - self.i);
                serde_json::from_slice::<Value>(&self.s[self.i..self.i + n]).map_err(|_| anyhow!("unexpected text at byte {}", self.i))?;
                self.i += n;
                Kind::Scalar
            }
            None => bail!("unexpected end"),
        };
        Ok(Node { start, end: self.i, kind })
    }

    fn string(&mut self) -> Result<Value> {
        let start = self.i;
        self.expect(b'"')?;
        while let Some(&b) = self.s.get(self.i) {
            self.i += 1;
            match b {
                b'\\' => self.i += 1,
                b'"' => return serde_json::from_slice(&self.s[start..self.i]).map_err(|_| anyhow!("bad string at byte {start}")),
                _ => {}
            }
        }
        bail!("unclosed string at byte {start}")
    }

    fn expect(&mut self, b: u8) -> Result<()> {
        if self.s.get(self.i) != Some(&b) {
            bail!("expected {} at byte {}", b as char, self.i);
        }
        self.i += 1;
        Ok(())
    }
}

/// The indentation the file uses per level: the least of its indented lines, else two spaces.
fn indent_unit(text: &str) -> String {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let pad = l.len() - l.trim_start_matches([' ', '\t']).len();
            (pad > 0 && pad < l.len()).then(|| &l[..pad])
        })
        .min_by_key(|p| p.len())
        .unwrap_or("  ")
        .to_string()
}

/// What lies on the way down to an entry, outermost first: a member, or an array's item.
#[derive(Clone)]
enum Hop<'a> {
    Key(&'a str),
    Item,
}

/// The text setup writes for containers that hold only the next hop, `inner` inside them.
fn chain(hops: &[Hop], inner: &str, unit: &str, depth: usize) -> String {
    let pad = |d: usize| unit.repeat(d);
    match hops.first() {
        None => inner.to_string(),
        Some(Hop::Key(k)) => format!("{{\n{}{}: {}\n{}}}", pad(depth + 1), Value::from(*k), chain(&hops[1..], inner, unit, depth + 1), pad(depth)),
        Some(Hop::Item) => format!("[\n{}{}\n{}]", pad(depth + 1), chain(&hops[1..], inner, unit, depth + 1), pad(depth)),
    }
}

fn splice(text: &str, at: usize, to: usize, with: &str) -> String {
    format!("{}{with}{}", &text[..at], &text[to..])
}

/// `entry` (a member's `"key": value`, or an item) added as the last entry of `c`, at `depth`.
fn insert(text: &str, c: &Node, depth: usize, entry: &str, unit: &str) -> String {
    match c.entries().last() {
        Some(&(_, end)) => splice(text, end, end, &format!(",\n{}{entry}", unit.repeat(depth + 1))),
        // right after the brace: what setup makes itself is laid out otherwise (`chain`), so the
        // two never read the same and `--uninstall` knows which to take back
        None => splice(text, c.start + 1, c.start + 1, entry),
    }
}

/// The entry `idx` of `c` taken out, as `insert` put it in.
fn remove(text: &str, c: &Node, idx: usize) -> String {
    let e = c.entries();
    if e.len() == 1 {
        splice(text, c.start + 1, e[0].1, "")
    } else if idx > 0 {
        splice(text, e[idx - 1].1, e[idx].1, "")
    } else {
        splice(text, e[0].0, e[1].0, "")
    }
}

/// The containers on the way down `keys`, root first, as far as they exist.
fn walk<'n>(root: &'n Node, keys: &[&str]) -> Result<Vec<&'n Node>> {
    let mut nodes = vec![root];
    for k in keys {
        let n = *nodes.last().expect("the root");
        if !matches!(n.kind, Kind::Object(_)) {
            bail!("{} is not an object", if nodes.len() == 1 { "the file".to_string() } else { keys[..nodes.len() - 1].join(".") });
        }
        match n.member(k) {
            Some((_, v)) => nodes.push(v),
            None => break,
        }
    }
    Ok(nodes)
}

/// The text after an edit, if it reads as `expected`.
fn checked(text: String, expected: &Value) -> Result<String> {
    let got = parse(&text).map(|n| n.value(&text)).ok();
    if got.as_ref() != Some(expected) {
        bail!("the edit would change more than the entry, so nothing is written");
    }
    Ok(text)
}

fn set_value(v: &mut Value, keys: &[&str], new: Value) {
    let mut at = v;
    for k in &keys[..keys.len() - 1] {
        at = at.as_object_mut().expect("checked to be an object").entry(k.to_string()).or_insert_with(|| Value::Object(Default::default()));
    }
    at.as_object_mut().expect("checked to be an object").insert(keys[keys.len() - 1].to_string(), new);
}

fn nested(keys: &[&str], inner: J) -> J {
    keys.iter().rev().fold(inner, |v, k| J::O(vec![(k.to_string(), v)]))
}

/// `text` with the member at `keys` set to `v` (the objects on the way made when missing); `None`
/// when it already holds `v`. A missing file (`None`) becomes one that holds only that member,
/// ending in a newline.
fn set(text: Option<&str>, keys: &[&str], v: &J) -> Result<Option<String>> {
    let Some(text) = text else {
        return Ok(Some(format!("{}\n", nested(keys, v.clone()).render("  ", 0))));
    };
    let root = parse(text)?;
    let unit = indent_unit(text);
    let nodes = walk(&root, keys)?;
    let mut expected = root.value(text);
    if !expected.is_object() {
        bail!("the file is not an object");
    }
    set_value(&mut expected, keys, v.value());
    let depth = nodes.len() - 1;
    let after = if depth == keys.len() {
        let old = nodes[depth];
        if old.value(text) == v.value() {
            return Ok(None);
        }
        splice(text, old.start, old.end, &v.render(&unit, depth))
    } else {
        let parent = nodes[depth];
        if !matches!(parent.kind, Kind::Object(_)) {
            bail!("{} is not an object", keys[..depth].join("."));
        }
        let rest = nested(&keys[depth + 1..], v.clone());
        insert(text, parent, depth, &format!("{}: {}", Value::from(keys[depth]), rest.render(&unit, depth + 1)), &unit)
    };
    checked(after, &expected).map(Some)
}

/// The outermost container on the way to the entry that holds nothing else, in the form setup
/// writes it: removing that container's own entry takes back exactly what setup added.
/// `nodes[0]` is the root; `hops[i]` leads from `nodes[i]` down. `None`: the whole file.
fn outermost_made(text: &str, nodes: &[&Node], hops: &[Hop], inner: &str, unit: &str) -> Option<Option<usize>> {
    let single = |n: &Node| n.entries().len() == 1;
    if nodes.iter().all(|n| single(n)) && text == format!("{}\n", chain(hops, inner, unit, 0)) {
        return Some(None);
    }
    (1..nodes.len()).find(|&i| nodes[i..].iter().all(|n| single(n)) && text[nodes[i].start..nodes[i].end] == chain(&hops[i..], inner, unit, i)).map(Some)
}

/// `text` without the member at `keys`, and without the objects setup made for it: `None` when
/// there is no such member, `Some(None)` when the file held nothing else.
fn unset(text: &str, keys: &[&str]) -> Result<Option<Option<String>>> {
    let root = parse(text)?;
    let unit = indent_unit(text);
    let nodes = walk(&root, keys)?;
    if nodes.len() != keys.len() + 1 {
        return Ok(None);
    }
    let containers = &nodes[..keys.len()];
    let entry = nodes[keys.len()];
    let hops: Vec<Hop> = keys.iter().map(|k| Hop::Key(k)).collect();
    cut(text, &root, containers, &hops, &text[entry.start..entry.end], &unit, |c, i| c.member(keys[i]).map(|m| m.0).expect("walked"))
}

/// Removes the entry under `containers` (the last one holds it at the last hop), or the
/// outermost container setup made for it.
fn cut(
    text: &str,
    root: &Node,
    containers: &[&Node],
    hops: &[Hop],
    inner: &str,
    unit: &str,
    index: impl Fn(&Node, usize) -> usize,
) -> Result<Option<Option<String>>> {
    let mut expected = root.value(text);
    let at = match outermost_made(text, containers, hops, inner, unit) {
        Some(None) => return Ok(Some(None)),
        Some(Some(i)) => i - 1,
        None => containers.len() - 1,
    };
    let c = containers[at];
    let idx = index(c, at);
    // the expected value: the same entry removed from the parsed value
    let mut v = &mut expected;
    for (i, h) in hops[..at].iter().enumerate() {
        v = match h {
            Hop::Key(k) => v.get_mut(*k).expect("walked"),
            Hop::Item => v.get_mut(index(containers[i], i)).expect("walked"),
        };
    }
    match &hops[at] {
        Hop::Key(k) => {
            v.as_object_mut().expect("an object").remove(*k);
        }
        Hop::Item => {
            v.as_array_mut().expect("an array").remove(idx);
        }
    }
    checked(remove(text, c, idx), &expected).map(|t| Some(Some(t)))
}

/// `text` with `item` in the array at `keys` (made when missing), in place of the item `ours`
/// says is setup's; `None` when that item is already `item`.
fn push(text: Option<&str>, keys: &[&str], item: &J, ours: fn(&Value) -> bool) -> Result<Option<String>> {
    let Some(t) = text else {
        return set(None, keys, &J::A(vec![item.clone()]));
    };
    let root = parse(t)?;
    let unit = indent_unit(t);
    let nodes = walk(&root, keys)?;
    if nodes.len() != keys.len() + 1 {
        return set(Some(t), keys, &J::A(vec![item.clone()]));
    }
    let arr = nodes[keys.len()];
    let Kind::Array(items) = &arr.kind else { bail!("{} is not a list", keys.join(".")) };
    let mut expected = root.value(t);
    let mut list: Vec<Value> = items.iter().map(|n| n.value(t)).collect();
    let after = match list.iter().position(ours) {
        Some(i) if list[i] == item.value() => return Ok(None),
        Some(i) => {
            list[i] = item.value();
            splice(t, items[i].start, items[i].end, &item.render(&unit, keys.len() + 1))
        }
        None => {
            list.push(item.value());
            insert(t, arr, keys.len(), &item.render(&unit, keys.len() + 1), &unit)
        }
    };
    set_value(&mut expected, keys, Value::Array(list));
    checked(after, &expected).map(Some)
}

/// `text` without the item of the array at `keys` that `ours` says is setup's (and the
/// containers setup made for it): `None` when there is none, `Some(None)` when nothing else is left.
fn pull(text: &str, keys: &[&str], ours: fn(&Value) -> bool) -> Result<Option<Option<String>>> {
    let root = parse(text)?;
    let unit = indent_unit(text);
    let nodes = walk(&root, keys)?;
    if nodes.len() != keys.len() + 1 {
        return Ok(None);
    }
    let Kind::Array(items) = &nodes[keys.len()].kind else { return Ok(None) };
    let Some(idx) = items.iter().position(|n| ours(&n.value(text))) else { return Ok(None) };
    let item = &items[idx];
    let mut hops: Vec<Hop> = keys.iter().map(|k| Hop::Key(k)).collect();
    hops.push(Hop::Item);
    cut(
        text,
        &root,
        &nodes,
        &hops,
        &text[item.start..item.end],
        &unit,
        |c, i| if i == keys.len() { idx } else { c.member(keys[i]).map(|m| m.0).expect("walked") },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_temporary_file_has_the_targets_mode_before_anything_is_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scrutate_setup_tmp_mode_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("config.toml");
        std::fs::write(&target, "env = { TOKEN = \"secret\" }\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let tmp = dir.join(".config.toml.tmp");
        drop(create_tmp(&tmp, &target).unwrap());
        assert_eq!(std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn run(bin: &str) -> Value {
        obj(hook_run(bin, true)).value()
    }

    #[test]
    fn on_windows_the_hook_runs_in_git_bash_and_powershell_alike() {
        let plain = run(r"C:\Users\cris\.cargo\bin\scrutate.exe");
        assert_eq!(plain, serde_json::json!({"command": "C:/Users/cris/.cargo/bin/scrutate.exe hook grep"}));
        let spaced = run(r"C:\Program Files\scrutate\scrutate.exe");
        assert_eq!(spaced, serde_json::json!({"command": r"C:\Program Files\scrutate\scrutate.exe", "args": ["hook", "grep"]}));
        for h in [plain, spaced] {
            assert!(is_our_hook(&serde_json::json!({"matcher": "Grep", "hooks": [h]})));
        }
        assert!(is_our_hook(&serde_json::json!({"matcher": "Grep", "hooks": [{"command": r#""C:\x\scrutate.exe" hook grep"#}]})));
        assert!(!is_our_hook(&serde_json::json!({"matcher": "Grep", "hooks": [{"command": "scrutate.exe", "args": ["serve"]}]})));
    }

    fn entry() -> J {
        obj(vec![("command", J::S("/bin/scrutate".into())), ("args", J::A(vec![J::S("serve".into())]))])
    }

    fn round_trip(before: &str) {
        let keys = ["mcpServers", NAME];
        let after = set(Some(before), &keys, &entry()).unwrap().expect("a change");
        assert_eq!(set(Some(&after), &keys, &entry()).unwrap(), None, "a second run changes nothing");
        assert_eq!(unset(&after, &keys).unwrap(), Some(Some(before.to_string())), "after:\n{after}");
    }

    #[test]
    fn a_member_goes_in_and_comes_out_byte_for_byte() {
        round_trip("{}\n");
        round_trip("{\n}\n");
        round_trip("{\"a\":1}");
        round_trip("{\n  \"a\": 1\n}\n");
        round_trip("{\n  \"a\": 1,\n}\n");
        round_trip("{\n\t\"mcpServers\": {\n\t\t\"other\": {\"command\": \"x\"}\n\t}\n}\n");
        round_trip("{\n  \"mcpServers\": {}\n}\n");
        round_trip("// comment\n{\n  /* keep */ \"a\": [1, 2], // tail\n  \"b\": {\"c\": null}\n}\n");
    }

    #[test]
    fn a_file_setup_made_is_deleted_and_a_bad_one_refused() {
        let made = set(None, &["mcpServers", NAME], &entry()).unwrap().unwrap();
        assert_eq!(made, "{\n  \"mcpServers\": {\n    \"scrutate\": {\n      \"command\": \"/bin/scrutate\",\n      \"args\": [\n        \"serve\"\n      ]\n    }\n  }\n}\n");
        assert_eq!(unset(&made, &["mcpServers", NAME]).unwrap(), Some(None));
        assert!(set(Some("{\"a\": }"), &["mcpServers", NAME], &entry()).is_err());
        assert!(set(Some("{\"mcpServers\": 3}"), &["mcpServers", NAME], &entry()).is_err());
    }

    #[test]
    fn a_hook_item_goes_into_a_list_and_comes_out() {
        let item = obj(vec![
            ("matcher", J::S("Grep".into())),
            ("hooks", J::A(vec![obj(vec![("type", J::S("command".into())), ("command", J::S("/x/scrutate hook grep".into()))])])),
        ]);
        let keys = ["hooks", "PreToolUse"];
        for before in
            ["{}\n", "{\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\"matcher\": \"Bash\", \"hooks\": []}\n    ]\n  }\n}\n", "{\n  \"hooks\": {}\n}\n"]
        {
            let after = push(Some(before), &keys, &item, is_our_hook).unwrap().unwrap();
            assert_eq!(push(Some(&after), &keys, &item, is_our_hook).unwrap(), None);
            assert_eq!(pull(&after, &keys, is_our_hook).unwrap(), Some(Some(before.to_string())), "after:\n{after}");
        }
    }

    #[test]
    fn a_toml_table_goes_in_and_comes_out_with_the_comments() {
        for before in ["", "model = \"x\" # mine\n", "# top\n[mcp_servers.other]\ncommand = \"y\"\n", "a = 1"] {
            let old: toml::Table = before.parse().unwrap();
            let block = "[mcp_servers.scrutate]\ncommand = \"/bin/s\"\nargs = [\"serve\"]\n";
            let after = if before.is_empty() { block.to_string() } else { format!("{before}\n{block}") };
            let mid: toml::Table = after.parse().unwrap();
            assert_ne!(mid, old);
            assert_eq!(toml_unset(&after, &mid).unwrap(), before);
        }
        assert_eq!(toml_str("C:\\a \"b\""), "\"C:\\\\a \\\"b\\\"\"");
    }

    #[test]
    fn a_diff_shows_the_changed_lines() {
        let mut out = String::new();
        diff(&mut out, "a\nb\nc\n", "a\nb\nX\nc\n");
        assert_eq!(out, "  + X\n");
    }
}
