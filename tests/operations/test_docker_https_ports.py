from __future__ import annotations

import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
HELPER = runpy.run_path(str(ROOT / "scripts/docker-https-ports.py"))
HARDWARE = runpy.run_path(str(ROOT / "scripts/docker-hardware"))


class DockerHttpsPortTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for helper in (HELPER, HARDWARE["HTTPS"]):
            patcher = mock.patch.dict(helper["mapped_file"].__globals__, require_local_filesystem=lambda: None)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.service = {
            "environment": {"PLURX_DATA_DIR": "/var/lib/plurx"},
            "volumes": [{"type": "bind", "source": str(self.root), "target": "/var/lib/plurx"}],
        }
        self.document = {"services": {"plurxd": self.service}}

    def saved(self, settings):
        (self.root / "https-settings.json").write_text(json.dumps({
            "version": 1, "revision": 2, "settings": settings,
        }))

    def overlay(self, **environment):
        return HELPER["https_port_overlay"](self.document, environment)

    def port(self, target=32443, published=None):
        return {"ports": [{"target": target, "published": str(published or target), "protocol": "tcp"}]}

    def test_saved_https_enable_custom_port_and_disable_change_publication(self):
        self.assertEqual(self.overlay(), {})
        self.saved({"enabled": True})
        self.assertEqual(self.overlay(), self.port())
        self.saved({"enabled": True, "port": 33443})
        self.assertEqual(self.overlay(), self.port(33443))
        self.assertEqual(self.overlay(PLURX_HTTPS_HOST_PORT="443"), {
            **self.port(33443, 443), "environment": {"PLURX_HTTPS_REDIRECT_PORT": "443"},
        })
        self.saved({"enabled": False, "port": 33443})
        self.assertEqual(self.overlay(), {})

    def test_toml_saved_and_container_environment_precedence(self):
        config = self.root / "plurx.toml"
        config.write_text('[server.https]\nenabled = true\nport = 33443\n')
        self.service["command"] = ["--config", "/var/lib/plurx/plurx.toml"]
        self.assertEqual(self.overlay(), self.port(33443))
        self.saved({"enabled": False})
        self.assertEqual(self.overlay(), {})
        self.service["environment"].update(PLURX_HTTPS_ENABLED="true", PLURX_HTTPS_PORT="34443")
        self.assertEqual(self.overlay(), self.port(34443))
        self.service["environment"]["PLURX_HTTPS_ENABLED"] = "false"
        self.assertEqual(self.overlay(), {})

    def test_saved_object_replaces_toml_without_inheriting_its_port(self):
        (self.root / "plurx.toml").write_text('[server.https]\nenabled = true\nport = 33443\n')
        self.service["working_dir"] = "/var/lib/plurx"
        self.saved({"enabled": True})
        self.assertEqual(self.overlay(), self.port())

    def test_final_data_directory_and_deepest_bind_select_saved_state(self):
        (self.root / "plurx.toml").write_text('[storage]\ndata_dir = "/wrong"\n')
        self.service["environment"]["PLURX_CONFIG"] = "/var/lib/plurx/plurx.toml"
        self.saved({"enabled": True})
        self.assertEqual(self.overlay(), self.port())
        replacement = self.root / "alternate.json"
        replacement.write_text(json.dumps({"version": 1, "revision": 0, "settings": {"enabled": False}}))
        self.service["volumes"].append({"type": "bind", "source": str(replacement), "target": "/var/lib/plurx/https-settings.json"})
        self.assertEqual(self.overlay(), {})

    def test_compose_file_config_and_relative_data_directory(self):
        (self.root / "plurx.toml").write_text('[storage]\ndata_dir = "state"\n[server.https]\nenabled = true\n')
        self.service["environment"].clear()
        self.service["working_dir"] = "/app"
        self.service["configs"] = [{"source": "configuration", "target": "/app/plurx.toml"}]
        self.document["configs"] = {"configuration": {"file": str(self.root / "plurx.toml")}}
        self.service["volumes"][0]["target"] = "/app/state"
        self.assertEqual(self.overlay(), self.port())
        self.saved({"enabled": False})
        self.assertEqual(self.overlay(), {})

    def test_manual_publication_and_host_network_are_preserved(self):
        self.saved({"enabled": True})
        self.service["ports"] = [{"target": 32443, "published": "443", "host_ip": "127.0.0.1", "protocol": "tcp"}]
        self.assertEqual(self.overlay(), {"environment": {"PLURX_HTTPS_REDIRECT_PORT": "443"}})
        self.service["ports"][0].pop("published")
        with self.assertRaisesRegex(ValueError, "random HTTPS publications"):
            self.overlay()
        self.service["environment"]["PLURX_HTTPS_REDIRECT_PORT"] = "443"
        self.assertEqual(self.overlay(), {}, "an explicit public port retains a random publication")
        self.service["environment"].pop("PLURX_HTTPS_REDIRECT_PORT")
        self.service["ports"][0]["protocol"] = "udp"
        self.assertEqual(self.overlay(), self.port())
        self.service["network_mode"] = "host"
        self.assertEqual(self.overlay(), {})

    def test_ambiguous_manual_publications_require_an_explicit_redirect_port(self):
        self.saved({"enabled": True})
        self.service["ports"] = [
            {"target": 32443, "published": "443", "protocol": "tcp"},
            {"target": 32443, "published": "8443", "protocol": "tcp"},
        ]
        with self.assertRaisesRegex(ValueError, "multiple HTTPS publications"):
            self.overlay()
        self.service["environment"]["PLURX_HTTPS_REDIRECT_PORT"] = "8443"
        self.assertEqual(self.overlay(), {})

    def test_redirect_port_metadata_preserves_gpu_environment(self):
        self.saved({"enabled": True})
        (self.root / "docker-compose.yml").touch()
        output = self.root / "overlay.json"
        with mock.patch("pathlib.Path.cwd", return_value=self.root), mock.patch.dict(
            HARDWARE["prepare"].__globals__,
            resolved_compose_inputs=lambda: ({"PLURX_HTTPS_HOST_PORT": "443"}, self.document),
            local_linux_engine=lambda: True, gpu_selection=lambda *a: (True, False),
            gpu_overlay=lambda *a, **kw: {"services": {"plurxd": {"environment": {"NVIDIA_DRIVER_CAPABILITIES": "video"}}}},
        ):
            HARDWARE["prepare"](output)
        self.assertEqual(json.loads(output.read_text())["services"]["plurxd"]["environment"], {
            "NVIDIA_DRIVER_CAPABILITIES": "video", "PLURX_HTTPS_REDIRECT_PORT": "443",
        })

    def test_host_network_does_not_inspect_opaque_state(self):
        self.service["network_mode"] = "host"
        self.service["volumes"][0]["type"] = "volume"
        self.assertEqual(self.overlay(), {})

    def test_remote_engine_files_are_not_read_as_local_settings(self):
        guard = HELPER["require_local_filesystem"]
        with mock.patch.dict(os.environ, {"DOCKER_HOST": "ssh://operator@media1"}, clear=True):
            with self.assertRaisesRegex(ValueError, "run the Docker rollout target on the engine host"):
                guard()
        with mock.patch.dict(os.environ, {"DOCKER_CONTEXT": "remote"}, clear=True), mock.patch(
            "subprocess.run", return_value=mock.Mock(stdout="tcp://media1:2376\n")
        ):
            with self.assertRaisesRegex(ValueError, "requires local Docker"):
                guard()

    def test_invalid_environment_ports_and_opaque_settings_refuse(self):
        for raw in ("", "yes", "True"):
            with self.subTest(enabled=raw):
                self.service["environment"]["PLURX_HTTPS_ENABLED"] = raw
                with self.assertRaisesRegex(ValueError, "must be true or false"):
                    self.overlay()
        self.service["environment"]["PLURX_HTTPS_ENABLED"] = "true"
        for raw in ("", "0", "65536", "true", True):
            with self.subTest(port=raw):
                self.service["environment"]["PLURX_HTTPS_PORT"] = raw
                with self.assertRaisesRegex(ValueError, "between 1 and 65535"):
                    self.overlay()
        self.service["environment"].pop("PLURX_HTTPS_PORT")
        self.service["volumes"][0]["type"] = "volume"
        with self.assertRaisesRegex(ValueError, "opaque mount"):
            self.overlay()

    def test_invalid_saved_envelopes_and_nonregular_files_refuse(self):
        path = self.root / "https-settings.json"
        for body in ('broken', '{"version":2,"revision":1,"settings":{}}',
                     '{"version":1,"revision":-1,"settings":{}}', 'x' * 16385):
            with self.subTest(body=body[:60]):
                path.write_text(body)
                with self.assertRaises(ValueError):
                    self.overlay()
        path.unlink()
        os.mkfifo(path)
        with self.assertRaisesRegex(ValueError, "regular file"):
            self.overlay()
        path.unlink()
        path.symlink_to(self.root / "other.json")
        with self.assertRaises(OSError):
            self.overlay()

    def test_rollout_helper_includes_ports_even_in_manual_gpu_mode(self):
        self.saved({"enabled": True, "port": 33443})
        (self.root / "docker-compose.yml").touch()
        output = self.root / "overlay.json"
        with mock.patch("pathlib.Path.cwd", return_value=self.root), mock.patch.dict(
            HARDWARE["prepare"].__globals__,
            resolved_compose_inputs=lambda: ({"PLURX_DOCKER_GPU": "manual"}, self.document),
        ):
            files = HARDWARE["prepare"](output)
        self.assertEqual(json.loads(output.read_text())["services"]["plurxd"], self.port(33443))
        self.assertTrue(files.endswith(str(output)))

    @unittest.skipUnless(shutil.which("docker"), "Docker Compose CLI is unavailable")
    def test_real_compose_rollout_adds_changes_and_removes_only_https_mapping(self):
        base = self.root / "compose.json"
        # Use the tracked base and real Compose interpolation/merge, without an
        # engine or any running container. Keep fixtures outside the host data.
        base.write_text(json.dumps({"services": {"plurxd": {"volumes": [
            {"type": "bind", "source": str(self.root), "target": "/var/lib/plurx"}
        ]}}}))
        output = self.root / "generated.json"
        selected = str(ROOT / "deploy/docker-compose.yml") + os.pathsep + str(base)
        environment = dict(os.environ, COMPOSE_FILE=selected, COMPOSE_PATH_SEPARATOR=os.pathsep,
                           PLURX_DOCKER_GPU="manual", PLURX_HTTPS_HOST_PORT="")
        with mock.patch("pathlib.Path.cwd", return_value=ROOT / "deploy"), mock.patch.dict(os.environ, environment, clear=True):
            for enabled, port in ((False, 32443), (True, 32443), (True, 33443), (False, 33443)):
                with self.subTest(enabled=enabled, port=port):
                    self.saved({"enabled": enabled, "port": port})
                    files = HARDWARE["prepare"](output)
                    config = subprocess.run(["docker", "compose", "config", "--format", "json"],
                        cwd=ROOT / "deploy", env=dict(environment, COMPOSE_FILE=files),
                        check=True, capture_output=True, text=True)
                    ports = json.loads(config.stdout)["services"]["plurxd"]["ports"]
                    targets = {(item["target"], item["protocol"]) for item in ports}
                    self.assertEqual(targets, {(32400, "tcp"), (8096, "tcp"), (32414, "udp")} | ({(port, "tcp")} if enabled else set()))
            self.saved({"enabled": True, "port": 33443})
            with mock.patch.dict(os.environ, PLURX_HTTPS_HOST_PORT="443"):
                files = HARDWARE["prepare"](output)
            config = subprocess.run(["docker", "compose", "config", "--format", "json"],
                cwd=ROOT / "deploy", env=dict(environment, COMPOSE_FILE=files),
                check=True, capture_output=True, text=True)
            resolved = json.loads(config.stdout)["services"]["plurxd"]
            self.assertEqual(resolved["environment"]["PLURX_HTTPS_REDIRECT_PORT"], "443")
            self.assertEqual(resolved["environment"]["PLURX_DATA_DIR"], "/var/lib/plurx")
            self.assertTrue(any(item["target"] == 33443 and item["published"] == "443" for item in resolved["ports"]))
