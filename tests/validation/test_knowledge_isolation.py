"""Draft OCI-boundary tests with a wholly synthetic engine, never real proof.

Deferred until final review. No fixture image, observation or callback qualifies
an actual acceptance result. Temporary fixtures use durable owned target paths.
"""
from dataclasses import replace
import errno
import hashlib
import hmac
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from validation.knowledge import evaluation_isolation as isolation


class SyntheticEngine:
    """Engine protocol fixture; cannot issue a real runtime qualification."""
    def __init__(self):
        self.calls = []
        self.alive = set()
        self.network = "none"
        self.cleanup_fails = False
        self.image_available = True
        self.image_id = "sha256:" + "b" * 64
        self.exit_code = 0
        self.output_limited = False
        self.timed_out = False
        self.rust_version = "rustc 1.97.1 (synthetic fixture)"
        self.sentinel_readable = False
        self.callback_count = 0
        self.family_outcome = "passed"
        self.missing_dependency = False
        self.resource_failure = False
        self.oom_killed = False
        self.structured_packet = True

    def inspect(self, config, name):
        return [{"Image": self.image_id, "State": {"Running": True, "OOMKilled": self.oom_killed},
            "HostConfig": {"NetworkMode": self.network, "ReadonlyRootfs": True, "Privileged": False,
                "CapDrop": ["ALL"], "CapAdd": None, "SecurityOpt": ["no-new-privileges"], "Init": True,
                "AutoRemove": True, "PidsLimit": config.pids_limit, "Memory": config.memory_bytes,
                "MemorySwap": config.memory_bytes, "NanoCpus": int(config.cpus * 1000000000),
                "PidMode": "", "UTSMode": "", "UsernsMode": "", "IpcMode": "private", "CgroupnsMode": "private",
                "Devices": [], "DeviceRequests": []},
            "Config": {"User": f"{config.uid}:{config.gid}", "WorkingDir": "/work", "Image": config.image,
                "Entrypoint": [config.python_executable], "Cmd": ["-I", "-S", "-c", isolation._SUPERVISOR],
                "Env": [key + "=" + value for key, value in isolation._environment(config).items()],
                "Hostname": name},
            "Mounts": [{"Source": str(path), "Destination": target, "RW": not readonly, "Type": "bind"}
                       for path, target, readonly in isolation._mounts(config)]}]

    def __call__(self, config, arguments, **bounds):
        self.calls.append((arguments, bounds))
        def result(raw=b"", code=0):
            return isolation._Command(code, raw, b"")
        if arguments[:2] == ["image", "inspect"]:
            if not self.image_available:
                return result(code=1)
            return result(json.dumps([{"Id": self.image_id, "Os": "linux", "Config": {"Volumes": None},
                                       "RepoDigests": [] if config.image.startswith("sha256:") else [config.image]}]).encode())
        if arguments[0] == "run":
            name = arguments[arguments.index("--name") + 1]
            self.alive.add(name)
            return result(b"synthetic-container-id\n")
        if arguments[0] == "inspect":
            return result(json.dumps(self.inspect(config, arguments[1])).encode())
        if arguments[0] == "exec" and isolation._PROBE_CODE in arguments:
            data = {"outside": {"readable": self.sentinel_readable, "errno": errno.ENOENT},
                    "network": {"interfaces": ["lo"], "errno": errno.ENETUNREACH},
                    "filesystem": {"candidate": {"writable": True, "errno": 0},
                                   "runtime": {"writable": True, "errno": 0},
                                   "grader": {"writable": False, "errno": errno.EROFS}},
                    "versions": {"python": "3.13.0", "python_executable": config.python_executable,
                                 "rustc": {"returncode": 0, "output": self.rust_version},
                                 "cargo": {"returncode": 0, "output": "cargo 1.97.1 (synthetic fixture)"}}}
            return result(json.dumps(data).encode())
        if arguments[0] == "exec" and isolation._OUTCOME_CONTROLLER in arguments:
            self.callback_count += 1
            if self.exit_code or not self.structured_packet:
                return isolation._Command(self.exit_code, b"untrusted child stdout: assertion failed", b"Docker exec unavailable")
            source, digest, mode, argv, context, limit = arguments[-6:]
            context = json.loads(context)
            auth = json.loads(bounds["input_data"])
            classification = "prerequisite-passed" if mode == "prerequisite" else self.family_outcome
            if self.missing_dependency:
                classification = "unavailable"
            evidence = "prerequisite" if mode == "prerequisite" else "compile" if classification == "compile-failed" else "inert-bytes"
            body = {"nonce": auth["nonce"], "argv": json.loads(argv), "mode": mode, "context": context,
                    "driver_sha256": digest, "driver_started": True, "driver_completed": True,
                    "hardened": True, "resources_verified": True, "resource_failure": self.resource_failure,
                    "result": {"protocol": 1, "outcome": classification, "completed": classification != "unavailable",
                               "evidence": {"kind": evidence, "identity": "synthetic actual completed check", "sha256": "d" * 64}},
                    "stdout_base64": "", "stderr_base64": ""}
            signature = hmac.new(bytes.fromhex(auth["secret"]), isolation._json(body).encode(), hashlib.sha256).hexdigest()
            return isolation._Command(0, isolation._json({"body": body, "hmac": signature}).encode(), b"", self.timed_out, self.output_limited)
        if arguments[0] == "exec":
            self.callback_count += 1
            return isolation._Command(self.exit_code, b"PRIVATE acceptance output\xff", b"PRIVATE stderr",
                                      self.timed_out, self.output_limited)
        if arguments[0] == "rm":
            if not self.cleanup_fails:
                self.alive.discard(arguments[-1])
            return result(code=1 if self.cleanup_fails else 0)
        if arguments[:2] == ["container", "ls"]:
            return result(("\n".join(sorted(self.alive))).encode())
        raise AssertionError("unexpected engine command")


class KnowledgeIsolationTests(unittest.TestCase):
    def setUp(self):
        base = Path(os.environ.get(
            "PLURX_TEST_TMPDIR",
            Path.cwd() / "target" / "agent-fixtures" / ".hermes" /
            "hdr-collaboration" / "workspaces" / "knowledge-isolation-tests"))
        base.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(prefix="isolation-", dir=base)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for name in ("candidate", "grader-export", "runtime"):
            (self.root / name).mkdir()
        (self.root / "candidate/input.py").write_text("# synthetic candidate\n")
        (self.root / "grader-export/outcome-driver.py").write_text("# synthetic protocol fixture only; never actual qualification\n")
        (self.root / "grader-export/check.py").write_text("# synthetic grader, never executed\n")
        engine = self.root / "engine-fixture"
        engine.write_text("synthetic executable fixture; never run")
        engine.chmod(0o755)
        self.argv = ("/usr/bin/python3", "/grader/check.py", "/work")
        self.config = isolation.IsolationConfig(engine=engine, endpoint="unix:///var/run/docker.sock",
            image="registry.example/knowledge@sha256:" + "a" * 64, owned_root=self.root,
            candidate_root=self.root / "candidate", grader_root=self.root / "grader-export",
            runtime_root=self.root / "runtime", acceptance_argv=(self.argv,))
        self.plan = isolation.FamilyPlan("synthetic-inert-family", (self.argv,), (self.argv,),
            isolation.TrustedSourceFile("outcome-driver.py", hashlib.sha256((self.root / "grader-export/outcome-driver.py").read_bytes()).hexdigest()))
        self.family_cache = {}
        self.engine = SyntheticEngine()
        self.mock = patch("validation.knowledge.evaluation_isolation._command", side_effect=self.engine)
        self.mock.start()
        self.addCleanup(self.mock.stop)
        self.addCleanup(isolation._ISSUED.clear)
        self.addCleanup(isolation._QUALIFIED.clear)
        self.addCleanup(isolation._OWNED_CONTAINERS.clear)

    def family_state(self, config):
        key = (config.image, config.acceptance_argv, config.require_rust)
        if key not in self.family_cache:
            plan = replace(self.plan, prerequisite_argv=config.acceptance_argv, acceptance_argv=config.acceptance_argv)
            receipt = isolation.probe(config)
            prerequisite = isolation.qualify_family(config, plan, receipt)
            self.family_cache[key] = (plan, prerequisite)
            self.engine.callback_count = 0
        return self.family_cache[key]

    def execute(self, config, argv, receipt):
        plan, prerequisite = self.family_state(config)
        return isolation.execute(config, argv, receipt, family=plan, prerequisite=prerequisite)

    def test_valid_synthetic_probe_records_observations_then_exact_callback_and_cleanup(self):
        receipt = isolation.probe(self.config)
        self.assertEqual(receipt.status, "verified")
        recorded = json.loads(receipt.private_path.read_text())
        self.assertEqual(recorded["observations"]["observed_probes"]["versions"]["python"], "3.13.0")
        self.assertTrue(recorded["cleanup_proved"])
        result = self.execute(self.config, self.argv, receipt)
        self.assertEqual(result.status, "passed")
        self.assertEqual(self.engine.callback_count, 1)
        self.assertEqual(self.engine.alive, set())
        self.assertEqual(set(result.public()), {"status", "digest"})
        self.assertNotIn("PRIVATE", json.dumps(result.public()))
        self.assertNotIn(str(self.root), json.dumps(receipt.public()))
        self.assertIn("stdout_base64", json.loads(result.private_path.read_text())["output"])

    def test_immutable_full_image_id_works_without_repository_digest(self):
        config = replace(self.config, image=self.engine.image_id)
        receipt = isolation.probe(config)
        self.assertEqual(receipt.status, "verified")
        self.assertEqual(self.execute(config, self.argv, receipt).status, "passed")

    def test_archive_bound_quiet_wrapper_preserves_exact_argv_and_rejects_changes(self):
        wrapper = self.config.candidate_root / "scripts/quiet"
        wrapper.parent.mkdir()
        wrapper.write_text('#!/bin/sh\nexec "$@"\n')
        wrapper.chmod(0o755)
        trusted = isolation.TrustedSourceFile("scripts/quiet", hashlib.sha256(wrapper.read_bytes()).hexdigest())
        argv = ("/work/scripts/quiet", self.config.cargo_executable, "test", "--locked", "--offline")
        config = replace(self.config, require_rust=True, acceptance_argv=(argv,), trusted_wrappers=(trusted,),
                         source_commit="a" * 40, source_archive_sha256="c" * 64)
        receipt = isolation.probe(config)
        self.assertEqual(receipt.status, "verified")
        self.assertEqual(self.execute(config, argv, receipt).status, "passed")
        callback = [args for args, _ in self.engine.calls if args[0] == "exec" and isolation._OUTCOME_CONTROLLER in args][-1]
        self.assertEqual(tuple(json.loads(callback[-3])), argv)
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(replace(config, source_archive_sha256=""))
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(replace(config, acceptance_argv=(("/work/scripts/quiet", "/bin/sh", "-c", "arbitrary"),)))
        wrapper.write_text("changed wrapper")
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(config)

    def test_only_exact_archive_bound_rust_source_exceptions_are_permitted(self):
        relative = "crates/plurx-core/src/secrets.rs"
        path = self.config.candidate_root / relative
        path.parent.mkdir(parents=True)
        path.write_text("// tracked public Rust source, not credentials\n")
        trusted = isolation.TrustedSourceFile(relative, hashlib.sha256(path.read_bytes()).hexdigest())
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(self.config)
        config = replace(self.config, trusted_source_files=(trusted,), source_commit="a" * 40,
                         source_archive_sha256="c" * 64)
        self.assertEqual(isolation.probe(config).status, "verified")
        path.write_text("// changed file must require a new verified archive declaration\n")
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(config)
        for forbidden in (".cargo/config.toml", ".cargo/credentials.toml", "auth.json"):
            with self.assertRaises(isolation.IsolationError):
                isolation.probe(replace(config, trusted_source_files=(isolation.TrustedSourceFile(forbidden, "d" * 64),)))

    def test_runtime_cache_caps_are_finite_bound_and_stream_hashing_is_chunked(self):
        runtime_file = self.config.runtime_root / "large-fixture.bin"
        runtime_file.write_bytes(b"x" * (2 * 1024 * 1024 + 17))
        config = replace(self.config, runtime_max_bytes=4 * 1024 * 1024)
        receipt = isolation.probe(config)
        self.assertEqual(receipt.status, "verified")
        raw = json.loads(receipt.private_path.read_text())
        self.assertIn(config.runtime_max_bytes, raw["configuration"]["limits"])
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(replace(config, runtime_max_bytes=1))
        for maximum in (0, 129 * 1024**3):
            with self.assertRaises(isolation.IsolationError):
                isolation.probe(replace(config, runtime_max_bytes=maximum))
            with self.assertRaises(isolation.IsolationError):
                isolation.probe(replace(config, cache_max_bytes=maximum))
        sizes = []
        real_fdopen = isolation.os.fdopen
        class ObservedStream:
            def __init__(self, stream):
                self.stream = stream
            def __enter__(self):
                return self
            def __exit__(self, *args):
                self.stream.close()
            def fileno(self):
                return self.stream.fileno()
            def read(self, size):
                sizes.append(size)
                return self.stream.read(size)
        with patch("validation.knowledge.evaluation_isolation.os.fdopen", side_effect=lambda *args, **kwargs: ObservedStream(real_fdopen(*args, **kwargs))):
            actual = isolation._read_member(self.config.runtime_root, runtime_file, runtime_file.lstat(), max_bytes=config.runtime_max_bytes)
        self.assertEqual(actual, hashlib.sha256(runtime_file.read_bytes()).hexdigest())
        self.assertTrue(sizes and max(sizes) <= 1024 * 1024)

    def test_engine_symlink_is_resolved_once_and_bound_to_actual_executable(self):
        alias = self.root / "engine-alias"
        alias.symlink_to(self.config.engine)
        config = replace(self.config, engine=alias)
        self.assertEqual(config.engine, self.config.engine)
        receipt = isolation.probe(config)
        self.assertEqual(json.loads(receipt.private_path.read_text())["configuration"]["engine"], str(self.config.engine))

    def test_required_container_flags_are_fixed_and_no_socket_mount_or_shell(self):
        isolation.probe(self.config)
        run = next(arguments for arguments, _ in self.engine.calls if arguments[0] == "run")
        for flag in ("--pull=never", "--network=none", "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--init", "--rm", "--ipc=private", "--cgroupns=private"):
            self.assertIn(flag, run)
        self.assertNotIn("--privileged", run)
        self.assertNotIn("/bin/sh", run)
        self.assertNotIn("/var/run/docker.sock", " ".join(run))
        self.assertIn("--entrypoint", run)
        self.assertEqual(run[-4:-2], ["-I", "-S"])
        mounts = [run[n + 1] for n, item in enumerate(run) if item == "--mount"]
        self.assertTrue(any("dst=/grader" in item and ",readonly" in item for item in mounts))

    def test_malformed_image_unbounded_resources_and_root_user_refuse_before_engine(self):
        for changes in ({"image": "ubuntu:latest"}, {"pids_limit": 0}, {"max_output_bytes": 0},
                        {"timeout_seconds": 0}, {"cpus": float("inf")}, {"cpus": float("nan")},
                        {"uid": 0}, {"gid": 0}, {"endpoint": "tcp://daemon.example:2375"}):
            with self.subTest(changes=changes):
                with self.assertRaises(isolation.IsolationError):
                    isolation.probe(replace(self.config, **changes))
        self.assertEqual(self.engine.calls, [])

    def test_broad_overlap_private_and_symlink_mounts_refuse(self):
        for changes in ({"candidate_root": self.root}, {"candidate_root": Path.home()},
                        {"runtime_root": self.config.candidate_root}):
            with self.assertRaises(isolation.IsolationError):
                isolation.probe(replace(self.config, **changes))
        private = self.config.candidate_root / ".git"
        private.mkdir()
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(self.config)
        private.rmdir()
        (self.config.candidate_root / "escape").symlink_to(self.root.parent)
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(self.config)
        self.assertEqual(self.engine.calls, [])

    def test_cache_credential_roots_and_environment_injection_refuse(self):
        cache = self.root / ".cargo"
        cache.mkdir()
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(replace(self.config, readonly_caches=(isolation.ReadonlyCache(cache, "/offline-cache/cargo"),)))
        for environment in ((("LD_PRELOAD", "/work/inject.so"),), (("HOME", "/home/owner"),), (("API_TOKEN", "private"),), (("RUSTC", "/work/fake-rustc"),)):
            with self.assertRaises(isolation.IsolationError):
                isolation.probe(replace(self.config, environment=environment))
        self.assertEqual(self.engine.calls, [])

    def test_missing_engine_or_image_stays_unavailable(self):
        self.engine.image_available = False
        receipt = isolation.probe(self.config)
        self.assertEqual(receipt.status, "unavailable")
        result = self.execute(self.config, self.argv, receipt)
        self.assertEqual(result.status, "unavailable")
        self.assertEqual(self.engine.callback_count, 0)

    def test_observed_network_or_outside_sentinel_failure_cannot_attest(self):
        self.engine.network = "bridge"
        self.assertEqual(isolation.probe(self.config).status, "unavailable")
        self.assertEqual(self.engine.alive, set())
        self.engine.network = "none"
        self.engine.sentinel_readable = True
        self.assertEqual(isolation.probe(self.config).status, "unavailable")
        self.assertEqual(self.engine.callback_count, 0)

    def test_pinned_rust_version_is_observed_and_wrong_version_unavailable(self):
        config = replace(self.config, require_rust=True)
        receipt = isolation.probe(config)
        self.assertEqual(receipt.status, "verified")
        recorded = json.loads(receipt.private_path.read_text())
        self.assertTrue(recorded["observations"]["observed_probes"]["versions"]["rustc"]["output"].startswith("rustc 1.97.1 "))
        self.engine.rust_version = "rustc 1.96.0 (synthetic wrong runtime)"
        self.assertEqual(isolation.probe(config).status, "unavailable")

    def test_forged_tampered_or_changed_receipt_cannot_execute(self):
        receipt = isolation.probe(self.config)
        self.assertEqual(self.execute(self.config, self.argv, replace(receipt)).status, "unavailable")
        self.assertEqual(self.execute(replace(self.config, pids_limit=32), self.argv, receipt).status, "unavailable")
        receipt.private_path.write_text('{"status":"verified","cleanup_proved":true}')
        self.assertEqual(self.execute(self.config, self.argv, receipt).status, "unavailable")
        self.assertEqual(self.engine.callback_count, 0)

    def test_source_change_after_probe_or_unregistered_argv_refuses(self):
        receipt = isolation.probe(self.config)
        with self.assertRaises(isolation.IsolationError):
            self.execute(self.config, ("/usr/bin/python3", "-c", "unregistered"), receipt)
        (self.config.candidate_root / "input.py").write_text("# changed candidate\n")
        self.assertEqual(self.execute(self.config, self.argv, receipt).status, "unavailable")
        self.assertEqual(self.engine.callback_count, 0)

    def test_bounded_output_timeout_and_cleanup_failure_are_unavailable_not_model_regression(self):
        receipt = isolation.probe(self.config)
        self.engine.output_limited = True
        self.assertEqual(self.execute(self.config, self.argv, receipt).status, "unavailable")
        self.engine.output_limited = False
        self.engine.timed_out = True
        self.assertEqual(self.execute(self.config, self.argv, receipt).status, "unavailable")
        self.engine.timed_out = False
        self.engine.cleanup_fails = True
        self.assertEqual(self.execute(self.config, self.argv, receipt).status, "unavailable")

    def test_r4_security_probe_is_not_dependency_or_family_qualification(self):
        receipt = isolation.probe(self.config)
        self.assertEqual(receipt.status, "verified")
        self.assertEqual(isolation.execute(self.config, self.argv, receipt).status, "unavailable")
        unsupported = replace(self.plan, driver=None)
        prerequisite = isolation.qualify_family(self.config, unsupported, receipt)
        self.assertEqual(prerequisite.status, "unavailable")
        self.assertEqual(isolation.execute(self.config, self.argv, receipt, family=unsupported, prerequisite=prerequisite).status, "unavailable")
        self.assertEqual(self.engine.callback_count, 0)

    def test_r4_missing_dependency_executor_failure_and_oom_stay_unavailable(self):
        receipt = isolation.probe(self.config)
        plan, prerequisite = self.family_state(self.config)
        self.assertEqual(prerequisite.status, "qualified")
        for field, value in (("missing_dependency", True), ("exit_code", 1), ("oom_killed", True), ("resource_failure", True), ("structured_packet", False)):
            with self.subTest(field=field):
                before = getattr(self.engine, field)
                setattr(self.engine, field, value)
                result = isolation.execute(self.config, self.argv, receipt, family=plan, prerequisite=prerequisite)
                self.assertEqual(result.status, "unavailable")
                self.assertEqual(result.classification, "unavailable")
                setattr(self.engine, field, before)
        self.assertEqual(self.engine.alive, set())

    def test_r4_genuine_completed_assertion_and_compile_failure_require_issued_family(self):
        receipt = isolation.probe(self.config)
        plan, prerequisite = self.family_state(self.config)
        self.assertEqual(prerequisite.status, "qualified")
        for classification in ("assertion-failed", "compile-failed"):
            with self.subTest(classification=classification):
                self.engine.family_outcome = classification
                result = isolation.execute(self.config, self.argv, receipt, family=plan, prerequisite=prerequisite)
                self.assertEqual(result.status, "failed")
                self.assertEqual(result.classification, classification)
                self.assertEqual(result.prerequisite_digest, prerequisite.digest)
                self.assertTrue(json.loads(result.private_path.read_text())["cleanup_proved"])
        forged = replace(prerequisite)
        self.assertEqual(isolation.execute(self.config, self.argv, receipt, family=plan, prerequisite=forged).status, "unavailable")
        self.engine.cleanup_fails = True
        self.assertEqual(isolation.execute(self.config, self.argv, receipt, family=plan, prerequisite=prerequisite).status, "unavailable")

    def test_callback_failure_preserves_failed_only_after_cleanup_proof(self):
        receipt = isolation.probe(self.config)
        self.engine.family_outcome = "assertion-failed"
        result = self.execute(self.config, self.argv, receipt)
        self.assertEqual(result.status, "failed")
        self.assertTrue(json.loads(result.private_path.read_text())["cleanup_proved"])
        self.assertEqual(self.engine.alive, set())


if __name__ == "__main__":
    unittest.main()
