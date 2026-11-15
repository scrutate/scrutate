#!/usr/bin/env python3
"""fuzz/Cargo.lock, the lock of the fuzz workspace, holds the root Cargo.lock's version of every crate
the two share, so the fuzzed code is the code the binary ships with; only the fuzz-only crates
(libfuzzer-sys, arbitrary) are its own.

    python3 tools/fuzz_lock.py          # make it: the root lock copied in, then cargo adds the rest
    python3 tools/fuzz_lock.py --check  # fail when a shared crate's version differs from the root's
"""

import os
import re
import shutil
import subprocess
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")


def versions(path: str) -> dict[str, set[str]]:
    with open(path, encoding="utf-8") as f:
        text = f.read()
    out: dict[str, set[str]] = {}
    for block in text.split("[[package]]")[1:]:
        name = re.search(r'^name = "([^"]+)"', block, re.M)
        version = re.search(r'^version = "([^"]+)"', block, re.M)
        if name and version:
            out.setdefault(name.group(1), set()).add(version.group(1))
    return out


def main() -> int:
    root, fuzz = os.path.join(ROOT, "Cargo.lock"), os.path.join(ROOT, "fuzz", "Cargo.lock")
    if "--check" not in sys.argv:
        shutil.copyfile(root, fuzz)
        subprocess.run(["cargo", "metadata", "--manifest-path", os.path.join(ROOT, "fuzz", "Cargo.toml"), "--format-version", "1"], check=True, stdout=subprocess.DEVNULL)
    if not os.path.exists(fuzz):
        print("fuzz/Cargo.lock is missing: python3 tools/fuzz_lock.py makes it")
        return 1
    ours, theirs = versions(root), versions(fuzz)
    differ = sorted(f"{n}: root {sorted(ours[n])}, fuzz {sorted(v)}" for n, v in theirs.items() if n in ours and v - ours[n])
    for line in differ:
        print("fuzz/Cargo.lock differs from Cargo.lock:", line)
    if differ:
        print("python3 tools/fuzz_lock.py makes it again from the root lock")
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main())
