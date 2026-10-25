"""Group-editable source inputs are distinct from private desktop authority."""
import contextlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import pwd
import stat
import tempfile
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('compose_group_write_installer', str(ROOT / 'scripts/install-compose-tray'))
spec = importlib.util.spec_from_loader(loader.name, loader)
installer = importlib.util.module_from_spec(spec)
loader.exec_module(installer)
control = installer.permission_policy()
UID = os.getuid()
BASE = 'name: selected\nservices:\n  plurxd:\n    restart: "${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}"\n  plurx-discovery:\n    restart: "${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}"\n'


def meta(mode, uid=UID, gid=2000):
    return types.SimpleNamespace(st_mode=stat.S_IFDIR | mode, st_uid=uid, st_gid=gid)


class ComposeGroupWriteTests(unittest.TestCase):
    @contextlib.contextmanager
    def tree(self, changed=None):
        paths = {Path('/'): meta(0o755, 0, 0), Path('/home'): meta(0o755, 0, 0),
                 Path('/home/operator'): meta(0o750), Path('/home/operator/cinema'): meta(0o775),
                 Path('/home/operator/cinema/deploy'): meta(0o775)}
        paths.update(changed or {})
        with mock.patch.object(Path, 'is_symlink', return_value=False), mock.patch.object(Path, 'exists', autospec=True, side_effect=lambda path: path in paths), mock.patch.object(Path, 'stat', autospec=True, side_effect=paths.__getitem__):
            yield Path('/home/operator/cinema/deploy')

    @contextlib.contextmanager
    def inputs(self):
        with tempfile.TemporaryDirectory() as work:
            checkout = Path(work).resolve()
            deploy = checkout / 'deploy'
            deploy.mkdir()
            (deploy / 'docker-compose.yml').write_text(BASE)
            (deploy / 'docker-compose.override.yml').write_text('services:\n  plurxd:\n    volumes: []\n')
            (deploy / '.env').write_text('SECRET=fixture\n')
            checkout.chmod(0o775)
            deploy.chmod(0o775)
            for path in deploy.iterdir():
                path.chmod(0o664)
            with mock.patch.dict(os.environ, {}, clear=True):
                yield checkout, deploy

    def test_explicit_owned_source_accepts_group_write_without_membership_guessing(self):
        # Incomplete NSS enumeration is immaterial: this is explicitly selected
        # source, not a claim that a shared group has no other members.
        with self.tree() as path, mock.patch.object(pwd, 'getpwall', side_effect=OSError('NSS enumeration disabled')) as lookup:
            self.assertEqual(installer.safe_path(path), path)
            lookup.assert_not_called()

    def test_desktop_destinations_keep_group_write_protection(self):
        # Historical regression name retained: owned desktop paths may now be
        # group editable; foreign ownership remains a separate refusal.
        with self.tree() as path:
            self.assertEqual(installer.safe_path(path), path)
        with self.tree({Path('/home/operator/cinema'): meta(0o775, UID + 1)}) as path:
            with self.assertRaisesRegex(ValueError, 'belongs to another user'):
                installer.safe_path(path)

    def test_world_writable_source_names_the_offending_parent(self):
        with self.tree({Path('/home/operator/cinema'): meta(0o777)}) as path:
            with self.assertRaisesRegex(ValueError, '/home/operator/cinema .*mode=0777'):
                installer.safe_path(path)

    def test_foreign_source_ownership_names_the_offending_parent(self):
        with self.tree({Path('/home/operator/cinema'): meta(0o755, UID + 1)}) as path:
            with self.assertRaisesRegex(ValueError, 'belongs to another user: /home/operator/cinema '):
                installer.safe_path(path)

    def test_group_writable_base_override_and_environment_are_accepted_unchanged(self):
        with self.inputs() as (checkout, deploy):
            before = {path: (path.stat().st_mode, path.read_bytes()) for path in deploy.iterdir()}
            self.assertEqual(installer.safe_path(deploy), deploy)
            environment, _, _ = control.compose_environment(str(checkout), 'selected')
            self.assertEqual(environment, deploy / '.env')
            self.assertEqual(before, {path: (path.stat().st_mode, path.read_bytes()) for path in deploy.iterdir()})
            self.assertEqual(stat.S_IMODE(checkout.stat().st_mode), 0o775)

    def test_world_writable_yaml_names_each_selected_file(self):
        with self.inputs() as (checkout, deploy):
            for name in ('docker-compose.yml', 'docker-compose.override.yml'):
                with self.subTest(name=name):
                    path = deploy / name
                    path.chmod(0o666)
                    with self.assertRaisesRegex(control.ControlError, name + r'.*mode=0666'):
                        control.compose_environment(str(checkout), 'selected')
                    path.chmod(0o664)

    def test_world_writable_environment_names_the_file(self):
        with self.inputs() as (checkout, deploy):
            (deploy / '.env').chmod(0o666)
            with self.assertRaisesRegex(control.ControlError, r'\.env .*mode=0666'):
                control.compose_environment(str(checkout), 'selected')

    def test_symlinked_compose_input_is_still_refused(self):
        with self.inputs() as (checkout, deploy):
            path = deploy / 'docker-compose.override.yml'
            path.rename(deploy / 'target.yml')
            path.symlink_to('target.yml')
            with self.assertRaises(OSError):
                control.compose_environment(str(checkout), 'selected')

    def test_group_writable_executable_symlink_is_refused_without_acl_exception(self):
        with tempfile.TemporaryDirectory() as work:
            target = Path(work) / 'target'
            target.write_text('#!/bin/sh\n')
            target.chmod(0o775)
            link = Path(work) / 'docker'
            link.symlink_to(target)
            with self.assertRaises(control.ControlError):
                control.trusted_executable(str(link))

    def test_non_group_writable_executable_symlink_remains_supported(self):
        with tempfile.TemporaryDirectory() as work:
            target = Path(work) / 'target'
            target.write_text('#!/bin/sh\n')
            target.chmod(0o755)
            link = Path(work) / 'docker'
            link.symlink_to(target)
            self.assertEqual(control.trusted_executable(str(link)), link)

    def test_private_state_is_not_relaxed_for_group_editable_source(self):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work).resolve()
            path.chmod(0o775)
            with mock.patch.object(installer, 'safe_path', side_effect=lambda value: value):
                with self.assertRaisesRegex(ValueError, 'private user-owned directory'):
                    installer.private_dir(path)
            with self.assertRaises(control.ControlError):
                control.private_directory(path)

    def test_root_owned_group_editable_source_does_not_gain_owner_exception(self):
        with mock.patch.object(control.os, 'getuid', return_value=1000):
            self.assertTrue(control.unsafe_source_mode(meta(0o775, uid=0)))
            self.assertFalse(control.unsafe_source_mode(meta(0o775, uid=1000)))

    def test_source_preflight_refuses_bad_yaml_before_build_or_installation(self):
        with self.inputs() as (checkout, deploy):
            (deploy / 'docker-compose.yml').chmod(0o666)
            server, discovery = 'a' * 64, 'b' * 64
            def output(argv, **kwargs):
                if 'ps' in argv:
                    return server if argv[-1] == 'plurxd' else discovery
                return json.dumps({'project': 'selected', 'service': 'plurxd' if argv[-1] == server else 'plurx-discovery',
                                   'working_dir': str(deploy), 'ports': [{'HostIp': '127.0.0.1', 'HostPort': '32400'}]})
            args = types.SimpleNamespace(checkout=checkout)
            helper = types.SimpleNamespace(eligible_session=lambda: None)
            with mock.patch.object(installer.sys, 'platform', 'linux'), mock.patch.object(installer, 'native_helper', return_value=helper), mock.patch.object(installer.shutil, 'which', return_value='/usr/bin/docker'), mock.patch.dict(os.environ, {'DOCKER_HOST': 'unix:///run/docker.sock'}), mock.patch.object(installer, 'output', side_effect=output), mock.patch.object(installer, 'build_stage') as build, mock.patch.object(installer, 'private_dir') as state:
                with self.assertRaisesRegex(ValueError, 'docker-compose.yml.*mode=0666'):
                    installer.install(args)
                build.assert_not_called()
                state.assert_not_called()

    def test_startup_policy_update_preserves_group_write_despite_umask(self):
        with self.inputs() as (checkout, deploy):
            record = {'checkout': str(checkout), 'project': 'selected', 'docker': '/usr/bin/docker', 'endpoint': 'unix:///run/docker.sock'}
            _, record['environment_sha256'], record['compose_sha256'] = control.compose_environment(str(checkout), 'selected')
            backend = types.SimpleNamespace(record=record, path=checkout / 'unused-record')
            previous = os.umask(0o077)
            try:
                with mock.patch.object(control, 'verify_effective_restart'), mock.patch.object(control, 'save'):
                    control.update_environment(backend, 'no')
            finally:
                os.umask(previous)
            self.assertEqual(stat.S_IMODE((deploy / '.env').stat().st_mode), 0o664)
            self.assertEqual((deploy / '.env').read_text(), 'SECRET=fixture\nPLURX_CONTAINER_RESTART_POLICY=no\n')
