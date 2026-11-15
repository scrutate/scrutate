"""Time command line calls with and without the session a running `scrutate serve` offers.

    python3 bench/session_calls.py BIN ROOT [--runs N]

Each round runs every command once through the session and once in a fresh process
(SCRUTATE_NO_SESSION=1), in alternating order, so a slow moment slows both. A call counts as
answered by the session only when SCRUTATE_SESSION_DEBUG says so. Prints one JSON object.
"""
import argparse
import json
import os
import platform
import statistics
import subprocess
import time

COMMANDS = [["symbol", "Session"], ["callers", "open"], ["outline", "crates/scrutate/src/session.rs"], ["refs", "Identity"]]


def call(bin, root, args, fresh):
    env = dict(os.environ, SCRUTATE_ROOT=".", SCRUTATE_SESSION_DEBUG="1")
    env["SCRUTATE_NO_SESSION"] = "1" if fresh else "0"
    t = time.perf_counter()
    p = subprocess.run([bin, *args], cwd=root, env=env, capture_output=True, stdin=subprocess.DEVNULL, check=False)
    ms = (time.perf_counter() - t) * 1000
    return ms, p.returncode, p.stdout, b"answered by the session" in p.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bin")
    ap.add_argument("root")
    ap.add_argument("--runs", type=int, default=30)
    a = ap.parse_args()
    for args in COMMANDS:  # the first build, done by a fresh process before the server opens it
        call(a.bin, a.root, args, True)
    serve = subprocess.Popen([a.bin, "serve"], cwd=a.root, env=dict(os.environ, SCRUTATE_ROOT="."), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    msg = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "find_symbol", "arguments": {"name": "Session"}}}
    serve.stdin.write((json.dumps(msg) + "\n").encode())
    serve.stdin.flush()
    serve.stdout.readline()  # answered after its first build
    rows = {}
    try:
        for args in COMMANDS:
            key = " ".join(args)
            via, fresh, missed = [], [], 0
            for i in range(a.runs):
                order = [False, True] if i % 2 else [True, False]
                for f in order:
                    ms, code, out, answered = call(a.bin, a.root, args, f)
                    if f:
                        fresh.append(ms)
                        want = (code, out)
                    elif answered:
                        via.append(ms)
                    else:
                        missed += 1
                # the session's bytes are a fresh process's
                ms, code, out, answered = call(a.bin, a.root, args, False)
                assert not answered or (code, out) == want, key
            q = lambda xs, p: sorted(xs)[min(len(xs) - 1, int(p * len(xs)))] if xs else None
            rows[key] = {
                "session_median_ms": statistics.median(via) if via else None,
                "session_p90_ms": q(via, 0.9),
                "fresh_median_ms": statistics.median(fresh),
                "fresh_p90_ms": q(fresh, 0.9),
                "session_answered": len(via),
                "session_missed": missed,
            }
    finally:
        serve.stdin.close()
        serve.wait(timeout=60)
    print(json.dumps({"machine": platform.platform(), "cpu": platform.machine(), "runs": a.runs, "rows": rows}, indent=1))


if __name__ == "__main__":
    main()
