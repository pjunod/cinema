"""Private primary groups do not create another desktop-code writer."""
import contextlib
import errno
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('compose_private_group_installer', str(ROOT / 'scripts/install-compose-tray'))
spec = importlib.util.spec_from_loader(loader.name, loader)
installer = importlib.util.module_from_spec(spec)
loader.exec_module(installer)
control = installer.permission_policy()
UID, GID = 1000, 1000
USER = types.SimpleNamespace(pw_uid=UID, pw_gid=GID, pw_name='operator')


def meta(mode, uid=UID, gid=GID):
    return types.SimpleNamespace(st_mode=stat.S_IFDIR | mode, st_uid=uid, st_gid=gid)


class ComposePrivateGroupTests(unittest.TestCase):
    @contextlib.contextmanager
    def accounts(self, *, members=(), extra=(), acl=False):
        group = types.SimpleNamespace(gr_name='operator', gr_mem=list(members))
        accounts = [USER, *extra]
        by_name = {account.pw_name: account for account in accounts}
        with mock.patch.object(control.os, 'getuid', return_value=UID), mock.patch.object(control.pwd, 'getpwuid', return_value=USER), mock.patch.object(control.grp, 'getgrgid', return_value=group), mock.patch.object(control.pwd, 'getpwnam', side_effect=by_name.__getitem__), mock.patch.object(control.pwd, 'getpwall', return_value=accounts), mock.patch.object(control.os, 'getxattr', create=True, **({'return_value': b'extended-access-acl'} if acl else {'side_effect': OSError(errno.ENODATA, 'no ACL')})):
            yield

    @contextlib.contextmanager
    def tree(self, changed=None):
        paths = {Path('/'): meta(0o755, 0, 0), Path('/home'): meta(0o755, 0, 0),
                 Path('/home/operator'): meta(0o750), Path('/home/operator/cinema'): meta(0o775),
                 Path('/home/operator/cinema/deploy'): meta(0o775)}
        paths.update(changed or {})
        with mock.patch.object(Path, 'is_symlink', return_value=False), mock.patch.object(Path, 'exists', autospec=True, side_effect=lambda path: path in paths), mock.patch.object(Path, 'stat', autospec=True, side_effect=paths.__getitem__):
            yield Path('/home/operator/cinema/deploy')

    def test_private_primary_group_accepts_group_writable_checkout_and_parent(self):
        with self.accounts(), self.tree() as path:
            self.assertEqual(installer.safe_path(path), path)

    def test_supplementary_foreign_group_member_still_refuses(self):
        other = types.SimpleNamespace(pw_uid=2000, pw_gid=2000, pw_name='guest')
        with self.accounts(members=['guest'], extra=[other]), self.tree() as path:
            with self.assertRaisesRegex(ValueError, '/home/operator/cinema/deploy.*mode=0775'):
                installer.safe_path(path)

    def test_foreign_primary_group_member_still_refuses(self):
        other = types.SimpleNamespace(pw_uid=2000, pw_gid=GID, pw_name='guest')
        with self.accounts(extra=[other]), self.tree() as path:
            with self.assertRaisesRegex(ValueError, 'shared group'):
                installer.safe_path(path)

    def test_world_writable_checkout_still_refuses_with_exact_path(self):
        with self.accounts(), self.tree({Path('/home/operator/cinema'): meta(0o777)}) as path:
            with self.assertRaisesRegex(ValueError, '/home/operator/cinema .*mode=0777'):
                installer.safe_path(path)

    def test_foreign_owned_checkout_is_distinct_from_write_permissions(self):
        with self.accounts(), self.tree({Path('/home/operator/cinema'): meta(0o755, 2000)}) as path:
            with self.assertRaisesRegex(ValueError, 'belongs to another user: /home/operator/cinema .*uid=2000'):
                installer.safe_path(path)

    def test_rejected_ancestor_is_named_instead_of_reporting_leaf(self):
        with self.accounts(), self.tree({Path('/home/operator'): meta(0o770, UID, 2000)}) as path:
            with self.assertRaisesRegex(ValueError, 'shared group: /home/operator .*gid=2000'):
                installer.safe_path(path)

    def test_unknown_group_database_fails_conservatively(self):
        with self.accounts(), self.tree() as path, mock.patch.object(control.pwd, 'getpwall', side_effect=OSError('NSS unavailable')):
            with self.assertRaisesRegex(ValueError, 'shared group'):
                installer.safe_path(path)

    def test_extended_access_acl_does_not_get_private_group_exception(self):
        with self.accounts(acl=True), self.tree() as path:
            with self.assertRaisesRegex(ValueError, 'shared group'):
                installer.safe_path(path)

    def test_root_sticky_shared_parent_remains_supported(self):
        with self.accounts(), self.tree({Path('/home'): meta(0o1777, 0, 0)}) as path:
            self.assertEqual(installer.safe_path(path), path)

    def environment(self, shared=False):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / '.env'
            path.write_bytes(b'PLURX_DATA=/media/state\n')
            info = types.SimpleNamespace(st_mode=stat.S_IFREG | 0o664, st_uid=UID,
                                         st_gid=GID, st_nlink=1, st_size=path.stat().st_size)
            other = types.SimpleNamespace(pw_uid=2000, pw_gid=GID, pw_name='guest')
            with self.accounts(extra=[other] if shared else []), mock.patch.object(control.os, 'fstat', return_value=info):
                return control.read_environment(path)

    def test_private_group_writable_compose_environment_is_read_without_mutation(self):
        content, info = self.environment()
        self.assertEqual(content, b'PLURX_DATA=/media/state\n')
        self.assertEqual(stat.S_IMODE(info.st_mode), 0o664)

    def test_shared_group_writable_environment_still_refuses_with_path(self):
        with self.assertRaisesRegex(control.ControlError, r'\.env .*mode=0664'):
            self.environment(shared=True)

    def test_private_installation_state_still_requires_private_mode(self):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work)
            with self.accounts(), mock.patch.object(installer, 'safe_path', side_effect=lambda value: value), mock.patch.object(Path, 'stat', return_value=meta(0o775)):
                with self.assertRaisesRegex(ValueError, 'private user-owned directory'):
                    installer.private_dir(path)

    def test_current_users_private_group_executable_is_trusted(self):
        with self.accounts(), mock.patch.object(Path, 'stat', return_value=types.SimpleNamespace(st_mode=stat.S_IFREG | 0o775, st_uid=UID, st_gid=GID)), mock.patch.object(control.os, 'access', return_value=True):
            self.assertEqual(control.trusted_executable('/home/operator/bin/docker'), Path('/home/operator/bin/docker'))

    def test_shared_group_executable_remains_untrusted(self):
        other = types.SimpleNamespace(pw_uid=2000, pw_gid=GID, pw_name='guest')
        with self.accounts(extra=[other]), mock.patch.object(Path, 'stat', return_value=types.SimpleNamespace(st_mode=stat.S_IFREG | 0o775, st_uid=UID, st_gid=GID)), mock.patch.object(control.os, 'access', return_value=True):
            with self.assertRaises(control.ControlError):
                control.trusted_executable('/home/operator/bin/docker')
