//! `scrutate review` on git repositories made for each test: a base with a call chain, a test
//! file and a duplicate pair, and a head that edits the deepest function and adds a copy of
//! another; then the GitLab Code Quality report and the GitHub Action's comment script against a
//! stand-in `gh` that replays a recorded list of comments.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

/// git's own configuration is the only one read: no global or system file.
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("review_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Tree { root: canon(root) }
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        let mut c = Command::new("git");
        c.args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]).args(args).current_dir(&self.root);
        let out = c.envs(GIT_ENV).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// (exit code, standard output, standard error)
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN)
            .args(["--root", self.root.to_str().unwrap()])
            .args(args)
            .current_dir(&self.root)
            .envs(GIT_ENV)
            .env("SCRUTATE_SEMANTIC", "none")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn review(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(&[&["review"], args].concat());
        assert_eq!(code, 0, "review {args:?}: {out}{err}");
        out
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A function body long enough for `dupes` (30 body tokens), its words from `seed`.
fn long_fn(name: &str, seed: &str) -> String {
    format!(
        "def {name}(rows):\n    total = 0\n    for row in rows:\n        if row.amount > {seed}_limit and row.status == \"open\":\n            \
         total += compute_{seed}_weight(row.amount, row.discount, row.region)\n        elif row.status == \"closed\":\n            \
         total -= penalty_for_{seed}(row.region, row.amount, row.owner)\n        else:\n            total = adjust_{seed}(total, row.created, row.updated)\n    \
         return normalize_{seed}_total(total, rows, scale_factor, offset_value, rounding_mode)\n"
    )
}

/// The base commit: `top` calls `middle` calls `deepest`, one test reaches `top`, another tests
/// `spare`; `old_a` and `old_b` are already exact duplicates, `unique` has no copy.
fn base(name: &str) -> (Tree, String) {
    let t = Tree::new(name);
    t.git(&["init", "-q"]);
    t.write("lib/__init__.py", "");
    t.write("lib/core.py", "def deepest(x):\n    return x + 1\n\n\ndef spare(x):\n    return x\n");
    t.write("lib/mid.py", "from lib.core import deepest\n\n\ndef middle(x):\n    return deepest(x) * 2\n");
    t.write("lib/top.py", "from lib.mid import middle\n\n\ndef top(x):\n    return middle(x)\n");
    t.write("tests/test_top.py", "from lib.top import top\n\n\ndef test_top():\n    assert top(1) == 4\n");
    t.write("tests/test_other.py", "from lib.core import spare\n\n\ndef test_spare():\n    assert spare(1) == 1\n");
    t.write("lib/dups.py", &format!("{}\n\n{}\n\n{}", long_fn("old_a", "alpha"), long_fn("old_b", "alpha"), long_fn("unique", "beta")));
    let sha = t.commit("base");
    (t, sha)
}

/// The head: `deepest` edited, `copy_of_unique` added in a new file, `old_b` edited by one word
/// (still a duplicate of `old_a`, as in the base).
fn head(t: &Tree) -> String {
    t.write("lib/core.py", "def deepest(x):\n    return x + 2\n\n\ndef spare(x):\n    return x\n");
    t.write("lib/fresh.py", &long_fn("copy_of_unique", "beta"));
    let dups = fs::read_to_string(t.root.join("lib/dups.py")).unwrap();
    let edited = dups.replacen("rounding_mode", "rounding_style", 2);
    let (a, b) = edited.split_once("def old_b").unwrap();
    t.write("lib/dups.py", &format!("{}def old_b{}", a.replace("rounding_style", "rounding_mode"), b));
    t.commit("head")
}

#[test]
fn the_comment_names_the_changed_symbols_their_callers_the_tests_and_only_the_new_duplicate() {
    let (t, base_sha) = base("comment");
    let head_sha = head(&t);
    // the pair the base already had is a duplicate in the head too: only the new copy is new
    let (_, dupes, _) = t.run(&["dupes", "--json"]);
    assert!(dupes.contains("old_a") && dupes.contains("copy_of_unique"), "{dupes}");

    let md = t.review(&["--base", &base_sha]);
    let lines: Vec<&str> = md.lines().collect();
    assert_eq!(lines[0], "<!-- scrutate-review -->", "{md}");
    assert_eq!(lines[1], format!("**scrutate review** of `HEAD` ({}) against `{}`", &head_sha[..12], &base_sha[..12]), "{md}");
    assert!(md.contains("\n- `lib/core.py:1-2` function `deepest` (edited)\n"), "{md}");
    // `old_b` is changed, and still the duplicate of `old_a` it was in the base
    assert!(md.contains("\n- `lib/dups.py:13-22` function `old_b` (edited, no test reaches it)\n"), "{md}");
    assert!(md.contains("\n- `lib/fresh.py:1-10` function `copy_of_unique` (added, no test reaches it)\n"), "{md}");
    assert!(md.contains("\n- `lib/mid.py:4-5` function `middle` (distance 1)\n- `lib/top.py:4-5` function `top` (distance 2)\n"), "{md}");
    assert!(md.contains("#### Tests to run\n\n- `tests/test_top.py` (distance 3, through `test_top`)\n"), "{md}");
    assert!(!md.contains("test_other"), "{md}");
    let dup_section = md.split("#### New duplicates\n\n").nth(1).unwrap_or_else(|| panic!("{md}"));
    let dup_lines: Vec<&str> = dup_section.lines().take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(dup_lines.len(), 1, "{md}");
    assert!(dup_lines[0].starts_with("- exact, ") && dup_lines[0].contains("`copy_of_unique`") && dup_lines[0].contains("`unique`"), "{md}");
    assert!(!dup_section.contains("old_a"), "{md}");
    assert!(lines[3].starts_with("3 changed definitions, ") && lines[3].contains("1 new duplicate"), "{md}");

    // the same on every run, an explicit head is the same report, and links point at the lines
    assert_eq!(t.review(&["--base", &base_sha]), md);
    assert_eq!(t.review(&["--base", &base_sha, "--head", &head_sha]).lines().skip(2).collect::<Vec<_>>(), lines[2..].to_vec());
    let linked = t.review(&["--base", &base_sha, "--link", "https://github.com/o/r/blob/abc/"]);
    assert!(linked.contains("- [`lib/core.py:1-2`](https://github.com/o/r/blob/abc/lib/core.py#L1-L2) function `deepest` (edited)"), "{linked}");

    // the JSON holds the same items
    let v: Value = serde_json::from_str(&t.review(&["--base", &base_sha, "--format", "json"])).unwrap();
    let r = &v["result"];
    assert_eq!(r["head_commit"], head_sha);
    assert_eq!(r["new_duplicates"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["untested"], serde_json::json!(["lib/dups.py:13-22", "lib/fresh.py:1-10"]));
    assert_eq!(r["tests"][0]["path"], "tests/test_top.py");
}

#[test]
fn review_and_impact_report_one_list_of_findings() {
    let (t, base_sha) = base("one_list");
    head(&t);
    let review: Value = serde_json::from_str(&t.review(&["--base", &base_sha, "--format", "json"])).unwrap();
    let (code, out, err) = t.run(&["impact", "--base", &base_sha, "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let impact: Value = serde_json::from_str(&out).unwrap();
    let findings = &review["result"]["findings"];
    assert_eq!(findings.as_array().map(Vec::len), Some(3), "{review}");
    assert_eq!(findings, &impact["result"]["findings"]);
    // the older keys are the same list read two ways
    let kinds: Vec<&str> = findings.as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["duplicate", "untested-change", "untested-change"]);
    assert_eq!(review["result"]["new_duplicates"].as_array().unwrap().len(), 1);
    let untested: Vec<String> =
        findings.as_array().unwrap()[1..].iter().map(|f| format!("{}:{}-{}", f["path"].as_str().unwrap(), f["start"], f["end"])).collect();
    assert_eq!(review["result"]["untested"], serde_json::json!(untested));
}

/// Checks a value against GitLab's Code Quality format: a list of issues, each with a
/// `description`, `check_name`, `fingerprint`, `severity` of the five GitLab knows, and a
/// `location` with a relative `path` and `lines.begin`, a line number from 1.
fn code_quality_errors(v: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(issues) = v.as_array() else { return vec!["not a list".into()] };
    let mut prints = std::collections::HashSet::new();
    for (n, i) in issues.iter().enumerate() {
        let mut err = |what: &str| errors.push(format!("issue {n}: {what}: {i}"));
        for key in ["description", "check_name", "fingerprint"] {
            if !i[key].as_str().is_some_and(|s| !s.is_empty()) {
                err(&format!("{key} is not a non-empty string"));
            }
        }
        if !matches!(i["severity"].as_str(), Some("info" | "minor" | "major" | "critical" | "blocker")) {
            err("severity is not one of info, minor, major, critical, blocker");
        }
        match i["location"]["path"].as_str() {
            Some(p) if !p.is_empty() && !p.starts_with('/') && !p.contains('\\') => {}
            _ => err("location.path is not a relative path"),
        }
        if !i["location"]["lines"]["begin"].as_u64().is_some_and(|l| l >= 1) {
            err("location.lines.begin is not a line number");
        }
        if !prints.insert(i["fingerprint"].as_str().unwrap_or_default().to_string()) {
            err("fingerprint is not unique");
        }
    }
    errors
}

#[test]
fn the_gitlab_report_is_code_quality_json_with_the_new_duplicate_and_the_untested_change() {
    let (t, base_sha) = base("gitlab");
    head(&t);
    let out = t.review(&["--base", &base_sha, "--format", "gitlab"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(code_quality_errors(&v), Vec::<String>::new());
    let issues: Vec<(&str, &str, &str, u64)> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["check_name"].as_str().unwrap(),
                i["severity"].as_str().unwrap(),
                i["location"]["path"].as_str().unwrap(),
                i["location"]["lines"]["begin"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        issues,
        [
            ("scrutate-duplicate", "minor", "lib/fresh.py", 1),
            ("scrutate-untested-change", "info", "lib/dups.py", 13),
            ("scrutate-untested-change", "info", "lib/fresh.py", 1)
        ],
        "{out}"
    );
    assert!(v[0]["description"].as_str().unwrap().contains("unique at lib/dups.py:"), "{out}");
    // the fingerprints stay when the lines move, so GitLab matches the issue across pushes
    t.write("lib/fresh.py", &format!("\n\n{}", long_fn("copy_of_unique", "beta")));
    t.commit("moved");
    let moved: Value = serde_json::from_str(&t.review(&["--base", &base_sha, "--format", "gitlab"])).unwrap();
    assert_eq!(moved[0]["fingerprint"], v[0]["fingerprint"]);
    assert_eq!(moved[0]["location"]["lines"]["begin"], 3);
    // the checker refuses what GitLab refuses
    assert!(!code_quality_errors(
        &serde_json::json!([{"description": "x", "check_name": "c", "fingerprint": "f", "severity": "bad", "location": {"path": "/a", "lines": {"begin": 0}}}])
    )
    .is_empty());
}

#[test]
fn no_change_is_a_report_that_says_so_and_a_bad_revision_is_an_error() {
    let (t, base_sha) = base("empty");
    let (code, out, _) = t.run(&["review", "--base", &base_sha]);
    assert_eq!(code, 1);
    assert!(out.starts_with("<!-- scrutate-review -->\n") && out.contains("No changed definitions and no new duplicates."), "{out}");
    let (code, out, _) = t.run(&["review", "--base", &base_sha, "--format", "gitlab"]);
    assert_eq!((code, out.trim()), (1, "[]"));
    let (code, _, err) = t.run(&["review", "--base=--output=x"]);
    assert_eq!(code, 2);
    assert!(err.contains("not a revision: --output=x"), "{err}");
    let (code, _, err) = t.run(&["review", "--base", &base_sha, "--head=--output=x"]);
    assert_eq!(code, 2);
    assert!(err.contains("not a revision: --output=x"), "{err}");
    let (code, _, err) = t.run(&["review", "--base", &base_sha, "--head", "nope"]);
    assert_eq!(code, 2);
    assert!(err.contains("not a revision of this repository: nope"), "{err}");
    assert!(!t.root.join("x").exists());
}

#[test]
fn a_moved_duplicate_stays_out_and_a_third_copy_of_a_pair_is_new() {
    let (t, base_sha) = base("moved");
    // `old_b` moves to another file unchanged: the pair is the base's, by its body text
    let dups = fs::read_to_string(t.root.join("lib/dups.py")).unwrap();
    let old_b = long_fn("old_b", "alpha");
    t.write("lib/dups.py", &dups.replacen(&old_b, "", 1));
    t.write("lib/moved.py", &old_b);
    t.commit("moved");
    let md = t.review(&["--base", &base_sha]);
    assert!(!md.contains("#### New duplicates"), "{md}");
    // a third copy of the pair makes a group the base did not have
    t.write("lib/third.py", &long_fn("old_c", "alpha"));
    t.commit("third");
    let md = t.review(&["--base", &base_sha]);
    let dup = md.split("#### New duplicates\n\n").nth(1).unwrap_or_else(|| panic!("{md}"));
    assert!(dup.starts_with("- exact, ") && dup.contains("`old_a`") && dup.contains("`old_b`") && dup.contains("`old_c`"), "{md}");
}

/// The GitHub Action's comment script, run with a stand-in `gh` that logs its arguments, prints
/// the recorded comments for a list call and keeps the body it was given.
#[cfg(unix)]
mod comment_script {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.github/actions/scrutate/comment.sh");
    const FAKE_GH: &str = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_LOG\"\n[ -z \"$FAKE_FAIL\" ] || exit 1\ncase \"$*\" in\n  *--paginate*) cat \"$FAKE_COMMENTS\" ;;\n  \"api user -q .login\") [ -n \"$FAKE_LOGIN\" ] || exit 1; echo \"$FAKE_LOGIN\" ;;\n  *) for a; do case $a in body=@*) cp \"${a#body=@}\" \"$FAKE_BODY\" ;; esac; done; echo '{}' ;;\nesac\n";

    struct Run {
        code: i32,
        out: String,
        log: String,
        body: String,
    }

    fn run(t: &Tree, comments: &str, env: &[(&str, &str)]) -> Run {
        let dir = t.root.join(".fake");
        fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        fs::write(&gh, FAKE_GH).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("comments.json"), comments).unwrap();
        let _ = fs::remove_file(dir.join("log"));
        let _ = fs::remove_file(dir.join("body"));
        let bin_dir = Path::new(BIN).parent().unwrap();
        let path = format!("{}:{}:{}", dir.display(), bin_dir.display(), std::env::var("PATH").unwrap_or_default());
        let out = Command::new("bash")
            .arg(SCRIPT)
            .current_dir(&t.root)
            .envs(GIT_ENV)
            .env("PATH", path)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("FAKE_LOG", dir.join("log"))
            .env("FAKE_COMMENTS", dir.join("comments.json"))
            .env("FAKE_BODY", dir.join("body"))
            .env("FAKE_FAIL", "")
            .env("RUNNER_TEMP", &dir)
            .env("REPO", "o/r")
            .env("SERVER_URL", "https://github.com")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            log: fs::read_to_string(dir.join("log")).unwrap_or_default(),
            body: fs::read_to_string(dir.join("body")).unwrap_or_default(),
        }
    }

    fn comment(id: u64, kind: &str, body: &str) -> Value {
        serde_json::json!({"id": id, "user": {"type": kind, "login": if kind == "Bot" { "github-actions[bot]" } else { "someone" }}, "body": body})
    }

    #[test]
    fn the_comment_is_posted_once_then_edited_in_place() {
        let (t, base_sha) = base("script");
        head(&t);
        // .fake is no part of the repository
        t.write(".gitignore", ".fake/\n");
        t.commit("ignore");
        let pr = [("PR_NUMBER", "5"), ("BASE_SHA", base_sha.as_str())];

        // no earlier comment: one is posted on the pull request
        let first = run(&t, "[]", &pr);
        assert_eq!(first.code, 0, "{}", first.out);
        let calls: Vec<&str> = first.log.lines().collect();
        assert_eq!(
            calls,
            [
                "api --paginate repos/o/r/issues/5/comments?per_page=100",
                &format!("api -X POST repos/o/r/issues/5/comments -F body=@{}", t.root.join(".fake/scrutate-review.md").display())
            ]
        );
        assert!(first.body.starts_with("<!-- scrutate-review -->\n") && first.body.contains("`copy_of_unique`"), "{}", first.body);
        assert!(first.body.contains("](https://github.com/o/r/blob/"), "{}", first.body);

        // the bot's comment with the marker is edited, on the second page too; a person's comment
        // that quotes the marker and a bot's comment without it are left alone
        let pages = format!(
            "{}\n{}",
            Value::Array(vec![comment(3, "User", "<!-- scrutate-review -->\nquoted"), comment(4, "Bot", "another bot")]),
            Value::Array(vec![comment(7, "Bot", &first.body), comment(9, "Bot", "<!-- scrutate-review -->\nlater")])
        );
        let second = run(&t, &pages, &pr);
        assert_eq!(second.code, 0, "{}", second.out);
        assert_eq!(
            second.log.lines().nth(1).unwrap(),
            format!("api -X PATCH repos/o/r/issues/comments/7 -F body=@{}", t.root.join(".fake/scrutate-review.md").display())
        );
        assert_eq!(second.log.lines().count(), 2);
        assert!(second.out.contains("edited comment 7"), "{}", second.out);
    }

    #[test]
    fn with_a_persons_token_the_comment_of_that_person_is_edited() {
        let (t, base_sha) = base("scriptuser");
        head(&t);
        t.write(".gitignore", ".fake/\n");
        t.commit("ignore");
        let theirs = serde_json::json!({"id": 11, "user": {"type": "User", "login": "the-owner"}, "body": "<!-- scrutate-review -->\nearlier"});
        let other = serde_json::json!({"id": 12, "user": {"type": "User", "login": "someone"}, "body": "<!-- scrutate-review -->\nquoted"});
        let comments = Value::Array(vec![other, theirs]).to_string();
        let pr = [("PR_NUMBER", "5"), ("BASE_SHA", base_sha.as_str()), ("FAKE_LOGIN", "the-owner")];
        let run1 = run(&t, &comments, &pr);
        assert_eq!(run1.code, 0, "{}", run1.out);
        assert!(run1.log.contains("api -X PATCH repos/o/r/issues/comments/11"), "{}", run1.log);
        // GITHUB_TOKEN cannot ask who it is: a person's comment is never taken for the bot's
        let run2 = run(&t, &comments, &[("PR_NUMBER", "5"), ("BASE_SHA", base_sha.as_str()), ("FAKE_LOGIN", "")]);
        assert_eq!(run2.code, 0, "{}", run2.out);
        assert!(run2.log.contains("api -X POST repos/o/r/issues/5/comments"), "{}", run2.log);
    }

    #[test]
    fn a_failure_is_a_warning_and_no_input_reaches_a_shell() {
        let (t, base_sha) = base("scriptfail");
        // gh fails: a warning, exit 0
        let failed = run(&t, "[]", &[("PR_NUMBER", "5"), ("BASE_SHA", &base_sha), ("FAKE_FAIL", "1")]);
        assert_eq!(failed.code, 0);
        assert!(failed.out.contains("::warning::scrutate review comment: cannot list"), "{}", failed.out);
        // not a pull request: nothing runs
        let none = run(&t, "[]", &[("PR_NUMBER", ""), ("BASE_SHA", &base_sha)]);
        assert_eq!((none.code, none.log.as_str()), (0, ""));
        assert!(none.out.contains("not a pull request"), "{}", none.out);
        // values that would run a command in a shell are refused before anything runs
        let marker = t.root.join("ran");
        let evil = format!("$(touch {})", marker.display());
        let cases = [
            [("PR_NUMBER", evil.as_str()), ("BASE_SHA", base_sha.as_str()), ("REPO", "o/r")],
            [("PR_NUMBER", "5"), ("BASE_SHA", evil.as_str()), ("REPO", "o/r")],
            [("PR_NUMBER", "5"), ("BASE_SHA", base_sha.as_str()), ("REPO", evil.as_str())],
        ];
        for env in cases {
            let r = run(&t, "[]", &env);
            assert_eq!((r.code, r.log.as_str()), (0, ""), "{}", r.out);
            assert!(r.out.contains("::warning::"), "{}", r.out);
        }
        assert!(!marker.exists());
        // a base commit the checkout does not have
        let missing = run(&t, "[]", &[("PR_NUMBER", "5"), ("BASE_SHA", &"1".repeat(40))]);
        assert!(missing.out.contains("fetch-depth: 0") && missing.log.is_empty(), "{}", missing.out);
    }
}

#[test]
fn a_test_file_the_head_deletes_is_no_test_to_run() {
    let (t, base_sha) = base("rm_test");
    t.git(&["rm", "-q", "tests/test_other.py"]);
    t.commit("drop a test");
    let md = t.review(&["--base", &base_sha]);
    assert!(!md.contains("`tests/test_other.py` (distance"), "{md}");
}

/// A renamed file's old path is written by whoever opened the pull request: it is a code span,
/// as every other path is, so it opens no comment, link or mention.
#[test]
fn a_rename_note_quotes_the_old_path() {
    let (t, _) = base("rename_note");
    let mut odd = vec!["lib/@octocat [click](https_evil.example).py"];
    if cfg!(unix) {
        odd.push("lib/x<!--.py");
    }
    for (i, old) in odd.iter().enumerate() {
        t.write(old, &format!("def moved_{i}():\n    return {i}\n"));
    }
    t.commit("odd names");
    let base_sha2 = t.git(&["rev-parse", "HEAD"]);
    for (i, old) in odd.iter().enumerate() {
        t.git(&["mv", old, &format!("lib/renamed_{i}.py")]);
    }
    t.commit("renames");
    let md = t.review(&["--base", &base_sha2]);
    for (i, old) in odd.iter().enumerate() {
        assert!(md.contains(&format!("- `lib/renamed_{i}.py`: renamed from `{old}`\n")), "{md}");
    }
}

/// Four changed functions with 500 callers each, and a fifth with one: the walk stops at its
/// 2,000, and the comment's list of them runs past its cap.
fn many_callers(name: &str, base_ref: &str) -> (Tree, String) {
    let t = Tree::new(name);
    t.git(&["init", "-q"]);
    for i in 0..5 {
        t.write(&format!("lib/f{i}.py"), &format!("def f{i}(x):\n    return x + {i}\n"));
        let callers = if i < 4 { 500 } else { 0 };
        for file in 0..callers / 100 {
            let defs: String = (0..100).map(|j| format!("def caller_number_{i}_{file}_{j}(x):\n    return f{i}(x)\n\n\n")).collect();
            t.write(&format!("calls{i}/m{file}.py"), &format!("from lib.f{i} import f{i}\n\n\n{defs}"));
        }
    }
    // the caller past the 2,000 stops the walk
    t.write("calls4/m0.py", "from lib.f4 import f4\n\n\ndef past_the_cut(x):\n    return f4(x)\n");
    t.commit("base");
    t.git(&["branch", base_ref]);
    for i in 0..5 {
        t.write(&format!("lib/f{i}.py"), &format!("def f{i}(x):\n    return x + {i} + 1\n"));
    }
    let head = t.commit("head");
    (t, head)
}

#[test]
fn a_capped_comment_keeps_the_line_that_the_walk_stopped() {
    let (t, _) = many_callers("capped_stop", "base");
    let md = t.review(&["--base", "base", "--depth", "1"]);
    assert!(md.len() <= 65_536, "{}", md.len());
    assert!(md.contains("_Cut at 65536 characters"), "not capped: {}", md.len());
    assert!(md.contains("_The walk stopped at 2000 callers._"), "{}", &md[md.len() - 300..]);
}

/// The base a pull request names goes into the cut note: in a code span, as every other name.
#[test]
fn the_cut_note_quotes_the_base_it_names() {
    let odd = "b`<!--x";
    let (t, _) = many_callers("capped_base", odd);
    let md = t.review(&["--base", odd, "--depth", "1"]);
    assert!(md.contains("`` scrutate review --base b`<!--x ``"), "{}", &md[md.len() - 300..]);
}
