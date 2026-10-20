"""Host-only Compose companion identity, wire and graceful lifecycle contracts.

These fixtures never contact Docker, change host services or run the controller's
worker as a subprocess. The fake engine returns only the fields the UI needs.
"""
from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import hashlib
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("plurx_compose_control", str(ROOT / "scripts/plurx-compose-control"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
control = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(control)
SERVER = "a" * 64
DISCOVERY = "b" * 64
IMAGE = "sha256:" + "c" * 64
INSTALLATION = "c7de5385-b888-4bf3-bd54-44d885784332"
TRANSACTION = "09c1e668-d964-4595-bb25-fb1bf1960f36"


class ComposeControlTests(unittest.TestCase):
    @contextlib.contextmanager
    def backend(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            root.chmod(0o700)
            backend = object.__new__(control.Backend)
            backend.path = root / "installation.json"
            backend.state = root / "installation.json.state"
            backend.state.mkdir(mode=0o700)
            backend.record = {"schema_version": 1, "owner_uid": os.getuid(), "installation_id": INSTALLATION,
                              "docker": "/usr/bin/docker", "endpoint": "unix:///run/docker.sock", "engine_id": "engine-one",
                              "project": "plurx", "server_service": "plurxd", "discovery_service": "plurx-discovery",
                              "server_id": SERVER, "discovery_id": DISCOVERY, "server_image": IMAGE, "discovery_image": IMAGE,
                              "browser_origin": "http://127.0.0.1:32400", "version": "1.2.3", "tray": "/usr/bin/plurx-tray",
                              "checkout": "/checkout", "update_mode": "source", "image_reference": None}
            control.save(backend.path, backend.record)
            states = {}
            for kind, identity in (("server", SERVER), ("discovery", DISCOVERY)):
                states[kind] = {"id": identity, "image": IMAGE, "project": "plurx", "service": backend.record[kind + "_service"],
                                "running": True, "status": "running", "health": "healthy", "restart": "unless-stopped", "stop_timeout": 3900}
            deploy = root / "checkout" / "deploy"
            deploy.mkdir(parents=True)
            compose = "name: plurx\nservices:\n  plurxd:\n    restart: ${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}\n  plurx-discovery:\n    restart: ${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}\n"
            (deploy / "docker-compose.yml").write_text(compose)
            (deploy / ".env").write_bytes(b"# operator settings\nSECRET=keep-private\nPLURX_DATA=/media/state\n")
            backend.record.update(checkout=str(root / "checkout"),
                                  environment_sha256=hashlib.sha256((deploy / ".env").read_bytes()).hexdigest(),
                                  compose_sha256=hashlib.sha256(compose.encode()).hexdigest())
            control.save(backend.path, backend.record)
            commands = []
            def docker(*args, **kwargs):
                commands.append(args)
                if args[:2] == ("info", "--format"):
                    return json.dumps("engine-one")
                if args[0] == "inspect":
                    item = states["server" if args[-1] == SERVER else "discovery"]
                    return json.dumps(item)
                if args[0] == "logs":
                    return "first log\nsecond log\n"
                kind = "server" if args[-1] == SERVER else "discovery"
                if args[0] == "kill":
                    states[kind].update(running=False, status="exited")
                elif args[0] == "start":
                    states[kind].update(running=True, status="running", health="healthy")
                elif args[0] == "update":
                    states[kind]["restart"] = args[2]
                return ""
            with patch.object(backend, "docker", side_effect=docker), patch.object(backend, "entry", return_value=root / "autostart" / control.ENTRY):
                yield backend, states, commands, root

    def journal(self, backend, operation="stop", state="shutdown_committed"):
        value = {"installation_id": INSTALLATION, "transaction_id": TRANSACTION,
                 "operation": operation, "state": state, "detail": "fixture", "version": None}
        backend.store_journal(value)
        return value

    def test_inspection_template_never_requests_environment_or_full_config(self):
        self.assertNotIn(".Config.Env", control.INSPECT)
        self.assertNotIn("json .Config}}", control.INSPECT)
        self.assertIn("com.docker.compose.project", control.INSPECT)
        self.assertIn("com.docker.compose.service", control.INSPECT)

    def test_status_reports_selected_engine_and_healthy_containers(self):
        with self.backend() as (backend, _, commands, _):
            result = backend.status()
            self.assertEqual(result["kind"], "status")
            self.assertEqual(result["service_state"], "running")
            self.assertEqual(result["browser_origin"], "http://127.0.0.1:32400")
            self.assertEqual(result["server_startup"]["state"], "enabled")
            self.assertIsNone(result["transaction_id"])
            self.assertEqual(commands[0], ("info", "--format", "{{json .ID}}"))

    def test_unavailable_engine_is_unknown_and_never_starts_another(self):
        with self.backend() as (backend, _, _, _), patch.object(backend, "engine", side_effect=control.ControlError("unavailable", "offline")):
            result = backend.status()
            self.assertEqual(result["service_state"], "unknown")
            self.assertEqual(result["server_startup"]["state"], "unknown")

    def test_changed_compose_label_refuses_before_mutation(self):
        with self.backend() as (backend, states, commands, _):
            states["server"]["project"] = "another-stack"
            with self.assertRaises(control.ControlError) as error:
                backend.lifecycle("stop", True)
            self.assertEqual(error.exception.code, "identity_mismatch")
            self.assertFalse(any(item[0] in ("kill", "start", "update") for item in commands))

    def test_changed_image_refuses_before_mutation(self):
        with self.backend() as (backend, states, _, _):
            states["server"]["image"] = "sha256:" + "d" * 64
            with self.assertRaises(control.ControlError) as error:
                backend.logs(10)
            self.assertEqual(error.exception.code, "identity_mismatch")

    def test_engine_identity_mismatch_is_not_treated_as_stopped(self):
        with self.backend() as (backend, _, _, _), patch.object(backend, "docker", return_value=json.dumps("engine-two")):
            with self.assertRaises(control.ControlError) as error:
                backend.status()
            self.assertEqual(error.exception.code, "identity_mismatch")

    def test_inspection_states_preserve_unknown_and_unhealthy(self):
        self.assertEqual(control.Backend.service_state({"running": True, "health": None}), "unknown")
        self.assertEqual(control.Backend.service_state({"running": True, "health": "starting"}), "starting")
        self.assertEqual(control.Backend.service_state({"running": True, "health": "unhealthy"}), "unhealthy")
        self.assertEqual(control.Backend.service_state({"running": False, "status": "exited"}), "stopped")
        self.assertEqual(control.Backend.service_state({"running": False, "status": "dead"}), "unknown")

    def test_stop_needs_confirmation(self):
        with self.backend() as (backend, _, commands, _):
            with self.assertRaises(control.ControlError) as error:
                backend.lifecycle("stop", False)
            self.assertEqual(error.exception.code, "invalid_request")
            self.assertFalse(commands)

    def test_duplicate_committed_operation_is_refused(self):
        with self.backend() as (backend, _, _, _), patch.object(control.subprocess, "Popen") as spawn:
            self.journal(backend)
            with self.assertRaises(control.ControlError) as error:
                backend.lifecycle("restart", True)
            self.assertEqual(error.exception.code, "busy")
            spawn.assert_not_called()

    def test_worker_dispatch_happens_after_operation_lock_is_released(self):
        with self.backend() as (backend, _, _, _):
            def spawn(*args, **kwargs):
                with backend.locked():
                    self.assertEqual(backend.journal()["operation"], "stop")
            with patch.object(control.subprocess, "Popen", side_effect=spawn):
                result = backend.lifecycle("stop", True)
            self.assertEqual(result["operation"], "stop")
            self.assertEqual(backend.journal()["state"], "shutdown_committed")

    def test_stop_signals_exact_containers_with_term_and_never_forces_kill(self):
        with self.backend() as (backend, states, commands, _):
            self.journal(backend)
            backend.worker(TRANSACTION)
            signals = [item for item in commands if item[0] == "kill"]
            self.assertEqual(signals, [("kill", "--signal", "TERM", DISCOVERY), ("kill", "--signal", "TERM", SERVER)])
            self.assertFalse(any(item[0] == "stop" or "KILL" in item for item in commands))
            self.assertFalse(states["server"]["running"])
            self.assertEqual(states["server"]["restart"], "unless-stopped")
            self.assertEqual(backend.journal()["state"], "complete")

    def test_restart_health_checks_server_before_starting_discovery(self):
        with self.backend() as (backend, states, commands, _):
            self.journal(backend, "restart")
            backend.worker(TRANSACTION)
            starts = [item for item in commands if item[0] == "start"]
            self.assertEqual(starts, [("start", SERVER), ("start", DISCOVERY)])
            self.assertTrue(states["server"]["running"])
            self.assertTrue(states["discovery"]["running"])
            self.assertEqual(backend.journal()["state"], "complete")

    def test_wrong_worker_transaction_has_no_container_mutation(self):
        with self.backend() as (backend, _, commands, _):
            self.journal(backend)
            with self.assertRaises(control.ControlError):
                backend.worker("a0cb7453-ad5e-45fc-a5b1-7149e77c81c9")
            self.assertFalse(commands)

    def test_committed_lifecycle_cannot_be_cancelled(self):
        with self.backend() as (backend, _, _, _):
            self.journal(backend)
            with self.assertRaises(control.ControlError) as error:
                backend.updates("cancel", TRANSACTION)
            self.assertEqual(error.exception.code, "unsafe_shutdown")

    def test_logs_are_bounded_and_target_exact_server_id(self):
        with self.backend() as (backend, _, commands, _):
            result = backend.logs(12)
            self.assertEqual(result["lines"], ["first log", "second log"])
            self.assertIn(("logs", "--tail", "12", SERVER), commands)

    def test_explicit_startup_on_uses_always_and_explains_engine_boot_requirement(self):
        with self.backend() as (backend, states, _, _):
            result = backend.set_startup("server", "on")
            self.assertEqual(states["server"]["restart"], "always")
            self.assertEqual(states["discovery"]["restart"], "always")
            self.assertEqual(result["server"]["state"], "enabled")
            self.assertIn("policy: always", result["server"]["detail"])
            self.assertIn("Docker itself must start at boot", result["server"]["detail"])
            self.assertEqual(backend.set_startup("server", "off")["server"]["state"], "disabled")

    def test_existing_unless_stopped_policy_is_preserved_until_explicit_change(self):
        with self.backend() as (backend, states, commands, _):
            result = backend.startup()
            self.assertEqual(states["server"]["restart"], "unless-stopped")
            self.assertIn("policy: unless-stopped", result["server"]["detail"])
            self.assertIn("remain stopped across Docker restarts", result["server"]["detail"])
            self.assertFalse(any(command[0] == "update" for command in commands))

    def test_startup_policy_persists_only_owned_compose_environment_key(self):
        with self.backend() as (backend, states, _, _):
            result = backend.set_startup("server", "off")
            self.assertEqual(result["server"]["state"], "disabled")
            self.assertEqual(states["server"]["restart"], "no")
            saved = control.private_file(backend.path)
            content = (Path(saved["checkout"]) / "deploy/.env").read_bytes()
            self.assertEqual(content, b"# operator settings\nSECRET=keep-private\nPLURX_DATA=/media/state\nPLURX_CONTAINER_RESTART_POLICY=no\n")
            self.assertEqual(saved["environment_sha256"], hashlib.sha256(content).hexdigest())

    def test_changed_environment_is_not_overwritten(self):
        with self.backend() as (backend, _, commands, _):
            path = Path(backend.record["checkout"]) / "deploy/.env"
            path.write_text("operator changed settings\n")
            with self.assertRaises(control.ControlError) as error:
                backend.set_startup("server", "off")
            self.assertEqual(error.exception.code, "policy_conflict")
            self.assertEqual(path.read_text(), "operator changed settings\n")
            self.assertFalse(any(item[0] == "update" for item in commands))

    def test_environment_symlink_is_refused(self):
        with self.backend() as (backend, _, _, root):
            path = Path(backend.record["checkout"]) / "deploy/.env"
            target = root / "unrelated"
            target.write_text("untouched")
            path.unlink()
            path.symlink_to(target)
            with self.assertRaises(OSError):
                backend.set_startup("server", "off")
            self.assertEqual(target.read_text(), "untouched")

    def test_existing_restart_key_is_changed_without_touching_other_bytes(self):
        with self.backend() as (backend, _, _, _):
            path = Path(backend.record["checkout"]) / "deploy/.env"
            content = b"SECRET=keep\r\nPLURX_CONTAINER_RESTART_POLICY=no\r\nOTHER=value\r\n"
            path.write_bytes(content)
            backend.record["environment_sha256"] = hashlib.sha256(content).hexdigest()
            backend.set_startup("server", "on")
            self.assertEqual(path.read_bytes(), content.replace(b"POLICY=no", b"POLICY=always"))

    def test_unsupported_compose_restart_layout_is_refused(self):
        with self.backend() as (backend, _, _, _):
            path = Path(backend.record["checkout"]) / "deploy/docker-compose.yml"
            path.write_text("name: plurx\nservices:\n  plurxd:\n    restart: always\n")
            with self.assertRaises(control.ControlError) as error:
                backend.set_startup("server", "off")
            self.assertEqual(error.exception.code, "unsupported")

    def test_tray_startup_preserves_explicit_backend_and_record(self):
        with self.backend() as (backend, _, _, root):
            result = backend.set_startup("tray", "on")
            text = (root / "autostart" / control.ENTRY).read_text()
            self.assertEqual(result["tray"]["state"], "enabled")
            self.assertIn("--control", text)
            self.assertIn("--installation", text)
            self.assertIn(str(backend.path), text)
            self.assertIn("X-Plurx-Installation=" + INSTALLATION, text)
            self.assertEqual(backend.set_startup("tray", "off")["tray"]["state"], "disabled")

    def test_foreign_tray_entry_is_preserved(self):
        with self.backend() as (backend, _, _, root):
            entry = root / "autostart" / control.ENTRY
            entry.parent.mkdir()
            entry.write_text("[Desktop Entry]\nExec=some-other-install\n")
            with self.assertRaises(control.ControlError) as error:
                backend.set_startup("tray", "on")
            self.assertEqual(error.exception.code, "policy_conflict")
            self.assertIn("some-other-install", entry.read_text())

    def test_source_updates_are_honest_and_never_claim_signature_verification(self):
        with self.backend() as (backend, _, _, _):
            with self.assertRaises(control.ControlError) as check:
                backend.updates("check")
            self.assertEqual(check.exception.code, "unsupported")
            self.assertIn("make docker-up", check.exception.message)
            result = backend.updates("status")
            self.assertEqual(result["state"], "idle")
            self.assertIn("make docker-up", result["detail"])
            self.assertNotEqual(result["state"], "prepared")
            with self.assertRaises(control.ControlError) as error:
                backend.updates("prepare")
            self.assertEqual(error.exception.code, "unsupported")

    def test_published_updates_keep_existing_qualified_image_deployment_path(self):
        with self.backend() as (backend, _, _, _):
            backend.record["update_mode"] = "published"
            with self.assertRaises(control.ControlError) as check:
                backend.updates("check")
            self.assertEqual(check.exception.code, "unsupported")
            self.assertIn("make docker-image-up", check.exception.message)
            result = backend.updates("status")
            self.assertIn("make docker-image-up", result["detail"])
            self.assertIn("No image signature verification", result["detail"])
            self.assertNotEqual(result["state"], "prepared")

    def test_remote_endpoint_is_rejected_before_any_process(self):
        with patch.object(control, "run") as run:
            with self.assertRaises(control.ControlError):
                control.endpoint("tcp://example.com:2376")
            run.assert_not_called()

    def test_nonloopback_browser_address_is_refused(self):
        for value in ("http://example.com:32400", "http://127.0.0.1:32400/path", "http://user:pass@localhost:32400", "https://localhost:32400"):
            with self.subTest(value=value), self.assertRaises(control.ControlError):
                control.browser(value)

    def test_private_record_reader_refuses_symlink_and_public_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "record"
            path.write_text("{}")
            path.chmod(0o644)
            with self.assertRaises(control.ControlError):
                control.private_file(path)
            path.chmod(0o600)
            link = Path(directory) / "link"
            link.symlink_to(path)
            with self.assertRaises(control.ControlError):
                control.private_file(link)

    def test_wire_invalid_arguments_have_one_error_envelope(self):
        with contextlib.redirect_stdout(io.StringIO()) as output:
            result = control.main(["--installation", "/tmp/unused", "logs", "--arbitrary"])
        value = json.loads(output.getvalue())
        self.assertEqual(result, 1)
        self.assertEqual(value["schema_version"], 1)
        self.assertFalse(value["ok"])
        self.assertIsNone(value["result"])
        self.assertEqual(value["error"]["code"], "invalid_request")


if __name__ == "__main__":
    unittest.main()


def wire_examples():
    case = ComposeControlTests()
    with case.backend() as (backend, states, commands, root):
        values = [backend.status(), backend.startup(), backend.logs(2), backend.updates("status")]
        case.journal(backend)
        values.append(backend.updates("status"))
        return [{"schema_version": 1, "ok": True, "result": value, "error": None} for value in values]


class ComposeWireCompatibilityTests(unittest.TestCase):
    def test_actual_backend_responses_match_strict_ui_golden_contract(self):
        golden = ROOT / "crates/plurx-tray/tests/fixtures/compose-wire-v1.json"
        self.assertEqual(wire_examples(), json.loads(golden.read_text()))
