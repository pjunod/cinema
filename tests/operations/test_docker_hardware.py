from __future__ import annotations

import json
import os
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
HELPER = runpy.run_path(str(ROOT / "scripts/docker-hardware"))
GLOBALS = HELPER["prepare"].__globals__


class DockerHardwareTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.dri = self.root / "dri"

    def nodes(self):
        self.dri.mkdir()
        for name in ("card0", "renderD128", "renderD129"):
            (self.dri / name).symlink_to("/dev/null")
        (self.dri / "by-path").mkdir()

    def overlay(self, service=None, nvidia=False):
        return HELPER["gpu_overlay"](service or {}, dri=self.dri, nvidia=nvidia)[
            "services"
        ]["plurxd"]

    def test_hybrid_gpu_gets_devices_numeric_groups_and_nvenc_libraries(self):
        self.nodes()
        value = self.overlay(nvidia=True)
        self.assertEqual(value["devices"], [f"{self.dri}:{self.dri}"])
        self.assertEqual(value["group_add"], [str(Path("/dev/null").stat().st_gid)])
        self.assertEqual(
            value["deploy"]["resources"]["reservations"]["devices"],
            [{"driver": "nvidia", "count": "all", "capabilities": ["gpu"]}],
        )
        self.assertEqual(
            set(value["environment"]["NVIDIA_DRIVER_CAPABILITIES"].split(",")),
            {"compute", "video", "utility", "graphics"},
        )

    def test_cpu_only_and_empty_dri_do_not_require_devices(self):
        self.assertEqual(self.overlay(), {})
        self.dri.mkdir()
        self.assertEqual(self.overlay(), {})

    def test_explicit_render_device_selection_is_not_broadened(self):
        self.nodes()
        value = self.overlay(
            {
                "devices": [
                    {
                        "source": str(self.dri / "renderD129"),
                        "target": "/dev/dri/renderD128",
                        "permissions": "rwm",
                    }
                ]
            },
            nvidia=True,
        )
        self.assertNotIn("devices", value)
        self.assertNotIn("deploy", value)
        self.assertEqual(value["group_add"], [str(Path("/dev/null").stat().st_gid)])

    def test_existing_nvidia_gpu_ids_and_extra_capabilities_are_preserved(self):
        service = {
            "deploy": {
                "resources": {
                    "reservations": {
                        "devices": [
                            {
                                "driver": "nvidia",
                                "device_ids": ["GPU-chosen"],
                                "capabilities": ["gpu"],
                            }
                        ]
                    }
                }
            },
            "environment": {"NVIDIA_DRIVER_CAPABILITIES": "graphics,utility"},
        }
        value = self.overlay(service, nvidia=True)
        self.assertNotIn("deploy", value)
        self.assertEqual(
            set(value["environment"]["NVIDIA_DRIVER_CAPABILITIES"].split(",")),
            {"compute", "video", "utility", "graphics"},
        )
        self.assertEqual(
            service["deploy"]["resources"]["reservations"]["devices"][0]["device_ids"],
            ["GPU-chosen"],
        )

    def test_all_capabilities_and_existing_group_need_no_addition(self):
        self.nodes()
        gid = str(Path("/dev/null").stat().st_gid)
        value = self.overlay(
            {
                "devices": [f"{self.dri}:{self.dri}"],
                "group_add": [gid],
                "gpus": "all",
                "environment": {"NVIDIA_DRIVER_CAPABILITIES": "all"},
            },
            nvidia=True,
        )
        self.assertEqual(value, {})

    def test_default_and_custom_compose_files_keep_override_order(self):
        for name in ("docker-compose.yml", "docker-compose.override.yml"):
            (self.root / name).touch()
        self.assertEqual(
            HELPER["compose_files"]({}, self.root),
            [
                str(self.root / "docker-compose.yml"),
                str(self.root / "docker-compose.override.yml"),
            ],
        )
        self.assertEqual(
            HELPER["compose_files"](
                {"COMPOSE_FILE": "base.yml;host.yml", "COMPOSE_PATH_SEPARATOR": ";"},
                self.root,
            ),
            [str(self.root / "base.yml"), str(self.root / "host.yml")],
        )

    def test_non_linux_and_remote_engines_never_probe_local_devices(self):
        function = HELPER["local_linux_engine"]
        with mock.patch("platform.system", return_value="Darwin"), mock.patch.dict(
            GLOBALS, run=mock.Mock()
        ) as values:
            self.assertFalse(function())
            values["run"].assert_not_called()
        with mock.patch("platform.system", return_value="Linux"), mock.patch.dict(
            os.environ, {"DOCKER_HOST": "ssh://remote"}, clear=True
        ):
            self.assertFalse(function())
        calls = mock.Mock(side_effect=["unix:///desktop.sock", "Docker Desktop"])
        with mock.patch("platform.system", return_value="Linux"), mock.patch.dict(
            os.environ, {}, clear=True
        ), mock.patch.dict(GLOBALS, run=calls):
            self.assertFalse(function())

    def test_context_overrides_docker_host(self):
        calls = mock.Mock(side_effect=["unix:///var/run/docker.sock", "Ubuntu"])
        with mock.patch("platform.system", return_value="Linux"), mock.patch.dict(
            os.environ,
            {"DOCKER_CONTEXT": "local", "DOCKER_HOST": "ssh://remote"},
            clear=True,
        ), mock.patch.dict(GLOBALS, run=calls):
            self.assertTrue(HELPER["local_linux_engine"]())
        self.assertEqual(calls.call_count, 2)

    def test_unrelated_devices_do_not_suppress_hybrid_gpu_detection(self):
        self.nodes()
        value = self.overlay(
            {"devices": [{"source": "/dev/dvb", "target": "/dev/dvb"}]}, nvidia=True
        )
        self.assertIn("devices", value)
        self.assertIn("deploy", value)

    def test_explicit_nvidia_selection_does_not_expose_other_dri_gpus(self):
        self.nodes()
        for service in (
            {"gpus": [{"driver": "nvidia", "device_ids": ["GPU-chosen"]}]},
            {
                "deploy": {
                    "resources": {
                        "reservations": {
                            "devices": [
                                {"driver": "nvidia", "device_ids": ["GPU-chosen"]}
                            ]
                        }
                    }
                }
            },
        ):
            value = self.overlay(service, nvidia=True)
            self.assertNotIn("devices", value)
            self.assertNotIn("group_add", value)
            self.assertNotIn("deploy", value)
            self.assertIn("video", value["environment"]["NVIDIA_DRIVER_CAPABILITIES"])

    def test_explicit_dri_and_cdi_do_not_require_legacy_nvidia_hook(self):
        (self.root / "docker-compose.yml").touch()
        for source in ("/dev/dri/renderD129", "nvidia.com/gpu=GPU-chosen"):
            calls = mock.Mock(
                side_effect=[
                    "",
                    json.dumps(
                        {
                            "services": {
                                "plurxd": {
                                    "devices": [{"source": source, "target": source}]
                                }
                            }
                        }
                    ),
                ]
            )
            probe = mock.Mock(
                side_effect=AssertionError("explicit GPU must not probe NVIDIA")
            )
            with mock.patch(
                "pathlib.Path.cwd", return_value=self.root
            ), mock.patch.dict(
                GLOBALS,
                run=calls,
                local_linux_engine=lambda: True,
                nvidia_available=probe,
            ):
                HELPER["prepare"](self.root / "generated.json")
            probe.assert_not_called()

    def test_automatic_nvidia_without_toolkit_keeps_startup_available(self):
        (self.root / "docker-compose.yml").touch()
        calls = mock.Mock(side_effect=["", json.dumps({"services": {"plurxd": {}}})])
        output = self.root / "generated.json"
        with mock.patch("pathlib.Path.cwd", return_value=self.root), mock.patch(
            "shutil.which", return_value=None
        ), mock.patch.dict(
            GLOBALS,
            run=calls,
            local_linux_engine=lambda: True,
            nvidia_available=lambda: True,
        ):
            HELPER["prepare"](output)
        self.assertNotIn("deploy", json.loads(output.read_text())["services"]["plurxd"])

    def test_symlinked_compose_base_keeps_lexical_project_directory(self):
        shared = self.root / "shared"
        shared.mkdir()
        (shared / "base.yml").touch()
        (self.root / "docker-compose.yml").symlink_to(shared / "base.yml")
        for environment in ({}, {"COMPOSE_FILE": "docker-compose.yml"}):
            self.assertEqual(
                HELPER["compose_files"](environment, self.root),
                [str(self.root / "docker-compose.yml")],
            )

    def test_compose_226_without_environment_flag_preserves_file_and_gpu_precedence(self):
        project = self.root / "selected-project"
        project.mkdir()
        for name in ("base.yml", "host.yml"):
            (project / name).touch()
        name = "plurx-hardware-environment-probe"
        calls = []
        def compose(*arguments):
            calls.append(arguments)
            if "--environment" in arguments:
                raise HELPER["HardwareError"]("unknown flag: --environment")
            selected = [arguments[index + 1] for index, value in enumerate(arguments) if value == "-f"]
            probe = json.loads(Path(selected[-1]).read_text())
            self.assertEqual(probe["services"][name]["labels"]["PLURX_DOCKER_GPU"], "${PLURX_DOCKER_GPU:-auto}")
            labels = {"COMPOSE_FILE": "selected-project/base.yml;selected-project/host.yml", "COMPOSE_PATH_SEPARATOR": ";", "PLURX_DOCKER_GPU": "auto"}
            services = {name: {"labels": labels}}
            if len(selected) > 1:
                self.assertEqual(selected[:-1], [str(project / "base.yml"), str(project / "host.yml")])
                # The actual project resolves the mode; the first probe's mode
                # must not decide whether local GPU discovery is performed.
                labels["PLURX_DOCKER_GPU"] = "manual"
                services["plurxd"] = {"devices": [{"source": "/dev/video19", "target": "/dev/video19"}]}
            return json.dumps({"services": services})
        probe = mock.Mock(side_effect=AssertionError("resolved manual mode probed host"))
        output = self.root / "generated.json"
        with mock.patch("pathlib.Path.cwd", return_value=self.root), mock.patch.dict(
            GLOBALS, run=compose, local_linux_engine=probe
        ):
            files = HELPER["prepare"](output).split(os.pathsep)
        self.assertEqual(files, [str(project / "base.yml"), str(project / "host.yml"), str(output)])
        self.assertEqual(json.loads(output.read_text()), {"services": {"plurxd": {}}})
        self.assertEqual(len(calls), 3)
        self.assertFalse(list(self.root.glob(".plurx-hardware-probe-*")))
        probe.assert_not_called()

    def test_manual_mode_uses_compose_env_precedence_and_does_not_probe(self):
        (self.root / "docker-compose.yml").touch()
        (self.root / "docker-compose.override.yml").touch()
        probe = mock.Mock(side_effect=AssertionError("manual mode probed host"))
        calls = mock.Mock(
            side_effect=[
                "PLURX_DOCKER_GPU=manual\n",
                json.dumps({"services": {"plurxd": {}}}),
            ]
        )
        output = self.root / "generated.json"
        with mock.patch("pathlib.Path.cwd", return_value=self.root), mock.patch.dict(
            GLOBALS, run=calls, local_linux_engine=probe
        ):
            files = HELPER["prepare"](output).split(os.pathsep)
        self.assertEqual(
            files,
            [
                str(self.root / "docker-compose.yml"),
                str(self.root / "docker-compose.override.yml"),
                str(output),
            ],
        )
        self.assertEqual(json.loads(output.read_text()), {"services": {"plurxd": {}}})
        probe.assert_not_called()

    def test_pi_detection_requires_arm64_vendor_proof(self):
        compatible = self.root / 'compatible'
        compatible.write_bytes(b'brcm,bcm2712\0raspberrypi,5-model-b\0')
        with mock.patch('platform.system', return_value='Linux'), mock.patch('platform.machine', return_value='aarch64'):
            self.assertTrue(HELPER['raspberry_pi_host'](compatible))
            compatible.write_bytes(b'brcm,bcm2712\0')
            self.assertFalse(HELPER['raspberry_pi_host'](compatible))
        with mock.patch('platform.system', return_value='Linux'), mock.patch('platform.machine', return_value='x86_64'):
            self.assertFalse(HELPER['raspberry_pi_host'](compatible))

    def test_pi_prepare_retains_host_compose_files_and_configuration(self):
        (self.root / 'base.yml').touch()
        (self.root / 'nas.yml').touch()
        host = {'volumes': [{'source': '/srv/data', 'target': '/var/lib/plurx'},
                            {'source': '/nas', 'target': '/media', 'read_only': True}],
                'environment': {'PLURX_DATA_DIR': '/var/lib/plurx', 'CUSTOM': 'keep'},
                'networks': ['operator'], 'group_add': ['123'],
                'security_opt': ['apparmor=operator-profile']}
        original = json.loads(json.dumps(host))
        runtime = {'service': {'image': 'plurx-pi-fixture:source',
                   'build': {'context': str(ROOT), 'dockerfile': 'Dockerfile.pi'},
                   'devices': ['/dev/video19:/dev/video19:rw'], 'group_add': ['44'],
                   'environment': {'PLURX_FFMPEG': '/opt/plurx-runtime/pi/bin/ffmpeg'}},
                   'profile': '/var/lib/owned/profile.json'}
        provider = mock.Mock(return_value=runtime)
        environment = {'COMPOSE_FILE': 'base.yml:nas.yml'}
        with mock.patch.dict(GLOBALS, resolved_compose_inputs=mock.Mock(return_value=(environment, {'services': {'plurxd': host}})),
                             local_linux_engine=mock.Mock(return_value=True), raspberry_pi_host=mock.Mock(return_value=True),
                             prepare_pi_runtime=provider), mock.patch('pathlib.Path.cwd', return_value=self.root):
            files = HELPER['prepare'](self.root / 'runtime.json', prepare_pi=True).split(os.pathsep)
        self.assertEqual(files[:2], [str(self.root / 'base.yml'), str(self.root / 'nas.yml')])
        self.assertEqual(host, original)
        overlay = json.loads((self.root / 'runtime.json').read_text())['services']
        self.assertEqual(overlay['plurxd']['security_opt'], ['apparmor=operator-profile', 'seccomp=/var/lib/owned/profile.json'])
        self.assertEqual(overlay['plurxd']['cap_drop'], ['ALL'])
        self.assertEqual(overlay['plurx-discovery'], {'image': runtime['service']['image']})
        self.assertNotIn('networks', overlay['plurxd'])
        self.assertNotIn('PLURX_DATA_DIR', overlay['plurxd']['environment'])
        provider.assert_called_once()

    def test_remote_engine_never_prepares_pi_runtime(self):
        (self.root / 'docker-compose.yml').touch()
        provider = mock.Mock(side_effect=AssertionError('remote runtime mutation'))
        with mock.patch.dict(GLOBALS, resolved_compose_inputs=mock.Mock(return_value=({}, {'services': {'plurxd': {}}})),
                             local_linux_engine=mock.Mock(return_value=False), prepare_pi_runtime=provider), \
             mock.patch('pathlib.Path.cwd', return_value=self.root):
            HELPER['prepare'](self.root / 'runtime.json', prepare_pi=True)
        provider.assert_not_called()

    def test_pi_security_conflict_precedes_provider_mutation(self):
        (self.root / 'docker-compose.yml').touch()
        provider = mock.Mock(side_effect=AssertionError('provider must not run'))
        with mock.patch.dict(GLOBALS, resolved_compose_inputs=mock.Mock(return_value=({}, {'services': {'plurxd': {'security_opt': ['seccomp=unconfined']}}})),
                             local_linux_engine=mock.Mock(return_value=True), raspberry_pi_host=mock.Mock(return_value=True),
                             prepare_pi_runtime=provider), mock.patch('pathlib.Path.cwd', return_value=self.root):
            with self.assertRaises(HELPER['HardwareError']):
                HELPER['prepare'](self.root / 'runtime.json', prepare_pi=True)
        provider.assert_not_called()


    def test_pi_existing_profile_is_reused_without_duplicate_seccomp(self):
        (self.root / 'docker-compose.yml').touch()
        existing = 'seccomp=/var/lib/previously-owned/pi-worker-seccomp.json'
        runtime = {'service': {'image': 'pi:verified', 'build': {'dockerfile': 'Dockerfile.pi'}},
                   'profile': existing[8:]}
        provider = mock.Mock(return_value=runtime)
        with mock.patch.dict(GLOBALS, resolved_compose_inputs=mock.Mock(return_value=({'PLURX_DOCKER_GPU': 'manual'}, {'services': {'plurxd': {'security_opt': [existing]}}})),
                             local_linux_engine=mock.Mock(return_value=True), raspberry_pi_host=mock.Mock(return_value=True),
                             pi_security_options=mock.Mock(return_value=[existing]), prepare_pi_runtime=provider), \
             mock.patch('pathlib.Path.cwd', return_value=self.root):
            HELPER['prepare'](self.root / 'runtime.json', prepare_pi=True)
        provider.assert_called_once_with(existing[8:])
        options = json.loads((self.root / 'runtime.json').read_text())['services']['plurxd']['security_opt']
        self.assertEqual(options, [existing])
        self.assertEqual(set([existing] + options), {existing})


    def test_pi_manual_gpu_keeps_required_decoder_without_broadening_render_selection(self):
        runtime = {'service': {'image': 'pi:verified',
                   'devices': ['/dev/video19:/dev/video19:rw', '/dev/dri/renderD128:/dev/dri/renderD128:rw'],
                   'group_add': ['44', '104']}, 'profile': '/var/lib/owned/profile.json'}
        with mock.patch('pathlib.Path.stat', return_value=mock.Mock(st_gid=44)):
            overlay = HELPER['pi_runtime_overlay']({}, runtime, manual_gpu=True)['services']['plurxd']
        self.assertEqual(overlay['devices'], ['/dev/video19:/dev/video19:rw'])
        self.assertEqual(overlay['group_add'], ['44'])
        self.assertEqual(overlay['image'], 'pi:verified')
        host = {'devices': [{'source': '/dev/dri/renderD129', 'target': '/dev/dri/renderD128'}]}
        with mock.patch('pathlib.Path.stat', return_value=mock.Mock(st_gid=44)):
            overlay = HELPER['pi_runtime_overlay'](host, runtime)['services']['plurxd']
        self.assertEqual(overlay['devices'], ['/dev/video19:/dev/video19:rw'])
        self.assertEqual(host['devices'][0]['source'], '/dev/dri/renderD129')



if __name__ == "__main__":
    unittest.main()
