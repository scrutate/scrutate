#!/usr/bin/env python3
"""The host's machine facts, for a benchmark that runs in a container: run on the host, before
`docker run`, with the directory that holds the container's data (for a named volume, Docker's
volume directory), and mount the output into the container as SCRUTATE_BENCH_HOST_FACTS.

usage: host_facts.py DIR > host-facts.json

Keys: kernel, virtualization, virtualization_source, cpu_governor, disk {source, fstype, model,
rotational}, cpu (lscpu's model name); a fact the host does not report is null.
"""
import json
import sys

from leaderboard import machine_facts, stdout_of

if __name__ == "__main__":
    facts = machine_facts(sys.argv[1], system="Linux", host_facts=None)
    if facts.pop("container"):
        sys.exit("host_facts.py: this is a container; run it on the host")
    facts["cpu"] = next((l.split(":", 1)[1].strip() for l in stdout_of(["lscpu"]).splitlines() if l.startswith("Model name:")), None)
    json.dump(facts, sys.stdout, indent=1)
    print()
