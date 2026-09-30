#!/usr/bin/env python3
"""M7 host/runner-class measurement, NOT workflow-job or unit acceptance.

Run on the high-cpu runner host with an audited source-only git archive and
an already present, audited compiler image. Never supplies a registry login.
The image must contain Rust 1.97.1, GNU time and build prerequisites.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import tomllib
import urllib.request
import uuid

GIB = 1024 ** 3
TRIALS = (("thin", 16), ("thin", 1), ("fat", 16), ("fat", 1))
CAPS = {"cpu": 8, "memory": 24 * GIB, "pids": 1024,
        "trial_seconds": 2700, "fetch_seconds": 600,
        "total_seconds": 10800, "scratch": 20 * GIB}


def command(*args):
    return subprocess.check_output(args, text=True, timeout=30).strip()


def inspect(name):
    return json.loads(command("docker", "inspect", name))[0]


def capacity():
    mem = dict(re.findall(r"^(\w+):\s+(\d+) kB", Path("/proc/meminfo").read_text(), re.M))
    return {"available": int(mem["MemAvailable"]) * 1024,
            "swap": (int(mem["SwapTotal"]) - int(mem["SwapFree"])) * 1024,
            "load": os.getloadavg()[0]}


def guard(root, baseline, ready_url, initial=False, check_scratch=True):
    c = capacity()
    floor = 36 * GIB if initial else 16 * GIB
    disk_floor = 40 * GIB if initial else 20 * GIB
    if c["available"] < floor or shutil.disk_usage(root).free < disk_floor:
        raise RuntimeError("host memory/disk headroom crossed")
    if c["load"] > (8 if initial else 12) or c["swap"] - baseline["swap"] > 512 * 1024 ** 2:
        raise RuntimeError("host load/swap pressure crossed")
    p = inspect("plurxd")
    if p["RestartCount"] != baseline["restarts"] or p["State"].get("Health", {}).get("Status") != "healthy":
        raise RuntimeError("production health/restarts changed")
    with urllib.request.urlopen(ready_url, timeout=5) as response:
        if response.status != 200:
            raise RuntimeError("production readiness changed")
    # du measures allocated scratch, including targets and warm downloads.
    allocated = int(command("du", "-s", "-B1", str(root)).split()[0]) if check_scratch else None
    # Reserve the full bounded tmpfs/log allocation even when not yet used:
    # these live outside the host bind root and must not evade the budget.
    upper_bound = allocated + GIB + 10 * 1024 ** 2 if allocated is not None else None
    if upper_bound is not None and upper_bound > CAPS["scratch"]:
        raise RuntimeError("owned scratch budget crossed")
    return {**c, "scratch_allocated_binds": allocated, "scratch_upper_bound": upper_bound,
            "disk_free": shutil.disk_usage(root).free}


def validate_archive(archive):
    with tarfile.open(archive) as source:
        expanded = 0
        for member in source.getmembers():
            if not (member.isfile() or member.isdir()):
                raise ValueError("archive requires regular files/directories only")
            expanded += member.size
            if expanded > GIB:
                raise ValueError("expanded source exceeds 1 GiB")
            parts = Path(member.name).parts
            if member.name.startswith("/") or ".." in parts or member.issym() or member.islnk():
                raise ValueError("unsafe archive member")
            if set(parts) & {".git", ".ssh", "forgejo_token", ".ssh-deploy-key", ".env", "id_rsa", "id_ed25519"}:
                raise ValueError("credential/history archive member")


def validate_caps(data, mounts):
    h = data["HostConfig"]
    assert h["NanoCpus"] == 8 * 10 ** 9
    assert h["Memory"] == h["MemorySwap"] == 24 * GIB
    assert h["PidsLimit"] == 1024 and not h["Privileged"]
    assert not h.get("Devices") and h["NetworkMode"] != "host"
    assert "ALL" in h["CapDrop"] and not h.get("CapAdd")
    assert "no-new-privileges" in h["SecurityOpt"]
    assert h["ReadonlyRootfs"]
    assert h["Tmpfs"] == {"/tmp": "rw,nosuid,nodev,size=1g"}
    assert h["LogConfig"] == {"Type": "local", "Config": {"max-size": "10m", "max-file": "1", "compress": "false"}}
    assert data["Config"]["User"] == f"{os.getuid()}:{os.getgid()}"
    assert {x["Destination"]: (x["Source"], x["RW"]) for x in data["Mounts"] if x["Type"] == "bind"} == {
        "/source": (str(mounts[0]), False), "/target": (str(mounts[1]), True),
        "/cargo": (str(mounts[2]), True)}


def resource_sample(name):
    # Inside this container's private cgroup namespace, not the host root.
    raw = command("docker", "exec", name, "sh", "-c",
                  "cat /sys/fs/cgroup/memory.events; echo PEAK; cat /sys/fs/cgroup/memory.peak; "
                  "echo CPU; cat /sys/fs/cgroup/cpu.stat; echo TMP; du -s -B1 /tmp")
    return {"monotonic": time.monotonic(), "cgroup": raw}


def cleanup(name, receipt, receipts):
    identity = receipt.get("container_id")
    if not identity or inspect(name)["Id"] != identity:
        raise RuntimeError("container identity unverified; preserve exact scratch")
    try:
        subprocess.run(["docker", "stop", "-t", "5", identity], timeout=15,
                       check=False, capture_output=True)
    except subprocess.TimeoutExpired:
        command("docker", "kill", identity)
    state = inspect(identity)["State"]
    receipt["final_state"] = {k: state[k] for k in ("ExitCode", "OOMKilled", "Running")}
    logs = subprocess.run(["docker", "logs", identity], timeout=30,
                          capture_output=True, text=True, check=True)
    (receipts / (name + ".log")).write_text(logs.stdout + logs.stderr)
    command("docker", "rm", "-f", identity)
    if subprocess.run(["docker", "inspect", identity], capture_output=True, timeout=30).returncode == 0:
        raise RuntimeError("owned container cleanup failed")


def measure(args, context):
    if os.getuid() == 0:
        raise ValueError("require non-root host controller and matching compiler UID:GID")
    if os.uname().machine != "x86_64" or not re.fullmatch(r"[0-9a-f]{40}", args.source):
        raise ValueError("require native AMD64 and exact committed source")
    if not re.fullmatch(r"(?:[^\s]+@)?sha256:[0-9a-f]{64}", args.image):
        raise ValueError("require immutable audited compiler image")
    archive = Path(args.archive).resolve()
    validate_archive(archive)
    with archive.open("rb") as data:
        archive_sha = hashlib.file_digest(data, "sha256").hexdigest()
    if archive_sha != args.archive_sha:
        raise ValueError("archive digest mismatch")
    image = json.loads(command("docker", "image", "inspect", args.image))[0]
    if args.image.startswith("sha256:") and image["Id"] != args.image:
        raise ValueError("local compiler Docker image ID mismatch")
    if image["Architecture"] != "amd64" or image["Os"] != "linux":
        raise ValueError("compiler image platform mismatch")
    if any(re.search(r"TOKEN|PASSWORD|SECRET|CREDENTIAL", item.split("=", 1)[0], re.I)
           for item in image["Config"].get("Env", [])):
        raise ValueError("unexpected compiler credential environment")
    root = Path(tempfile.mkdtemp(prefix="p02-m7-", dir=args.scratch_parent)).resolve()
    context["owned_root"] = root
    receipts = Path(args.receipts).resolve()
    receipts.mkdir(parents=True, exist_ok=True)
    (root / "docker-config").mkdir()
    # Host controller also ignores persisted Docker registry auth.
    os.environ["DOCKER_CONFIG"] = str(root / "docker-config")
    source = root / "source"
    source.mkdir()
    subprocess.run(["tar", "--no-same-owner", "-xf", str(archive), "-C", str(source)], check=True)
    release = tomllib.loads((source / "Cargo.toml").read_text())["profile"]["release"]
    if (release.get("debug", 0), release.get("strip"), release.get("panic", "unwind"),
            release.get("overflow-checks", False)) != (0, "symbols", "unwind", False):
        shutil.rmtree(root)
        raise ValueError("release profile baseline changed; do not compare different debug policies")
    cargo = root / "cargo"
    cargo.mkdir()
    baseline = {**capacity(), "restarts": inspect("plurxd")["RestartCount"]}
    provenance = {"kind": "high-cpu-runner-host-not-workflow-job", "source": args.source,
                  "archive_sha256": archive_sha, "image": args.image, "docker_image_id": image["Id"],
                  "host": os.uname().nodename, "kernel": os.uname().release,
                  "cpu": command("lscpu"), "caps": CAPS, "baseline": baseline,
                  "monitoring": {"health_cgroup_seconds": 2, "full_scratch_seconds": 10,
                                 "observer_overhead_included_in_wall_time": True,
                                 "scratch_is_monitored_not_filesystem_quota": True,
                                 "scratch_detection_latency_seconds": 10},
                  "profile_fixed": {"debug": 0, "strip": "symbols", "panic": "unwind", "overflow_checks": False}}
    provenance["compiler_user"] = f"{os.getuid()}:{os.getgid()}"
    provenance["compiler_home"] = "/tmp/home"
    (receipts / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    deadline = time.monotonic() + CAPS["total_seconds"]
    failed = False
    try:
        guard(root, baseline, args.ready_url, initial=True)
        # Download phase is separately bounded and excluded from every timing.
        for lto, cgu in (("fetch", 0),) + TRIALS:
            if failed:
                break  # no retry, and no follow-on workload after a failed trial
            target = root / "target"
            target.mkdir()
            name = root.name + "-" + lto + "-" + str(cgu)
            started = time.monotonic()
            receipt = {"lto": lto, "codegen_units": cgu, "status": "failed", "samples": []}
            created = False
            try:
                guard(root, baseline, args.ready_url, initial=True)
                budget = min(CAPS["trial_seconds" if cgu else "fetch_seconds"],
                             int(deadline - time.monotonic()))
                if budget <= 0:
                    raise TimeoutError("cumulative deadline before container creation")
                receipt["compiler_timeout_seconds"] = budget
                receipt["termination_grace_seconds"] = 5
                shell = """set -eu
mkdir -p "$HOME"
test -r /usr/local/rustup/settings.toml
test "$(rustc +1.97.1 --version)" = 'rustc 1.97.1 (8bab26f4f 2026-07-14)'
command -v cmake; command -v clang; command -v nasm; command -v pkg-config
test -x /usr/bin/time
cd /source
"""
                if cgu == 0:
                    shell += f"exec /usr/bin/time -v -o /target/time.txt timeout --signal=TERM --kill-after=5s {budget}s cargo +1.97.1 fetch --locked\n"
                else:
                    shell += f"exec /usr/bin/time -v -o /target/time.txt timeout --signal=TERM --kill-after=5s {budget}s cargo +1.97.1 build --offline --locked --release -p plurxd --bin plurxd\n"
                command("docker", "create", "--name", name, "--init", "--cpus=8",
                        "--user=" + f"{os.getuid()}:{os.getgid()}",
                        "--memory=24g", "--memory-swap=24g", "--pids-limit=1024",
                        "--cap-drop=ALL", "--security-opt=no-new-privileges",
                        "--read-only", "--tmpfs=/tmp:rw,nosuid,nodev,size=1g",
                        "--log-driver=local", "--log-opt=max-size=10m", "--log-opt=max-file=1", "--log-opt=compress=false",
                        "--network=" + ("bridge" if cgu == 0 else "none"),
                        "--mount", f"type=bind,src={source},dst=/source,readonly",
                        "--mount", f"type=bind,src={target},dst=/target",
                        "--mount", f"type=bind,src={cargo},dst=/cargo",
                        "--env", "CARGO_HOME=/cargo", "--env", "CARGO_TARGET_DIR=/target",
                        "--env", "HOME=/tmp/home", "--env", "RUSTUP_HOME=/usr/local/rustup",
                        "--env", "CARGO_BUILD_JOBS=8", "--env", "RUSTUP_TOOLCHAIN=1.97.1",
                        "--env", f"CARGO_PROFILE_RELEASE_LTO={lto if cgu else 'thin'}",
                        "--env", f"CARGO_PROFILE_RELEASE_CODEGEN_UNITS={cgu or 16}",
                        "--env", "CARGO_PROFILE_RELEASE_DEBUG=0", "--env", "CARGO_PROFILE_RELEASE_STRIP=symbols",
                        "--env", "CARGO_PROFILE_RELEASE_PANIC=unwind", "--env", "CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=false",
                        "--env", "PLURX_BUILD_SHA=" + args.source,
                        args.image, "sh", "-c", shell)
                created = True
                data = inspect(name)
                receipt["container_id"] = data["Id"]
                receipt["container_name"] = name
                validate_caps(data, (source, target, cargo))
                command("docker", "start", name)
                last_disk_check = 0
                while inspect(name)["State"]["Running"]:
                    if time.monotonic() >= min(deadline, started + CAPS["trial_seconds" if cgu else "fetch_seconds"]):
                        raise TimeoutError("fixed trial/cumulative deadline")
                    sample_started = time.monotonic()
                    disk_due = sample_started - last_disk_check >= 10
                    receipt["samples"].append(guard(root, baseline, args.ready_url, check_scratch=disk_due))
                    if disk_due:
                        last_disk_check = sample_started
                    try:
                        receipt["last_resource_sample"] = resource_sample(name)
                        receipt.setdefault("resource_samples", []).append(receipt["last_resource_sample"])
                    except subprocess.CalledProcessError:
                        if inspect(name)["State"]["Running"]:
                            raise RuntimeError("running cgroup measurement unavailable")
                    receipt["monitor_seconds"] = receipt.get("monitor_seconds", 0) + time.monotonic() - sample_started
                    time.sleep(2)
                state = inspect(name)["State"]
                receipt["exit_code"] = state["ExitCode"]
                receipt["oom_killed"] = state["OOMKilled"]
                if state["ExitCode"] or state["OOMKilled"]:
                    raise RuntimeError("compile failed/OOM; no retry")
                if cgu:
                    binary = target / "release/plurxd"
                    receipt["binary_bytes"] = binary.stat().st_size
                    with binary.open("rb") as data:
                        receipt["binary_sha256"] = hashlib.file_digest(data, "sha256").hexdigest()
                receipt["status"] = "passed"
            except Exception as error:
                receipt["failure"] = str(error)
                failed = True
            finally:
                receipt_path = receipts / (name + ".json")
                receipt["wall_seconds"] = time.monotonic() - started
                receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
                try:
                    if created:
                        cleanup(name, receipt, receipts)
                except Exception as error:
                    failed = True
                    receipt["status"] = "failed"
                    receipt["cleanup_failure"] = str(error)
                finally:
                    if (target / "time.txt").exists():
                        shutil.copyfile(target / "time.txt", receipts / (name + ".time.txt"))
                    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
                if not receipt.get("cleanup_failure"):
                    shutil.rmtree(target)  # exact generated owned cold target only
    finally:
        remaining = command("docker", "ps", "-a", "--filter", "name=" + root.name, "--format", "{{.Names}}")
        if remaining:
            # Do not unlink live bind mounts after a failed cleanup.
            raise RuntimeError("owned container remains; preserve scratch for exact cleanup: " + root.name)
        shutil.rmtree(root)  # exact mkdtemp owned source/cache; compact receipts retained
    return 1 if failed else 0


def run(args):
    # Setup/audit/extraction/preflight errors get the same compact evidence as
    # trial failures. Preserve binds if an owned container still exists.
    context = {}
    try:
        return measure(args, context)
    except BaseException as error:
        receipt = {"status": "failed", "phase": "setup_or_supervisor",
                   "source": args.source, "archive": args.archive,
                   "archive_sha256": args.archive_sha, "docker_image_id_requested": args.image,
                   "failure": str(error)}
        root = context.get("owned_root")
        if root and root.exists():
            receipt["owned_scratch"] = str(root)
            try:
                remaining = command("docker", "ps", "-a", "--filter", "name=" + root.name, "--format", "{{.Names}}")
                if remaining:
                    receipt["preserved_owned_containers"] = remaining.splitlines()
                else:
                    shutil.rmtree(root)
                    receipt["owned_scratch_removed"] = True
            except Exception as cleanup_error:
                receipt["cleanup_failure"] = str(cleanup_error)
        receipts = Path(args.receipts).resolve()
        receipts.mkdir(parents=True, exist_ok=True)
        path = receipts / ("setup-failure-" + uuid.uuid4().hex + ".json")
        path.write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"status": "failed", "receipt": str(path)}))
        return 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true")
    for name in ("source", "archive", "archive-sha", "image", "receipts"):
        parser.add_argument("--" + name)
    parser.add_argument("--scratch-parent", default="/var/tmp")
    parser.add_argument("--ready-url", default="http://127.0.0.1:32400/readyz")
    arguments = parser.parse_args()
    if arguments.run:
        if not all(getattr(arguments, name) for name in ("source", "archive", "archive_sha", "image", "receipts")):
            parser.error("run requires exact provenance and owned receipt path")
        raise SystemExit(run(arguments))
    print(json.dumps({"caps": CAPS, "trials": TRIALS, "execution": "not started"}, indent=2))
