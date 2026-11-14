#!/usr/bin/env python3
"""Install the built wheels into fresh virtual environments, one per interpreter, and run the
bindings tests there.

usage: wheel_matrix.py --wheels DIR --cli PATH [--json OUT] PYTHON...

PYTHON is anything `uv venv --python` accepts (3.12, 3.14t, a path). The installer picks the wheel
from DIR, as it would from an index: cp39-abi3 on a standard interpreter up to 3.14, cp314t on
free-threaded 3.14, and the abi3t wheel on 3.15 and newer, both builds. A free-threaded
interpreter must refuse the abi3 wheel, and its GIL must still be disabled after
`import scrutate`, with warnings treated as errors. Exit status 0 only when every check passed.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import platform
import subprocess
import sys
import tempfile

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))


def sh(cmd, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    return r.returncode, (r.stdout + r.stderr).strip()


def tags(wheel):
    """The tag triples a wheel file name stands for (cp315-abi3.abi3t-x is two)."""
    py, abi, plat = os.path.basename(wheel)[:-4].split("-")[-3:]
    return {f"{p}-{a}-{q}" for p in py.split(".") for a in abi.split(".") for q in plat.split(".")}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--wheels", required=True)
    ap.add_argument("--cli", required=True)
    ap.add_argument("--json")
    ap.add_argument("pythons", nargs="+")
    a = ap.parse_args()
    abi3 = glob.glob(os.path.join(a.wheels, "scrutate-*-abi3-*.whl"))
    wheels = glob.glob(os.path.join(a.wheels, "scrutate-*.whl"))
    rows, ok_all = [], True
    for py in a.pythons:
        with tempfile.TemporaryDirectory() as d:
            venv = os.path.join(d, "v")
            rc, out = sh(["uv", "venv", "-q", "--python", py, venv])
            if rc:
                rows.append({"python": py, "ok": False, "error": out[-400:]})
                ok_all = False
                continue
            exe = os.path.join(venv, "Scripts" if os.name == "nt" else "bin", "python")
            _, info = sh([exe, "-c", "import sys,sysconfig,json,platform;print(json.dumps([sys.version.split()[0], bool(sysconfig.get_config_var('Py_GIL_DISABLED')), platform.system() + ' ' + platform.machine()]))"])
            version, free, arch = json.loads(info.splitlines()[-1])
            row = {"python": version, "free_threaded": free, "platform": arch, "checks": {}}
            if free and abi3:
                rc, out = sh(["uv", "pip", "install", "-q", "--python", exe, abi3[0]])
                row["checks"]["abi3 wheel refused"] = rc != 0
            rc, out = sh(["uv", "pip", "install", "-q", "--python", exe, "--no-index", "--find-links", a.wheels, "scrutate"])
            row["checks"]["installed"] = rc == 0
            if rc == 0:
                _, meta = sh([exe, "-c", "import importlib.metadata as m; print(m.distribution('scrutate').read_text('WHEEL'))"])
                got = {line.split(": ", 1)[1] for line in meta.splitlines() if line.startswith("Tag: ")}
                row["wheel"] = next((os.path.basename(w) for w in wheels if tags(w) == got), "?")
                if free:
                    row["checks"]["free-threaded wheel chosen"] = "t-" in row["wheel"]
                rc, out = sh(["uv", "pip", "install", "-q", "--python", exe, "pytest"])
                row["checks"]["pytest installed"] = rc == 0
            if rc == 0:
                probe = "import sys, scrutate; print(getattr(sys, '_is_gil_enabled', lambda: True)())"
                rc, out = sh([exe, "-W", "error", "-c", probe])
                if free:
                    row["checks"]["GIL stays disabled after import (-W error)"] = rc == 0 and out.splitlines()[-1] == "False"
                else:
                    row["checks"]["imports"] = rc == 0
                rc, out = sh([exe, "-m", "pytest", "-q", "-p", "no:cacheprovider", "tests/test_bindings.py"],
                             cwd=os.path.join(ROOT, "python"), env={**os.environ, "SCRUTATE_BIN": os.path.abspath(a.cli)})
                row["checks"]["bindings tests"] = rc == 0
                row["pytest"] = out.splitlines()[-1] if out else ""
            row["ok"] = bool(row["checks"]) and all(row["checks"].values())
            ok_all &= row["ok"]
            rows.append(row)
    host = f"{platform.system()} {platform.machine()}"
    print("| Platform | Python | Wheel | Checks | Result |\n|---|---|---|---|---|")
    for r in rows:
        host = r.get("platform", host)
        checks = ", ".join(f"{k}: {'yes' if v else 'NO'}" for k, v in r.get("checks", {}).items()) or r.get("error", "")
        tag = f"{r['python']}{' (free-threaded)' if r.get('free_threaded') else ''}"
        print(f"| {host} | {tag} | {r.get('wheel', '-')} | {checks}; pytest: {r.get('pytest', '-')} | {'PASS' if r['ok'] else 'FAIL'} |")
    if a.json:
        with open(a.json, "w") as f:
            json.dump({"host": host, "platform": platform.platform(), "rows": rows}, f, indent=1)
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
