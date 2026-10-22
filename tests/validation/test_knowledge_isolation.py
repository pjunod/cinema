"""Synthetic boundary contracts and opt-in real isolated-controller regressions.

Deferred until final review. The synthetic engine never proves OCI isolation;
real controller tests require explicit reviewed image/engine configuration.
All sources are synthetic and never qualify actual study acceptance.
"""
from dataclasses import replace
import base64
import errno
import hashlib
import hmac
import json
import os
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import patch

from validation.knowledge import evaluation_isolation as isolation


def synthetic_guest_errno():
    # Explicit Linux guest values: tests must not inherit their host's errno ABI.
    return {"schema_version": 1, "platform": "linux", "codes": {
        "EPERM": 1, "ENOENT": 2, "EACCES": 13, "EROFS": 30, "ENETUNREACH": 101, "EHOSTUNREACH": 113}}


def synthetic_guest_error(code):
    names = {value: key for key, value in synthetic_guest_errno()["codes"].items()}
    return {"errno": code, "error": "OK" if type(code) is int and code == 0 else names.get(code, "UNKNOWN")}


def synthetic_closed_network():
    return {"interfaces": ["lo"], "links": {"lo": {"flags": 0x9, "operstate": "unknown"}},
            "ipv4_routes": [], "ipv6_routes": [], **synthetic_guest_error(101)}


class SyntheticEngine:
    """Engine protocol fixture; cannot issue a real runtime qualification."""
    def __init__(self):
        self.calls = []
        self.alive = set()
        self.network = "none"
        self.network_observation = None
        self.guest_errno = synthetic_guest_errno()
        self.mutate_probe = None
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
        self.mutate_callback = None
        self.completed_checks = 1
        self.outcome_protocol = None
        self.bad_hmac = False
        self.oom_killed = False
        self.structured_packet = True
        self.quiet_writable_override = None
        self.build_jobs_override = None
        self.build_environment_override = None

    def inspect(self, config, name):
        return [{"Image": self.image_id, "State": {"Running": True, "OOMKilled": self.oom_killed},
            "HostConfig": {"NetworkMode": self.network, "ReadonlyRootfs": True, "Privileged": False,
                "CapDrop": ["ALL"], "CapAdd": None, "SecurityOpt": ["no-new-privileges"], "Init": True,
                "AutoRemove": True, "PidsLimit": config.pids_limit, "Memory": config.memory_bytes,
                "MemorySwap": config.memory_bytes, "NanoCpus": int(config.cpus * 1000000000),
                "PidMode": "", "UTSMode": "", "UsernsMode": "", "IpcMode": "private", "CgroupnsMode": "private",
                "Devices": [], "DeviceRequests": []},
            "Config": {"User": f"{config.uid}:{config.gid}", "WorkingDir": config.working_directory, "Image": config.image,
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
        if arguments[0] == "exec" and isolation._RESOURCE_CODE in arguments:
            return result(json.dumps({"memory.events": {"oom": 0, "oom_kill": 0, "max": 0}, "pids.events": {"max": 0}}).encode())
        if arguments[0] == "exec" and isolation._PROBE_CODE in arguments:
            data = {"outside": {"readable": self.sentinel_readable, **synthetic_guest_error(0 if self.sentinel_readable else 2)},
                    "guest_errno": self.guest_errno,
                    "network": synthetic_closed_network() if self.network_observation is None else self.network_observation,
                    "filesystem": {"candidate": {"writable": not config.candidate_readonly, **synthetic_guest_error(30 if config.candidate_readonly else 0)},
                                   "runtime": {"writable": True, **synthetic_guest_error(0)},
                                   "grader": {"writable": False, **synthetic_guest_error(30)},
                                   "quiet_logs": {"writable": isolation._quiet_log_mount(config) if self.quiet_writable_override is None else self.quiet_writable_override,
                                                  **synthetic_guest_error(0 if isolation._quiet_log_mount(config) else 2)}},
                    "build_environment": ({key: isolation._environment(config).get(key) for key in ("CARGO_BUILD_JOBS", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG")}
                        | ({"CARGO_BUILD_JOBS": self.build_jobs_override} if self.build_jobs_override is not None else {})
                        | (self.build_environment_override or {})) if config.require_rust else {},
                    "versions": {"python": "3.13.0", "python_executable": config.python_executable,
                                 "rustc": {"returncode": 0, "output": self.rust_version},
                                 "cargo": {"returncode": 0, "output": "cargo 1.97.1 (synthetic fixture)"}}}
            if self.mutate_probe is not None:
                self.mutate_probe(data)
            return result(json.dumps(data).encode())
        if arguments[0] == "exec" and isolation._OUTCOME_CONTROLLER in arguments:
            self.callback_count += 1
            if self.exit_code or not self.structured_packet:
                return isolation._Command(self.exit_code, b"untrusted child stdout: assertion failed", b"Docker exec unavailable")
            source, digest, mode, argv, context, limit = arguments[-6:]
            context = json.loads(context)
            auth = json.loads(bounds["input_data"])
            version = context.get("protocol", 1)
            step = context.get("step", {})
            classification = ("setup-passed" if step.get("kind") == "setup" else
                              "prerequisite-passed" if mode == "prerequisite" else self.family_outcome)
            if self.missing_dependency:
                classification = "unavailable"
            evidence = step["kind"] if version == 2 else "prerequisite" if mode == "prerequisite" else "compile" if classification == "compile-failed" else "inert-bytes"
            if self.mutate_callback is not None:
                self.mutate_callback(config, mode, step)
            body = {"nonce": auth["nonce"], "argv": json.loads(argv), "mode": mode, "context": context,
                    "driver_sha256": digest, "driver_started": True, "driver_completed": True,
                    "hardened": True, "resources_verified": True, "resource_failure": self.resource_failure,
                    "result": {"protocol": self.outcome_protocol or version, "outcome": classification, "completed": classification != "unavailable",
                               "evidence": {"kind": evidence, "identity": "synthetic actual completed check", "sha256": "d" * 64,
                                            "completed_checks": self.completed_checks,
                                            "failed_checks": int(classification.endswith("-failed")), "skipped_checks": 0}},
                    "stdout_base64": "", "stderr_base64": ""}
            signature = hmac.new(bytes.fromhex(auth["secret"]), isolation._json(body).encode(), hashlib.sha256).hexdigest()
            if self.bad_hmac:
                signature = "0" * 64
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
            selector = arguments[arguments.index("--filter") + 1]
            if not selector.startswith("name=^/") or not selector.endswith("$"):
                raise AssertionError("expected exact owned-container name filter")
            selected = selector[len("name=^/"):-1]
            return result(selected.encode() if selected in self.alive else b"")
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
        self.addCleanup(isolation._STEPS.clear)
        self.addCleanup(isolation._CONSUMED_STEPS.clear)
        self.addCleanup(isolation._ACTIVE_ROOTS.clear)
        self.addCleanup(isolation._RESERVED_STEPS.clear)

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

    def quiet_configuration(self):
        wrapper = self.config.candidate_root / "scripts/quiet"
        wrapper.parent.mkdir(exist_ok=True)
        wrapper.write_bytes((Path(__file__).resolve().parents[2] / "scripts/quiet").read_bytes())
        wrapper.chmod(0o755)
        trusted = isolation.TrustedSourceFile("scripts/quiet", hashlib.sha256(wrapper.read_bytes()).hexdigest())
        argv = ("/work/scripts/quiet", self.config.cargo_executable, "--version")
        return replace(self.config, require_rust=True, candidate_readonly=True, acceptance_argv=(argv,),
                       trusted_wrappers=(trusted,), source_commit="a" * 40, source_archive_sha256="c" * 64)

    def test_quiet_mount_is_narrow_measured_and_bound_to_fresh_probe(self):
        config = self.quiet_configuration()
        receipt = isolation.probe(config)
        self.assertEqual(receipt.status, "verified")
        before = json.loads(receipt.private_path.read_bytes())["configuration"]
        mounts = isolation._mounts(config)
        self.assertIn((config.candidate_root, "/work", True), mounts)
        self.assertIn((config.runtime_root / "quiet-logs", "/work/target/quiet", False), mounts)
        self.assertEqual(len(mounts), 4)
        self.assertTrue(before["quiet_log_mount"])
        self.assertEqual(len(before["mount_identity"]), len(mounts))
        self.assertEqual([row[0] for row in before["mountpoint_identity"]], [".runtime", "target", "target/quiet"])
        args = isolation._container_arguments(config, "synthetic-name")
        self.assertIn("type=bind,src=" + str(config.candidate_root) + ",dst=/work,bind-propagation=rprivate,readonly", args)
        self.assertIn("type=bind,src=" + str(config.runtime_root / "quiet-logs") + ",dst=/work/target/quiet,bind-propagation=rprivate", args)
        log = config.runtime_root / "quiet-logs/synthetic.log"
        log.write_text("controller-owned wrapper output")
        private, after = isolation._prepare(config)
        self.assertEqual(before["sources"]["candidate"], after["sources"]["candidate"])
        self.assertNotEqual(before["sources"]["runtime"], after["sources"]["runtime"])
        self.assertIsNone(isolation._issued_probe(receipt, private, after))
        self.assertEqual(list((config.candidate_root / "target/quiet").iterdir()), [])
        old_mountpoint = config.candidate_root / "target/quiet"
        old_mountpoint.rename(config.candidate_root / "target/empty-old-mountpoint")
        old_mountpoint.mkdir()
        _, replaced_mountpoint = isolation._prepare(config)
        self.assertEqual(after["sources"]["candidate"], replaced_mountpoint["sources"]["candidate"])
        self.assertNotEqual(after["mountpoint_identity"], replaced_mountpoint["mountpoint_identity"])
        observed = self.engine.inspect(config, "synthetic-name")
        observed[0]["Mounts"][-1]["RW"] = False
        with self.assertRaises(isolation.IsolationError): isolation._observed_security(config, observed)

    def test_quiet_mountpoint_and_actual_write_observation_cannot_be_forged(self):
        config = self.quiet_configuration()
        self.engine.quiet_writable_override = False
        self.assertEqual(isolation.probe(config).status, "unavailable")
        self.assertEqual(self.engine.alive, set())
        source_log = config.candidate_root / "target/quiet/source-file"
        source_log.write_text("must not be hidden by runtime mount")
        with self.assertRaises(isolation.IsolationError): isolation.probe(config)
        source_log.unlink()
        (config.runtime_root / "quiet-logs").rmdir()
        (config.runtime_root / "quiet-logs").symlink_to(config.runtime_root / "tmp", target_is_directory=True)
        with self.assertRaises(isolation.IsolationError): isolation.probe(config)

    def test_controller_declares_packaging_environment_without_allowing_ambient_extras(self):
        self.assertEqual(isolation._environment(self.config)["DEBIAN_FRONTEND"], "noninteractive")
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(replace(self.config, environment=(("DEBIAN_FRONTEND", "interactive"),)))
        observed = self.engine.inspect(self.config, "synthetic")
        observed[0]["Config"]["Env"].append("UNDECLARED_IMAGE_VALUE=1")
        with self.assertRaises(isolation.IsolationError): isolation._observed_security(self.config, observed)

    def dormant_network(self):
        value = synthetic_closed_network()
        value["links"]["tunnel0"] = {"flags": 0x80, "operstate": "down"}
        value["interfaces"] = sorted(value["links"])
        return value

    def test_dormant_kernel_links_prove_denial_by_state_without_name_exceptions(self):
        value = self.dormant_network()
        self.assertTrue(isolation._network_denied(value, synthetic_guest_errno()))
        value["ipv6_routes"] = [{"interface": "lo", "destination": "0" * 31 + "1", "destination_prefix": 128,
                                 "source": "0" * 32, "source_prefix": 0, "gateway": "0" * 32, "flags": 0x80200001}]
        self.assertTrue(isolation._network_denied(value, synthetic_guest_errno()))
        self.engine.network_observation = value
        receipt = isolation.probe(self.config)
        self.assertEqual(receipt.status, "verified")
        recorded = json.loads(receipt.private_path.read_bytes())["observations"]["observed_probes"]["network"]
        self.assertEqual(recorded, value)

    def test_network_proof_refuses_active_unknown_inconsistent_or_routed_links(self):
        for flags, operstate in ((0x81, "up"), (0x81, "down"), (0x80, "unknown"),
                                 (0x80, "dormant"), (0x80, "up"), (0xc0, "down"), (True, "down"), (0x80, None)):
            value = self.dormant_network()
            value["links"]["tunnel0"] = {"flags": flags, "operstate": operstate}
            with self.subTest(flags=flags, operstate=operstate):
                self.assertFalse(isolation._network_denied(value, synthetic_guest_errno()))
        value = self.dormant_network(); del value["links"]["tunnel0"]["operstate"]
        self.assertFalse(isolation._network_denied(value, synthetic_guest_errno()))
        for family, row in (("ipv4_routes", {"interface": "tunnel0", "destination": "00000000",
                "gateway": "00000000", "mask": "00000000", "flags": 1}),
                ("ipv6_routes", {"interface": "tunnel0", "destination": "0" * 32, "destination_prefix": 0,
                "source": "0" * 32, "source_prefix": 0, "gateway": "0" * 32, "flags": 1})):
            value = self.dormant_network(); value[family] = [row]
            with self.subTest(family=family): self.assertFalse(isolation._network_denied(value, synthetic_guest_errno()))
        for field in ("links", "ipv4_routes", "ipv6_routes"):
            value = self.dormant_network(); del value[field]
            with self.subTest(missing=field): self.assertFalse(isolation._network_denied(value, synthetic_guest_errno()))
        value = self.dormant_network(); value.update(synthetic_guest_error(0))
        self.assertFalse(isolation._network_denied(value, synthetic_guest_errno()))
        self.engine.network_observation = value
        self.assertEqual(isolation.probe(self.config).status, "unavailable")
        self.assertEqual(self.engine.alive, set())

    def test_linux_guest_errors_remain_valid_when_host_errno_uses_darwin_numbers(self):
        with patch.object(errno, "ENETUNREACH", 51), patch.object(errno, "EHOSTUNREACH", 65):
            self.assertNotEqual(errno.ENETUNREACH, synthetic_guest_errno()["codes"]["ENETUNREACH"])
            for readonly in (False, True):
                config = replace(self.config, candidate_readonly=readonly)
                proof = isolation.probe(config)
                self.assertEqual(proof.status, "verified")
                observed = json.loads(proof.private_path.read_bytes())["observations"]["observed_probes"]
                self.assertEqual(observed["network"]["errno"], 101)
                self.assertEqual(observed["network"]["error"], "ENETUNREACH")
                self.assertEqual(observed["outside"]["error"], "ENOENT")
                self.assertEqual(observed["filesystem"]["runtime"]["error"], "OK")
                self.assertEqual(observed["filesystem"]["grader"]["error"], "EROFS")
                self.assertEqual(observed["filesystem"]["candidate"]["error"], "EROFS" if readonly else "OK")
                self.assertEqual(observed["filesystem"]["quiet_logs"]["error"], "ENOENT")

    def test_guest_errno_table_and_label_number_consistency_fail_closed(self):
        protocol = synthetic_guest_errno()
        allowed = {"ENETUNREACH", "EHOSTUNREACH", "EACCES", "EPERM"}
        for observation in ({"errno": 0, "error": "ENETUNREACH"}, {"errno": 101, "error": "OK"},
                            {"errno": 101, "error": "EACCES"}, {"errno": 101, "error": "UNKNOWN"},
                            {"errno": True, "error": "EPERM"}, {"errno": "101", "error": "ENETUNREACH"},
                            {"errno": 101}, {"error": "ENETUNREACH"}, {"errno": 30, "error": "EROFS"}):
            with self.subTest(observation=observation):
                self.assertFalse(isolation._guest_error_matches(observation, protocol, allowed))
        self.assertFalse(isolation._guest_error_matches({"errno": 101, "error": "OK"}, protocol, {"OK"}))
        for changed in ({}, {**protocol, "schema_version": True}, {**protocol, "platform": "darwin"},
                        {**protocol, "extra": "uncontrolled"}, {**protocol, "codes": {**protocol["codes"], "ENETUNREACH": 51}},
                        {**protocol, "codes": {**protocol["codes"], "EHOSTUNREACH": 101}},
                        {**protocol, "codes": {key: value for key, value in protocol["codes"].items() if key != "EROFS"}}):
            with self.subTest(protocol=changed):
                self.assertFalse(isolation._guest_error_matches(synthetic_guest_error(101), changed, allowed))
        network = self.dormant_network(); network.update(synthetic_guest_error(0))
        self.assertFalse(isolation._network_denied(network, protocol))

    def test_every_guest_boundary_rejects_missing_or_inconsistent_error_labels(self):
        config = replace(self.config, candidate_readonly=True)
        for boundary in ("outside", "network", "candidate", "runtime", "grader", "quiet_logs"):
            def corrupt(body, boundary=boundary):
                observation = body[boundary] if boundary in {"outside", "network"} else body["filesystem"][boundary]
                observation["error"] = "EROFS" if boundary == "runtime" else "OK"
            self.engine.mutate_probe = corrupt
            with self.subTest(boundary=boundary):
                self.assertEqual(isolation.probe(config).status, "unavailable")
                self.assertEqual(self.engine.alive, set())
        self.engine.mutate_probe = lambda body: body.pop("guest_errno")
        self.assertEqual(isolation.probe(config).status, "unavailable")

    def test_rust_jobs_default_preserves_and_probes_explicit_frozen_extra(self):
        config = replace(self.config, require_rust=True)
        self.assertEqual(isolation._environment(config)["CARGO_BUILD_JOBS"], "1")
        self.assertEqual(isolation._environment(config)["CARGO_PROFILE_DEV_DEBUG"], "0")
        self.assertEqual(isolation._environment(config)["CARGO_PROFILE_TEST_DEBUG"], "0")
        self.assertNotIn("CARGO_BUILD_JOBS", isolation._environment(self.config))
        self.assertNotIn("CARGO_PROFILE_DEV_DEBUG", isolation._environment(self.config))
        self.assertNotIn("CARGO_PROFILE_TEST_DEBUG", isolation._environment(self.config))
        one = isolation.probe(config)
        explicit = replace(config, environment=(
            ("CARGO_BUILD_JOBS", "2"), ("CARGO_PROFILE_DEV_DEBUG", "1"), ("CARGO_PROFILE_TEST_DEBUG", "2")))
        two = isolation.probe(explicit)
        self.assertEqual((one.status, two.status), ("verified", "verified"))
        self.assertNotEqual(one.config_digest, two.config_digest)
        self.assertEqual(json.loads(two.private_path.read_bytes())["observations"]["observed_probes"]["build_environment"],
                         {"CARGO_BUILD_JOBS": "2", "CARGO_PROFILE_DEV_DEBUG": "1", "CARGO_PROFILE_TEST_DEBUG": "2"})
        self.engine.build_jobs_override = "2"
        self.assertEqual(isolation.probe(config).status, "unavailable")
        self.assertEqual(self.engine.alive, set())
        self.engine.build_jobs_override = None
        self.engine.build_environment_override = {"CARGO_PROFILE_DEV_DEBUG": "2"}
        self.assertEqual(isolation.probe(config).status, "unavailable")
        self.engine.build_environment_override = None
        self.engine.mutate_probe = lambda body: body["build_environment"].pop("CARGO_PROFILE_TEST_DEBUG")
        self.assertEqual(isolation.probe(config).status, "unavailable")
        self.engine.mutate_probe = None
        self.assertEqual(self.engine.alive, set())

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

    def v2_plan(self):
        setup = isolation.ExecutionStep("baseline-install", ("/usr/bin/python3", "/grader/setup.py"),
            environment=(("PYTHONPATH", "/work"),), writes={"runtime"}, kind="setup")
        ready = isolation.ExecutionStep("baseline-ready", ("/usr/bin/python3", "/grader/ready.py"),
            environment=(("PYTHONPATH", "/work/installed"),), kind="prerequisite")
        install = isolation.ExecutionStep("patch-install", ("/usr/bin/python3", "/grader/install.py"),
            environment=(("PYTHONPATH", "/work"),), writes={"candidate"}, kind="setup")
        check = isolation.ExecutionStep("check", self.argv, environment=(("PYTHONPATH", "/work/installed"),))
        plan = isolation.FamilyPlanV2("synthetic-step-chain", (setup, ready), (install, check), self.plan.driver)
        config = replace(self.config, acceptance_argv=tuple(step.argv for step in (*plan.prerequisite_steps, *plan.acceptance_steps)))
        return config, plan

    def v2_config(self, config, step):
        return replace(config, environment=step.environment, working_directory=step.cwd,
                       candidate_readonly=config.candidate_readonly and step.protected_readonly)

    def v2_qualified(self, config, plan):
        first = self.v2_config(config, plan.prerequisite_steps[0])
        result = isolation.qualify_family(first, plan, isolation.probe(first))
        self.assertEqual(result.status, "qualified")
        return result

    def test_v2_ordered_state_changes_and_per_step_environment_are_freshly_bound(self):
        config, plan = self.v2_plan()
        def install(config, mode, step):
            if step["id"] == "baseline-install":
                (config.runtime_root / "dependency.txt").write_text("synthetic offline dependency")
            elif step["id"] == "patch-install":
                (config.candidate_root / "installed.py").write_text("# inert installed fixture")
        self.engine.mutate_callback = install
        prerequisite = self.v2_qualified(config, plan)
        first = self.v2_config(config, plan.acceptance_steps[0])
        result = isolation.execute_plan(first, isolation.probe(first), family=plan, prerequisite=prerequisite)
        self.assertEqual((result.status, result.classification), ("passed", "passed"))
        steps = [body for receipt, body in isolation._STEPS.values() if receipt.mode == "acceptance"]
        self.assertEqual([body["index"] for body in steps], [0, 1])
        self.assertNotEqual(steps[0]["before"]["candidate"], steps[0]["after"]["candidate"])
        self.assertEqual(steps[0]["after"], steps[1]["before"])
        self.assertGreaterEqual(steps[1]["post_probe_sequence"], steps[0]["post_probe_sequence"])
        contexts = [json.loads(args[-2]) for args, _ in self.engine.calls
                    if args[0] == "exec" and isolation._OUTCOME_CONTROLLER in args]
        self.assertEqual([context["step"]["id"] for context in contexts],
                         ["baseline-install", "baseline-ready", "patch-install", "check"])
        self.assertEqual([context["effective_environment"]["PYTHONPATH"] for context in contexts],
                         ["/work", "/work/installed", "/work", "/work/installed"])
        self.assertEqual(self.engine.alive, set())
        self.assertEqual(set(result.public()), {"status", "digest"})

    def test_v2_skip_reorder_replay_and_copied_step_receipts_cannot_advance(self):
        config, plan = self.v2_plan()
        prerequisite = self.v2_qualified(config, plan)
        first = self.v2_config(config, plan.acceptance_steps[0])
        second = self.v2_config(config, plan.acceptance_steps[1])
        self.assertEqual(isolation.execute_step(second, plan, 1, isolation.probe(second), prerequisite=prerequisite).status, "unavailable")
        step = isolation.execute_step(first, plan, 0, isolation.probe(first), prerequisite=prerequisite)
        self.assertEqual(step.status, "passed")
        self.assertEqual(isolation.execute_step(second, plan, 1, isolation.probe(second), prerequisite=prerequisite, previous=replace(step)).status, "unavailable")
        done = isolation.execute_step(second, plan, 1, isolation.probe(second), prerequisite=prerequisite, previous=step)
        self.assertEqual(done.status, "passed")
        self.assertEqual(isolation.execute_step(second, plan, 1, isolation.probe(second), prerequisite=prerequisite, previous=step).status, "unavailable")
        with self.assertRaises(isolation.IsolationError):
            isolation.execute(first, plan.acceptance_steps[0].argv, isolation.probe(first), family=plan, prerequisite=prerequisite)

    def test_v2_identical_content_in_another_mount_cannot_resume_step_chain(self):
        config, plan = self.v2_plan()
        prerequisite = self.v2_qualified(config, plan)
        first = self.v2_config(config, plan.acceptance_steps[0])
        step = isolation.execute_step(first, plan, 0, isolation.probe(first), prerequisite=prerequisite)
        other = config.owned_root / "other-candidate"
        other.mkdir()
        (other / "input.py").write_bytes((config.candidate_root / "input.py").read_bytes())
        second = replace(self.v2_config(config, plan.acceptance_steps[1]), candidate_root=other)
        self.assertEqual(isolation.execute_step(second, plan, 1, isolation.probe(second),
                         prerequisite=prerequisite, previous=step).status, "unavailable")

    def test_v2_changed_plan_cwd_environment_runtime_or_source_invalidates_lineage(self):
        config, plan = self.v2_plan()
        prerequisite = self.v2_qualified(config, plan)
        first = self.v2_config(config, plan.acceptance_steps[0])
        receipt = isolation.probe(first)
        changed = replace(plan, acceptance_steps=(replace(plan.acceptance_steps[0], environment=(("PYTHONPATH", "/different"),)), plan.acceptance_steps[1]))
        other = self.v2_config(config, changed.acceptance_steps[0])
        self.assertEqual(isolation.execute_step(other, changed, 0, isolation.probe(other), prerequisite=prerequisite).status, "unavailable")
        with self.assertRaises(isolation.IsolationError):
            isolation.execute_step(replace(first, working_directory="/"), plan, 0, receipt, prerequisite=prerequisite)
        (config.runtime_root / "unreviewed-dependency").write_text("changed")
        self.assertEqual(isolation.execute_step(first, plan, 0, isolation.probe(first), prerequisite=prerequisite).status, "unavailable")
        (config.runtime_root / "unreviewed-dependency").unlink()
        step = isolation.execute_step(first, plan, 0, isolation.probe(first), prerequisite=prerequisite)
        (config.candidate_root / "input.py").write_text("# outside step mutation")
        second = self.v2_config(config, plan.acceptance_steps[1])
        self.assertEqual(isolation.execute_step(second, plan, 1, isolation.probe(second), prerequisite=prerequisite, previous=step).status, "unavailable")
        self.assertEqual(isolation.execute_step(first, plan, 0, receipt, prerequisite=prerequisite).status, "unavailable")

    def test_v2_undeclared_changes_zero_checks_wrong_protocol_and_forged_output_are_unavailable(self):
        config, plan = self.v2_plan()
        prerequisite = self.v2_qualified(config, plan)
        first = self.v2_config(config, plan.acceptance_steps[0])
        for field, value in (("completed_checks", 0), ("outcome_protocol", 1), ("bad_hmac", True)):
            with self.subTest(field=field):
                original = getattr(self.engine, field)
                setattr(self.engine, field, value)
                self.assertEqual(isolation.execute_plan(first, isolation.probe(first), family=plan, prerequisite=prerequisite).status, "unavailable")
                setattr(self.engine, field, original)
        def undeclared(config, mode, step):
            (config.runtime_root / "undeclared").write_text("must invalidate candidate-only setup")
        self.engine.mutate_callback = undeclared
        self.assertEqual(isolation.execute_plan(first, isolation.probe(first), family=plan, prerequisite=prerequisite).status, "unavailable")
        self.assertEqual(self.engine.alive, set())

    def test_v2_setup_only_cannot_pass_and_incomplete_prerequisites_cannot_seal(self):
        config, plan = self.v2_plan()
        setup_only = replace(plan, acceptance_steps=(plan.acceptance_steps[0],))
        with self.assertRaises(isolation.IsolationError):
            isolation._plan_v2_data(setup_only)
        with self.assertRaises(isolation.IsolationError):
            isolation.seal_prerequisites(plan, ())
        first = self.v2_config(config, plan.prerequisite_steps[0])
        step = isolation.qualify_step(first, plan, 0, isolation.probe(first))
        with self.assertRaises(isolation.IsolationError):
            isolation.seal_prerequisites(plan, (step,))

    def test_v2_completed_assertion_failure_is_failed_only_with_cleanup_and_post_probe(self):
        config, plan = self.v2_plan()
        prerequisite = self.v2_qualified(config, plan)
        first = self.v2_config(config, plan.acceptance_steps[0])
        self.engine.family_outcome = "assertion-failed"
        result = isolation.execute_plan(first, isolation.probe(first), family=plan, prerequisite=prerequisite)
        self.assertEqual((result.status, result.classification), ("failed", "assertion-failed"))
        self.engine.cleanup_fails = True
        self.assertEqual(isolation.execute_plan(first, isolation.probe(first), family=plan, prerequisite=prerequisite).status, "unavailable")

    def test_v2_fresh_receipt_identity_does_not_change_semantic_family_binding(self):
        config, plan = self.v2_plan()
        first = self.v2_qualified(config, plan)
        second = self.v2_qualified(config, plan)
        self.assertNotEqual(first.digest, second.digest)
        self.assertEqual(first.binding_digest, second.binding_digest)

    def test_v2_post_step_probe_failure_cannot_publish_completed_setup(self):
        config, plan = self.v2_plan()
        prerequisite = self.v2_qualified(config, plan)
        first = self.v2_config(config, plan.acceptance_steps[0])
        receipt = isolation.probe(first)
        def change_isolation(config, mode, step):
            self.engine.network = "bridge"
        self.engine.mutate_callback = change_isolation
        result = isolation.execute_step(first, plan, 0, receipt, prerequisite=prerequisite)
        self.assertEqual((result.status, result.classification), ("unavailable", "unavailable"))
        self.assertEqual(self.engine.alive, set())

    def test_exact_tracked_source_permits_do_not_grant_private_ancestor_siblings(self):
        path = self.config.candidate_root / "vendor/fixture/.cargo/config.toml"
        path.parent.mkdir(parents=True)
        raw = b"# synthetic tracked offline config\n"
        path.write_bytes(raw)
        member = isolation.TrustedSourceMember("vendor/fixture/.cargo/config.toml", hashlib.sha256(raw).hexdigest(),
                    hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest())
        config = replace(self.config, source_commit="c" * 40, source_archive_sha256="d" * 64, trusted_source_members=(member,))
        self.assertEqual(isolation.probe(config).status, "verified")
        sibling = path.parent / "unpermitted.toml"
        sibling.write_text("# sibling not in archive permit")
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(config)
        sibling.unlink()
        with self.assertRaises(isolation.IsolationError):
            isolation.probe(replace(config, trusted_source_members=(replace(member, git_blob="0" * 40),)))
        for forbidden in (".git/config", ".aws/config", ".cargo/credentials", ".env", "auth.json"):
            with self.subTest(forbidden=forbidden), self.assertRaises(isolation.IsolationError):
                isolation.probe(replace(config, trusted_source_members=(replace(member, path=forbidden),)))

    def protected_plan(self):
        config, plan = self.v2_plan()
        for name in ("worker-candidate", "worker-export", "worker-runtime"):
            (self.root / name).mkdir(exist_ok=True)
        raw = b"# synthetic worker; never real candidate execution\n"
        (self.root / "worker-export/worker.py").write_bytes(raw)
        contract = b'{"protocol":2,"synthetic":true}\n'
        (self.root / "worker-export/rpc-contract.json").write_bytes(contract)
        (self.root / "worker-candidate/input.py").write_text("# synthetic candidate role only")
        argv = ("/usr/bin/python3", "/grader/worker.py")
        worker_config = replace(self.config, candidate_root=self.root / "worker-candidate",
            grader_root=self.root / "worker-export", runtime_root=self.root / "worker-runtime",
            acceptance_argv=(argv,), uid=1000, gid=1000, candidate_readonly=True)
        worker = isolation.ProtectedWorkerPlan(worker_config,
            isolation.TrustedSourceFile("worker.py", hashlib.sha256(raw).hexdigest()), argv,
            ("health", "call", "cache_root"), hashlib.sha256(contract).hexdigest(), "e" * 64,
            exception_types=("ReceiptError", "ValueError"))
        return replace(config, candidate_readonly=True), replace(plan, worker=worker)

    def protected_broker(self, config, name, argv, plan, mode, context, worker_config, worker_name):
        # Synthetic authenticated-controller result seam, never runtime proof.
        step = context["step"]
        classification = ("setup-passed" if step["kind"] == "setup" else
                          "prerequisite-passed" if mode == "prerequisite" else self.engine.family_outcome)
        packet = {"driver_started": True, "driver_completed": True, "hardened": True,
            "resources_verified": True, "resource_failure": False,
            "result": {"protocol": 2, "outcome": classification, "completed": True,
                "evidence": {"kind": step["kind"], "identity": "synthetic protected assertion",
                    "sha256": "f" * 64, "completed_checks": 1,
                    "failed_checks": int(classification.endswith("-failed")), "skipped_checks": 0}},
            "broker_proof": {"requests": 1, "bytes": 123, "topology": "separate-container-stdio-v1"}}
        return isolation._Command(0, b"synthetic raw private output", b""), packet

    def test_protected_roles_bind_both_sources_runtimes_and_cleanup_before_scoring(self):
        config, plan = self.protected_plan()
        with patch.object(isolation, "_duplex_broker", side_effect=self.protected_broker):
            prerequisite = self.v2_qualified(config, plan)
            first = self.v2_config(config, plan.acceptance_steps[0])
            result = isolation.execute_plan(first, isolation.probe(first), family=plan, prerequisite=prerequisite)
        self.assertEqual(result.status, "passed")
        steps = [body for receipt, body in isolation._STEPS.values() if receipt.mode == "acceptance"]
        self.assertTrue(all("worker_candidate" in step["before"] and "worker_runtime" in step["after"] for step in steps))
        self.assertTrue(all(step["structured_outcome"]["worker_proof"]["cleanup"]["list"]["stdout"] == "" for step in steps))
        self.assertNotEqual(steps[0]["work_identity"]["mounts"], steps[0]["work_identity"]["worker"]["mounts"])
        self.assertEqual(self.engine.alive, set())
        self.assertEqual(set(result.public()), {"status", "digest"})

    def test_protected_non_idempotent_install_uses_initial_source_and_final_runtime(self):
        config, plan = self.protected_plan()
        baseline_install = replace(plan.prerequisite_steps[0], writes={"candidate", "runtime"}, protected_readonly=False)
        patch_install = replace(plan.acceptance_steps[0], protected_readonly=False)
        plan = replace(plan, prerequisite_steps=(baseline_install, plan.prerequisite_steps[1]),
                       acceptance_steps=(patch_install, plan.acceptance_steps[1]))
        marker = config.candidate_root / "synthetic-installed-assertion.txt"
        def install(*args):
            current, _, _, _, _, context, _, _ = args
            if context["step"]["kind"] == "setup":
                self.assertFalse(current.candidate_readonly)
                self.assertFalse(marker.exists(), "installer must not be repeated on its output")
                marker.write_text("synthetic protected-only assertion fixture")
            else:
                self.assertTrue(current.candidate_readonly)
            return self.protected_broker(*args)
        with patch.object(isolation, "_duplex_broker", side_effect=install):
            prerequisite = self.v2_qualified(config, plan)
            marker.unlink()  # Trusted stager restores the pristine protected view.
            first = self.v2_config(config, plan.acceptance_steps[0])
            result = isolation.execute_plan(first, isolation.probe(first), family=plan, prerequisite=prerequisite)
        self.assertEqual(result.status, "passed")
        self.assertTrue(marker.exists())
        self.assertFalse((plan.worker.config.candidate_root / marker.name).exists())
        accepted = [body for receipt, body in isolation._STEPS.values() if receipt.mode == "acceptance"]
        self.assertEqual([body["step"]["protected_readonly"] for body in accepted], [False, True])
        self.assertEqual(accepted[0]["after"], accepted[1]["before"])

    def test_protected_worker_cannot_mount_grader_share_uid_or_extra_exports(self):
        config, plan = self.protected_plan()
        for worker_config in (replace(plan.worker.config, uid=config.uid),
                              replace(plan.worker.config, grader_root=config.grader_root),
                              replace(plan.worker.config, candidate_root=config.candidate_root)):
            with self.subTest(worker_config=worker_config), self.assertRaises(isolation.IsolationError):
                isolation._worker_configuration(config, replace(plan, worker=replace(plan.worker, config=worker_config)))
        with self.assertRaises(isolation.IsolationError):
            isolation._worker_configuration(replace(config, candidate_readonly=False), plan)
        (plan.worker.config.grader_root / "forbidden-grader.py").write_text("# never visible to candidate")
        with self.assertRaises(isolation.IsolationError):
            isolation._worker_configuration(config, plan)

    def test_worker_never_inherits_protected_quiet_wrapper_or_runtime_logs(self):
        protected = self.quiet_configuration()
        config, plan = self.protected_plan()
        config = replace(config, require_rust=True, trusted_wrappers=protected.trusted_wrappers,
                         source_commit=protected.source_commit, source_archive_sha256=protected.source_archive_sha256)
        _, payload = isolation._prepare(config)
        current, worker_payload = isolation._worker_configuration(config, plan)
        self.assertTrue(payload["quiet_log_mount"])
        self.assertFalse(worker_payload["quiet_log_mount"])
        self.assertNotIn("/work/target/quiet", [target for _, target, _ in isolation._mounts(current)])
        for left, _, _ in isolation._mounts(config):
            for right, _, _ in isolation._mounts(current):
                self.assertFalse(left.is_relative_to(right) or right.is_relative_to(left))
        rust_plan = replace(plan, worker=replace(plan.worker, config=replace(plan.worker.config, require_rust=True)))
        rust_worker, _ = isolation._worker_configuration(config, rust_plan)
        self.assertEqual(isolation._environment(rust_worker)["CARGO_BUILD_JOBS"], "1")
        explicit_worker, _ = isolation._worker_configuration(replace(config, environment=(("CARGO_BUILD_JOBS", "3"),)), rust_plan)
        self.assertEqual(isolation._environment(explicit_worker)["CARGO_BUILD_JOBS"], "3")
        copied = replace(plan, worker=replace(plan.worker, config=replace(plan.worker.config,
                                        trusted_wrappers=config.trusted_wrappers)))
        with self.assertRaises(isolation.IsolationError): isolation._worker_configuration(config, copied)

    def test_protected_rpc_rejects_worker_outcomes_unordered_frames_and_code_objects(self):
        config, plan = self.protected_plan()
        worker = plan.worker
        forged = {"kind": "outcome", "packet": {"status": "passed"}}
        with self.assertRaises(isolation.IsolationError):
            isolation._rpc_reply(forged, worker, 1)
        valid = {"protocol": 2, "id": 1, "result": {"value": {"tag": "tuple", "items": [1, "inert"]}}}
        self.assertEqual(isolation._rpc_reply(valid, worker, 1), valid)
        for frame in ({**valid, "id": 2}, {**valid, "secret": "not allowed"},
                      {**valid, "result": {"exception": {"type": "SystemExit", "message": "forged"}}}):
            with self.subTest(frame=frame), self.assertRaises(isolation.IsolationError):
                isolation._rpc_reply(frame, worker, 1)
        with self.assertRaises(isolation.IsolationError):
            isolation._rpc_request({"kind": "rpc", "request": {"protocol": 2, "id": 1,
                "method": "__import__", "args": [], "kwargs": {}}}, worker, 1)
        for value in (object(), float("nan"), 2**100):
            with self.subTest(value=type(value).__name__), self.assertRaises(isolation.IsolationError):
                isolation._rpc_inert(value)
        for raw in ('{"id":1,"id":2}', '{"value":NaN}'):
            with self.assertRaises(isolation.IsolationError):
                isolation._wire_json(raw)

    def test_protected_worker_oom_or_missing_completion_remains_unavailable(self):
        config, plan = self.protected_plan()
        with patch.object(isolation, "_duplex_broker", side_effect=self.protected_broker):
            prerequisite = self.v2_qualified(config, plan)
            first = self.v2_config(config, plan.acceptance_steps[0])
            receipt = isolation.probe(first)
            self.engine.oom_killed = True
            result = isolation.execute_step(first, plan, 0, receipt, prerequisite=prerequisite)
            self.assertEqual(result.status, "unavailable")
            self.engine.oom_killed = False
        with patch.object(isolation, "_duplex_broker", return_value=(isolation._Command(1, b"forged stdout passed", b""), None)):
            result = isolation.execute_step(first, plan, 0, isolation.probe(first), prerequisite=prerequisite)
        self.assertEqual(result.status, "unavailable")
        self.assertEqual(self.engine.alive, set())


@unittest.skipUnless(os.environ.get("PLURX_KNOWLEDGE_OCI_IMAGE") and os.environ.get("PLURX_KNOWLEDGE_OCI_ENGINE"),
                     "requires explicit reviewed immutable OCI image and engine; never synthetic proof")
class ReviewedControllerOciTests(unittest.TestCase):
    """Opt-in real controller argv/provenance tests; execute only after review.

    Uses wholly synthetic source/grader fixtures and the public OCI boundary.
    No account, model, private plan, Gold, or actual study acceptance is loaded.
    """
    def setUp(self):
        base = Path.cwd() / "target/agent-fixtures/.hermes/hdr-collaboration/workspaces/controller-oci"
        base.mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(prefix="roles-", dir=base)).resolve()
        self.fixture_succeeded = False
        self.observation_sequence = 0
        self.addCleanup(self.finish_fixture)
        for name in ("protected-source", "protected-export", "protected-runtime",
                     "worker-source", "worker-export", "worker-runtime"):
            (self.root / name).mkdir(mode=0o755)
        for role in ("protected", "worker"):
            source = self.root / (role + "-source")
            (source / "validation").mkdir()
            (source / "validation/__init__.py").write_text("")
            (source / "validation/synthetic_role.py").write_text("VALUE = " + repr(role) + "\n")
            (source / ("only_" + role + ".py")).write_text("VALUE = " + repr(role) + "\n")
            (source / "sitecustomize.py").write_text("raise RuntimeError('ambient startup must stay disabled')\n")
        worker_source = r"""
import importlib.util,sys
from pathlib import Path
import validation.synthetic_role as fixture
def dispatch(method,args,kwargs,context):
    assert method=='health' and args==[] and kwargs=={}
    assert sys.flags.isolated==1 and sys.flags.no_site==1
    assert fixture.__file__=='/work/validation/synthetic_role.py'
    assert importlib.util.find_spec('only_protected') is None
    return {'value':{'role':fixture.VALUE,'quiet_files':[p.name for p in Path('/work/target/quiet').glob('*')]}}
"""
        protected_source = r"""
import hashlib,importlib.util,subprocess,sys
from pathlib import Path
import validation.synthetic_role as fixture
def run(mode,argv,context):
    assert sys.flags.isolated==1 and sys.flags.no_site==1
    assert fixture.__file__=='/work/validation/synthetic_role.py' and fixture.VALUE=='protected'
    assert importlib.util.find_spec('only_worker') is None
    response=rpc('health',[],{})
    assert response=={'value':{'role':'worker','quiet_files':[]}}
    if argv[0]=='/work/scripts/quiet':
        result=subprocess.run(argv,cwd=context['step']['cwd'],env=context['effective_environment'],
            stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=30)
        assert result.returncode==0
        logs=sorted(Path('/work/target/quiet').glob('*.log'))
        assert logs and any('cargo ' in p.read_text() for p in logs)
        response=rpc('health',[],{})
        assert response=={'value':{'role':'worker','quiet_files':[]}}
    try:
        Path('/work/forbidden-source-write').write_text('must fail')
    except OSError:
        pass
    else:
        raise AssertionError('protected source writable')
    kind=context['step']['kind']
    return {'protocol':2,'outcome':'prerequisite-passed' if mode=='prerequisite' else 'passed','completed':True,
        'evidence':{'kind':kind,'identity':'synthetic actual isolated role imports and topology',
        'sha256':hashlib.sha256(Path(fixture.__file__).read_bytes()).hexdigest(),
        'completed_checks':3,'failed_checks':0,'skipped_checks':0}}
"""
        (self.root / "worker-export/worker.py").write_text(worker_source)
        (self.root / "worker-export/rpc-contract.json").write_text('{"protocol":2,"operations":["health"]}\n')
        (self.root / "protected-export/protected.py").write_text(protected_source)
        self.python = os.environ.get("PLURX_KNOWLEDGE_OCI_PYTHON", "/usr/bin/python3")
        self.config = isolation.IsolationConfig(engine=Path(os.environ["PLURX_KNOWLEDGE_OCI_ENGINE"]),
            endpoint=os.environ.get("PLURX_KNOWLEDGE_OCI_ENDPOINT", "unix:///var/run/docker.sock"),
            image=os.environ["PLURX_KNOWLEDGE_OCI_IMAGE"], owned_root=self.root,
            candidate_root=self.root / "protected-source", grader_root=self.root / "protected-export",
            runtime_root=self.root / "protected-runtime", candidate_readonly=True,
            python_executable=self.python,
            rustc_executable=os.environ.get("PLURX_KNOWLEDGE_OCI_RUSTC", "/usr/bin/rustc"),
            cargo_executable=os.environ.get("PLURX_KNOWLEDGE_OCI_CARGO", "/usr/bin/cargo"),
            acceptance_argv=((self.python, "/grader/protected.py", "/work"),), uid=65534, gid=65534)
        self.addCleanup(isolation._ISSUED.clear)
        self.addCleanup(isolation._QUALIFIED.clear)
        self.addCleanup(isolation._STEPS.clear)
        self.addCleanup(isolation._CONSUMED_STEPS.clear)
        self.addCleanup(isolation._ACTIVE_ROOTS.clear)
        self.addCleanup(isolation._RESERVED_STEPS.clear)

    def record_observation(self, event):
        path = self.root / ("observation-" + str(self.observation_sequence).zfill(6) + ".json")
        isolation._write_private(path, event)
        self.observation_sequence += 1

    def finish_fixture(self):
        self.record_observation({"kind": "fixture-finished", "passed": self.fixture_succeeded,
            "owned_containers": sorted(isolation._OWNED_CONTAINERS), "retained_stage": str(self.root)})
        print("retained synthetic OCI evidence: " + str(self.root), file=sys.stderr)

    def assert_receipt(self, receipt, expected):
        self.record_observation({"kind": "receipt", "status": receipt.status, "expected": expected,
                                 "digest": receipt.digest, "private_path": str(receipt.private_path)})
        self.assertEqual(receipt.status, expected,
            "isolation refused; retained receipt " + str(receipt.private_path) + "; command/refusal evidence " + str(self.root))

    def run_roles(self, *, quiet=False):
        config = self.config
        if quiet:
            wrapper = config.candidate_root / "scripts/quiet"
            wrapper.parent.mkdir()
            wrapper.write_bytes((Path(__file__).resolve().parents[2] / "scripts/quiet").read_bytes())
            wrapper.chmod(0o755)
            argv = ("/work/scripts/quiet", config.cargo_executable, "--version")
            config = replace(config, require_rust=True, acceptance_argv=(argv,),
                trusted_wrappers=(isolation.TrustedSourceFile("scripts/quiet", hashlib.sha256(wrapper.read_bytes()).hexdigest()),),
                source_commit="a" * 40, source_archive_sha256="c" * 64)
        argv = config.acceptance_argv[0]
        worker_argv = (self.python, "/grader/worker.py")
        worker_config = replace(config, candidate_root=self.root / "worker-source",
            grader_root=self.root / "worker-export", runtime_root=self.root / "worker-runtime",
            acceptance_argv=(worker_argv,), trusted_wrappers=(), uid=1000, gid=1000)
        worker = isolation.ProtectedWorkerPlan(worker_config,
            isolation.TrustedSourceFile("worker.py", hashlib.sha256((worker_config.grader_root / "worker.py").read_bytes()).hexdigest()),
            worker_argv, ("health",), hashlib.sha256((worker_config.grader_root / "rpc-contract.json").read_bytes()).hexdigest(),
            hashlib.sha256((config.grader_root / "protected.py").read_bytes()).hexdigest())
        prerequisite_step = isolation.ExecutionStep("imports", argv, writes={"runtime"}, kind="prerequisite")
        assertion_step = isolation.ExecutionStep("checks", argv, writes={"runtime"})
        plan = isolation.FamilyPlanV2("synthetic-real-role-provenance", (prerequisite_step,), (assertion_step,),
            isolation.TrustedSourceFile("protected.py", worker.proxy_sha256), worker=worker)
        calls = []
        actual_popen = isolation.subprocess.Popen
        actual_command = isolation._command
        actual_probe = isolation._configuration_probe
        def observed_popen(arguments, **kwargs):
            calls.append(arguments)
            return actual_popen(arguments, **kwargs)
        def observed_command(current, arguments, **kwargs):
            result = actual_command(current, arguments, **kwargs)
            self.record_observation({"kind": "command", "argv": arguments,
                "returncode": result.returncode, "timed_out": result.timed_out, "output_limited": result.output_limited,
                "stdout_base64": base64.b64encode(result.stdout).decode(),
                "stderr_base64": base64.b64encode(result.stderr).decode()})
            return result
        def observed_probe(*args):
            try:
                return actual_probe(*args)
            except BaseException as error:
                self.record_observation({"kind": "probe-refusal", "type": type(error).__name__, "reason": str(error)})
                raise
        with patch.object(isolation.subprocess, "Popen", side_effect=observed_popen), \
                patch.object(isolation, "_command", side_effect=observed_command), \
                patch.object(isolation, "_configuration_probe", side_effect=observed_probe):
            proof = isolation.probe(config)
            self.assert_receipt(proof, "verified")
            qualification = isolation.qualify_family(config, plan, proof)
            self.assert_receipt(qualification, "qualified")
            acceptance_proof = isolation.probe(config)
            self.assert_receipt(acceptance_proof, "verified")
            execution = isolation.execute_plan(config, acceptance_proof, family=plan, prerequisite=qualification)
            self.assert_receipt(execution, "passed")
            self.assertEqual(execution.classification, "passed")
        for code in (isolation._PROTECTED_CONTROLLER, isolation._WORKER_CONTROLLER):
            launches = [args for args in calls if code in args]
            self.assertTrue(launches, "must traverse actual isolated controller launch")
            for args in launches:
                index = args.index(code)
                self.assertEqual(args[index - 3:index], ["-I", "-S", "-c"])
        if quiet:
            logs = config.runtime_root / "quiet-logs"
            self.assertTrue(list(logs.glob("*.log")))
            self.assertEqual(list((config.candidate_root / "target/quiet").iterdir()), [])
            self.assertFalse((worker_config.runtime_root / "quiet-logs").exists())
        self.assertFalse((config.candidate_root / "forbidden-source-write").exists())
        self.assertEqual(isolation._OWNED_CONTAINERS, set())
        self.fixture_succeeded = True

    def test_actual_isolated_controller_argv_imports_only_its_verified_role_source(self):
        self.run_roles()

    def test_unchanged_quiet_wrapper_writes_only_protected_runtime_with_source_readonly(self):
        self.run_roles(quiet=True)


if __name__ == "__main__":
    unittest.main()
