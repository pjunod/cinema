"""Pinned snapshot transport controls; no network or APT mutations."""
from contextlib import redirect_stderr
import io
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from validation import apt_snapshot as apt


class AptSnapshotCase(unittest.TestCase):
    def test_https_preference_preserves_exact_selection_trust_and_unrelated_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            parts = root / 'sources.list.d'
            parts.mkdir()
            original = (b'# exact snapshot date and trust remain unchanged\r\n'
                        b'deb [signed-by=/usr/share/keyrings/debian-archive-keyring.gpg check-valid-until=no] '
                        b'http://snapshot.debian.org/archive/debian/20260901T000000Z bookworm main\r\n'
                        b'deb http://deb.debian.org/debian bookworm main\r\n')
            deb822 = (b'Types: deb\nURIs: http://snapshot.debian.org/archive/debian-security/20260901T000000Z/\n'
                      b'Suites: bookworm-security\nComponents: main\nSigned-By: /keys/debian.gpg\n'
                      b'Check-Valid-Until: no\n')
            sources = {'sources.list': original, 'sources.list.d/exact.list': original,
                       'sources.list.d/security.sources': deb822}
            for name, content in sources.items():
                (root / name).write_bytes(content)
            (root / 'sources.list').chmod(0o640)
            ignored = ['sources.list.d/backup.list.save', 'sources.list.d/notes.txt', 'other.list']
            for name in ignored:
                (root / name).write_bytes(original)
            (parts / 'nested').mkdir()
            (parts / 'nested/nested.list').write_bytes(original)
            self.assertEqual(apt.prefer_snapshot_https(root), 3)
            for name, content in sources.items():
                self.assertEqual((root / name).read_bytes(), content.replace(
                    b'http://snapshot.debian.org/', b'https://snapshot.debian.org/'))
            for name in ignored + ['sources.list.d/nested/nested.list']:
                self.assertEqual((root / name).read_bytes(), original)
            self.assertEqual((root / 'sources.list').stat().st_mode & 0o777, 0o640)
            self.assertEqual(apt.prefer_snapshot_https(root), 0)

    def test_https_preference_does_not_match_foreign_hosts_or_existing_https(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            content = (b'deb https://snapshot.debian.org/archive/debian/20260901T000000Z stable main\n'
                       b'deb http://snapshot.debian.org.evil/archive/debian stable main\n'
                       b'deb http://snapshot.debian.org:80/archive/debian stable main\n'
                       b'deb http://other.example/archive/debian stable main\n')
            (root / 'sources.list').write_bytes(content)
            self.assertEqual(apt.prefer_snapshot_https(root), 0)
            self.assertEqual((root / 'sources.list').read_bytes(), content)

    def test_https_preference_refuses_symlink_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outside = root / 'outside'
            outside.write_bytes(b'http://snapshot.debian.org/archive/debian/')
            (root / 'sources.list').symlink_to(outside)
            with self.assertRaises(ValueError):
                apt.prefer_snapshot_https(root)
            self.assertEqual(outside.read_bytes(), b'http://snapshot.debian.org/archive/debian/')

    def test_https_only_cli_never_runs_apt_and_reports_failure(self):
        with mock.patch.object(apt, 'prefer_snapshot_https', return_value=2) as prefer, \
                mock.patch('subprocess.run') as run, mock.patch('sys.stdout', new_callable=io.StringIO):
            self.assertEqual(apt.main(['--https-sources-only']), 0)
            prefer.assert_called_once_with()
            run.assert_not_called()
        with mock.patch.object(apt, 'prefer_snapshot_https', side_effect=OSError('denied')), \
                mock.patch('subprocess.run') as run, redirect_stderr(io.StringIO()):
            self.assertEqual(apt.main(['--https-sources-only']), 100)
            run.assert_not_called()
        with mock.patch('subprocess.run') as run, redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                apt.main(['--https-sources-only', 'libgtk-4-dev'])
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
