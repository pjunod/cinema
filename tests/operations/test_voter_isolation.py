"""Voter isolation (C0 of docs/cluster/PLAYBACK-QUORUM-GAPS-RCA.md).

The check script is exercised against a fake host, so no engine, cgroup or
systemd state is read or touched. The fixtures model the effective values a
voter reports, not the files that declare them.
"""
from __future__ import annotations

import configparser
import contextlib
import io
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]


def _load(name: str):
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), str(ROOT / "scripts" / name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    # Dataclasses resolve their string annotations through sys.modules.
    sys.modules[loader.name] = module
    loader.exec_module(module)
    return module


check = _load("voter-isolation-check")
build = _load("voter-build")

PLURX_SCOPE = "/plurx.slice/docker-aaa.scope"
OTHER_SCOPE = "/docker-other.slice/docker-bbb.scope"


class FakeHost(check.Host):
    def __init__(self, containers, slices=None, daemon=None):
        # containers: name -> (cgroup, memory.low, memory.max, cgroup parent)
        self.containers = containers
        self.slices = slices if slices is not None else {
            "plurx.slice": {"LoadState": "loaded", "MemoryLow": "3221225472", "CPUWeight": "1000"},
            "docker-other.slice": {
                "LoadState": "loaded",
                "MemoryHigh": "15032385536",
                "MemoryMax": "21045339750",
                "MemorySwapMax": "2147483648",
                "CPUWeight": "50",
            },
        }
        self.daemon = daemon if daemon is not None else {
            "cgroup-parent": "docker-other.slice",
            "live-restore": True,
        }
        self.files = {}
        for index, (name, (cgroup, low, high, _)) in enumerate(containers.items(), start=10):
            self.files[f"/proc/{index}/cgroup"] = f"0::{cgroup}\n"
            base = "/sys/fs/cgroup" + cgroup
            self.files[base + "/memory.low"] = low + "\n"
            self.files[base + "/memory.max"] = high + "\n"
            self.files[base + "/memory.swap.max"] = "max\n"
        self.files["/etc/docker/daemon.json"] = json.dumps(self.daemon)
        self.files["/sys/fs/cgroup/cgroup.controllers"] = "cpuset cpu io memory pids\n"
        self.files["/sys/block/nvme0n1/queue/scheduler"] = "[none] mq-deadline\n"
        self.files["/sys/block/loop0/queue/scheduler"] = "[none] mq-deadline\n"
        self.commands = []
        self.exited_before_inspect = set()

    def run(self, argv):
        self.commands.append(argv)
        if argv[:2] == ["docker", "ps"]:
            return "\n".join(self.containers) + "\n"
        if argv[:2] == ["docker", "inspect"]:
            name = argv[2]
            if name in self.exited_before_inspect:
                raise check.subprocess.CalledProcessError(1, argv, stderr="No such object")
            pid = 10 + list(self.containers).index(name)
            parent = self.containers[name][3]
            return json.dumps([{"Name": "/" + name, "State": {"Pid": pid},
                                "HostConfig": {"CgroupParent": parent}}])
        if argv[:2] == ["systemctl", "show"]:
            values = self.slices.get(argv[2])
            if values is None:
                return "LoadState=not-found\n"
            return "".join(f"{key}={value}\n" for key, value in values.items())
        raise AssertionError(f"unexpected command {argv}")

    def read(self, path):
        return self.files.get(str(path))

    def glob(self, pattern):
        return [Path(path) for path in self.files if path.startswith("/sys/block/")]


def isolated(**extra):
    containers = {"plurxd": (PLURX_SCOPE, "3221225472", "max", "plurx.slice"),
                  "sonarr": (OTHER_SCOPE, "0", "max", "")}
    containers.update(extra)
    return containers


class VoterIsolationCheckTests(unittest.TestCase):
    def test_a_set_up_voter_passes_and_reads_plurxd_memory_low(self):
        host = FakeHost(isolated())
        report = check.check(host)
        self.assertEqual(report.problems, [])
        self.assertEqual(report.plurxd.memory_low, "3221225472")
        self.assertEqual(report.io_schedulers, {"nvme0n1": "[none] mq-deadline"})
        self.assertIn("isolation OK", check.render(report))
        # Read-only: nothing but listing, inspecting and showing.
        for argv in host.commands:
            self.assertIn(argv[:2], (["docker", "ps"], ["docker", "inspect"], ["systemctl", "show"]))

    def test_an_uncapped_container_outside_the_bounded_slice_fails(self):
        host = FakeHost(isolated(build=("/system.slice/docker-ccc.scope", "0", "max", "")),
                        daemon={"live-restore": True})
        report = check.check(host)
        self.assertTrue(any("build" in problem and "default cgroup-parent" in problem
                            for problem in report.problems), report.problems)
        self.assertTrue(any("daemon.json cgroup-parent" in problem for problem in report.problems))
        with contextlib.redirect_stdout(io.StringIO()) as printed:
            self.assertEqual(check.main([], host=host), 1)
        self.assertIn("2 mismatch(es)", printed.getvalue())

    def test_an_explicit_foreign_cgroup_parent_fails(self):
        host = FakeHost(isolated(escape=("/escape.slice/docker-ddd.scope", "0", "8589934592",
                                         "escape.slice")))
        problems = check.check(host).problems
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("explicit --cgroup-parent escape.slice", problems[0])

    def test_plurxd_without_memory_low_or_outside_its_slice_fails(self):
        host = FakeHost({"plurxd": ("/system.slice/docker-aaa.scope", "0", "max", "")})
        problems = check.check(host).problems
        self.assertTrue(any("outside plurx.slice" in problem for problem in problems))
        self.assertTrue(any("memory.low is 0" in problem for problem in problems))

    def test_a_container_that_exits_mid_scan_is_reported_not_a_traceback(self):
        host = FakeHost(isolated(gone=(OTHER_SCOPE, "0", "max", ""),
                                 stopping=(OTHER_SCOPE, "0", "max", "")))
        host.exited_before_inspect.add("gone")
        # `stopping` is inspected after its process exited: no /proc entry.
        del host.files[f"/proc/{10 + list(host.containers).index('stopping')}/cgroup"]
        problems = check.check(host).problems
        self.assertIn("container gone exited during the scan; run the check again", problems)
        self.assertIn("container stopping exited during the scan; run the check again", problems)
        self.assertEqual(len(problems), 2, problems)
        with contextlib.redirect_stdout(io.StringIO()) as printed:
            self.assertEqual(check.main([], host=host), 1)
        self.assertIn("MISMATCH  container gone exited during the scan", printed.getvalue())

    def test_a_cgroup_v1_host_is_labelled_as_such(self):
        host = FakeHost(isolated())
        del host.files["/sys/fs/cgroup/cgroup.controllers"]
        for index in range(10, 12):
            host.files[f"/proc/{index}/cgroup"] = "12:memory:/docker/abc\n1:name=systemd:/docker/abc\n"
        problems = check.check(host).problems
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith("cgroup v2 required"), problems)
        self.assertFalse(any("outside" in problem for problem in problems))

    def test_missing_or_unbounded_slices_fail(self):
        slices = {"plurx.slice": {"LoadState": "loaded", "MemoryLow": "0"},
                  "docker-other.slice": {"LoadState": "loaded", "MemoryHigh": "infinity",
                                         "MemoryMax": "infinity", "MemorySwapMax": "infinity"}}
        problems = check.check(FakeHost(isolated(), slices=slices)).problems
        self.assertIn("plurx.slice MemoryLow is 0", problems)
        for key in ("MemoryHigh", "MemoryMax", "MemorySwapMax"):
            self.assertIn(f"docker-other.slice {key} is unbounded", problems)
        problems = check.check(FakeHost(isolated(), slices={})).problems
        self.assertIn("plurx.slice is not installed", problems)
        self.assertIn("docker-other.slice is not installed", problems)


class VoterBuildTests(unittest.TestCase):
    def test_the_caps_are_always_added_and_the_command_passes_through(self):
        argv = build.command(["--rm", "-v", "/w:/w", "--", "rust:1.97", "sh", "-c", "cargo build -m x"])
        self.assertEqual(argv[:5], ["docker", "run", "--rm", "-v", "/w:/w"])
        # The caps come after the caller's options: docker's last one wins.
        self.assertEqual(argv[5:13], ["--memory", "8g", "--memory-swap", "8g", "--cpus", "4",
                                      "--env", "CARGO_BUILD_JOBS=4"])
        self.assertEqual(argv[-4:], ["rust:1.97", "sh", "-c", "cargo build -m x"])
        self.assertEqual(build.command(["image"]), ["docker", "run", *build.CAPS, "image"])

    def test_options_that_would_lift_a_cap_are_refused(self):
        for option in (["--memory=32g"], ["-m", "16g"], ["-m16g"], ["--memory-swap", "-1"],
                       ["--cpus", "12"], ["--cgroup-parent", "system.slice"], ["--privileged"],
                       ["--pid=host"], ["--env", "CARGO_BUILD_JOBS=12"],
                       # Fable F6: every other spelling of the jobs override,
                       ["--env=CARGO_BUILD_JOBS=12"], ["-e=CARGO_BUILD_JOBS=12"],
                       ["-eCARGO_BUILD_JOBS=12"], ["-e", "CARGO_BUILD_JOBS"],
                       ["-ite", "CARGO_BUILD_JOBS=12"], ["--env-file", "jobs.env"],
                       ["--env-file=jobs.env"],
                       # an OOM score that makes plurxd the kernel's first victim,
                       ["--oom-score-adj=-1000"], ["--oom-score-adj", "-1000"],
                       # and capabilities that let the container lift its own limits.
                       ["--cap-add", "SYS_RESOURCE"], ["--cap-add=all"],
                       ["--cap-add", "NET_ADMIN,sys_admin"]):
            with self.subTest(option=option), self.assertRaises(SystemExit):
                build.command([*option, "--", "image"])

    def test_ordinary_options_pass_and_their_values_are_not_misread(self):
        options = ["-it", "-e", "RUST_LOG=info", "--env=HOME=/w", "-w", "/w", "--cap-add", "NET_ADMIN",
                   "--name", "build-1"]
        argv = build.command([*options, "--", "image"])
        self.assertEqual(argv[2:2 + len(options)], options)
        self.assertEqual(argv[-1], "image")


class AmdPackageBuilderGuardTests(unittest.TestCase):
    BUILDER = "plurx-fixture-runner"
    CONTAINER = "buildx_buildkit_plurx-fixture-runner0"
    CAPS = "8589934592 8589934592 400000 100000 0-3"
    CAP_FORMAT = ("{{.HostConfig.Memory}} {{.HostConfig.MemorySwap}} "
                  "{{.HostConfig.CpuQuota}} {{.HostConfig.CpuPeriod}} {{.HostConfig.CpusetCpus}}")
    INSPECTION = ("Name:          plurx-fixture-runner\nDriver:        docker-container\n"
                  "Last Activity: 2026-11-09\n\nNodes:\nName: plurx-fixture-runner0\n"
                  "Endpoint: unix:///var/run/docker.sock\nDriver: node-detail\n")

    def guard(self, *, inspection=None, builder=None, name=None, caps=None, buildx_status=0):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        step = workflow.split("      - name: Bound the AMD package builder before compilation\n", 1)[1]
        body = step.split("        run: |\n", 1)[1].split("      - name:", 1)[0]
        script = textwrap.dedent(body)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stub = root / "docker"
            stub.write_text("#!" + sys.executable + "\n" + textwrap.dedent('''\
                import json, os, sys
                args = sys.argv[1:]
                with open(os.environ['DOCKER_CALLS'], 'a') as output:
                    output.write(json.dumps(args) + '\\n')
                if args[:2] == ['buildx', 'inspect']:
                    if '--format' in args:
                        print('unknown flag: --format', file=sys.stderr)
                        raise SystemExit(64)
                    if args != ['buildx', 'inspect', os.environ['BUILDER']]:
                        raise SystemExit(65)
                    sys.stdout.write(os.environ['BUILDX_TEXT'])
                    raise SystemExit(int(os.environ['BUILDX_STATUS']))
                if args[:2] == ['inspect', '--format'] and len(args) == 4:
                    if args[2] == '{{.Name}}':
                        print(os.environ['CONTAINER_NAME'])
                    elif args[2] == os.environ['CAP_FORMAT']:
                        print(os.environ['CAPS_TEXT'])
                    else:
                        raise SystemExit(66)
                elif args[:1] == ['update']:
                    print(args[-1])
                else:
                    raise SystemExit(67)
                '''))
            stub.chmod(0o755)
            log = root / "calls.jsonl"
            result = subprocess.run(["bash", "-c", script], text=True, capture_output=True,
                                    timeout=10, env={**os.environ,
                                        "PATH": os.pathsep.join((str(root), str(Path(sys.executable).parent),
                                                                 os.environ.get("PATH", ""))),
                                        "BUILDER": self.BUILDER if builder is None else builder,
                                        "BUILDX_TEXT": self.INSPECTION if inspection is None else inspection,
                                        "BUILDX_STATUS": str(buildx_status),
                                        "CONTAINER_NAME": "/" + self.CONTAINER if name is None else name,
                                        "CAPS_TEXT": self.CAPS if caps is None else caps,
                                        "CAP_FORMAT": self.CAP_FORMAT, "DOCKER_CALLS": str(log)})
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        return result, calls

    def test_actual_workflow_checks_named_buildx_headers_then_exact_effective_caps(self):
        result, calls = self.guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [
            ["buildx", "inspect", self.BUILDER],
            ["inspect", "--format", "{{.Name}}", self.CONTAINER],
            ["update", "--memory", "8g", "--memory-swap", "8g", "--cpuset-cpus", "0-3",
             "--cpu-period", "100000", "--cpu-quota", "400000", self.CONTAINER],
            ["inspect", "--format", self.CAP_FORMAT, self.CONTAINER],
        ])

    def test_wrong_missing_duplicate_or_malformed_root_headers_refuse_before_container_update(self):
        variants = [self.INSPECTION.replace(self.BUILDER, "plurx-foreign", 1),
                    self.INSPECTION.replace("docker-container", "docker", 1),
                    self.INSPECTION.replace("Name:          " + self.BUILDER + "\n", "", 1),
                    self.INSPECTION.replace("Driver:        docker-container\n", "", 1),
                    "Name: " + self.BUILDER + "\n" + self.INSPECTION,
                    "Driver: docker-container\n" + self.INSPECTION,
                    self.INSPECTION.replace("Name:          ", " Name: ", 1),
                    self.INSPECTION.replace("Name:          ", "Name : ", 1),
                    self.INSPECTION.replace("Driver:        ", "Driver:", 1),
                    self.INSPECTION.replace(self.BUILDER + "\n", self.BUILDER + " extra\n", 1),
                    self.INSPECTION.replace("Nodes:", "Nodes: malformed", 1),
                    "Nodes:\nName: " + self.BUILDER + "\nDriver: docker-container\n",
                    self.INSPECTION + "x" * 65537,
                    self.INSPECTION + "ä" * 32769,
                    self.INSPECTION + "\n" * 65537]
        for inspection in variants:
            with self.subTest(inspection=inspection[:100]):
                result, calls = self.guard(inspection=inspection)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [["buildx", "inspect", self.BUILDER]])

    def test_buildx_failure_and_unsafe_configured_name_refuse_before_engine_inspection(self):
        result, calls = self.guard(buildx_status=17)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [["buildx", "inspect", self.BUILDER]])
        result, calls = self.guard(builder="foreign;unsafe")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])

    def test_wrong_actual_container_refuses_before_update_and_inexact_caps_never_pass(self):
        result, calls = self.guard(name="/foreign-container")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call[0] == "update" for call in calls))
        for index, replacement in [(0, "0"), (1, "-1"), (2, "399999"),
                                   (3, "99999"), (4, "0-2")]:
            caps = self.CAPS.split()
            caps[index] = replacement
            with self.subTest(field=index):
                result, calls = self.guard(caps=" ".join(caps))
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls[-1], ["inspect", "--format", self.CAP_FORMAT, self.CONTAINER])


class VoterSliceAndComposeTests(unittest.TestCase):
    def slice(self, name):
        parser = configparser.ConfigParser(strict=True, interpolation=None)
        parser.optionxform = str
        parser.read(ROOT / "deploy" / "voter-isolation" / name)
        return parser["Slice"]

    def test_the_slices_carry_the_planned_limits(self):
        plurx = self.slice("plurx.slice")
        self.assertEqual((plurx["MemoryLow"], plurx["CPUWeight"]), ("3G", "1000"))
        other = self.slice("docker-other.slice")
        self.assertEqual(
            (other["MemoryHigh"], other["MemoryMax"], other["MemorySwapMax"], other["CPUWeight"]),
            ("50%", "70%", "2G", "50"),
        )
        self.assertNotIn("IOWeight", other)
        daemon = json.loads((ROOT / "deploy/voter-isolation/daemon.json.example").read_text())
        self.assertEqual(daemon, {"cgroup-parent": "docker-other.slice", "live-restore": True})

    def test_compose_places_plurxd_in_its_slice_with_its_own_memory_low(self):
        compose = (ROOT / "deploy" / "docker-compose.yml").read_text()
        service = compose.split("\n  plurxd:\n", 1)[1].split("\n  plurx-discovery:\n", 1)[0]
        # `-`, not `:-`: an explicitly empty override means no slice.
        self.assertIn("    cgroup_parent: ${PLURX_CGROUP_PARENT-plurx.slice}\n", service)
        self.assertIn("    mem_reservation: ${PLURX_MEM_RESERVATION:-3g}\n", service)


if __name__ == "__main__":
    unittest.main()
