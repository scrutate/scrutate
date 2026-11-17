"""Results from dedicated Linux machines record what a reader needs to accept them as bare metal, and
the leaderboard and benchmark pages give each machine its own section."""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(__file__)
ROOT = os.path.join(HERE, "..", "..")
sys.path.insert(0, os.path.join(ROOT, "bench"))
import leaderboard as LB  # noqa: E402

RESULTS = os.path.join(ROOT, "bench", "results")
VM_WARM = os.path.join(RESULTS, "linux-aarch64", "superseded", "leaderboard-warm-261e.json")
COMPARE = os.path.join(RESULTS, "macos-arm64", "superseded", "compare-9aa0.json")

FACTS = {"kernel": "6.8.0-1021-aws", "virtualization": "none", "virtualization_source": "systemd-detect-virt",
         "cpu_governor": "performance", "disk": {"source": "/dev/nvme1n1", "fstype": "ext4", "model": "Amazon EC2 NVMe Instance Storage", "rotational": False}}


def _fake_root(tmp_path, governor="performance"):
    cpufreq = tmp_path / "sys" / "devices" / "system" / "cpu" / "cpu0" / "cpufreq"
    cpufreq.mkdir(parents=True)
    (cpufreq / "scaling_governor").write_text(governor + "\n")
    return str(tmp_path)


def _run(answers):
    return lambda cmd: answers.get(cmd[0], "")


def test_machine_facts_record_virtualization_governor_kernel_and_disk(tmp_path):
    run = _run({"systemd-detect-virt": "none\n", "findmnt": "/dev/nvme1n1 ext4\n",
                "lsblk": "nvme1n1 Amazon EC2 NVMe Instance Storage 0\n"})
    f = LB.machine_facts("/data", root=_fake_root(tmp_path), run=run, system="Linux")
    assert f["virtualization"] == "none" and f["virtualization_source"] == "systemd-detect-virt"
    assert f["cpu_governor"] == "performance"
    assert f["kernel"]
    assert f["disk"] == {"source": "/dev/nvme1n1", "fstype": "ext4", "model": "Amazon EC2 NVMe Instance Storage", "rotational": False}


def test_a_guest_without_systemd_names_its_hypervisor_from_sys(tmp_path):
    root = _fake_root(tmp_path)
    (tmp_path / "sys" / "hypervisor").mkdir()
    (tmp_path / "sys" / "hypervisor" / "type").write_text("xen\n")
    f = LB.machine_facts("/data", root=root, run=_run({}), system="Linux")
    assert (f["virtualization"], f["virtualization_source"]) == ("xen", "/sys/hypervisor/type")
    assert f["disk"] is None


def test_unknown_virtualization_is_not_reported_as_bare_metal(tmp_path):
    f = LB.machine_facts("/data", root=str(tmp_path), run=_run({}), system="Linux")
    assert f["virtualization"] is None and f["cpu_governor"] is None


def _container_root(tmp_path):
    root = _fake_root(tmp_path)
    (tmp_path / ".dockerenv").write_text("")
    return root


def test_a_container_without_host_facts_is_not_bare_metal(tmp_path):
    # systemd-detect-virt in a container prints the container's type, never the host's
    run = _run({"systemd-detect-virt": "docker\n"})
    f = LB.machine_facts("/bench", root=_container_root(tmp_path), run=run, system="Linux", host_facts=None)
    assert f["container"] == "docker" and f["virtualization"] is None
    import make_leaderboard_md as L
    assert L.kind(f) is None
    assert L.kind({**f, "virtualization": "docker", "virtualization_source": "systemd-detect-virt"}) is None


def test_a_container_with_the_hosts_facts_records_the_host(tmp_path):
    host = tmp_path / "host-facts.json"
    host.write_text(json.dumps(FACTS))
    f = LB.machine_facts("/bench", root=_container_root(tmp_path), run=_run({"systemd-detect-virt": "docker\n"}), system="Linux", host_facts=str(host))
    assert f["container"] == "docker"
    assert (f["virtualization"], f["virtualization_source"]) == ("none", "host: systemd-detect-virt")
    assert f["cpu_governor"] == "performance" and f["disk"] == FACTS["disk"] and f["kernel"] == FACTS["kernel"]
    import make_leaderboard_md as L
    assert L.kind(f) == "bare metal"
    assert L.machine_line(f)[0].startswith("- Machine: virtualization `none` (host: systemd-detect-virt), in a docker container, kernel")


def test_host_facts_script_writes_what_machine_facts_reads(tmp_path):
    out = subprocess.run([sys.executable, os.path.join(ROOT, "bench", "host_facts.py"), str(tmp_path)], check=True, capture_output=True, text=True).stdout
    facts = json.loads(out)
    assert {"kernel", "virtualization", "virtualization_source", "cpu_governor", "disk", "cpu"} <= set(facts)


def _leaderboard_file(tmp_path, sub, platform, machine, facts, cache="warm"):
    with open(VM_WARM, encoding="utf-8") as fh:
        d = json.load(fh)
    first = next(iter(d["benchmarks"]))
    d["benchmarks"] = {first: d["benchmarks"][first]}
    d["settings"]["cache"] = cache
    env = d["environment"]
    env.update({"platform": f"{platform}-6.8.0-1021-aws-{machine}-with-glibc2.39", "machine": machine, "cpu": "AMD EPYC 9R45", "cpus": 96}, **facts)
    p = tmp_path / sub / f"leaderboard-{cache}.json"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(d))
    return str(p)


def _render(script, args, tmp_path):
    out = tmp_path / "page.md"
    subprocess.run([sys.executable, os.path.join(ROOT, "bench", script), *args, "--out", str(out)], check=True, capture_output=True, text=True)
    return out.read_text()


def test_the_leaderboard_gives_each_bare_metal_machine_a_section_and_marks_the_vm_run_superseded(tmp_path):
    x86 = _leaderboard_file(tmp_path, "linux-x86_64", "Linux", "x86_64", FACTS)
    arm = _leaderboard_file(tmp_path, "linux-aarch64", "Linux", "aarch64", {**FACTS, "cpu_governor": None})
    page = _render("make_leaderboard_md.py", ["--engine-a", x86, "--engine-a", arm, "--engine-a", VM_WARM], tmp_path)
    assert "## Engine A and structural search: linux-x86_64, bare metal, warm cache" in page
    assert "## Engine A and structural search: linux-aarch64, bare metal, warm cache" in page
    assert "- Machine: virtualization `none` (systemd-detect-virt), kernel 6.8.0-1021-aws, CPU frequency governor `performance`, " \
           "corpora on Amazon EC2 NVMe Instance Storage (/dev/nvme1n1, ext4, solid state)." in page
    assert "CPU frequency governor not exposed" in page
    vm = page.index("## Engine A and structural search: linux-aarch64, warm cache")
    assert "- Superseded: measured in a virtual machine on a shared laptop; the bare-metal linux-aarch64 sections on this page replace it." in page[vm:]
    assert "Every machine not marked bare metal was shared with other work" in page


def test_the_leaderboard_without_bare_metal_input_is_as_before(tmp_path):
    page = _render("make_leaderboard_md.py", ["--engine-a", VM_WARM], tmp_path)
    assert "bare metal" not in page and "Superseded:" not in page
    assert "- Every machine was shared with other work" in page


def _compare_file(tmp_path, sub, facts):
    with open(COMPARE, encoding="utf-8") as fh:
        d = json.load(fh)
    d["environment"].update({"platform": "Linux-6.8.0-1021-aws-x86_64-with-glibc2.39", "cpu": "AMD EPYC 9R45", "cores": 96}, **facts)
    p = tmp_path / sub / "compare.json"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(d))
    return str(p)


def test_the_benchmarks_page_gives_each_results_file_a_section(tmp_path):
    x86 = _compare_file(tmp_path, "linux-x86_64", FACTS)
    arm = _compare_file(tmp_path, "linux-aarch64", FACTS)
    page = _render("make_benchmarks_md.py", [x86, arm], tmp_path)
    assert page.count("## Machine: linux-x86_64, bare metal") == 1 and page.count("## Machine: linux-aarch64, bare metal") == 1
    assert page.count("### Environment") == 2 and "\n## Environment" not in page
    assert "- Machine: virtualization `none` (systemd-detect-virt), kernel 6.8.0-1021-aws" in page


def test_the_benchmarks_page_from_one_file_is_as_before(tmp_path):
    page = _render("make_benchmarks_md.py", [COMPARE], tmp_path)
    assert "## Machine:" not in page and "\n## Environment\n" in page
