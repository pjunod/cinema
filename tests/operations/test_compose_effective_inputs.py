"""Persistent container startup follows the selected merged Compose inputs.

No engine, service or host configuration is touched. The fake config command
returns private fixture JSON, exercising only the narrow restart-policy probe.
"""
from __future__ import annotations

import contextlib
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("compose_effective_inputs_control", str(ROOT / "scripts/plurx-compose-control"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
control = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(control)


class ComposeEffectiveInputTests(unittest.TestCase):
    @contextlib.contextmanager
    def inputs(self, override=None):
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory)
            deploy = checkout / "deploy"
            deploy.mkdir()
            base = "name: plurx\nservices:\n  plurxd:\n    restart: \"${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}\"\n  plurx-discovery:\n    restart: \"${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}\"\n"
            (deploy / "docker-compose.yml").write_text(base)
            (deploy / ".env").write_text("SECRET=private-fixture-token\nPLURX_CONTAINER_RESTART_POLICY=no\n")
            if override is not None:
                (deploy / "docker-compose.override.yml").write_text(override)
            with patch.dict(os.environ, {}, clear=True):
                yield checkout, deploy, base

    def resolved(self, argv):
        policy = argv[1].split("=", 1)[1]
        return json.dumps({"name": "plurx", "services": {
            "plurxd": {"restart": policy, "environment": {"SECRET": "private-fixture-token"}},
            "plurx-discovery": {"restart": policy}}})

    def verify(self, checkout):
        control.verify_effective_restart("/usr/bin/docker", "unix:///run/docker.sock", str(checkout), "plurx")

    def test_base_only_digest_preserves_existing_registration_contract(self):
        with self.inputs() as (checkout, deploy, base):
            environment, environment_digest, compose_digest = control.compose_environment(str(checkout), "plurx")
            self.assertEqual(environment, deploy / ".env")
            self.assertEqual(environment_digest, hashlib.sha256(environment.read_bytes()).hexdigest())
            self.assertEqual(compose_digest, hashlib.sha256(base.encode()).hexdigest())

    def test_override_add_edit_and_removal_change_the_saved_input_guard(self):
        with self.inputs() as (checkout, deploy, _):
            original = control.compose_environment(str(checkout), "plurx")
            override = deploy / "docker-compose.override.yml"
            override.write_text("services:\n  plurxd:\n    volumes:\n      - /media:/media:ro\n")
            added = control.compose_environment(str(checkout), "plurx")
            override.write_text("services:\n  plurxd:\n    volumes:\n      - /movies:/media:ro\n")
            edited = control.compose_environment(str(checkout), "plurx")
            self.assertEqual(original[:2], added[:2])
            self.assertNotEqual(original[2], added[2])
            self.assertNotEqual(added[2], edited[2])
            override.unlink()
            self.assertEqual(control.compose_environment(str(checkout), "plurx"), original)

    def test_literal_restart_override_cannot_claim_persistent_off(self):
        override = "services:\n  plurxd:\n    restart: always\n  plurx-discovery:\n    restart: always\n"
        with self.inputs(override) as (checkout, _, _), patch.object(control, "run", return_value=json.dumps({"name": "plurx", "services": {"plurxd": {"restart": "always"}, "plurx-discovery": {"restart": "always"}}})) as run:
            with self.assertRaises(control.ControlError) as error:
                self.verify(checkout)
            self.assertEqual(error.exception.code, "unsupported")
            self.assertIn("override defeats", error.exception.message)
            run.assert_called_once()

    def test_literal_no_override_cannot_claim_persistent_on(self):
        override = "services:\n  plurxd:\n    restart: no\n  plurx-discovery:\n    restart: no\n"
        result = json.dumps({"name": "plurx", "services": {"plurxd": {"restart": "no"}, "plurx-discovery": {"restart": "no"}}})
        with self.inputs(override) as (checkout, _, _), patch.object(control, "run", return_value=result) as run:
            with self.assertRaises(control.ControlError):
                self.verify(checkout)
            self.assertEqual(run.call_count, 2)

    def test_owned_variable_override_survives_both_effective_policy_probes(self):
        override = "services:\n  plurxd:\n    restart: ${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}\n"
        with self.inputs(override) as (checkout, deploy, _), patch.object(control, "run", side_effect=self.resolved) as run:
            self.verify(checkout)
            self.assertEqual(run.call_count, 2)
            for call, policy in zip(run.call_args_list, ("no", "always")):
                argv = call.args[0]
                self.assertEqual(argv[:4], ["/usr/bin/env", "PLURX_CONTAINER_RESTART_POLICY=" + policy, "/usr/bin/docker", "--host"])
                self.assertEqual(argv[4], "unix:///run/docker.sock")
                self.assertEqual(argv[argv.index("--project-directory") + 1], str(deploy))
                self.assertEqual(argv[argv.index("--project-name") + 1], "plurx")
                self.assertEqual(argv[argv.index("--env-file") + 1], str(deploy / ".env"))
                selected = [argv[index + 1] for index, value in enumerate(argv) if value == "-f"]
                self.assertEqual(selected, [str(deploy / "docker-compose.yml"), str(deploy / "docker-compose.override.yml")])
                self.assertEqual(argv[-3:], ["config", "--format", "json"])

    def test_probe_does_not_log_private_resolved_environment_or_put_it_in_argv(self):
        with self.inputs("services:\n  plurxd:\n    volumes: []\n") as (checkout, _, _), patch.object(control, "run", side_effect=self.resolved) as run, contextlib.redirect_stdout(io.StringIO()) as stdout, contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.verify(checkout)
            self.assertEqual(stdout.getvalue(), "")
            self.assertEqual(stderr.getvalue(), "")
            self.assertNotIn("private-fixture-token", str(run.call_args_list))

    def test_shell_interpolation_override_is_refused_before_any_compose_command(self):
        with self.inputs() as (checkout, _, _), patch.dict(os.environ, {"PLURX_CONTAINER_RESTART_POLICY": "always"}), patch.object(control, "run") as run:
            with self.assertRaises(control.ControlError) as error:
                self.verify(checkout)
            self.assertEqual(error.exception.code, "unsupported")
            self.assertIn("shell restart-policy override", error.exception.message)
            run.assert_not_called()

    def test_environment_input_selectors_are_not_silently_ignored(self):
        for key in ("COMPOSE_FILE", "COMPOSE_ENV_FILES", "COMPOSE_PROJECT_NAME"):
            with self.subTest(key=key), self.inputs() as (checkout, deploy, _), patch.object(control, "run") as run:
                with (deploy / ".env").open("a") as stream:
                    stream.write(key + "=other-input\n")
                with self.assertRaises(control.ControlError) as error:
                    self.verify(checkout)
                self.assertEqual(error.exception.code, "unsupported")
                run.assert_not_called()

    def test_include_and_extends_cannot_add_unbound_transitive_inputs(self):
        for text in ("include:\n  - alternate.yml\n", "services:\n  plurxd:\n    extends:\n      file: alternate.yml\n      service: plurxd\n"):
            with self.subTest(text=text), self.inputs(text) as (checkout, _, _), patch.object(control, "run") as run:
                with self.assertRaises(control.ControlError) as error:
                    self.verify(checkout)
                self.assertEqual(error.exception.code, "unsupported")
                run.assert_not_called()

    def test_alternate_default_filenames_are_refused(self):
        with self.inputs() as (checkout, deploy, _), patch.object(control, "run") as run:
            (deploy / "compose.override.yaml").write_text("services: {}\n")
            with self.assertRaises(control.ControlError):
                self.verify(checkout)
            run.assert_not_called()

    def test_override_symlink_is_refused_without_resolving_private_contents(self):
        with self.inputs() as (checkout, deploy, _), patch.object(control, "run") as run:
            (deploy / "docker-compose.override.yml").symlink_to(deploy / ".env")
            with self.assertRaises(OSError):
                self.verify(checkout)
            run.assert_not_called()

    def test_override_drift_during_effective_probe_requires_refresh(self):
        with self.inputs("services:\n  plurxd:\n    volumes: []\n") as (checkout, deploy, _):
            calls = []
            def resolved(argv):
                calls.append(argv)
                if len(calls) == 2:
                    (deploy / "docker-compose.override.yml").write_text("services:\n  plurxd:\n    ports: []\n")
                return self.resolved(argv)
            with patch.object(control, "run", side_effect=resolved):
                with self.assertRaises(control.ControlError) as error:
                    self.verify(checkout)
            self.assertEqual(error.exception.code, "policy_conflict")


if __name__ == "__main__":
    unittest.main()
