"""Bounded adapter contracts. Local fake binaries and archives; no downloads."""
import contextlib
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
API = runpy.run_path(str(ROOT / 'scripts/ripwire'))


class RipwireTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'scripts').mkdir()
        for name in ('ripwire', 'ripwire.lock.json'):
            shutil.copy2(ROOT / 'scripts' / name, self.root / 'scripts' / name)
        (self.root / '.gitignore').write_text('target/\n')
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture baseline')
        self.key = API['host_platform']()
        self.lock = API['read_lock'](self.root)

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, stderr=subprocess.PIPE).decode().strip()

    def install(self, body=None):
        directory = self.root / 'target/ripwire/tools/0.5.0' / self.key
        directory.mkdir(parents=True, exist_ok=True)
        binary = directory / 'ripwire'
        code = (f'#!{sys.executable}\nimport json, os, sys, time\n'
                'if "--version" in sys.argv:\n print("ripwire 0.5.0"); sys.exit(0)\n'
                f'if "--help" in sys.argv:\n print({" ".join(API["FLAGS"])!r} + " "); sys.exit(0)\n')
        code += body or 'print(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd()}))\n'
        binary.write_text(code)
        binary.chmod(0o700)
        metadata = dict(version='0.5.0', platform=self.key,
                        source_commit=self.lock['source_commit'],
                        archive_sha256=self.lock['platforms'][self.key]['archive_sha256'],
                        binary_sha256=API['digest'](binary))
        (directory / 'install.json').write_text(json.dumps(metadata))
        return binary

    def invoke(self, *args, cwd=None, env=None):
        return subprocess.run([sys.executable, str(self.root / 'scripts/ripwire'), *args],
                              cwd=cwd or self.root, env={**os.environ, 'PLURX_RIPWIRE_DISABLED': '0', **(env or {})},
                              capture_output=True, timeout=15)

    def provenance(self, result):
        return json.loads(result.stderr.splitlines()[0])

    def test_absent_disabled_and_doctor_do_not_install(self):
        for mode in ('map', 'doctor'):
            result = self.invoke(mode)
            self.assertEqual(69, result.returncode, result.stderr)
        self.assertFalse((self.root / 'target/ripwire/tools').exists())
        self.install()
        self.assertEqual(69, self.invoke('map', env={'PLURX_RIPWIRE_DISABLED': '1'}).returncode)
        doctor = self.invoke('doctor', env={'PLURX_RIPWIRE_DISABLED': '1'})
        self.assertEqual(69, doctor.returncode)
        self.assertEqual('disabled', json.loads(doctor.stdout)['activation'])

    def test_arguments_are_data_and_nested_invocation_has_same_root(self):
        self.install()
        text = '-space "quoted" $(touch INJECTION) ; `touch INJECTION`'
        first = self.invoke('find', '--', text)
        second = self.invoke('find', '--', text, cwd=self.root / 'scripts')
        self.assertEqual(0, first.returncode, first.stderr)
        self.assertEqual(first.stdout, second.stdout)
        result = json.loads(first.stdout)
        self.assertIn('--for=' + text, result['argv'])
        self.assertEqual(str(self.root.resolve()), result['cwd'])
        self.assertFalse((self.root / 'INJECTION').exists())

    def test_exact_mapping_and_budget_honesty(self):
        self.install()
        cases = [('map', [], '--max-tokens=3000', True),
                 ('find', ['task'], '--format=candidates', False),
                 ('outline', ['a.rs:f'], '--outline=a.rs:f', False),
                 ('body', ['a.rs:f'], '--expand=a.rs:f', False),
                 ('callers', ['a.rs:f'], '--callers=a.rs:f', False),
                 ('impact', ['a.rs:f'], '--impact=a.rs:f', False),
                 ('coverage', [], '--skipped', False)]
        for mode, positional, flag, enforced in cases:
            with self.subTest(mode=mode):
                result = self.invoke(mode, *positional)
                self.assertEqual(0, result.returncode, result.stderr)
                argv = json.loads(result.stdout)['argv']
                self.assertIn(flag, argv)
                self.assertIn('--max-file-size=4194304', argv)
                self.assertEqual(enforced, self.provenance(result)['budget_enforced'])

    def test_profiles_cold_and_family_isolation(self):
        self.install()
        default = self.invoke('find', 'task')
        history = self.invoke('map', '--profile', 'history')
        cold = self.invoke('map', '--cold')
        d, h, c = [json.loads(r.stdout)['argv'] for r in (default, history, cold)]
        self.assertIn('--exclude=./docs/archive', d)
        self.assertNotIn('--exclude=./docs/archive', h)
        self.assertTrue(any(v.endswith('/default/rich.ripwirecache') for v in d))
        self.assertTrue(any(v.endswith('/history/lean.ripwirecache') for v in h))
        self.assertIn('--no-cache', c)
        self.assertFalse(any(v.startswith('--cache=') for v in c))
        config = json.loads((self.root / 'target/ripwire/cache/0.5.0/default/config.json').read_text())
        self.assertEqual(str(self.root.resolve()), config['root'])

    def test_invalid_arguments(self):
        self.install()
        for args in [('map', '--budget', '499'), ('map', '--timeout', '121'),
                     ('map', '--bud', '500'), ('map', 'extra'), ('setup', '--cold'),
                     ('doctor', '--profile', 'history'), ('pr-context',), ('find', 'é' * 2049)]:
            with self.subTest(args=args[:2]):
                self.assertEqual(2, self.invoke(*args).returncode)
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            API['arguments'](['body', 'nul\0symbol'])

    def test_empty_success_and_upstream_refusals(self):
        for upstream, expected in [(0, 0), (3, 3), (4, 1)]:
            self.install(f'sys.stderr.write("diagnostic\\n"); sys.exit({upstream})\n')
            result = self.invoke('map')
            self.assertEqual(expected, result.returncode)
            self.assertEqual(b'', result.stdout)
            self.assertEqual(upstream, self.provenance(result)['upstream_exit'])
            self.assertTrue(result.stderr.endswith(b'diagnostic\n'))

    def test_corrupt_binary_is_not_executed(self):
        binary = self.install()
        binary.write_text(binary.read_text() + 'open("CORRUPT", "w").close()\n')
        for mode in ('map', 'doctor'):
            self.assertEqual(2, self.invoke(mode).returncode)
        self.assertFalse((self.root / 'CORRUPT').exists())

    def test_timeout_and_output_refusal_withhold_all_output(self):
        for body, code in [('print("partial", flush=True); time.sleep(20)', 124),
                           ('sys.stdout.write("x" * 140000)', 3),
                           ('print("partial"); sys.stderr.write("x" * 70000)', 3)]:
            self.install(body)
            result = self.invoke('map', '--timeout', '1')
            self.assertEqual(code, result.returncode, result.stderr)
            self.assertEqual(b'', result.stdout)

    def test_timeout_kills_descendant_with_inherited_pipes(self):
        self.install('import subprocess\nsubprocess.Popen([sys.executable, "-c", '
                     '"import time; from pathlib import Path; time.sleep(2); Path(\'LEAK\').touch()"]); '
                     'time.sleep(20)\n')
        result = self.invoke('map', '--timeout', '1')
        self.assertEqual(124, result.returncode)
        time.sleep(1.2)
        self.assertFalse((self.root / 'LEAK').exists())

    def test_cancellation_reaps_detached_children_before_unlock(self):
        for sig in (signal.SIGTERM, signal.SIGINT):
            with self.subTest(signal=sig):
                ready = self.root / 'READY'
                ready.unlink(missing_ok=True)
                self.install('import subprocess\nsubprocess.Popen([sys.executable, "-c", '
                             '"import time; from pathlib import Path; time.sleep(1); Path(\'LEAK\').touch()"]); '
                             'open("READY", "w").close(); time.sleep(20)\n')
                proc = subprocess.Popen([sys.executable, str(self.root / 'scripts/ripwire'), 'map'],
                                        cwd=self.root, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        env={**os.environ, 'PLURX_RIPWIRE_DISABLED': '0'})
                try:
                    deadline = time.monotonic() + 5
                    while not ready.exists() and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertTrue(ready.exists(), 'query did not start')
                    state = self.root / 'target/ripwire'
                    with (state / 'run.lock').open('a') as handle:
                        with self.assertRaises(BlockingIOError):
                            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    proc.send_signal(sig)
                    out, err = proc.communicate(timeout=3)
                    self.assertEqual(128 + sig, proc.returncode, err)
                    self.assertEqual(b'', out)
                    with (state / 'run.lock').open('a') as handle:
                        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    time.sleep(1.1)
                    self.assertFalse((self.root / 'LEAK').exists())
                finally:
                    if proc.poll() is None:
                        proc.kill()
                    proc.communicate()

    def test_smoke_identities_include_nested_map_symbols(self):
        smoke = runpy.run_path(str(ROOT / 'scripts/ripwire-smoke'))
        identities = smoke['identities']
        old = b'<r><f p="lib/a.rs"><s n="old" id="a::old"/><c n="edge"/></f></r>'
        new = b'<r><f p="lib/a.rs"><s n="new" id="a::new"/></f></r>'
        self.assertIn(('lib/a.rs', 'old', '', 'a::old'), identities(old))
        self.assertNotEqual(identities(old), identities(new))
        self.assertNotIn(('lib/a.rs', 'edge', '', ''), identities(old))
        self.assertEqual([('a.rs', 'f', '2', 'a::f')],
                         identities(b'<r><s p="a.rs" n="f" l="2" id="a::f"/></r>'))

    def test_lock_contention_is_bounded(self):
        self.install()
        state = self.root / 'target/ripwire'
        with (state / 'run.lock').open('a') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            start = time.monotonic()
            for mode in ('map', 'setup'):
                self.assertEqual(75, self.invoke(mode).returncode)
            self.assertLess(time.monotonic() - start, 6)
        self.assertEqual(0, self.invoke('map').returncode)

    def test_pr_context_validates_base_and_cleanliness(self):
        self.install()
        result = self.invoke('pr-context', '--base', 'HEAD')
        self.assertEqual(0, result.returncode, result.stderr)
        record = self.provenance(result)
        self.assertEqual(self.git('rev-parse', 'HEAD'), record['merge_base'])
        self.assertIn('--pr-context=' + record['resolved_ref'], json.loads(result.stdout)['argv'])
        self.assertEqual(2, self.invoke('pr-context', '--base=--help').returncode)
        self.assertEqual(2, self.invoke('pr-context', '--base', 'missing-ref').returncode)
        for stage in ('untracked', 'staged', 'unstaged'):
            (self.root / 'change').write_text(stage)
            if stage == 'staged':
                self.git('add', 'change')
            if stage == 'unstaged':
                self.git('commit', '-qm', 'tracked change')
                (self.root / 'change').write_text('changed again')
            self.assertEqual(2, self.invoke('pr-context', '--base', 'HEAD').returncode)
        self.git('reset', '--hard', '-q', 'HEAD')
        previous = self.git('rev-parse', 'HEAD')
        self.git('checkout', '--orphan', 'unrelated')
        self.git('commit', '--allow-empty', '-qm', 'unrelated')
        self.assertEqual(2, self.invoke('pr-context', '--base', previous).returncode)

    def archive(self, members):
        archive = self.root / 'fixture.tar.gz'
        with tarfile.open(archive, 'w:gz') as bundle:
            for name, kind, data in members:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.size = len(data) if kind == tarfile.REGTYPE else 0
                bundle.addfile(member, io.BytesIO(data) if member.size else None)
        entry = dict(binary_member='release/ripwire', archive_sha256=API['digest'](archive))
        return archive, entry

    def test_archive_exact_member_and_integrity(self):
        valid = ('release/ripwire', tarfile.REGTYPE, b'binary')
        for members in [[valid, valid], [('release/ripwire', tarfile.SYMTYPE, b'')],
                        [('other/ripwire', tarfile.REGTYPE, b'wrong')]]:
            archive, entry = self.archive(members)
            with self.assertRaises(API['Refusal']):
                API['extract_binary'](archive, self.root / 'extracted', entry)
        archive, entry = self.archive([('../outside', tarfile.REGTYPE, b'ignored'), valid])
        API['extract_binary'](archive, self.root / 'extracted', entry)
        self.assertEqual(b'binary', (self.root / 'extracted').read_bytes())
        entry['archive_sha256'] = '0' * 64
        with self.assertRaises(API['Refusal']):
            API['extract_binary'](archive, self.root / 'extracted', entry)

    def test_setup_failure_preserves_previous_generation(self):
        binary = self.install()
        parent = binary.parent.parent
        directory = binary.parent
        old = parent / ('.' + self.key + '-old')
        directory.rename(old)
        directory.symlink_to(old.name, target_is_directory=True)
        original = API['digest'](binary)
        # Force an upgrade attempt without changing the installed bytes.
        changed = json.loads(json.dumps(self.lock))
        changed['platforms'][self.key]['archive_sha256'] = '0' * 64
        setup = API['setup']
        with patch.dict(setup.__globals__, download=lambda url, path: path.write_bytes(b'bad')):
            with self.assertRaises(API['Refusal']):
                setup(self.root, changed, self.key)
        self.assertEqual(original, API['digest'](binary))
        self.assertEqual(0, self.invoke('map').returncode)
        self.assertEqual({old.name, self.key}, {p.name for p in parent.iterdir()})

    def test_version_and_flag_mismatches_are_refused(self):
        binary = self.install()
        for body in ('print("ripwire 9.9.9")',
                     'import sys; print("ripwire 0.5.0" if "--version" in sys.argv else "--for=TASK")'):
            binary.write_text(f'#!{sys.executable}\n' + body + '\n')
            binary.chmod(0o700)
            with self.assertRaises(API['Refusal']) as error:
                API['compatible'](binary, self.root, self.lock)
            self.assertEqual(2, error.exception.code)

    def test_binary_size_refusal_does_not_publish(self):
        archive, entry = self.archive([('release/ripwire', tarfile.REGTYPE, b'12345')])
        extract = API['extract_binary']
        with patch.dict(extract.__globals__, MAX_BINARY=4):
            with self.assertRaises(API['Refusal']):
                extract(archive, self.root / 'too-large', entry)
        self.assertFalse((self.root / 'too-large').exists())

    def test_dual_pipe_pressure_does_not_deadlock(self):
        self.install('sys.stdout.write("o" * 100000); sys.stdout.flush(); '
                     'sys.stderr.write("e" * 60000); sys.stderr.flush()')
        result = self.invoke('map', '--timeout', '3')
        self.assertEqual(0, result.returncode, result.stderr[:1000])
        self.assertEqual(b'o' * 100000, result.stdout)
        self.assertTrue(result.stderr.endswith(b'e' * 60000))

    def test_setup_is_idempotent_and_publishes_metadata_with_binary(self):
        directory = self.root / 'target/ripwire/tools/0.5.0' / self.key
        fake = self.install()
        contents = fake.read_bytes()
        shutil.rmtree(directory)
        member = self.lock['platforms'][self.key]['binary_member']
        archive, entry = self.archive([(member, tarfile.REGTYPE, contents)])
        lock = json.loads(json.dumps(self.lock))
        lock['platforms'][self.key]['archive_sha256'] = entry['archive_sha256']
        setup = API['setup']
        def local_download(url, target):
            shutil.copy2(archive, target)
        with patch.dict(setup.__globals__, download=local_download):
            first = setup(self.root, lock, self.key)
        self.assertEqual('installed', first['outcome'])
        self.assertTrue(directory.is_symlink())
        installed, metadata = API['installed'](self.root, lock, self.key, check_cli=True)
        self.assertEqual(hashlib.sha256(contents).hexdigest(), metadata['binary_sha256'])
        with patch.dict(setup.__globals__, download=lambda *_: self.fail('idempotent setup downloaded')):
            self.assertEqual('already-installed', setup(self.root, lock, self.key)['outcome'])
        self.assertEqual(contents, installed.read_bytes())

    def test_cleanup_failure_reports_the_published_installation(self):
        binary = self.install()
        directory = binary.parent
        old = directory.with_name('.' + self.key + '-old')
        directory.rename(old)
        directory.symlink_to(old.name, target_is_directory=True)
        contents = binary.read_bytes() + b'# replacement generation\n'
        member = self.lock['platforms'][self.key]['binary_member']
        archive, entry = self.archive([(member, tarfile.REGTYPE, contents)])
        lock = json.loads(json.dumps(self.lock))
        lock['platforms'][self.key]['archive_sha256'] = entry['archive_sha256']
        setup = API['setup']
        remove = shutil.rmtree
        def removal(path, *args, **kwargs):
            if path == old.resolve():
                raise PermissionError('fixture cleanup refusal')
            return remove(path, *args, **kwargs)
        with patch.dict(setup.__globals__, download=lambda url, target: shutil.copy2(archive, target)):
            with patch('shutil.rmtree', side_effect=removal):
                result = setup(self.root.resolve(), lock, self.key)
        self.assertEqual('installed', result['outcome'])
        self.assertEqual(str(old.resolve()), result['cleanup_pending']['path'])
        self.assertTrue(old.exists())
        installed, metadata = API['installed'](self.root, lock, self.key, check_cli=True)
        self.assertEqual(contents, installed.read_bytes())
        self.assertEqual(hashlib.sha256(contents).hexdigest(), metadata['binary_sha256'])

    def test_unsupported_host_and_invalid_lock(self):
        with patch('platform.system', return_value='Unsupported'):
            with self.assertRaises(API['Refusal']):
                API['host_platform']()
        lock = self.root / 'scripts/ripwire.lock.json'
        lock.write_text('{"version": "../../escape"}')
        self.assertEqual(2, self.invoke('doctor').returncode)


if __name__ == '__main__':
    unittest.main()
