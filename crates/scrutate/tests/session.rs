//! A command line call that uses the session `scrutate serve` runs: the same bytes as a fresh
//! process, the files as they are now, and every way the session can be passed over.
#![cfg(unix)]

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/tag.rs"]
mod tag;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_sess_{name}_{}", std::process::id()));
        tag::claim(&root);
        Repo::at(root)
    }

    fn at(root: PathBuf) -> Repo {
        let _ = fs::remove_dir_all(&root);
        // a repository: no index in a directory above takes it over
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(
            root.join("pkg/geo.py"),
            "import os\n\n\nclass Point:\n    def norm(self):\n        return helper(self)\n\n\ndef helper(p):\n    return os.path.join(\"a\", \"b\")\n",
        )
        .unwrap();
        fs::write(
            root.join("pkg/use.py"),
            "from pkg.geo import Point, helper\n\n\ndef main():\n    return helper(Point())\n\n\ndef twin():\n    return helper(Point())\n",
        )
        .unwrap();
        Repo { root: canon(&root) }
    }

    fn socket(&self) -> PathBuf {
        self.root.join(".scrutate/session.sock")
    }

    /// (exit code, stdout, stderr) of `scrutate ARGS` in the root, with `env`.
    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        cli_with(BIN, &self.root, args, env)
    }

    /// `cli` with the session's debug lines on: (the answer as without them, whether the session gave it).
    /// A call whose greeting did not come within the command line's 20 ms (a loaded machine) did
    /// the work itself; when that changed nothing, it is asked again, at most 5 times.
    fn through(&self, args: &[&str], env: &[(&str, &str)]) -> ((i32, String, String), String) {
        let mut env = env.to_vec();
        env.push(("SCRUTATE_SESSION_DEBUG", "1"));
        // a CI runner can be too busy to greet in 20 ms; what is tested is the answer, not the wait
        env.push(("SCRUTATE_SESSION_WAIT_MS", "5000"));
        for attempt in 1.. {
            let (code, out, err) = self.cli(args, &env);
            let (said, rest): (Vec<&str>, Vec<&str>) = err.lines().partition(|l| l.starts_with("[scrutate] session: "));
            let rest = rest.iter().map(|l| format!("{l}\n")).collect::<String>();
            let said = said.join("\n");
            if attempt < 5 && said.starts_with("[scrutate] session: no greeting") && !rest.contains("indexed ") {
                continue;
            }
            return ((code, out, rest), said);
        }
        unreachable!()
    }

    fn fresh(&self, args: &[&str]) -> (i32, String, String) {
        self.cli(args, &[("SCRUTATE_NO_SESSION", "1")])
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn cli_with(bin: &str, root: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = Command::new(bin);
    c.args(args).current_dir(root).env("SCRUTATE_ROOT", ".").stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    // a copy of the binary just written can be busy for a moment on Linux: a child another test
    // forked meanwhile holds the writing descriptor until it runs its own program
    let mut tries = 0;
    let out = loop {
        match c.output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 100 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            r => break r.unwrap(),
        }
    };
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// `scrutate serve` on a root, its first build done and its socket in place (when it makes one).
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    n: i64,
}

impl Server {
    fn start(root: &Path) -> Server {
        let mut child = Command::new(BIN)
            .args(["serve"])
            .current_dir(root)
            .env("SCRUTATE_ROOT", ".")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (stdin, stdout) = (child.stdin.take(), BufReader::new(child.stdout.take().unwrap()));
        let mut s = Server { child, stdin, stdout, n: 0 };
        // an answer to a tool call comes after the first build
        assert!(s.call("find_symbol", json!({"name": "helper"})).contains("pkg/geo.py"));
        s
    }

    fn call(&mut self, name: &str, args: Value) -> String {
        self.n += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.n, "method": "tools/call", "params": {"name": name, "arguments": args}});
        writeln!(self.stdin.as_mut().unwrap(), "{msg}").unwrap();
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == self.n {
                return v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
            }
        }
    }

    /// Close stdin and wait for the exit, as a client that is done.
    fn stop(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const COMMANDS: &[&[&str]] = &[
    &["symbol", "helper"],
    &["symbol", "Point.norm", "--json"],
    &["symbol", "no_such_name"],
    &["callers", "helper"],
    &["callers", "helper", "--json"],
    &["callees", "main"],
    &["refs", "helper"],
    &["refs", "helper", "--textual", "--json"],
    &["outline", "pkg/use.py"],
    &["outline", "pkg/none.py"],
    &["deps", "pkg/use.py", "--json"],
    &["rdeps", "pkg/geo.py"],
    &["read", "--symbol", "Point.norm"],
    &["read", "pkg/geo.py", "--start", "4", "--end", "6"],
    &["similar", "main"],
    &["dupes", "--min-tokens", "3", "--json"],
    &["--max-bytes", "1000", "--offset", "1", "callers", "helper"],
];

/// A ranked search and a ranked find, with every option they take.
const SEARCHES: &[&[&str]] = &[
    &["search", "helper"],
    &["search", "join a path", "-k", "2"],
    &["search", "helper", "--json"],
    &["search", "point norm", "--why"],
    &["search", "point norm", "-a", "helper", "-a", "", "-a", "point norm", "--why", "-k", "3"],
    &["search", "zzqx_nothing_here"],
    &["search", "zzqx_nothing_here", "--json"],
    &["search", "helper", "--also", "."],
    &["--max-bytes", "120", "search", "helper"],
    &["--max-bytes", "300", "--offset", "1", "search", "helper", "--why"],
    &["--offset", "9", "search", "helper"],
    &["find", "helper"],
    &["find", "how is a path joined?"],
    &["find", "helper", "-k", "1", "--json"],
    &["find", "zzqx_nothing_here"],
    &["find", "zzqx(nothing"],
    &["--max-bytes", "100", "find", "point norm"],
];

#[test]
fn a_ranked_search_through_a_session_prints_what_a_fresh_process_prints() {
    let r = Repo::new("search");
    let _server = Server::start(&r.root);
    for args in SEARCHES {
        let (through, said) = r.through(args, &[]);
        assert_eq!(said, "[scrutate] session: answered by the session", "{args:?}");
        assert_eq!(through, r.fresh(args), "{args:?}");
    }
    // an edit made just before is in the answer
    fs::write(r.root.join("pkg/late.py"), "def frobnicate_widgets():\n    return 1\n").unwrap();
    let ((code, out, err), said) = r.through(&["search", "frobnicate widgets", "-k", "1"], &[]);
    assert_eq!(said, "[scrutate] session: answered by the session");
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("pkg/late.py:1-2"), "{out}");
    assert!(err.contains("indexed 1 changed files"), "{err}");
    // a search of several repositories opens each one and does its own work
    let other = Repo::new("search_other");
    let args = ["search", "helper", "--also", other.root.to_str().unwrap()];
    r.fresh(&args); // the other repository's first build, said once
    let want = r.fresh(&args);
    let (through, said) = r.through(&args, &[]);
    assert_eq!(said, "");
    assert_eq!(through, want);
}

#[test]
fn a_ranked_search_does_the_work_itself_when_the_session_is_not_its_own_or_is_gone() {
    let r = Repo::new("search_differs");
    let server = Server::start(&r.root);
    for args in [&["search", "helper", "--why"][..], &["find", "helper", "--json"]] {
        let want = r.fresh(args);
        let (got, said) = r.through(args, &[("SCRUTATE_THREADS", "2")]);
        assert!(said.contains("the session differs"), "{said}");
        assert_eq!(got, want);
        let (got, said) = r.through(args, &[("SCRUTATE_NO_SESSION", "1")]);
        assert_eq!((said.as_str(), &got), ("", &want));
    }
    drop(server);
    let (got, said) = r.through(&["search", "helper"], &[]);
    assert!(said.contains("no session at"), "{said}");
    assert_eq!(got, r.fresh(&["search", "helper"]));
}

#[test]
fn a_long_socket_path_leaves_relative_arguments_to_the_callers_directory() {
    let base = std::env::temp_dir().join(format!("scrutate_sess_far_{}", std::process::id()));
    let rel = format!("{}/{}", "d".repeat(60), "e".repeat(60));
    let r = Repo::at(base.join(&rel));
    assert!(r.socket().as_os_str().len() > 110);
    let _server = Server::start(&r.root);
    let base = canon(&base);
    let file = format!("{rel}/pkg/use.py");
    let run = |args: &[&str], env: &[(&str, &str)]| {
        let mut c = Command::new(BIN);
        c.arg("--root").arg(&rel).args(args).current_dir(&base).env_remove("SCRUTATE_ROOT").env("SCRUTATE_SESSION_DEBUG", "1").stdin(Stdio::null());
        // a loaded machine can take more than the 20 ms greeting wait, and the call then does the
        // work itself: what is tested is where it runs relative paths from
        c.env("SCRUTATE_SESSION_WAIT_MS", "5000");
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    for args in [&["search", "helper"][..], &["outline", &file]] {
        let (code, out, err) = run(args, &[("SCRUTATE_NO_SESSION", "1")]);
        assert_eq!(code, 0, "{args:?}: {err}");
        let (c, o, e) = run(args, &[]);
        assert!(e.contains("answered by the session"), "{args:?}: {e}");
        assert_eq!((c, &o), (code, &out), "{args:?}");
        // a session that differs: the command line came back from the socket's directory and
        // does the work itself, from where it was started
        let (c, o, e) = run(args, &[("SCRUTATE_THREADS", "2")]);
        assert!(e.contains("the session differs"), "{args:?}: {e}");
        assert_eq!((c, &o), (code, &out), "{args:?}");
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_command_line_call_through_a_session_prints_what_a_fresh_process_prints() {
    let r = Repo::new("same");
    let _server = Server::start(&r.root);
    assert!(r.socket().exists());
    for args in COMMANDS {
        let (through, said) = r.through(args, &[]);
        assert_eq!(said, "[scrutate] session: answered by the session", "{args:?}");
        assert_eq!(through, r.fresh(args), "{args:?}");
    }
}

#[test]
fn an_edit_is_in_the_next_answer_through_the_session() {
    let r = Repo::new("edit");
    let _server = Server::start(&r.root);
    fs::write(r.root.join("pkg/late.py"), "def arrived_late():\n    return 1\n").unwrap();
    let ((code, out, err), said) = r.through(&["symbol", "arrived_late"], &[]);
    assert_eq!(said, "[scrutate] session: answered by the session");
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("pkg/late.py:1-2"), "{out}");
    assert!(err.contains("indexed 1 changed files"), "{err}");
    // a definition renamed in place is gone from the next answer, and its new name is there
    fs::write(r.root.join("pkg/late.py"), "def renamed_now():\n    return 1\n").unwrap();
    assert_eq!(r.through(&["symbol", "arrived_late"], &[]).0 .0, 1);
    assert!(r.through(&["symbol", "renamed_now"], &[]).0 .1.starts_with("pkg/late.py:1-2"));
    fs::remove_file(r.root.join("pkg/late.py")).unwrap();
    assert_eq!(r.through(&["symbol", "renamed_now"], &[]).0 .0, 1);
}

#[test]
fn a_call_does_the_work_itself_when_the_session_is_not_its_own() {
    let r = Repo::new("differs");
    let _server = Server::start(&r.root);
    let want = r.fresh(&["callers", "helper"]);
    // another setting that may change an answer
    let (got, said) = r.through(&["callers", "helper"], &[("SCRUTATE_THREADS", "2")]);
    assert!(said.contains("the session differs"), "{said}");
    assert_eq!(got, want);
    // another executable: a build of another version would answer by other rules
    let copy = r.root.join("scrutate-copy");
    fs::copy(BIN, &copy).unwrap();
    let (code, out, err) = cli_with(copy.to_str().unwrap(), &r.root, &["callers", "helper"], &[("SCRUTATE_SESSION_DEBUG", "1")]);
    assert!(err.contains("the session differs"), "{err}");
    assert_eq!((code, out), (want.0, want.1.clone()));
    // the switch that turns the use off: no session is asked
    let (got, said) = r.through(&["callers", "helper"], &[("SCRUTATE_NO_SESSION", "1")]);
    assert_eq!(said, "");
    assert_eq!(got, want);
}

#[test]
fn a_socket_nobody_listens_on_or_that_is_no_socket_is_passed_over() {
    let r = Repo::new("stale");
    let server = Server::start(&r.root);
    let want = r.fresh(&["symbol", "helper"]);
    // a session that died without removing its socket
    drop(server);
    assert!(r.socket().exists(), "a killed server leaves its socket");
    let (got, said) = r.through(&["symbol", "helper"], &[]);
    // refused at the connect, so no greeting or answer wait ran: a wait would end "no greeting"
    // or "no answer"
    assert!(said.contains("no session at") && said.contains("Connection refused"), "{said}");
    assert_eq!(got, want);
    // the next server takes the place of the stale socket
    let _server = Server::start(&r.root);
    assert_eq!(r.through(&["symbol", "helper"], &[]).1, "[scrutate] session: answered by the session");
    drop(_server);
    fs::remove_file(r.socket()).unwrap();
    fs::write(r.socket(), "not a socket").unwrap();
    let (got, said) = r.through(&["symbol", "helper"], &[]);
    assert!(said.contains("is not a socket of this user"), "{said}");
    assert_eq!(got, want);
    // nor does a server take it over
    let _server = Server::start(&r.root);
    assert_eq!(fs::read_to_string(r.socket()).unwrap(), "not a socket");
}

/// A session that greets and then reads nothing holds no call whose request is longer than the
/// socket takes: the command line gives it up and does the work itself.
#[cfg(unix)]
#[test]
fn a_session_that_reads_no_request_is_given_up_and_the_call_does_its_own_work() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    let r = Repo::new("reads_nothing");
    let server = Server::start(&r.root);
    // the socket through a short link: macOS's temporary directory makes its own path longer than
    // a socket address holds (the product goes through session::unix::short)
    let near = PathBuf::from(format!("/tmp/scrutate-rn-{}", std::process::id()));
    let _ = fs::remove_file(&near);
    std::os::unix::fs::symlink(r.root.join(".scrutate"), &near).unwrap();
    let socket = near.join("session.sock");
    // a real session's greeting for this root and build, replayed by a listener that never reads
    let hello = {
        let c = UnixStream::connect(&socket).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut line = String::new();
        BufReader::new(&c).read_line(&mut line).unwrap();
        line
    };
    server.stop();
    let _ = fs::remove_file(r.socket());
    let listener = UnixListener::bind(&socket).unwrap();
    let _ = fs::remove_file(&near);
    fs::set_permissions(r.socket(), fs::Permissions::from_mode(0o600)).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for mut c in listener.incoming().flatten() {
            let _ = c.write_all(hello.as_bytes());
            held.push(c);
        }
    });
    // eight alternatives of 100,000 bytes: a request well over the socket's send buffer
    let long = "x".repeat(100_000);
    let mut args = vec!["search", "point"];
    for _ in 0..8 {
        args.extend(["--alt", long.as_str()]);
    }
    let want = r.fresh(&args);
    let mut child = Command::new(BIN)
        .args(&args)
        .current_dir(&r.root)
        .env("SCRUTATE_ROOT", ".")
        .env("SCRUTATE_SESSION_DEBUG", "1")
        .env("SCRUTATE_SESSION_WAIT_MS", "10000")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if t0.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("the call still waits on a session that reads nothing");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("request not taken"), "{said}");
    let got = (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned());
    assert_eq!((got.0, got.1.as_str()), (want.0, want.1.as_str()));
}

#[test]
fn a_second_server_on_the_root_leaves_the_first_ones_socket_alone_and_an_exit_removes_it() {
    let r = Repo::new("two");
    let first = Server::start(&r.root);
    let second = Server::start(&r.root);
    assert_eq!(r.through(&["symbol", "helper"], &[]).1, "[scrutate] session: answered by the session");
    second.stop();
    assert_eq!(r.through(&["symbol", "helper"], &[]).1, "[scrutate] session: answered by the session");
    first.stop();
    assert!(!r.socket().exists(), "the server that made the socket removes it at its exit");
    assert_eq!(r.through(&["symbol", "helper"], &[]).1, "");
}

#[test]
fn a_session_whose_root_was_moved_answers_no_call() {
    let r = Repo::new("moved");
    let _server = Server::start(&r.root);
    let moved = Repo { root: r.root.with_extension("moved") };
    let _ = fs::remove_dir_all(&moved.root);
    fs::rename(&r.root, &moved.root).unwrap();
    let (got, said) = moved.through(&["symbol", "helper"], &[]);
    assert!(said.contains("the session differs"), "{said}");
    assert_eq!(got, moved.fresh(&["symbol", "helper"]));
    assert_eq!(got.0, 0);
}

#[test]
fn a_root_deeper_than_a_socket_address_holds_is_served_too() {
    let deep = std::env::temp_dir().join(format!("scrutate_sess_deep_{}", std::process::id())).join("d".repeat(60)).join("e".repeat(60));
    let r = Repo::at(deep);
    assert!(r.socket().as_os_str().len() > 110);
    let _server = Server::start(&r.root);
    assert!(r.socket().exists());
    let (got, said) = r.through(&["symbol", "helper"], &[]);
    assert_eq!(said, "[scrutate] session: answered by the session");
    assert_eq!(got, r.fresh(&["symbol", "helper"]));
    let _ = fs::remove_dir_all(r.root.parent().unwrap().parent().unwrap());
}

#[test]
fn a_socket_is_for_its_owner_alone() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new("mode");
    let _server = Server::start(&r.root);
    assert_eq!(fs::metadata(r.socket()).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn eight_calls_sent_together_are_all_answered() {
    let r = Repo::new("eight");
    for i in 0..60 {
        let body: String = (0..30).map(|j| format!("def f{i}_{j}():\n    return helper(Point())\n\n\n")).collect();
        fs::write(r.root.join(format!("pkg/m{i}.py")), format!("from pkg.geo import Point, helper\n\n\n{body}")).unwrap();
    }
    let mut s = Server::start(&r.root);
    let refs = |s: &mut Server, id: i64| {
        let msg =
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "references", "arguments": {"name": "helper", "max_results": 2000}}});
        writeln!(s.stdin.as_mut().unwrap(), "{msg}").unwrap();
    };
    let wait = |s: &mut Server, ids: &[i64]| {
        let mut left: Vec<i64> = ids.to_vec();
        while !left.is_empty() {
            let mut line = String::new();
            assert!(s.stdout.read_line(&mut line).unwrap() > 0);
            let v: Value = serde_json::from_str(&line).unwrap();
            left.retain(|i| v["id"] != *i);
        }
    };
    let t = Instant::now();
    refs(&mut s, 100);
    wait(&mut s, &[100]);
    let one = t.elapsed();
    let t = Instant::now();
    for i in 0..8 {
        refs(&mut s, 200 + i);
    }
    wait(&mut s, &(200..208).collect::<Vec<_>>());
    let last = t.elapsed();
    // that they run side by side is mcp::tests::eight_calls_sent_together_are_in_flight_at_once;
    // a wall-clock ratio here would fail on a loaded machine with nothing waiting in line
    eprintln!("one call {one:?}, the last of eight sent together {last:?}");
}
