"""Ordinary group-editable XDG folders must not prevent a tray installation."""
import argparse
import contextlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('compose_desktop_permissions', str(ROOT / 'scripts/install-compose-tray'))
spec = importlib.util.spec_from_loader(loader.name, loader)
installer = importlib.util.module_from_spec(spec)
loader.exec_module(installer)


class ComposeDesktopPermissionTests(unittest.TestCase):
    @contextlib.contextmanager
    def installation(self, foreign_icons=False):
        with tempfile.TemporaryDirectory() as work:
            home = Path(work).resolve()
            checkout = home / 'cinema'
            shared = [checkout, home / 'data/applications', home / 'config/autostart',
                      home / 'data/icons/hicolor', home / 'data/icons/hicolor/symbolic/apps',
                      home / 'data/icons/hicolor/scalable/apps']
            for path in shared:
                path.mkdir(parents=True, exist_ok=True)
                path.chmod(0o775)
            identity = {'docker': '/usr/bin/docker', 'endpoint': 'unix:///run/docker.sock', 'project': 'selected',
                        'server_id': 'a' * 64, 'discovery_id': 'b' * 64, 'browser_origin': 'http://127.0.0.1:32400',
                        'checkout': str(checkout)}
            uid = os.getuid() or 1000
            real_stat = Path.stat
            def owned_stat(path, *args, **kwargs):
                info = real_stat(path, *args, **kwargs)
                if path.is_relative_to(home):
                    values = list(info)
                    values[4] = uid + 1 if foreign_icons and path == home / 'data/icons/hicolor' else uid
                    return os.stat_result(values)
                return info
            def stage(argv):
                destination = Path(argv[argv.index('--stage-tray') + 1])
                destination.mkdir()
                (destination / 'plurx-tray').write_text('#!/bin/sh\nexit 0\n')
                (destination / 'plurx-tray').chmod(0o755)
                (destination / 'icons').mkdir()
                for name in ('tv.plurx.Server.svg', 'tv.plurx.Server-running-symbolic.svg'):
                    (destination / 'icons' / name).write_text('<svg/>')
                return {'tray': str(destination / 'plurx-tray'), 'source_commit': 'c' * 40}
            def register(argv):
                path = Path(argv[argv.index('--installation') + 1])
                if 'register' in argv:
                    path.write_text(json.dumps({'installation_id': 'fixture'}))
                    path.chmod(0o600)
                else:
                    (home / 'config/autostart' / installer.ENTRY).write_text('[Desktop Entry]\nX-Plurx-Installation=fixture\n')
            args = argparse.Namespace(checkout=checkout, dry_run=False, no_autostart=False, update_mode='source', image_reference=None)
            env = {'XDG_STATE_HOME': str(home / 'state'), 'XDG_DATA_HOME': str(home / 'data'), 'XDG_CONFIG_HOME': str(home / 'config')}
            with mock.patch.dict(os.environ, env), mock.patch.object(installer.os, 'getuid', return_value=uid), mock.patch.object(installer.sys, 'platform', 'linux'), mock.patch.object(installer.Path, 'home', return_value=home), mock.patch.object(Path, 'stat', autospec=True, side_effect=owned_stat), mock.patch.object(installer, 'native_helper', return_value=types.SimpleNamespace(eligible_session=lambda: None)), mock.patch.object(installer, 'select_deployment', return_value=identity), mock.patch.object(installer, 'build_stage', side_effect=stage) as staging, mock.patch.object(installer, 'run', side_effect=register) as registration, mock.patch.object(installer.subprocess, 'Popen') as launch:
                yield home, shared, args, staging, registration, launch

    def test_complete_install_accepts_775_desktop_folders_without_changing_permissions(self):
        with self.installation() as (home, shared, args, _, _, launch):
            before = {path: path.stat().st_mode for path in shared}
            installer.install(args)
            self.assertEqual(before, {path: path.stat().st_mode for path in shared})
            self.assertTrue((home / 'data/icons/hicolor/scalable/apps/tv.plurx.Server.svg').is_file())
            self.assertTrue((home / 'data/icons/hicolor/symbolic/apps/tv.plurx.Server-running-symbolic.svg').is_file())
            self.assertTrue((home / 'config/autostart' / installer.ENTRY).is_file())
            launch.assert_called_once()
            record = next((home / 'state/plurx-compose').glob('*/installation.json'))
            self.assertEqual(record.stat().st_mode & 0o777, 0o600)
            self.assertEqual(record.parent.stat().st_mode & 0o777, 0o700)

    def test_genuinely_foreign_desktop_path_is_reported_before_staging_or_registration(self):
        with self.installation(foreign_icons=True) as (_, _, args, staging, registration, launch):
            with self.assertRaisesRegex(ValueError, 'belongs to another user: .*icons/hicolor'):
                installer.install(args)
            staging.assert_not_called()
            registration.assert_not_called()
            launch.assert_not_called()
