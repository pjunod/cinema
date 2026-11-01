"""ci-fleet-agent: report a runner host's resources; carry out its controls.

Runs on every runner host (Linux or macOS) and inside every Incus runner
guest. It never decides placement — the forge records that — it measures what
the placed work costs: per-runner and per-task cgroup CPU, memory, swap, pids
and OOM kills against their caps, the host's load by owner (CI, plurx
production, everything else), disks, and the janitor and watchdog receipts.

Controls are executed by the tool that already owns the resource:
  * an Incus runner VM: the runner watchdog (pause marker, `--vm`), which keeps
    its memory-headroom check and restart backoff;
  * a host-mode systemd runner: a pause marker, then `systemctl stop` once idle;
  * a launchd runner: `launchctl disable`, then SIGTERM once idle (the Apple
    janitor already honours a disabled service);
  * the standby (backup) Apple runner: the standby controller's pause marker.

Python 3.9 compatible: macOS runner hosts have only the system python3.
"""

from __future__ import annotations

import argparse
import http.client
import json
import os
from pathlib import Path
import platform
import re
import shutil
import socket
import subprocess
import threading
import time
import urllib.request

VERSION = "1"
INTERVAL = 5
DU_SECONDS = 3600
DU_TIMEOUT = 300
DU_BACKOFF = 24 * 3600  # a tree too big to measure in DU_TIMEOUT is retried a day later
INCUS_SECONDS = 30
TASK_CONTAINER = re.compile(r"ACTIONS-TASK-(\d+)")
CGROUP = Path("/sys/fs/cgroup")


def run(*args, timeout=20, check=True) -> str:
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if result.returncode and check:
        raise RuntimeError(f"{args[0]} failed ({result.returncode}): {result.stderr.strip()[:200]}")
    if result.returncode:  # the watchdog prints its receipt and exits 1 when it refused: keep the receipt
        return f"exit {result.returncode}: {(result.stdout or result.stderr).strip()}"
    return result.stdout


# -- parsers (pure, tested) ----------------------------------------------------
def parse_proc_stat(text: str) -> tuple[int, int]:
    """(busy, total) jiffies from the aggregate cpu line."""
    fields = [int(value) for value in text.splitlines()[0].split()[1:]]
    idle = fields[3] + (fields[4] if len(fields) > 4 else 0)
    total = sum(fields[:8])
    return total - idle, total


def parse_meminfo(text: str) -> dict:
    values = {}
    for line in text.splitlines():
        name, _, rest = line.partition(":")
        parts = rest.split()
        if parts:
            values[name] = int(parts[0]) * 1024
    return values


def parse_flat_keyed(text: str) -> dict:
    out = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("-").isdigit():
            out[parts[0]] = int(parts[1])
    return out


def parse_cpu_max(text: str) -> float | None:
    quota, _, period = text.strip().partition(" ")
    if quota == "max" or not period:
        return None
    return int(quota) / int(period)


def parse_vm_stat(text: str) -> tuple[int, dict]:
    size = int(re.search(r"page size of (\d+) bytes", text).group(1))
    pages = {}
    for line in text.splitlines()[1:]:
        name, _, value = line.partition(":")
        value = value.strip().rstrip(".")
        if value.isdigit():
            pages[name.strip()] = int(value)
    return size, pages


def parse_swapusage(text: str) -> tuple[int, int]:
    def bytes_of(label):
        match = re.search(label + r" = ([\d.]+)([KMG])", text)
        if not match:
            return 0
        return int(float(match.group(1)) * {"K": 1 << 10, "M": 1 << 20, "G": 1 << 30}[match.group(2)])
    return bytes_of("total"), bytes_of("used")


def task_id_of(container_names) -> int | None:
    for name in container_names:
        match = TASK_CONTAINER.search(name)
        if match:
            return int(match.group(1))
    return None


# -- Linux cgroups -------------------------------------------------------------
class CgroupReader:
    """Delta-based CPU per cgroup; static reads for memory, pids and caps."""

    def __init__(self, root: Path = CGROUP, clock=time.monotonic):
        self.root = root
        self.clock = clock
        self.previous = {}

    def read(self, path: Path) -> dict | None:
        if not path.is_dir():
            return None

        def text(name):
            try:
                return (path / name).read_text()
            except OSError:
                return ""

        stat = parse_flat_keyed(text("cpu.stat"))
        now = self.clock()
        usage = stat.get("usage_usec", 0)
        periods, throttled = stat.get("nr_periods", 0), stat.get("nr_throttled", 0)
        before = self.previous.get(path)
        self.previous[path] = (now, usage, periods, throttled)
        cores = throttled_pct = None
        if before and now > before[0]:
            cores = round((usage - before[1]) / 1e6 / (now - before[0]), 3)
            dperiods = periods - before[2]
            throttled_pct = round(100 * (throttled - before[3]) / dperiods, 1) if dperiods > 0 else 0.0

        def number(name):
            value = text(name).strip()
            return int(value) if value.isdigit() else None

        events = parse_flat_keyed(text("memory.events"))
        return {
            "cpu": cores,
            "cpu_cap": parse_cpu_max(text("cpu.max")),
            "throttled_pct": throttled_pct,
            "rss": number("memory.current"),
            "mem_cap": number("memory.max"),
            "swap": number("memory.swap.current"),
            "pids": number("pids.current"),
            "pids_cap": number("pids.max"),
            "oom_kills": events.get("oom_kill", 0),
        }


def qemu_rss(vm: str, proc: Path = Path("/proc")) -> int | None:
    """Resident memory of the qemu process running Incus VM `vm`, in bytes."""
    needle = b"-name\x00" + vm.encode()
    for entry in proc.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            cmdline = (entry / "cmdline").read_bytes()
            if b"qemu" not in cmdline.split(b"\x00", 1)[0] or needle not in cmdline:
                continue
            fields = (entry / "statm").read_text().split()
            return int(fields[1]) * os.sysconf("SC_PAGE_SIZE")
        except (OSError, ValueError, IndexError):
            continue
    return None


def docker_containers() -> list[dict]:
    """Running containers via the Docker socket (no docker CLI needed)."""
    sock = "/var/run/docker.sock"
    if not os.path.exists(sock):
        return []

    class Conn(http.client.HTTPConnection):
        def connect(self):
            self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.sock.settimeout(5)
            self.sock.connect(sock)

    conn = Conn("docker")
    try:
        conn.request("GET", "/containers/json")
        return json.loads(conn.getresponse().read())
    except (OSError, ValueError):
        return []
    finally:
        conn.close()


def container_cgroup(root: Path, container_id: str) -> Path:
    for candidate in (root / "system.slice" / f"docker-{container_id}.scope", root / "docker" / container_id):
        if candidate.is_dir():
            return candidate
    return root / "system.slice" / f"docker-{container_id}.scope"


# -- the agent -----------------------------------------------------------------
class Agent:
    def __init__(self, config: dict, *, execute=run, root: Path = CGROUP):
        self.config = config
        self.execute = execute
        self.root = root
        self.cgroups = CgroupReader(root)
        self.cpu_before = None
        self.results = []
        self.sizes = {}
        self.du_skip = {}
        self.incus = {"at": float("-inf"), "vms": []}
        self.is_mac = platform.system() == "Darwin"
        self.paused_dir = Path(config.get("paused_dir", "/var/lib/ci-fleet/paused"))
        self.watchdog_paused = Path(config.get("watchdog_paused_dir", "/var/lib/plurx-runner-watchdog/paused"))

    # -- collection ----------------------------------------------------------
    def report(self) -> dict:
        results, self.results = self.results, []
        body = {"agent": self.config["agent"], "host": self.config.get("host", self.config["agent"]),
                "role": self.config.get("role", "host"), "version": VERSION, "ts": int(time.time()),
                "results": results}
        try:
            if self.is_mac:
                body["host_metrics"] = self.mac_metrics()
            else:
                body["host_metrics"], body["tasks"] = self.linux_metrics()
        except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
            body["error"] = str(error)[:300]
        body["runners"] = [self.runner_report(runner) for runner in self.config.get("runners", [])]
        if self.config.get("incus_project"):
            body["vms"] = self.incus_vms()
        state = self._json(self.config.get("watchdog_state", "/var/lib/plurx-runner-watchdog/state.json"))
        if state:
            body["watchdog"] = state.get("last_run")
        return body

    def _json(self, path):
        try:
            return json.loads(Path(path).read_text())
        except (OSError, ValueError):
            return None

    def linux_metrics(self):
        busy, total = parse_proc_stat(Path("/proc/stat").read_text())
        cores = os.cpu_count() or 1
        host_cores = None
        if self.cpu_before and total > self.cpu_before[1]:
            host_cores = round(cores * (busy - self.cpu_before[0]) / (total - self.cpu_before[1]), 3)
        self.cpu_before = (busy, total)
        mem = parse_meminfo(Path("/proc/meminfo").read_text())
        ci_cpu = ci_mem = plurx_cpu = plurx_mem = 0.0
        tasks = []
        plurx_prefixes = tuple(self.config.get("plurx_containers", ["plurxd", "plurx-"]))
        for container in docker_containers():
            names = [name.lstrip("/") for name in container.get("Names", [])]
            usage = self.cgroups.read(container_cgroup(self.root, container["Id"])) or {}
            task_id = task_id_of(names)
            if task_id:
                tasks.append({"task_id": task_id, "container": names[0] if names else "", "usage": usage})
                ci_cpu += usage.get("cpu") or 0
                ci_mem += usage.get("rss") or 0
            elif any(name.startswith(plurx_prefixes) for name in names):
                plurx_cpu += usage.get("cpu") or 0
                plurx_mem += usage.get("rss") or 0
        for runner in self.config.get("runners", []):
            if runner.get("kind") == "systemd":
                usage = self.cgroups.read(self.root / "system.slice" / runner["unit"]) or {}
                ci_cpu += usage.get("cpu") or 0
                ci_mem += usage.get("rss") or 0
        for vm in self.incus["vms"]:
            # Every Incus VM's qemu sits in incus.service's cgroup, so a VM's
            # CPU comes from Incus's own counter (incus_vms), not a cgroup.
            # A VM costs the host its qemu process's RSS, not what the guest uses.
            ci_mem += vm.get("host_rss") or vm.get("memory_bytes") or 0
            ci_cpu += vm.get("cpu") or 0
        used_mem = mem.get("MemTotal", 0) - mem.get("MemAvailable", 0)
        metrics = {
            "cpu_cores": cores,
            "cpu_busy": host_cores,
            "cpu_used": {"ci": round(ci_cpu, 3), "plurx": round(plurx_cpu, 3),
                         "other": round(max(0.0, (host_cores or 0) - ci_cpu - plurx_cpu), 3)},
            "mem_total": mem.get("MemTotal", 0),
            "mem_used": {"ci": int(ci_mem), "plurx": int(plurx_mem),
                         "other": int(max(0, used_mem - ci_mem - plurx_mem))},
            "swap_total": mem.get("SwapTotal", 0),
            "swap_used": mem.get("SwapTotal", 0) - mem.get("SwapFree", 0),
            "disks": self.disks(),
            "gpus": self.gpus(),
        }
        return metrics, tasks

    def mac_metrics(self) -> dict:
        cores = int(self.execute("sysctl", "-n", "hw.ncpu").strip())
        total = int(self.execute("sysctl", "-n", "hw.memsize").strip())
        size, pages = parse_vm_stat(self.execute("vm_stat"))
        free = sum(pages.get(name, 0) for name in ("Pages free", "Pages inactive", "Pages speculative")) * size
        swap_total, swap_used = parse_swapusage(self.execute("sysctl", "-n", "vm.swapusage"))
        ci_cpu = ci_mem = plurx_cpu = plurx_mem = all_cpu = 0.0
        for runner, procs in self._mac_trees().items():
            for _, cpu, rss in procs:
                ci_cpu += cpu
                ci_mem += rss
        for line in self.execute("ps", "-A", "-o", "%cpu=,rss=,comm=").splitlines():
            parts = line.split(None, 2)
            if len(parts) == 3:
                all_cpu += float(parts[0]) / 100
                if "plurxd" in parts[2]:
                    plurx_cpu += float(parts[0]) / 100
                    plurx_mem += int(parts[1]) * 1024
        used = total - free
        return {
            "cpu_cores": cores, "cpu_busy": round(all_cpu, 3),
            "cpu_used": {"ci": round(ci_cpu, 3), "plurx": round(plurx_cpu, 3),
                         "other": round(max(0.0, all_cpu - ci_cpu - plurx_cpu), 3)},
            "mem_total": total,
            "mem_used": {"ci": int(ci_mem), "plurx": int(plurx_mem), "other": int(max(0, used - ci_mem - plurx_mem))},
            "swap_total": swap_total, "swap_used": swap_used, "disks": self.disks(), "gpus": [],
        }

    def _mac_trees(self) -> dict:
        """Each launchd/standby runner's process tree: [(pid, cores, rss bytes)]."""
        rows = []
        for line in self.execute("ps", "-A", "-o", "pid=,ppid=,%cpu=,rss=,command=").splitlines():
            parts = line.split(None, 4)
            if len(parts) == 5:
                rows.append((int(parts[0]), int(parts[1]), float(parts[2]) / 100, int(parts[3]) * 1024, parts[4]))
        children = {}
        for pid, ppid, *_ in rows:
            children.setdefault(ppid, []).append(pid)
        by_pid = {row[0]: row for row in rows}
        trees = {}
        for runner in self.config.get("runners", []):
            match = runner.get("match")
            if runner.get("kind") not in ("launchd", "standby") or not match:
                continue
            roots = [row[0] for row in rows if match in row[4]]
            seen, stack = set(), list(roots)
            while stack:
                pid = stack.pop()
                if pid not in seen:
                    seen.add(pid)
                    stack.extend(children.get(pid, []))
            trees[runner["name"]] = [(pid, by_pid[pid][2], by_pid[pid][3]) for pid in seen if pid in by_pid]
        return trees

    def disks(self) -> list[dict]:
        out = []
        fraction = float(self.config.get("reserve_fraction", 0.2))
        for path in self.config.get("disks", ["/"]):
            try:
                usage = shutil.disk_usage(path)
            except OSError:
                continue
            reserve = min(max(int(usage.total * fraction), 10 << 30), usage.total // 4)
            out.append({"path": path, "total": usage.total, "used": usage.used, "free": usage.free,
                        "reserve": int(self.config.get("reserve_bytes", reserve))})
        return out

    def gpus(self) -> list[dict]:
        if not shutil.which("nvidia-smi"):
            return []
        try:
            text = self.execute("nvidia-smi", "--query-gpu=name,utilization.gpu,memory.used,memory.total",
                                "--format=csv,noheader,nounits", timeout=5)
        except (RuntimeError, subprocess.TimeoutExpired):
            return []
        gpus = []
        for line in text.splitlines():
            name, util, used, total = (part.strip() for part in line.split(","))
            gpus.append({"name": name, "util": int(util), "mem_used": int(used) << 20, "mem_total": int(total) << 20})
        return gpus

    def incus_vms(self) -> list[dict]:
        if time.monotonic() - self.incus["at"] < INCUS_SECONDS:
            return self.incus["vms"]
        project = self.config["incus_project"]
        managed = {r["vm"]: r for r in self.config.get("runners", []) if r.get("kind") == "incus"}
        previous = {vm["name"]: vm for vm in self.incus["vms"]}
        now = time.monotonic()
        vms = []
        try:
            listing = json.loads(self.execute("incus", "list", "--project", project, "--format", "json"))
        except (RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
            # Keep the last listing; a failed `incus list` is not every VM vanishing.
            self.incus["error"] = str(error)[:200]
            return self.incus["vms"]
        for instance in listing:
            if instance["name"] not in managed:
                continue
            state = instance.get("state") or {}
            paused = (self.watchdog_paused / instance["name"]).exists()
            controls = ["start_vm"] if instance["status"] == "Stopped" and not paused else []
            cpu_ns = (state.get("cpu") or {}).get("usage")
            before = previous.get(instance["name"], {})
            cores = None
            if cpu_ns is not None and before.get("cpu_ns") is not None and now > before.get("at", now):
                cores = round(max(0, cpu_ns - before["cpu_ns"]) / 1e9 / (now - before["at"]), 3)
            rss = qemu_rss(instance["name"]) if instance["status"] == "Running" else 0
            vms.append({"name": instance["name"], "status": instance["status"], "cpu_ns": cpu_ns, "at": now,
                        "cpu": cores, "host_rss": rss,
                        "memory_limit": instance.get("expanded_config", {}).get("limits.memory"),
                        "memory_bytes": (state.get("memory") or {}).get("usage"),
                        "autostart": instance.get("expanded_config", {}).get("boot.autostart") != "false",
                        "paused": paused, "controls": controls})
        self.incus = {"at": time.monotonic(), "vms": vms, "error": ""}
        return vms

    def runner_report(self, runner: dict) -> dict:
        kind = runner.get("kind", "systemd")
        report = {"name": runner["name"], "kind": kind, "capacity": runner.get("capacity", 1), "controls": []}
        if kind == "systemd":
            try:
                state = self.execute("systemctl", "show", runner["unit"], "-p", "ActiveState", "--value").strip()
            except (RuntimeError, subprocess.TimeoutExpired):
                state = "unknown"
            paused = (self.paused_dir / runner["name"]).exists()
            report["draining"] = self._draining(runner)
            report.update(state=state, paused=paused,
                          usage=self.cgroups.read(self.root / "system.slice" / runner["unit"]))
            report["controls"] = ["resume"] if paused else ["drain"]
            if state != "active" and not paused:
                report["offline_reason"] = f"unit {state}"
        elif kind == "incus":
            vm = next((v for v in self.incus["vms"] if v["name"] == runner["vm"]), None)
            paused = (self.watchdog_paused / runner["vm"]).exists()
            report["paused"] = paused
            report["draining"] = self._draining(runner)
            if vm is None:
                report["offline_reason"] = "VM not found"
            elif vm["status"] != "Running":
                report["offline_reason"] = f"VM {vm['status'].lower()}" + self._deferred(runner["vm"])
            report["controls"] = ["resume"] if paused else (["drain"] if vm and vm["status"] == "Running" else [])
        elif kind == "launchd":
            disabled = self._launchd_disabled(runner) or (self.paused_dir / runner["name"]).exists()
            report["paused"] = disabled
            report["draining"] = self._draining(runner)
            report["controls"] = ["resume"] if disabled else ["drain"]
        elif kind == "standby":
            paused = Path(runner["pause_marker"]).exists()
            report["paused"] = paused
            report["controls"] = ["resume"] if paused else ["drain"]
        elif kind == "lima":
            try:
                rows = [json.loads(line) for line in self.execute("limactl", "list", "--json").splitlines() if line.strip()]
            except (RuntimeError, ValueError, subprocess.TimeoutExpired, FileNotFoundError):
                rows = []
            vm = next((row for row in rows if row.get("name") == runner["vm"]), None)
            if not vm or vm.get("status") != "Running":
                report["offline_reason"] = f"Lima VM {(vm or {}).get('status', 'missing').lower()}"
        if self.is_mac and kind in ("launchd", "standby"):
            procs = self._mac_trees().get(runner["name"], [])
            report["usage"] = {"cpu": round(sum(p[1] for p in procs), 3), "rss": sum(p[2] for p in procs),
                               "pids": len(procs)}
        report["hygiene"] = self.hygiene(runner)
        if report.get("paused"):
            report["paused_by"] = self._paused_by(runner)
        if self.config.get("role", "host") != "host":
            # A guest reports usage only; the host agent owns pause and controls.
            for key in ("controls", "paused", "draining", "paused_by"):
                report.pop(key, None)
        return report

    def _paused_by(self, runner: dict) -> str | None:
        marker = self._marker_path(runner)
        try:
            return json.loads(marker.read_text()).get("actor") if marker else None
        except (OSError, ValueError):
            return None

    def _draining(self, runner: dict) -> bool:
        marker = self._marker_path(runner)
        try:
            return bool(marker) and json.loads(marker.read_text()).get("state") == "draining"
        except (OSError, ValueError):
            return False

    def _deferred(self, vm: str) -> str:
        state = self._json(self.config.get("watchdog_state", "/var/lib/plurx-runner-watchdog/state.json")) or {}
        for entry in (state.get("last_run") or {}).get("deferred", []):
            if entry.startswith(vm + ":"):
                return " — watchdog: " + entry.split(":", 1)[1].strip()
        return ""

    def _launchd_disabled(self, runner) -> bool:
        domain = runner.get("domain", "system")
        try:
            text = self.execute("launchctl", "print-disabled", domain)
        except (RuntimeError, subprocess.TimeoutExpired):
            return False
        return re.search(r'"%s" => (true|disabled)' % re.escape(runner["label"]), text) is not None

    def hygiene(self, runner: dict) -> dict:
        out = {}
        for key in ("work", "cache"):
            path = runner.get(key)
            if path:
                out[key + "_bytes"] = self.sizes.get(path)
        janitor = self._json(self.config.get("janitor_state", "/var/lib/plurx-ci-janitor/last-run.json"))
        if janitor:
            out["janitor"] = janitor
        return out

    def measure_sizes(self) -> None:
        """`du` the runners' work and cache directories, off the report loop."""
        while True:
            for runner in self.config.get("runners", []):
                for key in ("work", "cache"):
                    path = runner.get(key)
                    if not path:
                        continue
                    if time.time() < self.du_skip.get(path, 0):
                        continue
                    try:
                        self.sizes[path] = int(self.execute("du", "-sk", path, timeout=DU_TIMEOUT).split()[0]) * 1024
                    except subprocess.TimeoutExpired:
                        self.sizes[path] = None
                        self.du_skip[path] = time.time() + DU_BACKOFF
                    except (RuntimeError, ValueError, IndexError):
                        self.sizes[path] = None
            time.sleep(DU_SECONDS)

    # -- controls --------------------------------------------------------------
    def perform(self, command: dict) -> dict:
        action, target = command.get("action"), command.get("target")
        if self.config.get("role", "host") != "host":
            return {"id": command.get("id"), "action": action, "target": target, "actor": command.get("actor"),
                    "result": "failed: a guest agent carries out no controls"}
        runner = next((r for r in self.config.get("runners", []) if r["name"] == target or r.get("vm") == target), None)
        result = {"id": command.get("id"), "action": action, "target": target,
                  "actor": command.get("actor"), "reason": command.get("reason")}
        try:
            if runner is None:
                raise ValueError("not managed by this agent")
            result["result"] = self._perform(action, runner, command)
        except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired) as error:
            result["result"] = f"failed: {error}"[:300]
        return result

    def _marker(self, path: Path, command: dict, state: str = "draining") -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"actor": command.get("actor"), "reason": command.get("reason"),
                                    "at": int(time.time()), "state": state}) + "\n")

    def _marker_path(self, runner: dict) -> Path | None:
        kind = runner.get("kind", "systemd")
        if kind == "incus":
            return self.watchdog_paused / runner["vm"]
        if kind in ("systemd", "launchd"):
            return self.paused_dir / runner["name"]
        if kind == "standby":
            return Path(runner["pause_marker"])
        return None

    def _perform(self, action: str, runner: dict, command: dict) -> str:
        """Drain marks first, stops only when idle (see progress_drains).

        Stopping a runner mid-job can cancel the job (a container runner's
        stop timeout does not protect it), so a drain never stops a busy
        runner: the marker keeps the watchdog from restarting it, and the stop
        waits until no task container and no runner child process is left.
        """
        kind = runner.get("kind", "systemd")
        watchdog = self.config.get("watchdog", "/usr/local/sbin/plurx-runner-watchdog")
        marker = self._marker_path(runner)
        if action == "drain" and kind in ("systemd", "incus", "launchd"):
            if kind == "launchd":
                self.execute("launchctl", "disable", f"{runner.get('domain', 'system')}/{runner['label']}")
            self._marker(marker, command)
            self.progress_drains()
            return "draining: paused now; the runner stops once its current job is done"
        if action == "resume" and kind in ("systemd", "incus", "launchd"):
            marker.unlink(missing_ok=True)
            if kind == "incus":
                return "resumed: " + self.execute(watchdog, "--vm", runner["vm"], timeout=180, check=False).strip()[-300:]
            if kind == "systemd":
                self.execute("systemctl", "start", "--no-block", runner["unit"])
                return "resumed"
            service = f"{runner.get('domain', 'system')}/{runner['label']}"
            self.execute("launchctl", "enable", service)
            try:
                self.execute("launchctl", "bootstrap", runner.get("domain", "system"), runner["plist"])
            except (RuntimeError, KeyError):
                pass  # already loaded
            self.execute("launchctl", "kickstart", service)
            return "resumed"
        if kind == "incus" and action == "start_vm":
            return self.execute(watchdog, "--vm", runner["vm"], timeout=180, check=False).strip()[-300:]
        if kind == "standby" and action == "drain":
            self._marker(marker, command, state="stopped")
            return "draining: the standby controller drains the backup and stops offering it"
        if kind == "standby" and action == "resume":
            marker.unlink(missing_ok=True)
            return "resumed: the standby controller offers the backup again during an outage"
        raise ValueError(f"{action} is not available for a {kind} runner")

    def _busy(self, runner: dict) -> bool:
        kind = runner.get("kind", "systemd")
        probe = ("docker ps -q --filter name=ACTIONS-TASK- 2>/dev/null; "
                 "p=$(systemctl show -p MainPID --value %s); [ \"$p\" -gt 0 ] && pgrep -P \"$p\"; true")
        if kind == "incus":
            out = self.execute("incus", "exec", "--project", self.config["incus_project"], runner["vm"], "--",
                               "sh", "-c", probe % runner["unit"])
            return bool(out.strip())
        if kind == "systemd":
            return bool(self.execute("sh", "-c", probe % runner["unit"]).strip())
        if kind == "launchd":
            return len(self._mac_trees().get(runner["name"], [])) > 1
        return False

    def progress_drains(self) -> None:
        """Stop each draining runner once it is idle; record the stop for audit."""
        for runner in self.config.get("runners", []):
            marker = self._marker_path(runner)
            if runner.get("kind") not in ("systemd", "incus", "launchd") or not marker or not marker.exists():
                continue
            try:
                detail = json.loads(marker.read_text())
            except (OSError, ValueError):
                continue
            if detail.get("state") != "draining":
                continue
            try:
                if self._busy(runner):
                    continue
                kind = runner["kind"]
                if kind == "incus":
                    self.execute("incus", "exec", "--project", self.config["incus_project"], runner["vm"], "--",
                                 "systemctl", "stop", "--no-block", runner["unit"])
                elif kind == "systemd":
                    self.execute("systemctl", "stop", "--no-block", runner["unit"])
                else:
                    self.execute("launchctl", "kill", "SIGTERM", f"{runner.get('domain', 'system')}/{runner['label']}")
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                message = f"stop failed: {error}"[:300]
                if detail.get("error") != message:  # audit a failure once, retry quietly
                    detail["error"] = message
                    marker.write_text(json.dumps(detail) + "\n")
                    self.results.append({"action": "drain", "target": runner["name"], "actor": detail.get("actor"),
                                         "result": message})
                continue
            detail["state"] = "stopped"
            marker.write_text(json.dumps(detail) + "\n")
            self.results.append({"action": "drain", "target": runner["name"], "actor": detail.get("actor"),
                                 "reason": detail.get("reason"), "result": "idle: runner stopped"})

    # -- loop --------------------------------------------------------------------
    def push(self, body: dict) -> dict:
        token = self.config.get("token") or Path(self.config["token_file"]).read_text().strip()
        request = urllib.request.Request(self.config["collector"].rstrip("/") + "/api/agent",
                                         data=json.dumps(body).encode(), method="POST")
        request.add_header("Authorization", "Bearer " + token)
        request.add_header("Content-Type", "application/json")
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.loads(response.read() or b"{}")

    def loop(self) -> None:
        threading.Thread(target=self.measure_sizes, daemon=True).start()
        while True:
            started = time.time()
            try:
                if self.config.get("incus_project"):
                    self.incus_vms()
                self.progress_drains()
                reply = self.push(self.report())
                for command in reply.get("commands", []):
                    # Resume and Start VM can take minutes; the report loop keeps going.
                    threading.Thread(target=lambda c=command: self.results.append(self.perform(c)),
                                     daemon=True).start()
            except (OSError, ValueError, RuntimeError) as error:
                print(f"ci-fleet-agent: {error}", flush=True)
            time.sleep(max(1.0, INTERVAL - (time.time() - started)))


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="ci-fleet host agent")
    parser.add_argument("--config", default=os.environ.get("CI_FLEET_AGENT_CONFIG", "/etc/ci-fleet/agent.json"))
    parser.add_argument("--once", action="store_true", help="print one report and exit")
    args = parser.parse_args(argv)
    agent = Agent(json.loads(Path(args.config).read_text()))
    if args.once:
        agent.report()
        time.sleep(1)
        print(json.dumps(agent.report(), indent=2))
        return 0
    agent.loop()
    return 0
