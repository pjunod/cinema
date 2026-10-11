"""Normal-user HDMI installation and bounded diagnostic contracts; no Pi required."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import time
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('pi_player', str(ROOT / 'deploy/pi-player'))
spec = importlib.util.spec_from_loader(loader.name, loader)
player = importlib.util.module_from_spec(spec)
loader.exec_module(player)


class PiPlayerTests(unittest.TestCase):
    def test_url_rejects_credentials_tokens_and_shell_arguments(self):
        for url in ('http://user:password@host', 'http://host/?token=secret',
                    'http://host/#secret', 'http://host/token', 'file:///tmp/x',
                    'http://host/;touch /tmp/x', 'http://host\n--no-sandbox',
                    'http://host:99999', 'http://host/%3ftoken'):
            with self.subTest(url=url), self.assertRaises(ValueError):
                player.web_url(url)
        for url in ('http://localhost:32400', 'https://media.example/', 'http://[::1]:32400/'):
            self.assertEqual(player.web_url(url), url)

    def test_install_uninstall_preserves_profile_and_refuses_modified_files(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            with patch.object(Path, 'home', return_value=home), contextlib.redirect_stdout(io.StringIO()):
                args = types.SimpleNamespace(url='http://localhost:32400', chromium=sys.executable, dry_run=False)
                player.install(args)
                state, desktop = player.locations()
                profile = state / 'profile'
                profile.mkdir()
                (profile / 'login').write_text('retained')
                player.install(args)
                original = desktop.read_text()
                desktop.write_text(original + '# operator edit\n')
                with self.assertRaises(ValueError):
                    player.uninstall(args)
                self.assertTrue((state / 'launcher').exists())
                desktop.write_text(original)
                player.uninstall(args)
                player.uninstall(args)
                self.assertFalse(desktop.exists())
                self.assertEqual((profile / 'login').read_text(), 'retained')
                player.install(args)
                self.assertTrue(desktop.exists())
                player.uninstall(args)
                desktop.write_text('unrelated application')
                with self.assertRaises(ValueError):
                    player.install(args)
                self.assertEqual(desktop.read_text(), 'unrelated application')

    def test_autostart_owned_entry_is_verified_and_removed_without_profile_loss(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            with patch.object(Path, 'home', return_value=home), contextlib.redirect_stdout(io.StringIO()):
                args = types.SimpleNamespace(url='http://localhost:32400', chromium=sys.executable, dry_run=False, autostart=True)
                player.install(args)
                autostart = player.autostart_path()
                self.assertEqual(autostart.read_text(), player.locations()[1].read_text())
                original = autostart.read_text()
                autostart.write_text(original + '# user change\n')
                with self.assertRaises(ValueError):
                    player.uninstall(args)
                autostart.write_text(original)
                profile = player.locations()[0] / 'profile'
                profile.mkdir()
                (profile / 'login').write_text('retained')
                player.uninstall(args)
                self.assertFalse(autostart.exists())
                self.assertEqual((profile / 'login').read_text(), 'retained')
                autostart.write_text('another app')
                with self.assertRaises(ValueError):
                    player.install(args)
                self.assertEqual(autostart.read_text(), 'another app')

    def test_dry_run_and_symlink_refusal(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            with patch.object(Path, 'home', return_value=home), contextlib.redirect_stdout(io.StringIO()):
                args = types.SimpleNamespace(url='http://localhost:32400', chromium=sys.executable, dry_run=True)
                player.install(args)
                state, _ = player.locations()
                self.assertFalse(state.exists())
                state.parent.mkdir(parents=True)
                target = home / 'other'
                target.mkdir()
                state.symlink_to(target)
                with self.assertRaises(ValueError):
                    player.install(args)
                self.assertEqual(list(target.iterdir()), [])

    def test_launch_uses_argument_vector_isolated_profile_and_normal_sandbox(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(Path, 'home', return_value=Path(directory).resolve()), contextlib.redirect_stdout(io.StringIO()):
                args = types.SimpleNamespace(url='http://localhost:32400', chromium=sys.executable, dry_run=False)
                player.install(args)
                with patch.dict(player.os.environ, {'DISPLAY': ':1'}), patch.object(player.os, 'execv') as execute:
                    player.launch()
                binary, argv = execute.call_args.args
                self.assertEqual(binary, str(Path(sys.executable).resolve()))
                self.assertIn('--start-fullscreen', argv)
                self.assertIn('--user-data-dir=' + str(player.locations()[0] / 'profile'), argv)
                self.assertNotIn('--no-sandbox', argv)
                self.assertEqual(argv[-1], args.url)
        with patch.object(player.os, 'getuid', return_value=0), patch.object(sys, 'argv', ['pi-player', 'launch']), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(player.main(), 1)

    def test_diagnostics_missing_timeout_and_output_bounds(self):
        with patch.object(player.shutil, 'which', return_value=None):
            self.assertEqual(player.tool_report('ffmpeg', None)['status'], 'missing')
        started = time.monotonic()
        status, _ = player.bounded([sys.executable, '-c', 'import time; time.sleep(10)'], timeout=.1)
        self.assertEqual(status, 'timeout')
        self.assertLess(time.monotonic() - started, 2)
        status, output = player.bounded([sys.executable, '-c', 'print("x" * 50000)'], limit=1024)
        self.assertEqual(status, 'output_limit')
        self.assertLessEqual(len(output), 1024)

    def test_diagnostic_receipt_separates_candidates_and_discards_arbitrary_output(self):
        outputs = [('ok', 'ffmpeg version 7.1.1\nSECRET=password'), ('ok', 'Hardware acceleration methods:\ndrm\nSECRET=password\n'), ('ok', 'ffprobe version 7.1.1'), ('ok', 'Chromium 123.4')]
        args = types.SimpleNamespace(ffmpeg='/trusted/ffmpeg', ffprobe='/trusted/ffprobe', chromium='/trusted/chromium', json=True)
        captured = io.StringIO()
        with patch.object(player, 'bounded', side_effect=outputs), patch.object(player.glob, 'glob', return_value=[]), contextlib.redirect_stdout(captured):
            player.readiness(args)
        report = json.loads(captured.getvalue())
        self.assertEqual(report['hardware_decode'], 'unproven')
        self.assertEqual(report['ffmpeg']['advertised_candidates'], ['drm'])
        self.assertNotIn('SECRET', captured.getvalue())
        self.assertNotIn('/trusted/', captured.getvalue())


if __name__ == '__main__':
    unittest.main()
