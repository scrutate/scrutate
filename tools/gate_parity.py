#!/usr/bin/env python3
"""The gate and CI cannot drift: every step of the seven workflows (ci.yml, security.yml,
benchmarks.yml, release.yml, fuzz-weekly.yml, jetbrains.yml, scorecard.yml), `run:` and `uses:` alike, and a job
that calls another workflow, has an entry in tools/gate_map.json, keyed by workflow, job and a
hash of what decides whether and how it runs: its command or action and inputs, its environment,
shell, working directory, `if:` and `continue-on-error:`, the job's runner, matrix, `if:`,
`continue-on-error:`, `needs:`, container and services, and the workflow's `on:`. The entry says
what covers the step: `gate: NAME`
(a step of tools/ci_gate.sh), `setup: ...` (what the box's setup installs) or `ci-only: why`. A
step that is new or changed has no entry, an entry whose step is gone is stale, and a `gate:` name
that tools/ci_gate.sh does not run is wrong: each fails.

    python3 tools/gate_parity.py           # check
    python3 tools/gate_parity.py --missing # the entries to add, as JSON, for the map
"""

import hashlib
import json
import os
import re
import sys

import yaml

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
WORKFLOWS = ("ci.yml", "security.yml", "benchmarks.yml", "release.yml", "fuzz-weekly.yml", "jetbrains.yml", "scorecard.yml")
KINDS = ("gate:", "setup:", "ci-only:")


def steps():
    """(key, label) of every step, in file order."""
    def key(wf: str, job: str, what: dict) -> str:
        return f"{wf}:{job}:{hashlib.sha1(json.dumps(what, sort_keys=True).encode()).hexdigest()[:10]}"

    for wf in WORKFLOWS:
        with open(os.path.join(ROOT, ".github", "workflows", wf), encoding="utf-8") as f:
            doc = yaml.safe_load(f)
        top = doc.get("env") or {}
        top_run = (doc.get("defaults") or {}).get("run") or {}
        # PyYAML reads the `on:` key as True
        triggers = doc.get(True, doc.get("on"))
        for job, j in doc["jobs"].items():
            env = {**top, **(j.get("env") or {})}
            run_defaults = {**top_run, **((j.get("defaults") or {}).get("run") or {})}
            of_job = {"on": triggers, "runs-on": j.get("runs-on"), "matrix": (j.get("strategy") or {}).get("matrix"), "job-if": j.get("if"),
                      "job-continue-on-error": j.get("continue-on-error"), "needs": j.get("needs"), "container": j.get("container"), "services": j.get("services")}
            if "uses" in j:
                yield key(wf, job, {**of_job, "uses": j["uses"], "with": j.get("with")}), j.get("name") or j["uses"]
            for s in j.get("steps") or []:
                what = {**of_job, "env": {**env, **(s.get("env") or {})}, "if": s.get("if"), "continue-on-error": s.get("continue-on-error")}
                if "run" in s:
                    what |= {"run": s["run"], "shell": s.get("shell", run_defaults.get("shell")), "working-directory": s.get("working-directory", run_defaults.get("working-directory"))}
                    label = s.get("name") or s["run"].strip().splitlines()[0][:80]
                elif "uses" in s:
                    what |= {"uses": s["uses"], "with": s.get("with")}
                    label = s.get("name") or s["uses"]
                else:
                    continue
                yield key(wf, job, what), label


def gate_steps():
    """The names tools/ci_gate.sh gives its steps: `step NAME ...` where a command starts (a line's
    start, after `&&`, `;`, `then` or `else`), never in a comment or a string."""
    names = set()
    with open(os.path.join(ROOT, "tools", "ci_gate.sh"), encoding="utf-8") as f:
        for line in f:
            if line.lstrip().startswith("#"):
                continue
            names |= {m.strip('"') for m in re.findall(r'(?:^\s*|&&\s*|;\s*|\bthen\s+|\belse\s+)step\s+("[^"]+"|\S+)', line)}
    return names


def main() -> int:
    found = dict(steps())
    with open(os.path.join(ROOT, "tools", "gate_map.json"), encoding="utf-8") as f:
        known = json.load(f)
    missing = {k: {"step": v, "by": "TODO"} for k, v in found.items() if k not in known}
    if "--missing" in sys.argv:
        print(json.dumps(missing, indent=1))
        return 0
    problems = [f"not in tools/gate_map.json: {k}  {v['step']}" for k, v in missing.items()]
    problems += [f"stale in tools/gate_map.json (no such step now): {k}  {v.get('step')}" for k, v in known.items() if k not in found]
    names = gate_steps()
    for k, v in known.items():
        by = v.get("by", "")
        if not by.startswith(KINDS):
            problems.append(f"{k}: 'by' must start with one of {', '.join(KINDS)}: {by!r}")
        elif by.startswith("gate:"):
            for name in (n.strip() for n in by[len("gate:"):].split(",")):
                if name not in names:
                    problems.append(f"{k}: tools/ci_gate.sh runs no step {name!r}")
    for p in problems:
        print(p)
    if missing:
        print("python3 tools/gate_parity.py --missing prints the entries to fill in")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
