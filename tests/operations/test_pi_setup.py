"""Owned Pi setup orchestration, exercised with no host package/service mutations."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import shutil
import subprocess
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

    def test_native_unit_has_private_writable_temp_without_hiding_var_tmp_media(self):
        args = self.args(server_runtime='native', data_dir=Path('/var/tmp/plurx-pi-native-data'),
                         media=[Path('/var/tmp/plurx-pi-media')])
        runtime = {'ffmpeg': '/opt/plurx-runtime/ffmpeg', 'ffprobe': '/opt/plurx-runtime/ffprobe',
                   'bound_ffprobe': '/opt/plurx-runtime/static', 'groups': ['44']}
        unit = setup.native_unit(args, runtime)
        lines = unit.splitlines()
        self.assertIn('User=plurx', lines)
        self.assertIn('Group=plurx', lines)
        self.assertIn('RuntimeDirectory=plurx', lines)
        self.assertIn('RuntimeDirectoryMode=0700', lines)
        self.assertIn('Environment=TMPDIR=/run/plurx', lines)
        self.assertIn('ProtectSystem=strict', lines)
        self.assertIn('ReadWritePaths="/var/tmp/plurx-pi-native-data"', lines)
        self.assertNotIn('PrivateTmp=true', lines)
        self.assertFalse(any(line.startswith('ReadWritePaths=') and line.split('=', 1)[1] in ('/tmp', '/var/tmp') for line in lines))

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

    @contextlib.contextmanager
    def native_host(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'deploy').mkdir()
            (root / 'deploy/pi-runtime').write_text('provider fixture')
            unit, binary = root / 'unit', root / 'binary'
            unit.write_text('old unit')
            binary.write_text('old binary')
            state_path = root / 'state/ownership.json'
            state_path.parent.mkdir()
            previous = {'owner': setup.OWNER, 'uid': setup.os.getuid(), 'checkout': str(root),
                        'role': 'server', 'runtime': 'native', 'data_dir': str(root / 'data'),
                        'media': [], 'url': 'http://localhost:32400',
                        'files': {str(unit): setup.sha(unit), str(binary): setup.sha(binary)}}
            state_path.write_text(json.dumps(previous))
            events = []
            service = {'active': True, 'binary': 'old binary'}
            def write(path, content, mode=0o644):
                events.append(('write', str(path), content))
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content if isinstance(content, bytes) else content.encode())
                path.chmod(mode)
            def run(argv, **kwargs):
                command = list(map(str, argv))
                events.append(('run', command))
                if command[0] == 'git':
                    return 'abc\n'
                if command[:2] == ['systemctl', 'show']:
                    return ('active' if service['active'] else 'inactive') if 'ActiveState' in command[2] else 'enabled'
                if command[:2] == ['systemctl', 'stop']:
                    service['active'] = False
                elif command[:2] == ['systemctl', 'start'] or command[:3] == ['systemctl', 'enable', '--now']:
                    if not service['active']:
                        service.update(active=True, binary=binary.read_text())
                elif command[0] == 'rm':
                    path = Path(command[-1])
                    if path.is_dir():
                        shutil.rmtree(path)
                    else:
                        path.unlink(missing_ok=True)
                return '{}'
            with patch.multiple(setup, ROOT=root, UNIT=unit, BINARY=binary, STATE=state_path,
                                JOURNAL=state_path.parent / 'transaction.json', BACKUPS=state_path.parent / 'backups',
                                LOCK=state_path.parent / 'setup.lock'), \
                 patch.object(setup, 'write', side_effect=write), patch.object(setup, 'run', side_effect=run), \
                 patch.object(setup, 'root_protected', return_value=True):
                yield root, previous, events, service

    def test_failed_upgrade_stops_active_replacement_before_restoring_and_proves_old_ready(self):
        with self.native_host() as (root, previous, events, service):
            candidate = root / 'candidate'
            candidate.write_text('new binary')
            args = self.args(command='upgrade', server_runtime='native', binary=candidate, data_dir=root, media=[])
            runtime = {'ffmpeg': '/opt/plurx-runtime/ffmpeg', 'ffprobe': '/opt/plurx-runtime/ffprobe',
                       'bound_ffprobe': '/opt/plurx-runtime/static', 'groups': ['44']}
            waits = []
            def readiness(url, seconds):
                waits.append(seconds)
                if len(waits) == 1:
                    self.assertEqual(service['binary'], 'new binary')
                    raise ValueError('replacement is active but unhealthy')
                self.assertTrue(service['active'])
                self.assertEqual(service['binary'], 'old binary')
            with patch.object(setup, 'conflict'), patch.object(setup, 'provision', return_value=runtime), \
                 patch.object(setup, 'package_tools'), patch.object(setup, 'native_startup_budget', return_value=18135), \
                 patch.object(setup, 'native_media_groups', return_value=['777']), \
                 patch.object(setup.pwd, 'getpwnam', return_value=types.SimpleNamespace(pw_uid=1001, pw_gid=1001)), \
                 patch.object(setup, 'wait_ready', side_effect=readiness):
                with self.assertRaisesRegex(ValueError, 'active but unhealthy'):
                    setup.install(args, previous)
            self.assertEqual(setup.BINARY.read_text(), 'old binary')
            self.assertEqual(setup.UNIT.read_text(), 'old unit')
            self.assertEqual(waits, [18135, 18135])
            self.assertFalse(setup.JOURNAL.exists())
            restore = next(i for i, event in enumerate(events) if event[:2] == ('write', str(setup.BINARY)) and event[2] == b'old binary')
            stops = [i for i, event in enumerate(events) if event == ('run', ['systemctl', 'stop', 'plurxd'])]
            self.assertGreaterEqual(len(stops), 2)
            self.assertLess(stops[-1], restore)

    def test_interrupted_first_install_reconciles_new_owned_files_and_preserves_data(self):
        with self.native_host() as (root, previous, events, service):
            setup.STATE.unlink()
            setup.UNIT.unlink()
            setup.BINARY.unlink()
            data = root / 'data'
            data.mkdir()
            (data / 'watch-state').write_text('retained')
            args = self.args(server_runtime='native', media=[], data_dir=data)
            transaction = setup.begin_transaction(args, None)
            transaction['phase'] = 'deployment'
            setup.durable_journal(transaction)
            setup.UNIT.write_text('partially installed unit')
            setup.BINARY.write_text('partially installed binary')
            # Simulate power loss: nothing catches an exception; next invocation reads the durable journal.
            pending = setup.read_journal()
            setup.recover_transaction(pending)
            self.assertFalse(setup.UNIT.exists())
            self.assertFalse(setup.BINARY.exists())
            self.assertFalse(setup.STATE.exists())
            self.assertFalse(setup.JOURNAL.exists())
            self.assertEqual((data / 'watch-state').read_text(), 'retained')

    def test_interrupted_upgrade_reconciles_baseline_before_ownership_validation(self):
        with self.native_host() as (root, previous, events, service):
            args = self.args(command='upgrade', server_runtime='native', media=[])
            transaction = setup.begin_transaction(args, previous)
            transaction['phase'] = 'deployment'
            setup.durable_journal(transaction)
            setup.BINARY.write_text('new binary after power loss')
            setup.UNIT.write_text('new unit after power loss')
            service.update(active=True, binary='new binary after power loss')
            with patch.object(setup, 'native_startup_budget', return_value=18135), patch.object(setup, 'wait_ready') as ready:
                setup.recover_transaction(setup.read_journal())
            self.assertEqual(service['binary'], 'old binary')
            self.assertEqual(setup.receipt()['files'], previous['files'])
            ready.assert_called_once_with('http://127.0.0.1:32400', 18135)

    def test_active_process_lock_prevents_concurrent_recovery(self):
        with self.native_host() as (root, previous, events, service):
            setup.LOCK.write_text('')
            with setup.LOCK.open('rb') as held:
                setup.fcntl.flock(held, setup.fcntl.LOCK_EX | setup.fcntl.LOCK_NB)
                with patch.object(setup.sys, 'argv', ['pi-setup', 'install', '--yes']), \
                     patch.object(setup.os, 'getuid', return_value=1000), \
                     patch.object(setup, 'recover_transaction') as recover, \
                     contextlib.redirect_stderr(io.StringIO()) as output:
                    self.assertEqual(setup.main(), 1)
                self.assertIn('another Pi setup invocation is active', output.getvalue())
                recover.assert_not_called()

    def test_keyboard_interrupt_has_journal_before_provider_and_restores_baseline(self):
        with self.native_host() as (root, previous, events, service):
            candidate = root / 'candidate'
            candidate.write_text('candidate')
            args = self.args(command='upgrade', server_runtime='native', binary=candidate, media=[])
            def provider(args):
                self.assertEqual(setup.read_journal()['phase'], 'prepared')
                raise KeyboardInterrupt()
            with patch.object(setup, 'conflict'), patch.object(setup, 'package_tools'), \
                 patch.object(setup, 'native_startup_budget', return_value=18135), \
                 patch.object(setup, 'native_media_groups', return_value=[]), \
                 patch.object(setup.pwd, 'getpwnam', return_value=types.SimpleNamespace(pw_uid=1001, pw_gid=1001)), \
                 patch.object(setup, 'provision', side_effect=provider):
                with self.assertRaises(KeyboardInterrupt):
                    setup.install(args, previous)
            self.assertEqual(setup.BINARY.read_text(), 'old binary')
            self.assertTrue(service['active'])
            self.assertNotIn(('run', ['systemctl', 'stop', 'plurxd']), events)
            self.assertFalse(setup.JOURNAL.exists())

    def test_native_media_0750_root_supplies_group_without_changing_media(self):
        with tempfile.TemporaryDirectory() as directory:
            media = Path(directory).resolve()
            media.chmod(0o750)
            before = media.stat().st_mode
            original = Path.stat
            def info(path, *args, **kwargs):
                value = original(path, *args, **kwargs)
                if path == media:
                    return types.SimpleNamespace(st_mode=value.st_mode, st_uid=10, st_gid=777)
                if path in media.parents:
                    # Isolate this root-group case from private host temp ancestors.
                    return types.SimpleNamespace(st_mode=(value.st_mode & ~0o777) | 0o755, st_uid=0, st_gid=0)
                return value
            with patch.object(setup.pwd, 'getpwnam', return_value=types.SimpleNamespace(pw_uid=99999, pw_gid=88888)), \
                 patch.object(Path, 'stat', info), patch.object(setup, 'run') as run:
                groups = setup.native_media_groups([media], ['44'])
            self.assertIn('777', groups)
            self.assertIn('44', groups)
            self.assertEqual(media.stat().st_mode, before)
            command = list(map(str, run.call_args.args[0]))
            self.assertEqual(command[0], 'setpriv')
            self.assertIn('--groups=44,777,88888', command)
            checked = json.loads(run.call_args.kwargs['input_text'])
            self.assertIn([str(media), setup.os.R_OK | setup.os.X_OK], checked)

    def test_native_media_owner_only_root_refuses_before_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            media = Path(directory).resolve()
            media.chmod(0o700)
            with patch.object(setup.pwd, 'getpwnam', return_value=types.SimpleNamespace(pw_uid=99999, pw_gid=88888)), \
                 patch.object(setup, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'cannot read/traverse'):
                    setup.native_media_groups([media], [])
            run.assert_not_called()
            self.assertEqual(media.stat().st_mode & 0o777, 0o700)

    def test_existing_docker_without_compose_installs_only_compose_plugin(self):
        calls = []
        def run(argv, **kwargs):
            calls.append(argv)
            if argv == ['docker', 'compose', 'version'] and calls.count(argv) == 1:
                raise subprocess.CalledProcessError(1, argv)
            if argv == ['apt-cache', 'policy', 'docker-compose']:
                return 'docker-compose:\n  Candidate: 2.26.1-4\n'
        with patch.object(setup.shutil, 'which', return_value='/usr/bin/tool'), patch.object(setup, 'run', side_effect=run):
            setup.package_tools('docker')
        self.assertIn(['apt-get', 'install', '-y', 'docker-compose'], calls)
        self.assertFalse(any('docker.io' in call for call in calls))
        self.assertEqual(calls[-1], ['docker', 'compose', 'version'])

    def test_fresh_trixie_installs_engine_and_verified_compose_v2_package(self):
        calls = []
        def run(argv, **kwargs):
            calls.append(argv)
            if argv == ['apt-cache', 'policy', 'docker-compose']:
                return 'docker-compose:\n  Candidate: 2.26.1-4\n'
        def which(name):
            return '/usr/bin/apt-get' if name == 'apt-get' else None
        with patch.object(setup.shutil, 'which', side_effect=which), patch.object(setup, 'run', side_effect=run):
            setup.package_tools('docker')
        self.assertIn(['apt-get', 'install', '-y', 'docker.io', 'docker-compose'], calls)
        self.assertLess(calls.index(['apt-get', 'update']), calls.index(['apt-cache', 'policy', 'docker-compose']))
        self.assertEqual(calls[-1], ['docker', 'compose', 'version'])

    def test_compose_v1_candidate_refused_before_package_install(self):
        def run(argv, **kwargs):
            if argv == ['docker', 'compose', 'version']:
                raise subprocess.CalledProcessError(1, argv)
            if argv == ['apt-cache', 'policy', 'docker-compose']:
                return 'docker-compose:\n  Candidate: 1.29.2-3\n'
        with patch.object(setup.shutil, 'which', return_value='/usr/bin/tool'), patch.object(setup, 'run', side_effect=run) as commands:
            with self.assertRaisesRegex(ValueError, 'no Compose v2 candidate'):
                setup.package_tools('docker')
        self.assertFalse(any(call.args[0][:2] == ['apt-get', 'install'] for call in commands.call_args_list))

    def test_docker_install_wait_uses_resolved_startup_grace_over_1500_seconds(self):
        contract = setup.load_script('validate-docker-startup-budget', 'scripts')
        with self.native_host() as (root, previous, events, service):
            setup.STATE.unlink()
            setup.UNIT.unlink()
            setup.BINARY.unlink()
            args = self.args(data_dir=root, media=[])
            runtime = {'compose_service': {'image': 'plurx-pi:local', 'build': {'context': str(root), 'dockerfile': 'Dockerfile.pi'}}}
            def command(argv, **kwargs):
                values = list(map(str, argv))
                events.append(('docker-command', values))
                if values[:3] == ['git', 'log', '-1']:
                    return '1700000000\n'
                if '--emit-start-period' in values:
                    return '18135s\n'
                if values[0] == 'git':
                    return 'abc\n'
                return 'container-id\n'
            with patch.object(setup, 'conflict'), patch.object(setup, 'package_tools'), \
                 patch.object(setup, 'provision', return_value=runtime), \
                 patch.object(setup, 'run', side_effect=command), \
                 patch.object(setup, 'load_script', return_value=contract), \
                 patch.object(setup, 'finish_transaction'), \
                 patch.object(setup, 'wait_ready') as ready, contextlib.redirect_stdout(io.StringIO()):
                setup.install(args, None)
            ready.assert_called_once_with('http://127.0.0.1:32400', 18135.0)
            commands = [event[1] for event in events if event[0] == 'docker-command']
            deploy = next(i for i, command in enumerate(commands) if command[:2] == ['make', 'docker-up'])
            resolved = next(i for i, command in enumerate(commands) if '--emit-start-period' in command)
            self.assertLess(deploy, resolved)

    def test_native_readiness_budget_uses_retained_cluster_config(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / 'plurx.toml'
            config.write_text('[cluster]\nsnapshot_transfer_timeout_secs = 14400\ninstall_snapshot_timeout_secs = 3600\n')
            self.assertEqual(setup.native_startup_budget([config]), 18135)

    def test_status_and_dry_run_report_pending_transaction_without_reconciliation(self):
        pending = {'previous': None}
        for arguments in (['pi-setup', 'status'], ['pi-setup', 'install', '--dry-run']):
            with patch.object(setup.sys, 'argv', arguments), patch.object(setup.os, 'getuid', return_value=1000), \
                 patch.object(setup, 'read_journal', return_value=pending), patch.object(setup, 'LOCK', Path('/nonexistent/pi-test-lock')), \
                 patch.object(setup, 'recover_transaction') as recover, patch.object(setup, 'run') as run, \
                 contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(setup.main(), 0)
            self.assertEqual(json.loads(output.getvalue())['transaction'], 'recovery-needed')
            recover.assert_not_called()
            run.assert_not_called()

    def test_uninstall_retains_data_profiles_and_shared_packages(self):
        previous = {'role': 'both', 'runtime': 'docker', 'files': {'/managed/.env': 'hash'}, 'data_dir': '/srv/plurx'}
        with patch.object(setup, 'run', return_value='{}') as run, patch.object(setup, 'begin_transaction', return_value={'id': 'abc'}), \
             patch.object(setup, 'durable_journal'), patch.object(setup, 'finish_transaction'), \
             patch.object(Path, 'exists', return_value=True), contextlib.redirect_stdout(io.StringIO()):
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
