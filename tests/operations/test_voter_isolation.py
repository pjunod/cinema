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
from pathlib import Path
import sys
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
        self.files["/sys/block/nvme0n1/queue/scheduler"] = "[none] mq-deadline\n"
        self.files["/sys/block/loop0/queue/scheduler"] = "[none] mq-deadline\n"
        self.commands = []

    def run(self, argv):
        self.commands.append(argv)
        if argv[:2] == ["docker", "ps"]:
            return "\n".join(self.containers) + "\n"
        if argv[:2] == ["docker", "inspect"]:
            name = argv[2]
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
        self.assertIn("    cgroup_parent: ${PLURX_CGROUP_PARENT:-plurx.slice}\n", service)
        self.assertIn("    mem_reservation: ${PLURX_MEM_RESERVATION:-3g}\n", service)


if __name__ == "__main__":
    unittest.main()
