"""Offline contracts for the host wrapper; never run Docker or change networks."""

import contextlib
import copy
import io
import json
from pathlib import Path
import runpy
import subprocess
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
WRAPPER = runpy.run_path(str(ROOT / "scripts/unit-coverage"))
GLOBALS = WRAPPER["execute"].__globals__
COMMIT = "a" * 40
BRIDGE = {"Name": "bridge", "IPAM": {"Config": [{"Subnet": "172.17.0.0/16"}]}}
GIB = 1024**3
POLICY = {
    "Memory": 16 * GIB, "MemorySwap": 18 * GIB, "NanoCpus": 3_000_000_000,
    "PidsLimit": 4096, "NetworkMode": "bridge", "IpcMode": "private",
}


def prepare_cache(root):
    cache = root / "tools/plurx-ci/cargo/fixture/rust-1.97.1"
    for child in ("cargo-home", "coverage/target"):
        (cache / child).mkdir(parents=True)
    (cache / "coverage/.last-used").touch()
    return cache


class HostCoverageGuards(unittest.TestCase):
    def test_network_overlap_and_nonprivate_default_bridge_are_refused(self):
        WRAPPER["assert_network_space"]([{"dst": "default"}], [BRIDGE])
        for routes, networks in (
            ([{"dst": "100.64.0.0/10"}], [BRIDGE]),
            ([], [BRIDGE, {"IPAM": {"Config": [{"Subnet": "100.127.250.0/24"}]}}]),
            ([], [{"Name": "bridge", "IPAM": {"Config": [{"Subnet": "198.51.100.0/24"}]}}]),
        ):
            with self.subTest(routes=routes, networks=networks), self.assertRaises(ValueError):
                WRAPPER["assert_network_space"](routes, networks)

    def test_standard_docker_bounds_and_isolation_cannot_be_relaxed(self):
        mounts = {"/workspace/noirr/plurx": "/owned/source", "/opt/coverage-cache": "/owned/cache", "/opt/coverage-output": "/owned/output"}
        valid = {"HostConfig": POLICY, "Mounts": [{"Type": "bind", "Destination": k, "Source": v} for k, v in mounts.items()]}
        WRAPPER["assert_container_policy"](valid, mounts)
        for key, value in (
            ("Memory", 0), ("MemorySwap", 0), ("NanoCpus", 0), ("PidsLimit", 0),
            ("Privileged", True), ("CapAdd", ["NET_ADMIN"]),
            ("SecurityOpt", ["seccomp=unconfined"]), ("NetworkMode", "host"),
            ("PidMode", "host"), ("Devices", [{}]),
        ):
            wrong = copy.deepcopy(valid)
            wrong["HostConfig"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                WRAPPER["assert_container_policy"](wrong, mounts)
        wrong = copy.deepcopy(valid)
        wrong["Mounts"].append({"Type": "bind", "Destination": "/extra", "Source": "/var/run/docker.sock"})
        with self.assertRaises(ValueError):
            WRAPPER["assert_container_policy"](wrong, mounts)

    def test_prepared_cache_refuses_host_credentials_and_symlinked_components(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            cache = prepare_cache(root)
            self.assertEqual(WRAPPER["prepared_cache"](cache), (cache, root / "tools"))
            for name in ("credentials", "credentials.toml", ".credentials", ".credentials.toml", ".git-credentials"):
                path = cache / "cargo-home" / name
                path.write_text("synthetic-only")
                with self.subTest(name=name), self.assertRaises(ValueError):
                    WRAPPER["prepared_cache"](cache)
                path.unlink()
            config = cache / "cargo-home/config.toml"
            config.write_text('[registry]\ntoken = "synthetic-only"\n')
            with self.assertRaises(ValueError):
                WRAPPER["prepared_cache"](cache)
            config.unlink()
            (cache / "coverage/target").rmdir()
            (cache / "coverage/target").symlink_to(root)
            with self.assertRaises(ValueError):
                WRAPPER["prepared_cache"](cache)

    def test_disk_guard_refuses_less_than_25_gib(self):
        with patch.object(GLOBALS["shutil"], "disk_usage", return_value=types.SimpleNamespace(free=24 * GIB)):
            with self.assertRaises(RuntimeError):
                WRAPPER["require_space"](Path("/synthetic"))


class OfflineDocker:
    def __init__(self, root, outcome):
        self.root = root
        self.outcome = outcome
        self.commands = []
        self.container = False
        self.network = False

    def call(self, args, *, env, cwd=None, timeout=60):
        self.commands.append(list(args))
        if set(env) != {"PATH", "HOME", "DOCKER_CONFIG", "LANG", "RUNNER_TOOL_CACHE", "GIT_CONFIG_NOSYSTEM", "GIT_CONFIG_GLOBAL", "GIT_OPTIONAL_LOCKS"}:
            raise AssertionError("host environment escaped its allowlist")
        if args[:3] == ["git", "rev-parse", "--show-toplevel"]:
            return str(self.root)
        if args[:3] == ["git", "rev-parse", "HEAD"]:
            return COMMIT
        if args[:3] == ["git", "show", "-s"]:
            return "1700000000"
        if args[0] == "ip":
            return json.dumps([{"dst": "default"}])
        if args[0] != "docker":
            if Path(args[0]).name not in ("ci-cargo-cache-refresh", "ci-cache-prune"):
                raise AssertionError(args)
            return ""
        command = args[1:]
        if command[:2] == ["ps", "-aq"]:
            return "owned-container" if self.container else ""
        if command[:3] == ["network", "ls", "-q"]:
            return ("owned-network" if self.network else "") if "--filter" in command else "default-bridge"
        if command[:2] == ["image", "inspect"]:
            return json.dumps([{"Id": "sha256:synthetic", "Config": {}}])
        if command[:2] == ["network", "create"]:
            self.network = True
            self.labels = dict(command[i + 1].split("=", 1) for i, value in enumerate(command) if value == "--label")
            return "owned-network"
        if command[:2] == ["network", "inspect"]:
            return json.dumps([BRIDGE] if command[2] == "default-bridge" else [{"Internal": True, "Labels": self.labels}])
        if command[:2] == ["network", "connect"]:
            return ""
        if command[0] == "create":
            self.container = True
            self.mounts = {}
            for index, value in enumerate(command):
                if value == "--mount":
                    fields = dict(item.split("=", 1) for item in command[index + 1].split(","))
                    self.mounts[fields["dst"]] = fields["src"]
            if Path(env["DOCKER_CONFIG"]).is_relative_to(Path(self.mounts["/opt/coverage-output"])):
                raise AssertionError("Docker configuration must stay outside child binds")
            return "owned-container"
        if command[0] == "inspect":
            return json.dumps([{"HostConfig": POLICY, "Config": {"Labels": self.labels}, "Mounts": [{"Type": "bind", "Destination": k, "Source": v} for k, v in self.mounts.items()]}])
        if command[:2] == ["rm", "--force"]:
            self.container = False
        elif command[:2] == ["network", "rm"]:
            self.network = False
        elif command[0] not in ("start", "stop", "exec"):
            raise AssertionError(command)
        if self.outcome == "cleanup_failure" and "/usr/bin/chown" in command:
            raise RuntimeError("synthetic ownership restoration refusal")
        return ""

    def archive(self, args, *, cwd, env, stdout, check):
        if args != ["git", "archive", "--format=tar.gz", COMMIT]:
            raise AssertionError(args)
        with tarfile.open(fileobj=stdout, mode="w:gz", pax_headers={"comment": COMMIT}) as archive:
            member = tarfile.TarInfo("scripts/synthetic-source")
            body = b"synthetic committed source, not a media/coverage receipt"
            member.size = len(body)
            archive.addfile(member, io.BytesIO(body))
        return subprocess.CompletedProcess(args, 0)

    def logged(self, args, *, env, log):
        if args[:2] != ["docker", "exec"] or args[3:5] != ["/usr/bin/env", "-i"]:
            raise AssertionError("campaign must use a fresh child environment")
        log.write_text("synthetic command trace; no Docker executed\n")
        run = log.parent
        host = json.loads((run / "host.json").read_text())
        campaign = run / "campaign"
        campaign.mkdir()
        report = campaign / "cov.json"
        report.write_text('{"data":[{"totals":{"lines":{"count":10,"covered":9,"percent":90}}}]}\n')
        receipt = {"status": "passed", "source_commit": host["source_commit"], "source_sha256": host["source_sha256"], "report_sha256": WRAPPER["file_sha256"](report)}
        if self.outcome == "bad_digest":
            receipt["report_sha256"] = "0" * 64
        if self.outcome == "bad_source":
            receipt["source_commit"] = "b" * 40
        if self.outcome == "bad_status":
            receipt["status"] = "failed"
        (campaign / "receipt.json").write_text(json.dumps(receipt))
        if self.outcome == "foreign_owner":
            self.labels = dict(self.labels, **{WRAPPER["OWNER_LABEL"]: "not-this-run"})
        if self.outcome == "cancel":
            raise KeyboardInterrupt("synthetic cancellation")
        return 1 if self.outcome == "failure" else 0


class HostCoverageOrchestration(unittest.TestCase):
    def test_stale_owned_container_fails_closed_before_cache_or_source_work(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            cache = prepare_cache(root)
            fake = OfflineDocker(root, "success")
            fake.container = True
            arguments = types.SimpleNamespace(cache_root=cache, output=root / "output", image="ubuntu:24.04", native=False)
            with patch.dict(GLOBALS, {"call": fake.call, "require_space": lambda path: None, "sys": types.SimpleNamespace(platform="linux")}), contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(RuntimeError, "receipt-based recovery"):
                WRAPPER["execute"](arguments)
            self.assertTrue(fake.container)
            self.assertTrue(all(command[0] == "docker" and command[1] in ("ps", "network") for command in fake.commands))

    def test_cleanup_refuses_resources_without_exact_owner_labels(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            cache = prepare_cache(root)
            fake = OfflineDocker(root, "foreign_owner")
            output = root / "output"
            arguments = types.SimpleNamespace(cache_root=cache, output=output, image="ubuntu:24.04", native=False)
            with patch.dict(GLOBALS, {"call": fake.call, "logged": fake.logged, "require_space": lambda path: None, "sys": types.SimpleNamespace(platform="linux")}), patch.object(GLOBALS["subprocess"], "run", fake.archive), contextlib.redirect_stdout(io.StringIO()), self.assertRaises(RuntimeError):
                WRAPPER["execute"](arguments)
            self.assertTrue(fake.container)
            self.assertTrue(fake.network)
            self.assertFalse(any(command[:2] == ["docker", "rm"] or command[:3] == ["docker", "network", "rm"] or "/usr/bin/chown" in command for command in fake.commands))
            self.assertFalse((output / "cov.json").exists())
            self.assertFalse((output / "receipt.json").exists())

    def test_success_failure_cancellation_and_invalid_digest_have_safe_publication(self):
        for outcome in ("success", "failure", "cancel", "bad_digest", "bad_source", "bad_status", "cleanup_failure"):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                cache = prepare_cache(root)
                output = root / "output"
                fake = OfflineDocker(root, outcome)
                arguments = types.SimpleNamespace(cache_root=cache, output=output, image="ubuntu:24.04", native=False)
                with patch.dict(GLOBALS, {"call": fake.call, "logged": fake.logged, "require_space": lambda path: None, "sys": types.SimpleNamespace(platform="linux")}), patch.object(GLOBALS["subprocess"], "run", fake.archive), contextlib.redirect_stdout(io.StringIO()):
                    if outcome == "success":
                        WRAPPER["execute"](arguments)
                    else:
                        with self.assertRaises((RuntimeError, KeyboardInterrupt, ValueError)):
                            WRAPPER["execute"](arguments)
                host = json.loads(next(output.glob("run-*/host.json")).read_text())
                self.assertFalse(fake.container)
                self.assertFalse(fake.network)
                if outcome == "cleanup_failure":
                    self.assertIn("ownership restoration refusal", host["cleanup_errors"][0])
                else:
                    self.assertEqual(host["cleanup_errors"], [])
                commands = fake.commands
                stop = next(i for i, c in enumerate(commands) if c[:2] == ["docker", "stop"])
                chown = next(i for i, c in enumerate(commands) if "/usr/bin/chown" in c)
                remove = next(i for i, c in enumerate(commands) if c[:2] == ["docker", "rm"])
                self.assertLess(stop, chown)
                self.assertLess(chown, remove)
                self.assertEqual(set(fake.mounts), {"/workspace/noirr/plurx", "/opt/coverage-cache", "/opt/coverage-output"})
                if outcome == "success":
                    self.assertEqual(host["status"], "complete")
                    measured = json.loads((output / "receipt.json").read_text())
                    self.assertEqual(measured["report_sha256"], WRAPPER["file_sha256"](output / "cov.json"))
                else:
                    self.assertNotEqual(host["status"], "complete")
                    self.assertFalse((output / "cov.json").exists())
                    self.assertFalse((output / "receipt.json").exists())

    def test_existing_publication_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            cache = prepare_cache(root)
            output = root / "output"
            output.mkdir()
            (output / "cov.json").write_text("existing receipt material")
            arguments = types.SimpleNamespace(cache_root=cache, output=output, image="ubuntu:24.04", native=False)
            with patch.dict(GLOBALS, {"sys": types.SimpleNamespace(platform="linux")}), self.assertRaises(ValueError):
                WRAPPER["execute"](arguments)
            self.assertEqual((output / "cov.json").read_text(), "existing receipt material")
            self.assertFalse(list(output.glob("run-*")))

    def test_publication_race_preserves_existing_receipt_and_rolls_back_only_its_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run = root / "run"
            campaign = run / "campaign"
            campaign.mkdir(parents=True)
            (run / "source.tgz").write_bytes(b"synthetic archive")
            archive_digest = WRAPPER["file_sha256"](run / "source.tgz")
            (campaign / "cov.json").write_text('{"synthetic":true}')
            receipt = {"status": "passed", "source_commit": COMMIT, "source_sha256": archive_digest, "report_sha256": WRAPPER["file_sha256"](campaign / "cov.json")}
            (campaign / "receipt.json").write_text(json.dumps(receipt))
            (root / "receipt.json").write_text("unrelated existing receipt")
            with self.assertRaises(FileExistsError):
                WRAPPER["publish_result"](run, root, COMMIT, archive_digest)
            self.assertEqual((root / "receipt.json").read_text(), "unrelated existing receipt")
            self.assertFalse((root / "cov.json").exists())
            self.assertFalse(list(root.glob(".unit-coverage-publish-*")))
