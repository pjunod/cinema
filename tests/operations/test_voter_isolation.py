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
import textwrap
import tempfile
import subprocess
import shlex
import re
import os
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


class NonvoterHeavyBoundsTests(unittest.TestCase):
    WORKFLOWS = (".github/workflows/ci.yml", ".github/workflows/effort-ci.yml",
                 ".github/workflows/main-fast-lane.yml")
    ROUTE = "[ci-nonvoter-heavy-amd64]"
    # Jobs whose container the workflow bounds run on the two-carrier
    # compile pool; host-mode and publishing jobs stay on the heavy host.
    COMPILE_ROUTE = "[ci-nonvoter-compile-amd64]"
    COMPILE_JOBS = frozenset({"rust_compile", "cluster_daemon", "rust_units_archive", "rust_units_baseline"})
    CAPS = {"--memory": "20g", "--memory-swap": "20g", "--cpus": "4",
            "--cpuset-cpus": "0-3", "--pids-limit": "4096"}
    EFFECTIVE = {"memory.max": "21474836480", "memory.swap.max": "0",
                 "cpu.max": "400000 100000", "cpuset.cpus.effective": "0-3",
                 "pids.max": "4096"}

    def job(self, path, name):
        workflow = (ROOT / path).read_text()
        matches = re.findall(r"(?ms)^  " + name + r":\n.*?(?=^  [A-Za-z0-9_-]+:\n|\Z)", workflow)
        self.assertEqual(len(matches), 1)
        return matches[0]

    def guarded_jobs(self):
        jobs = [(path, "rust_compile", self.job(path, "rust_compile")) for path in self.WORKFLOWS]
        jobs.extend((self.WORKFLOWS[0], name, self.job(self.WORKFLOWS[0], name))
                    for name in ("check", "package_smoke", "cluster_wal", "cluster_daemon", "web_layout", "vod_web"))
        jobs.extend((".github/workflows/validation-nightly.yml", name,
                     self.job(".github/workflows/validation-nightly.yml", name))
                    for name in ("deep-validation", "mutation"))
        jobs.append((".github/workflows/publish-release.yml", "image",
                     self.job(".github/workflows/publish-release.yml", "image")))
        jobs.append((self.WORKFLOWS[0], "publish_main", self.job(self.WORKFLOWS[0], "publish_main")))
        jobs.append((".github/workflows/media-runtime.yml", "platform",
                     self.job(".github/workflows/media-runtime.yml", "platform")))
        jobs.append((self.WORKFLOWS[2], "worker_control_proof",
                     self.job(self.WORKFLOWS[2], "worker_control_proof")))
        # The PR-time Rust unit stage compiles its archive and its merge-base
        # check on the same bounded compile pool.
        jobs.extend((".github/workflows/rust-units.yml", name,
                     self.job(".github/workflows/rust-units.yml", name))
                    for name in ("rust_units_archive", "rust_units_baseline"))
        return jobs

    def verifier(self, job, package=False):
        steps = job.split("    steps:\n", 1)[1]
        first = steps.split("\n      - ", 1)[0]
        self.assertTrue(first.startswith("      - name: Verify effective nonvoter resource bounds\n"))
        self.assertIn("        shell: sh\n", first)
        self.assertEqual("        if: matrix.arch == 'amd64'\n" in first, package)
        script = textwrap.dedent(first.split("        run: |\n", 1)[1])
        self.assertEqual(script.count("cgroup_membership=/proc/self/cgroup\n"), 1)
        self.assertEqual(script.count("cgroup_mount=/sys/fs/cgroup\n"), 1)
        return script

    def run_verifier(self, script, *, values=None, relative="/", membership=None, missing=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            mount = root / "cgroup"
            leaf = mount / relative.lstrip("/") if ".." not in relative else mount
            leaf.mkdir(parents=True)
            for name, value in (self.EFFECTIVE if values is None else values).items():
                if name != missing:
                    (leaf / name).write_text(value + "\n")
            member = root / "membership"
            member.write_text(membership if membership is not None else "0::" + relative + "\n")
            marker = root / "tools-started"
            script = script.replace("cgroup_membership=/proc/self/cgroup\n", "cgroup_membership=" + shlex.quote(str(member)) + "\n", 1)
            script = script.replace("cgroup_mount=/sys/fs/cgroup\n", "cgroup_mount=" + shlex.quote(str(mount)) + "\n", 1)
            result = subprocess.run(["sh", "-c", script + "\nprintf tools > " + shlex.quote(str(marker)) + "\n"],
                                    text=True, capture_output=True, timeout=3,
                                    env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")})
            return result, marker.exists()

    def test_every_compile_pool_job_runs_in_an_exactly_bounded_container(self):
        # The pool spans two hosts, so its only resource bound is the job's own
        # container; a host-mode job on this label would escape it.
        found = set()
        for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
            text = path.read_text()
            for match in re.finditer(r"(?ms)^  ([A-Za-z0-9_-]+):\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)", text):
                name, block = match.groups()
                header = block.split("    steps:\n", 1)[0]
                if "ci-nonvoter-compile-amd64" not in header:
                    continue
                found.add((path.name, name))
                with self.subTest(workflow=path.name, job=name):
                    self.assertEqual(re.findall(r"(?m)^    runs-on: (.+)$", header), [self.COMPILE_ROUTE])
                    self.assertIn("    container:\n", header)
                    options = re.findall(r"(?m)^      options: (.+)$", header)
                    self.assertEqual(len(options), 1)
                    argv = shlex.split(options[0])
                    self.assertEqual(dict(zip(argv[::2], argv[1::2])), self.CAPS)
                    self.assertEqual(len(argv), len(self.CAPS) * 2)
                    self.verifier(block)
        self.assertEqual({name for _, name in found}, self.COMPILE_JOBS)

    def test_only_dedicated_nonvoter_jobs_declare_exact_limits_and_verify_before_tools(self):
        scripts = []
        for path, name, job in self.guarded_jobs():
            with self.subTest(workflow=path, job=name):
                header = job.split("    steps:\n", 1)[0]
                if name in {"package_smoke", "image", "platform"}:
                    self.assertIn("runs_on: '[\"ci-nonvoter-heavy-amd64\"]'", header)
                else:
                    expected_route = ("[incus, ci-nonvoter-heavy-amd64]" if name in {"deep-validation", "mutation"}
                                      else self.COMPILE_ROUTE if name in self.COMPILE_JOBS else self.ROUTE)
                    self.assertEqual(re.findall(r"(?m)^    runs-on: (.+)$", header), [expected_route])
                    if "    container:\n" in header:
                        options = re.findall(r"(?m)^      options: (.+)$", header)
                        self.assertEqual(len(options), 1)
                        argv = shlex.split(options[0])
                        self.assertEqual(len(argv), len(self.CAPS) * 2)
                        self.assertEqual(len(set(argv[::2])), len(self.CAPS))
                        self.assertEqual(dict(zip(argv[::2], argv[1::2])), self.CAPS)
                    else:
                        self.assertIn(name, {"cluster_wal", "deep-validation", "mutation", "publish_main", "worker_control_proof"})
                    if name not in {"publish_main", "worker_control_proof"}:
                        self.assertIn('      CARGO_BUILD_JOBS: "1"\n', header)
                scripts.append(self.verifier(job, name in {"package_smoke", "image", "platform"}))
        self.assertEqual(len(scripts), 17)
        self.assertEqual(len(set(scripts)), 1, "all fifteen routes must execute the same actual cgroup checks")

    def test_actual_posix_verifier_accepts_private_and_host_service_cgroups(self):
        for path, name, job in self.guarded_jobs():
            script = self.verifier(job, name in {"package_smoke", "image", "platform"})
            for relative in ("/", "/system.slice/plurx-heavy-runner.service"):
                with self.subTest(workflow=path, job=name, cgroup=relative):
                    result, started = self.run_verifier(script, relative=relative)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(started)

    def test_hybrid_host_v1_hierarchy_before_the_unified_line_is_ignored(self):
        # lab5 mounts a v1 net_cls hierarchy (VPN split tunnelling), so a job
        # container's /proc/self/cgroup lists it ahead of the unified line.
        for path, name, job in self.guarded_jobs():
            script = self.verifier(job, name in {"package_smoke", "image", "platform"})
            for membership in ("1:net_cls:/\n0::/\n", "2:net_cls,net_prio:/x\n0::/\n1:name=systemd:/\n"):
                with self.subTest(workflow=path, job=name, membership=membership):
                    result, started = self.run_verifier(script, membership=membership)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(started)

    def test_ignored_yaml_unlimited_swap_cpu_pid_and_missing_limits_stop_before_tools(self):
        variants = [{**self.EFFECTIVE, name: value} for name, value in (
            ("memory.max", "max"), ("memory.max", "17179869184"),
            ("memory.swap.max", "max"), ("memory.swap.max", "1"),
            ("cpu.max", "max 100000"), ("cpu.max", "800000 100000"),
            ("cpu.max", "400000 200000"), ("cpuset.cpus.effective", "0-7"),
            ("pids.max", "max"), ("pids.max", "8192"))]
        # Actual ignored-options shape: 16 GiB, extra swap and unlimited CPU.
        variants.append({**self.EFFECTIVE, "memory.max": "17179869184",
                         "memory.swap.max": "17179869184", "cpu.max": "max 100000"})
        for path, name, job in self.guarded_jobs():
            script = self.verifier(job, name in {"package_smoke", "image", "platform"})
            for values in variants:
                with self.subTest(workflow=path, job=name, effective=values):
                    result, started = self.run_verifier(script, values=values)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(started)
            for missing in self.EFFECTIVE:
                with self.subTest(workflow=path, job=name, missing=missing):
                    result, started = self.run_verifier(script, missing=missing)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(started)
            for membership in ("1:memory:/\n", "1:net_cls:/\n", "0::/\n0::/\n", "0:memory:/\n",
                               "0::relative\n", "0::/../escape\n", "0::/./escape\n"):
                with self.subTest(workflow=path, job=name, membership=membership):
                    result, started = self.run_verifier(script, membership=membership)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(started)

    def test_store_routes_exclude_legacy_voter_labels_and_preserve_two_shards(self):
        from validation.runner_fleet import eligible, self_hosted_label_sets
        jobs = ((".github/workflows/ci.yml", "cluster_store_legacy"),
                (".github/workflows/store-shards.yml", "shard"),
                (".github/workflows/cluster-store-backstop.yml", "cluster-store-backstop"),
                (".github/workflows/ci.yml", "cluster_topology"),
                (".github/workflows/ci.yml", "cluster_transport_recovery_contracts"),
                (".github/workflows/ci.yml", "cluster_transport_recovery_voter"),
                (".github/workflows/ci.yml", "cluster_transport_recovery_learner"),
                (".github/workflows/ci.yml", "cluster_transport_recovery"))
        legacy = {"self-hosted", "Linux", "X64", "lab", "incus", "baremetal", "ci-store"}
        for path, name in jobs:
            with self.subTest(workflow=path, job=name):
                selectors = self_hosted_label_sets(self.job(path, name))
                self.assertEqual(len(selectors), 1)
                self.assertIn("ci-nonvoter-store", selectors[0])
                self.assertNotIn("ci-store", selectors[0])
                self.assertFalse(eligible(legacy, selectors[0]))
                self.assertEqual(selectors[0], ("ci-nonvoter-store",))
        shards = self.job(".github/workflows/store-shards.yml", "shard")
        self.assertEqual(re.findall(r'(?m)^      SHARD_COUNT: "(\d+)"$', shards), ["2"])
        self.assertEqual(re.findall(r"(?m)^        shard_index: \[(.+)\]$", shards), ["0, 1"])

    def test_release_rust_invocations_keep_their_profile_and_leave_parallelism_to_the_builder(self):
        # The Dockerfile's build stage is what `make docker-up` compiles on a
        # node. A hard-coded CARGO_BUILD_JOBS=1 there (2026-11-09) bounded no
        # CI job and made every node build ~3x slower. Parallelism is the
        # builder's: unset, cargo uses every CPU; a bounded host passes
        # --build-arg CARGO_BUILD_JOBS=N.
        dockerfile = (ROOT / "Dockerfile").read_text()
        build_stage = dockerfile.split(" AS build\n", 1)[1].split("\nFROM ", 1)[0]
        builds = [line.strip() for line in build_stage.splitlines() if "cargo build " in line]
        self.assertEqual(len(builds), 2)
        self.assertTrue(all(line.startswith("&& CARGO_TARGET_DIR=") for line in builds))
        self.assertTrue(all(" --locked --release " in line for line in builds))
        self.assertNotIn("CARGO_PROFILE_", dockerfile)
        code = [line for line in build_stage.splitlines() if not line.lstrip().startswith("#")]
        self.assertFalse([line for line in code if re.search(r"CARGO_BUILD_JOBS=\S", line)])
        self.assertFalse([line for line in code if re.match(r"ENV\b.*CARGO_BUILD_JOBS", line)])
        self.assertIn("ARG CARGO_BUILD_JOBS", code)
        self.assertLess(code.index("ARG CARGO_BUILD_JOBS"),
                        next(i for i, line in enumerate(code) if "cargo build " in line))
        # So no voter-isolation bound rides on it: every CI build of a target
        # that copies from the build stage replaces that stage with a context.
        # The dependent stages are read from the Dockerfile: any stage that
        # copies from `build`, and any stage built FROM one of those.
        dependent = set()
        stage = None
        for line in dockerfile.splitlines():
            header = re.match(r"FROM\s+(\S+)\s+AS\s+(\S+)\s*$", line, re.I)
            if header:
                stage = header.group(2)
                if header.group(1) in dependent:
                    dependent.add(stage)
            elif stage and re.match(r"COPY\s+--from=build\s", line):
                dependent.add(stage)
        self.assertIn("runtime", dependent)
        self.assertIn("default-runtime", dependent)
        checked = 0
        for workflow in sorted((ROOT / ".github" / "workflows").glob("*.yml")):
            steps = re.split(r"(?m)^\s+- (?:name|uses):", workflow.read_text())
            for step in steps:
                named = re.search(r"(?m)^\s+file: ['\"]?(?:\./)?(?:release-source/)?Dockerfile['\"]?\s*$", step)
                # build-push-action reads <context>/Dockerfile when no file is named.
                implicit = "docker/build-push-action" in step and not re.search(r"(?m)^\s+file:", step)
                if not (named or implicit):
                    continue
                target = re.search(r"(?m)^\s+target: ['\"]?([^'\"\s]+)", step)
                # With no target, BuildKit builds the last stage.
                built = target.group(1) if target else stage
                if built in dependent:
                    checked += 1
                    with self.subTest(workflow=workflow.name, target=built):
                        self.assertRegex(step, r"(?m)^\s+build=\S")
        self.assertGreater(checked, 0)

class ForgejoRoleRoutingTests(unittest.TestCase):
    def test_exact_role_only_and_incus_selectors_are_local_including_matrix(self):
        from validation import runner_fleet as fleet
        for labels in sorted(fleet.FORGEJO_ROLE_LABEL_SETS):
            with self.subTest(labels=labels):
                direct = "    runs-on: [" + ", ".join(labels) + "]\n"
                matrix = "            runs_on: '" + json.dumps(labels) + "'\n"
                self.assertEqual(fleet.self_hosted_label_sets(direct), [labels])
                self.assertEqual(fleet.self_hosted_label_sets(matrix), [labels])

    def test_unknown_mixed_duplicate_and_wrong_typed_alias_selectors_refuse(self):
        from validation import runner_fleet as fleet
        for labels in (("ci-nonvoter-unknown",), ("ci-nonvoter-store", "ci-nonvoter-heavy-amd64"),
                       ("self-hosted", "ci-nonvoter-heavy-amd64"),
                       ("ci-nonvoter-store", "ci-nonvoter-store"),
                       ("general", "ci-nonvoter-store"), ("incus", "ci-nonvoter-store"),
                       ("ci-nonvoter-heavy-amd64", "incus"), ("ci-nonvoter-store", 1)):
            with self.subTest(labels=labels), self.assertRaises(ValueError):
                fleet.self_hosted_label_sets("            runs_on: '" + json.dumps(labels) + "'\n")

    def test_explicit_role_only_worker_does_not_gain_generic_pool_eligibility(self):
        from validation import runner_fleet as fleet
        old = {"name": "old", "os": "Linux", "arch": "X64", "labels": ["lab"]}
        roles = ["ci-nonvoter-heavy-amd64", "ci-nonvoter-compile-amd64", "ci-nonvoter-store", "incus"]
        new = {"name": "dedicated", "os": "Linux", "arch": "X64",
               "labels": roles, "implicit_labels": False}
        rows = dict(fleet.rostered_runners({"runners": [old, new]}))
        self.assertEqual(rows["old"], frozenset({"lab", "self-hosted", "Linux", "X64"}))
        self.assertEqual(rows["dedicated"], frozenset(roles))
        for labels in fleet.FORGEJO_ROLE_LABEL_SETS:
            self.assertTrue(fleet.eligible(rows["dedicated"], labels))
        for label in ("self-hosted", "Linux", "X64", "lab", "general", "high-cpu", "ci-store", "ci-topology"):
            self.assertFalse(fleet.eligible(rows["dedicated"], {label}), label)
        for invalid in (None, 0, 1, "false", [], {}):
            with self.subTest(implicit_labels=invalid), self.assertRaises(ValueError):
                fleet.rostered_runners({"runners": [{**new, "implicit_labels": invalid}]})
        with self.assertRaises(ValueError):
            fleet.rostered_runners({"runners": [{**new, "labels": ["ci-nonvoter-unknown"]}]})
        for invalid in (None, "ci-nonvoter-store", [None], ["incus", "incus"]):
            with self.subTest(labels=invalid), self.assertRaises(ValueError):
                fleet.rostered_runners({"runners": [{**new, "labels": invalid}]})

class AmdPackageBuilderGuardTests(unittest.TestCase):
    BUILDER = "plurx-fixture-runner"
    CONTAINER = "buildx_buildkit_plurx-fixture-runner0"
    CAPS = "21474836480 21474836480 400000 100000 0-3 4096"
    CAP_FORMAT = ("{{.HostConfig.Memory}} {{.HostConfig.MemorySwap}} "
                  "{{.HostConfig.CpuQuota}} {{.HostConfig.CpuPeriod}} {{.HostConfig.CpusetCpus}} {{.HostConfig.PidsLimit}}")
    INSPECTION = ("Name:          plurx-fixture-runner\nDriver:        docker-container\n"
                  "Last Activity: 2026-11-09\n\nNodes:\nName: plurx-fixture-runner0\n"
                  "Endpoint: unix:///var/run/docker.sock\nDriver: node-detail\n")

    WORKFLOW = ".github/workflows/ci.yml"
    GUARD_NAME = "Bound the AMD package builder before compilation"

    def guard(self, *, inspection=None, builder=None, name=None, caps=None, buildx_status=0):
        workflow = (ROOT / self.WORKFLOW).read_text()
        step = workflow.split("      - name: " + self.GUARD_NAME + "\n", 1)[1]
        body = re.split(r"(?m)^      - ", step.split("        run: |\n", 1)[1], maxsplit=1)[0]
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
            ["update", "--memory", "20g", "--memory-swap", "20g", "--pids-limit", "4096", "--cpuset-cpus", "0-3",
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
                                   (3, "99999"), (4, "0-2"), (5, "8192")]:
            caps = self.CAPS.split()
            caps[index] = replacement
            with self.subTest(field=index):
                result, calls = self.guard(caps=" ".join(caps))
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls[-1], ["inspect", "--format", self.CAP_FORMAT, self.CONTAINER])

class AmdPublisherBuilderGuardTests(AmdPackageBuilderGuardTests):
    WORKFLOW = ".github/workflows/publish-release.yml"
    GUARD_NAME = "Bound the AMD publisher builder before compilation"


class AmdMediaRuntimeBuilderGuardTests(AmdPackageBuilderGuardTests):
    WORKFLOW = ".github/workflows/media-runtime.yml"
    GUARD_NAME = "Bound the AMD media builder before compilation"


class MainPublisherBuilderGuardTests(AmdPackageBuilderGuardTests):
    WORKFLOW = ".github/workflows/ci.yml"
    GUARD_NAME = "Bound the main publisher builder before compilation"


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
