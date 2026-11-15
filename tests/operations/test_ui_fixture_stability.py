"""Capture fixtures must retire actual startup work before photographing it."""
from pathlib import Path
import json
import sqlite3
import subprocess
import runpy
import tempfile
from types import SimpleNamespace
from unittest.mock import MagicMock, patch
import unittest

ROOT = Path(__file__).resolve().parents[2]


class UiFixtureStabilityCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = runpy.run_path(str(ROOT / 'scripts/ui-baseline'), run_name='ui_fixture_stability')

    def wait(self, process_rows, logs, timeout=4):
        clock = [0.0]
        def sleep(seconds):
            clock[0] += seconds
        def read_processes(budget):
            self.assertGreater(budget, 0)
            return process_rows(clock[0])
        self.fixture['wait_for_fixture_idle'](read_processes, lambda: logs(clock[0]),
            lambda: True, timeout=timeout, quiet_seconds=1,
            now=lambda: clock[0], sleep=sleep)
        return clock[0]

    def test_empty_child_gap_cannot_substitute_boot_audit_completion(self):
        finished = self.wait(lambda t: [],
                             lambda t: '' if t < 2 else 'caption graph probe complete')
        self.assertGreaterEqual(finished, 3)

    def test_new_child_resets_the_quiet_interval(self):
        finished = self.wait(lambda t: [{'purpose': 'Live TV caption probe'}]
                             if 0.5 <= t < 1.5 else [],
                             lambda t: 'caption graph probe complete')
        self.assertGreaterEqual(finished, 2.5)

    def test_timeout_preserves_actual_child_diagnostics(self):
        with self.assertRaisesRegex(self.fixture['Fail'], 'Live TV caption probe'):
            self.wait(lambda t: [{'purpose': 'Live TV caption probe', 'pid': 123}],
                      lambda t: 'caption graph probe complete', timeout=1)

    def test_prior_boot_terminal_record_cannot_satisfy_the_capture_boot(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'daemon.log'
            old = b'caption graph probe complete\n'
            path.write_bytes(old + b'capture boot started\n')
            current = self.fixture['read_current_boot_log'](path, len(old))
            self.assertNotIn('caption graph probe complete', current)
            with self.assertRaises(self.fixture['Fail']):
                self.wait(lambda t: [], lambda t: current, timeout=1)
            with path.open('ab') as source:
                source.write(b'caption graph probe complete\n')
            self.assertIn('caption graph probe complete',
                          self.fixture['read_current_boot_log'](path, len(old)))

    def test_capture_child_forces_its_reserved_port_over_inherited_environment(self):
        with tempfile.TemporaryDirectory() as root:
            cls = self.fixture['SelfHost']
            with patch.dict(cls.__init__.__globals__,
                            {'free_ports': lambda preferred, count: [19000, 19003]}):
                host = cls(SimpleNamespace(out=root, port=0))
            host.port, host.jellyfin_port = 19000, 19003
            host.base_url = 'http://127.0.0.1:19000'
            host.log.parent.mkdir(parents=True)
            host.server_bin = lambda: Path('test-plurxd')
            child, response = MagicMock(), MagicMock()
            child.poll.return_value = None
            response.__enter__.return_value.status = 200
            globals_ = cls.spawn.__globals__
            with patch.dict(globals_['os'].environ, {
                    'PLURX_JELLYFIN_PORT': '0', 'PLURX_BIND': '127.0.0.1:1',
                    'PLURX_DATA_DIR': '/operator-data', 'PLURX_CACHE_DIR': '/operator-cache',
                    'PLURX_TRANSCODE_DIR': '/operator-transcode',
                    'PLURX_CREDENTIAL_KEY_FILE': '/operator-key',
                    'PLURX_SHARED_CACHE_DIR': '/operator-shared', 'PLURX_SHARED_CACHE_ID': 'operator',
                    'PLURX_SERVER_NAME': 'operator', 'PLURX_HTTPS_ENABLED': 'true',
                    'PLURX_HTTPS_PORT': '443', 'PLURX_HTTPS_CERT_FILE': '/operator-cert',
                    'PLURX_HTTPS_KEY_FILE': '/operator-key', 'PLURX_HTTPS_SANS': 'operator',
                    'PLURX_HTTPS_REDIRECT_PORT': '80'}), \
                 patch.object(globals_['subprocess'], 'Popen', return_value=child) as spawn, \
                 patch.object(globals_['urllib'].request, 'urlopen', return_value=response), \
                 patch.object(host, 'require_owned_server') as ownership:
                try:
                    host.spawn()
                    env = spawn.call_args.kwargs['env']
                    self.assertEqual(env['PLURX_JELLYFIN_PORT'], '19003')
                    self.assertEqual(env['PLURX_BIND'], '127.0.0.1:19000')
                    self.assertEqual(env['PLURX_DATA_DIR'], str(host.data_dir))
                    for key in ['PLURX_CACHE_DIR', 'PLURX_TRANSCODE_DIR',
                                'PLURX_CREDENTIAL_KEY_FILE', 'PLURX_SHARED_CACHE_DIR',
                                'PLURX_SHARED_CACHE_ID']:
                        self.assertEqual(env[key], '')
                    self.assertEqual(env['PLURX_SERVER_NAME'], 'plurx')
                    for key in ['PLURX_HTTPS_ENABLED', 'PLURX_HTTPS_PORT',
                                'PLURX_HTTPS_CERT_FILE', 'PLURX_HTTPS_KEY_FILE',
                                'PLURX_HTTPS_SANS', 'PLURX_HTTPS_REDIRECT_PORT']:
                        self.assertNotIn(key, env)
                    ownership.assert_called_once()
                finally:
                    if spawn.called:
                        spawn.call_args.kwargs['stdout'].close()


class NativeFixtureFreezeCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = runpy.run_path(str(ROOT / 'scripts/ui-baseline'), run_name='native_fixture_freeze')

    def host(self, root):
        host = object.__new__(self.fixture['SelfHost'])
        host.data_dir = Path(root)
        host.proc = None
        host.log = Path(root) / 'daemon.log'
        return host

    def seed(self, root):
        root = Path(root)
        (root / '.plurxd.lock').touch()
        manifest = dict(version=1, mode='sqlite', database='plurx.db',
                        instance_id='fixture-instance', node_id='fixture-node',
                        generation='aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', schema_version=1)
        (root / 'storage-mode.json').write_text(json.dumps(manifest))
        with sqlite3.connect(root / 'plurx.db') as db:
            db.executescript("""
                PRAGMA user_version=1;
                CREATE TABLE storage_identity(singleton,instance_id,node_id,generation);
                CREATE TABLE items(id,title,season_number,episode_number,added_at,updated_at);
                CREATE TABLE files(scanned_at);
                CREATE TABLE watch_state(updated_at);
                CREATE TABLE users(created_at);
                CREATE TABLE tokens(created_at,last_seen_at);
                CREATE TABLE settings(key,value,updated_at);
                CREATE TABLE libraries(created_at,last_scan_at);
                INSERT INTO items VALUES(1,'Z',NULL,NULL,99,99),(2,'A',NULL,NULL,99,99);
                INSERT INTO files VALUES(99);
                INSERT INTO watch_state VALUES(99);
                INSERT INTO users VALUES(99);
                INSERT INTO tokens VALUES(99,99);
                INSERT INTO libraries VALUES(99,99);
            """)
            db.execute('INSERT INTO storage_identity VALUES(1,?,?,?)',
                       [manifest[k] for k in ['instance_id', 'node_id', 'generation']])
            db.execute("INSERT INTO settings VALUES('instance.id',?,99)", [manifest['instance_id']])
        return manifest

    def test_http_readiness_requires_live_owned_native_identity_and_exact_setup_boot(self):
        for refusal in [None, 'instance', 'node', 'cluster', 'cluster-enabled',
                        'setup-complete', 'binding', 'stopped', 'changed-after-restart']:
            with self.subTest(refusal=refusal), tempfile.TemporaryDirectory() as root:
                manifest = self.seed(root)
                host = self.host(root)
                host.base_url = 'http://127.0.0.1:19000'
                host.token = None
                host.proc = MagicMock()
                host.proc.poll.return_value = None
                info = dict(instance_id=manifest['instance_id'], node_id=manifest['node_id'],
                            storage_mode='sqlite', cluster_enabled=False, setup_required=True)
                if refusal in ['instance', 'node']:
                    info[refusal + '_id'] = 'foreign-server'
                elif refusal == 'cluster':
                    info['storage_mode'] = 'cluster'
                elif refusal == 'cluster-enabled':
                    info['cluster_enabled'] = True
                elif refusal == 'setup-complete':
                    info['setup_required'] = False
                elif refusal == 'binding':
                    with sqlite3.connect(Path(root) / 'plurx.db') as db:
                        db.execute("UPDATE storage_identity SET node_id='foreign-database'")
                elif refusal == 'stopped':
                    host.proc.poll.return_value = 1
                elif refusal == 'changed-after-restart':
                    host._storage_binding = ('old-instance', 'old-node', 'old-generation')
                with patch.dict(host.require_owned_server.__globals__,
                                {'api': MagicMock(return_value=info)}) as globals_:
                    if refusal is not None:
                        with self.assertRaises(self.fixture['Fail']):
                            host.require_owned_server()
                    else:
                        host.require_owned_server()
                        self.assertEqual(host._storage_binding,
                                         tuple(manifest[key] for key in ['instance_id', 'node_id', 'generation']))
                        # The seeded capture restart keeps that exact real identity;
                        # it expects setup to remain complete, never calls setup again.
                        host.token = 'owned-token'
                        info['setup_required'] = False
                        host.require_owned_server()
                    if refusal in ['binding', 'stopped', 'changed-after-restart']:
                        globals_['api'].assert_not_called()
                    else:
                        self.assertTrue(all(call.args[1] == '/server'
                                            for call in globals_['api'].call_args_list))
                with sqlite3.connect(Path(root) / 'plurx.db') as db:
                    self.assertEqual(db.execute('SELECT added_at FROM items').fetchall(), [(99,), (99,)])

    def test_foreign_health_200_is_refused_before_the_seed_callback(self):
        cls = self.fixture['SelfHost']
        with tempfile.TemporaryDirectory() as root, patch.dict(cls.__init__.__globals__,
                {'free_ports': lambda *_: [19000, 19003], 'generate_media': lambda *_: None}):
            host = cls(SimpleNamespace(out=root, port=0, ffmpeg='fixture-ffmpeg'))
            child, response = MagicMock(), MagicMock()
            child.poll.return_value = None
            response.__enter__.return_value.status = 200
            globals_ = cls.spawn.__globals__
            foreign = self.fixture['Fail']('fixture HTTP listener belongs to another server')
            with patch.object(host, 'server_bin', return_value=Path('test-plurxd')), \
                    patch.object(globals_['subprocess'], 'Popen', return_value=child) as spawn, \
                    patch.object(globals_['urllib'].request, 'urlopen', return_value=response), \
                    patch.object(host, 'require_owned_server', side_effect=foreign) as ownership, \
                    patch.object(host, 'seed') as seed:
                try:
                    with self.assertRaisesRegex(self.fixture['Fail'], 'another server'):
                        host.start()
                    ownership.assert_called_once()
                    seed.assert_not_called()
                finally:
                    if spawn.called:
                        spawn.call_args.kwargs['stdout'].close()

    def test_freeze_owns_and_rewrites_only_the_bound_native_database(self):
        with tempfile.TemporaryDirectory() as root:
            manifest = self.seed(root)
            host = self.host(root)
            host._storage_binding = tuple(manifest[key] for key in ['instance_id', 'node_id', 'generation'])
            host.freeze_database()
            with sqlite3.connect(Path(root) / 'plurx.db') as db:
                frozen = self.fixture['FROZEN_EPOCH'] - 3 * 86400
                self.assertEqual(db.execute('SELECT title,added_at FROM items ORDER BY title').fetchall(),
                                 [('A', frozen), ('Z', frozen - 3600)])
                self.assertEqual(db.execute('SELECT generation FROM storage_identity').fetchone(),
                                 (manifest['generation'],))
                self.assertEqual(db.execute('SELECT created_at FROM tokens').fetchone(),
                                 (self.fixture['FROZEN_EPOCH'] - 30 * 86400,))
            self.assertEqual(json.loads((Path(root) / 'storage-mode.json').read_text()), manifest)

    def test_freeze_refuses_self_consistent_replacement_before_touching_timestamps(self):
        for replacement in ['instance_id', 'node_id', 'generation', 'unobserved']:
            with self.subTest(replacement=replacement), tempfile.TemporaryDirectory() as root:
                manifest = self.seed(root)
                host = self.host(root)
                host._storage_binding = tuple(manifest[key] for key in ['instance_id', 'node_id', 'generation'])
                if replacement == 'unobserved':
                    host._storage_binding = None
                else:
                    manifest[replacement] = 'replacement-' + manifest[replacement]
                    (Path(root) / 'storage-mode.json').write_text(json.dumps(manifest))
                    with sqlite3.connect(Path(root) / 'plurx.db') as db:
                        db.execute('UPDATE storage_identity SET instance_id=?,node_id=?,generation=?',
                                   [manifest[key] for key in ['instance_id', 'node_id', 'generation']])
                        db.execute("UPDATE settings SET value=? WHERE key='instance.id'", [manifest['instance_id']])
                with self.assertRaisesRegex(self.fixture['Fail'], 'verified seed boot'):
                    host.freeze_database()
                with sqlite3.connect(Path(root) / 'plurx.db') as db:
                    self.assertEqual(db.execute('SELECT added_at,updated_at FROM items').fetchall(), [(99, 99), (99, 99)])
                    self.assertEqual(db.execute('SELECT created_at,last_seen_at FROM tokens').fetchall(), [(99, 99)])
                    self.assertEqual(db.execute('SELECT scanned_at FROM files').fetchall(), [(99,)])
                self.assertEqual(json.loads((Path(root) / 'storage-mode.json').read_text()), manifest)

    def test_freeze_refuses_busy_root_running_daemon_and_unbound_identity(self):
        import fcntl
        for refusal in ['busy', 'running', 'identity', 'cluster', 'pending', 'symlink', 'missing-database']:
            with self.subTest(refusal=refusal), tempfile.TemporaryDirectory() as root:
                manifest = self.seed(root)
                host = self.host(root)
                host._storage_binding = tuple(manifest[key] for key in ['instance_id', 'node_id', 'generation'])
                with (Path(root) / '.plurxd.lock').open('r+') as owner:
                    if refusal == 'busy':
                        fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    elif refusal == 'running':
                        host.proc = MagicMock()
                        host.proc.poll.return_value = None
                    elif refusal in ['identity', 'cluster']:
                        manifest['generation' if refusal == 'identity' else 'mode'] = ('other' if refusal == 'identity' else 'cluster')
                        (Path(root) / 'storage-mode.json').write_text(json.dumps(manifest))
                    elif refusal == 'pending':
                        (Path(root) / 'storage-migration.json').write_text('{}')
                    elif refusal == 'symlink':
                        (Path(root) / 'plurx.db').rename(Path(root) / 'rollback.db')
                        (Path(root) / 'plurx.db').symlink_to('rollback.db')
                    else:
                        (Path(root) / 'plurx.db').unlink()
                    with self.assertRaises(self.fixture['Fail']):
                        host.freeze_database()
                    if refusal == 'missing-database':
                        self.assertFalse((Path(root) / 'plurx.db').exists())
                    else:
                        with sqlite3.connect(Path(root) / 'plurx.db') as db:
                            self.assertEqual(db.execute('SELECT added_at FROM items').fetchall(), [(99,), (99,)])

    def test_selfhost_initializes_and_captures_native_storage_in_two_normal_boots(self):
        cls = self.fixture['SelfHost']
        with tempfile.TemporaryDirectory() as root, patch.dict(cls.__init__.__globals__,
                {'free_ports': lambda preferred, count: [19000, 19003],
                 'generate_media': lambda *_: None}):
            host = cls(SimpleNamespace(out=root, port=0, ffmpeg='fixture-ffmpeg'))
            self.assertIsNone(host.port)
            self.assertIsNone(host.jellyfin_port)
            self.assertIsNone(host.base_url)
            calls = []
            def spawn():
                calls.append('boot')
                host.log.write_text('native boot')
            with patch.object(host, 'server_bin', side_effect=lambda: calls.append('build') or Path('test-plurxd')), \
                    patch.object(host, 'spawn', side_effect=spawn), \
                    patch.object(host, 'seed', side_effect=lambda: calls.append('http-seed')), \
                    patch.object(host, 'stop', side_effect=lambda **kw: calls.append(('stop', kw))), \
                    patch.object(host, 'freeze_database', side_effect=lambda: calls.append('freeze')), \
                    patch.object(host, 'wait_for_caption_probe'), \
                    patch.object(host, 'require_jellyfin_listener'), \
                    patch.object(host, 'discover'), \
                    patch.dict(host.start.__globals__, {
                        'require_fixture_software_encoder': lambda *_: None,
                        'free_ports': lambda *_: calls.append('ports') or [19300, 19303]}):
                host.start()
            self.assertEqual(calls, ['build', 'ports', 'boot', 'http-seed', ('stop', {'require_clean': True}), 'freeze', 'boot'])
            self.assertIn('mode = "sqlite"', host.config.read_text())
            self.assertNotIn('[cluster]', host.config.read_text())

    def test_only_clean_seed_exit_allows_freeze_and_forced_children_are_reaped(self):
        host = self.host('/unused-fixture')
        host.proc = MagicMock()
        host.proc.poll.return_value = None
        host.proc.returncode = 0
        host.stop(require_clean=True)
        host.proc.send_signal.assert_called_once()
        host.proc.wait.assert_called_once_with(timeout=20)
        host.proc.reset_mock()
        host.proc.wait.side_effect = [subprocess.TimeoutExpired('fixture', 20), -9]
        with self.assertRaisesRegex(self.fixture['Fail'], 'forced stop'):
            host.stop(require_clean=True)
        host.proc.kill.assert_called_once()
        self.assertEqual(host.proc.wait.call_count, 2)
        host.proc.reset_mock()
        host.proc.poll.return_value = 101
        host.proc.returncode = 101
        with self.assertRaisesRegex(self.fixture['Fail'], 'exited 101'):
            host.stop(require_clean=True)
