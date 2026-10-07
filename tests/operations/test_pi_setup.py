"""Owned Pi setup orchestration, exercised with no host package/service mutations."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('pi_setup', str(ROOT / 'deploy/pi-setup'))
spec = importlib.util.spec_from_loader(loader.name, loader)
setup = importlib.util.module_from_spec(spec)
loader.exec_module(setup)


class PiSetupTests(unittest.TestCase):
    def args(self, **values):
        defaults = dict(command='install', role='server', server_runtime='docker',
                        data_dir=Path('/srv/plurx'), media=[Path('/mnt/media')],
                        url='http://localhost:32400', binary=None, dry_run=False,
                        yes=True, autostart=False)
        return types.SimpleNamespace(**(defaults | values))

    def test_dry_run_offline_has_no_mutations(self):
        args = self.args(dry_run=True, role='both')
        with patch.object(setup, 'conflict'), patch.object(setup, 'run') as run, \
             patch.object(setup, 'provision') as provider, patch.object(setup, 'write') as write, \
             contextlib.redirect_stdout(io.StringIO()) as output:
            setup.install(args, None)
        run.assert_not_called()
        provider.assert_not_called()
        write.assert_not_called()
        report = json.loads(output.getvalue())
        self.assertEqual(report['server_runtime'], 'docker')
        self.assertEqual(report['media_read_only'], ['/mnt/media'])

    def test_unowned_runtime_refused_before_provider_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'deploy').mkdir()
            (root / 'deploy/.env').write_text('operator configuration')
            with patch.object(setup, 'ROOT', root), patch.object(setup, 'UNIT', root / 'unit'), \
                 patch.object(setup, 'BINARY', root / 'binary'), patch.object(setup, 'provision') as provider:
                with self.assertRaises(ValueError):
                    setup.install(self.args(), None)
                provider.assert_not_called()
            self.assertEqual((root / 'deploy/.env').read_text(), 'operator configuration')

    def test_native_renders_runtime_paths_and_device_groups(self):
        runtime = {'ffmpeg': '/opt/runtime/ffmpeg', 'ffprobe': '/opt/runtime/ffprobe',
                   'bound_ffprobe': '/opt/runtime/static-probe', 'groups': ['video', 'render']}
        unit = setup.native_unit(self.args(server_runtime='native'), runtime)
        self.assertIn('PLURX_FFMPEG=/opt/runtime/ffmpeg', unit)
        self.assertIn('PLURX_BOUND_FFPROBE=/opt/runtime/static-probe', unit)
        self.assertIn('SupplementaryGroups=video render', unit)
        self.assertIn('ProtectHome=read-only', unit)
        self.assertNotIn('docker', unit)
        with self.assertRaises(ValueError):
            setup.native_unit(self.args(), runtime | {'groups': ['video\nUser=root']})

    def test_compose_media_read_only_and_provider_build_selection(self):
        env, text = setup.docker_files(self.args(), {'compose_service': {'build': {'dockerfile': 'Dockerfile.pi'}}})
        service = json.loads(text)['services']['plurxd']
        self.assertTrue(service['volumes'][0]['read_only'])
        self.assertEqual(service['build']['dockerfile'], 'Dockerfile.pi')
        self.assertIn('PLURX_DATA=/srv/plurx', env)
        with self.assertRaises(ValueError):
            setup.docker_files(self.args(), {'compose_service': {'privileged': True}})

    def test_same_owned_install_does_not_restart(self):
        previous = {'role': 'server', 'runtime': 'docker', 'source': 'abc', 'media': ['/mnt/media'],
                    'data_dir': '/srv/plurx', 'url': 'http://localhost:32400', 'autostart': False, 'binary_sha256': None, 'binary_source': None}
        with patch.object(setup, 'conflict'), patch.object(setup, 'run', return_value='abc\n') as run, \
             patch.object(setup, 'provision') as provider, contextlib.redirect_stdout(io.StringIO()):
            setup.install(self.args(), previous)
        provider.assert_not_called()
        self.assertEqual(run.call_count, 1)

    def test_failed_upgrade_restores_native_binary_and_unit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'deploy').mkdir()
            (root / 'deploy/pi-runtime').write_text('provider fixture')
            unit, binary = root / 'unit', root / 'binary'
            unit.write_text('old unit')
            binary.write_text('old binary')
            candidate = root / 'candidate'
            candidate.write_text('new binary')
            args = self.args(command='upgrade', server_runtime='native', binary=candidate, data_dir=root)
            previous = {'files': {str(unit): 'ignored', str(binary): 'ignored'}}
            runtime = {'ffmpeg': '/opt/plurx-runtime/ffmpeg', 'ffprobe': '/opt/plurx-runtime/ffprobe', 'bound_ffprobe': '/opt/plurx-runtime/static'}
            def write(path, content, mode=0o644):
                path.write_bytes(content if isinstance(content, bytes) else content.encode())
            with patch.object(setup, 'ROOT', root), patch.object(setup, 'UNIT', unit), patch.object(setup, 'BINARY', binary), \
                 patch.object(setup, 'conflict'), patch.object(setup, 'provision', return_value=runtime), \
                 patch.object(setup, 'package_tools'), patch.object(setup, 'run', return_value='abc'), \
                 patch.object(setup, 'write', side_effect=write), patch.object(setup.subprocess, 'run', return_value=types.SimpleNamespace(returncode=0)), \
                 patch.object(setup, 'wait_ready', side_effect=ValueError('not ready')):
                with self.assertRaises(ValueError):
                    setup.install(args, previous)
            self.assertEqual(binary.read_text(), 'old binary')
            self.assertEqual(unit.read_text(), 'old unit')

    def test_uninstall_retains_data_profiles_and_shared_packages(self):
        previous = {'role': 'both', 'runtime': 'docker', 'files': {'/managed/.env': 'hash'}, 'data_dir': '/srv/plurx'}
        with patch.object(setup, 'run', return_value='{}') as run, contextlib.redirect_stdout(io.StringIO()):
            setup.uninstall(self.args(), previous)
        commands = [list(map(str, call.args[0])) for call in run.call_args_list]
        self.assertIn(['python3', str(ROOT / 'deploy/pi-player'), 'uninstall'], commands)
        self.assertIn(['docker', 'compose', 'down'], commands)
        self.assertFalse(any('purge' in command or '/srv/plurx' in command for command in commands))
        self.assertFalse(any('profile' in item for command in commands for item in command))

    def test_runtime_contract_refuses_external_executable_and_unrelated_docker_context(self):
        with self.assertRaises(ValueError):
            setup.runtime_contract(self.args(server_runtime='native'), {'ffmpeg': '/tmp/ffmpeg'})
        with self.assertRaises(ValueError):
            setup.runtime_contract(self.args(), {'compose_service': {'build': {'context': '/tmp/unrelated', 'dockerfile': 'Dockerfile.pi'}}})

    def test_browser_promotion_refuses_unmanaged_setuid_source_before_root_copy(self):
        with patch.object(setup, 'run') as run:
            with self.assertRaises(ValueError):
                setup.promote_browser({'browser_directory': '/tmp/unowned', 'chromium': '/tmp/unowned/chromium',
                                       'browser_sandbox': '/tmp/unowned/chrome-sandbox'}, None)
        run.assert_not_called()

    def test_native_does_not_require_docker(self):
        with patch.object(setup.shutil, 'which', return_value='/usr/bin/apt-get'), patch.object(setup, 'run') as run:
            setup.package_tools('native')
        run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
