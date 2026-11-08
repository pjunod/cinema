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
            host.log.parent.mkdir(parents=True)
            host.server_bin = lambda: Path('test-plurxd')
            child, response = MagicMock(), MagicMock()
            child.poll.return_value = None
            response.__enter__.return_value.status = 200
            globals_ = cls.spawn.__globals__
            with patch.dict(globals_['os'].environ, {'PLURX_JELLYFIN_PORT': '0'}), \
                 patch.object(globals_['subprocess'], 'Popen', return_value=child) as spawn, \
                 patch.object(globals_['urllib'].request, 'urlopen', return_value=response):
                try:
                    host.spawn()
                    self.assertEqual(spawn.call_args.kwargs['env']['PLURX_JELLYFIN_PORT'], '19003')
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

    def test_freeze_owns_and_rewrites_only_the_bound_native_database(self):
        with tempfile.TemporaryDirectory() as root:
            manifest = self.seed(root)
            self.host(root).freeze_database()
            with sqlite3.connect(Path(root) / 'plurx.db') as db:
                frozen = self.fixture['FROZEN_EPOCH'] - 3 * 86400
                self.assertEqual(db.execute('SELECT title,added_at FROM items ORDER BY title').fetchall(),
                                 [('A', frozen), ('Z', frozen - 3600)])
                self.assertEqual(db.execute('SELECT generation FROM storage_identity').fetchone(),
                                 (manifest['generation'],))
                self.assertEqual(db.execute('SELECT created_at FROM tokens').fetchone(),
                                 (self.fixture['FROZEN_EPOCH'] - 30 * 86400,))
            self.assertEqual(json.loads((Path(root) / 'storage-mode.json').read_text()), manifest)

    def test_freeze_refuses_busy_root_running_daemon_and_unbound_identity(self):
        import fcntl
        for refusal in ['busy', 'running', 'identity', 'cluster', 'pending', 'symlink', 'missing-database']:
            with self.subTest(refusal=refusal), tempfile.TemporaryDirectory() as root:
                manifest = self.seed(root)
                host = self.host(root)
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
            calls = []
            def spawn():
                calls.append('boot')
                host.log.write_text('native boot')
            with patch.object(host, 'spawn', side_effect=spawn), \
                    patch.object(host, 'seed', side_effect=lambda: calls.append('http-seed')), \
                    patch.object(host, 'stop', side_effect=lambda **kw: calls.append(('stop', kw))), \
                    patch.object(host, 'freeze_database', side_effect=lambda: calls.append('freeze')), \
                    patch.object(host, 'wait_for_caption_probe'), \
                    patch.object(host, 'require_jellyfin_listener'), \
                    patch.object(host, 'discover'), \
                    patch.dict(host.start.__globals__, {'require_fixture_software_encoder': lambda *_: None}):
                host.start()
            self.assertEqual(calls, ['boot', 'http-seed', ('stop', {'require_clean': True}), 'freeze', 'boot'])
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
