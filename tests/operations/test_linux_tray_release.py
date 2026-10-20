"""Prebuilt tray installation needs runtime libraries, never an implicit compiler."""
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(name, file):
    loader = importlib.machinery.SourceFileLoader(name, str(ROOT / 'scripts' / file))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    import sys
    sys.modules[name] = module
    loader.exec_module(module)
    return module


release = load('tray_release_tests', 'linux-tray-release')
runtime = load('tray_release_runtime_tests', 'install-linux-source-runtime')
COMMIT = 'a' * 40
VERSION = '0.7.1'


class LinuxTrayReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / 'binary'
        self.binary.write_bytes(bytes.fromhex('7f454c460201') + bytes(12) + bytes.fromhex('3e00'))
        self.icons = self.root / 'icons'
        self.icons.mkdir()
        for name in release.ICONS:
            (self.icons / name).write_text('<svg/>')
        self.archive = release.package(self.binary, self.icons, self.root / 'assets', VERSION, COMMIT, 'amd64')
        self.content = self.archive.read_bytes()
        self.name = self.archive.name
        self.digest = hashlib.sha256(self.content).hexdigest()

    def download(self, url, limit):
        if url.endswith('/latest'):
            return json.dumps({'tag_name': 'v' + VERSION, 'draft': False, 'prerelease': False}).encode()
        if url.endswith('/SHA256SUMS'):
            return (self.digest + '  ' + self.name + '\n').encode()
        if url.endswith('/' + self.name):
            return self.content
        raise AssertionError(url)

    def stage(self):
        with mock.patch.object(release, 'architecture', return_value='amd64'), mock.patch.object(release, 'fetch', side_effect=self.download):
            return release.stage(self.root / 'staged')

    def test_latest_stable_archive_round_trip_has_complete_identity_and_icons(self):
        receipt = self.stage()
        self.assertEqual(receipt['source_commit'], COMMIT)
        self.assertEqual(receipt['sha256'], self.digest)
        self.assertEqual(receipt['origin'], 'github-release')
        self.assertEqual((self.root / 'staged/plurx-tray').read_bytes(), self.binary.read_bytes())
        self.assertEqual({path.name for path in (self.root / 'staged/icons').iterdir()}, set(release.ICONS))

    def test_arm64_release_is_selected_and_rejects_amd64_payload(self):
        with mock.patch.object(release.platform, 'machine', return_value='aarch64'):
            self.assertEqual(release.architecture(), 'arm64')
        with self.assertRaisesRegex(ValueError, 'architecture'):
            release.validate_binary(self.binary.read_bytes(), 'arm64')

    def test_missing_and_duplicate_asset_checksums_refuse_without_staging(self):
        for contents in (b'', ((self.digest + '  ' + self.name + '\n') * 2).encode()):
            with self.subTest(contents=contents), self.assertRaisesRegex(ValueError, 'unique checksum'):
                release.checksum(contents, self.name)
        self.assertFalse((self.root / 'staged').exists())

    def test_checksum_mismatch_refuses_before_unpacking(self):
        self.digest = '0' * 64
        with self.assertRaisesRegex(ValueError, 'checksum'):
            self.stage()
        self.assertFalse((self.root / 'staged').exists())

    def test_unstable_tags_and_non_github_redirects_are_refused(self):
        for tag in ('../v0.7.1', 'main', 'v0.7.1-rc1'):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.release_tag(tag)
        for url in ('http://github.com/a', 'https://elsewhere.invalid/a', 'https://user:secret@github.com/a'):
            with self.subTest(url=url), self.assertRaises(ValueError):
                release.public_url(url)

    def archive_with_extra(self, name, link=False):
        value = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(self.content), mode='r:gz') as source, tarfile.open(fileobj=value, mode='w:gz') as target:
            for member in source:
                target.addfile(member, source.extractfile(member))
            member = tarfile.TarInfo(name)
            if link:
                member.type = tarfile.SYMTYPE
                member.linkname = '/outside'
            target.addfile(member, io.BytesIO(b''))
        return value.getvalue()

    def test_archive_links_traversal_and_duplicate_members_never_write(self):
        for name, link in (('../outside', False), ('plurx-tray', True), ('release.json', False)):
            with self.subTest(name=name, link=link), self.assertRaises(ValueError):
                release.unpack(self.archive_with_extra(name, link), self.root / 'staged', VERSION, 'amd64')
            self.assertFalse((self.root / 'staged').exists())

    def test_metadata_version_and_architecture_are_bound_to_selected_release(self):
        for version, arch in (('0.7.2', 'amd64'), (VERSION, 'arm64')):
            with self.subTest(version=version, arch=arch), self.assertRaisesRegex(ValueError, 'identity/protocol'):
                release.unpack(self.content, self.root / 'staged', version, arch)

    def runtime_context(self):
        destination = self.root / 'staged'
        session = types.SimpleNamespace(environment={'HOME': str(self.root)})
        return destination, session

    def test_default_stage_downloads_without_source_or_development_dependencies(self):
        destination, session = self.runtime_context()
        def stage(path, **kwargs):
            return release.unpack(self.content, path, VERSION, 'amd64') | {'tray': str(path / 'plurx-tray')}
        module = types.SimpleNamespace(stage=mock.Mock(side_effect=stage))
        with mock.patch.object(runtime, 'eligible_session', return_value=session), mock.patch.object(runtime, 'owning_user_context'), mock.patch.object(runtime, 'staging_destination'), mock.patch.object(runtime, 'tray_release_module', return_value=module), mock.patch.object(runtime, 'native_format', return_value='deb'), mock.patch.object(runtime, 'output', return_value=f'plurx-tray {VERSION} ({COMMIT})'), mock.patch.object(runtime, 'run') as run, mock.patch.object(runtime, 'build_context') as build:
            runtime.stage_tray(destination)
            build.assert_not_called()
            module.stage.assert_called_once_with(destination, tag=None)
            argv = run.call_args.args[0]
            self.assertIn('libgtk-4-1', argv)
            self.assertFalse(any('dev' in value or 'cargo' in value or 'rust' in value for value in argv))

    def test_missing_release_does_not_fall_back_to_compilation_or_package_changes(self):
        destination, session = self.runtime_context()
        module = types.SimpleNamespace(stage=mock.Mock(side_effect=ValueError('no published tray')))
        with mock.patch.object(runtime, 'eligible_session', return_value=session), mock.patch.object(runtime, 'owning_user_context'), mock.patch.object(runtime, 'staging_destination'), mock.patch.object(runtime, 'tray_release_module', return_value=module), mock.patch.object(runtime, 'run') as run, mock.patch.object(runtime, 'build_context') as build:
            with self.assertRaisesRegex(ValueError, 'no published tray'):
                runtime.stage_tray(destination)
            run.assert_not_called()
            build.assert_not_called()

    def test_runtime_install_failure_removes_only_candidate(self):
        destination, session = self.runtime_context()
        def stage(path, **kwargs):
            path.mkdir()
            return {'version': VERSION, 'source_commit': COMMIT}
        module = types.SimpleNamespace(stage=stage)
        with mock.patch.object(runtime, 'eligible_session', return_value=session), mock.patch.object(runtime, 'owning_user_context'), mock.patch.object(runtime, 'staging_destination'), mock.patch.object(runtime, 'tray_release_module', return_value=module), mock.patch.object(runtime, 'native_format', return_value='deb'), mock.patch.object(runtime, 'tray_runtime_dependencies', side_effect=ValueError('package conflict')):
            with self.assertRaisesRegex(ValueError, 'package conflict'):
                runtime.stage_tray(destination)
        self.assertFalse(destination.exists())

    def test_native_cohort_requires_exact_tray_source_before_any_compiler(self):
        module = types.SimpleNamespace(stage=mock.Mock(return_value={'version': VERSION, 'source_commit': 'b' * 40}))
        args = types.SimpleNamespace(build_tray=False, tray_release=None)
        with mock.patch.object(runtime, 'source_identity', return_value=(COMMIT, VERSION)), mock.patch.object(runtime, 'tray_release_module', return_value=module), mock.patch.object(runtime, 'build_context') as build:
            with self.assertRaisesRegex(ValueError, 'exact matching tray'):
                runtime.prepare(self.root, args, 'deb')
            build.assert_not_called()
            module.stage.assert_called_once_with(self.root / 'released-tray', tag='v' + VERSION)

    def test_dry_run_does_not_download_build_or_install(self):
        destination, session = self.runtime_context()
        with mock.patch.object(runtime, 'eligible_session', return_value=session), mock.patch.object(runtime, 'owning_user_context'), mock.patch.object(runtime, 'staging_destination'), mock.patch.object(runtime, 'tray_release_module') as download, mock.patch.object(runtime, 'build_context') as build, mock.patch.object(runtime, 'run') as run:
            receipt = runtime.stage_tray(destination, dry_run=True, release='v' + VERSION)
            self.assertIn('prebuilt', receipt['build'])
            download.assert_not_called()
            build.assert_not_called()
            run.assert_not_called()

    def test_explicit_release_pin_does_not_consult_latest_metadata(self):
        with mock.patch.object(release, 'fetch') as fetch:
            self.assertEqual(release.release_tag('v' + VERSION), 'v' + VERSION)
            fetch.assert_not_called()

    def test_download_byte_bound_is_enforced_before_unpacking(self):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.geturl.return_value = 'https://github.com/pjunod/cinema/releases/download/v0.7.1/archive'
        response.read.return_value = b'abc'
        opener = mock.Mock()
        opener.open.return_value = response
        with mock.patch.object(release.urllib.request, 'build_opener', return_value=opener):
            with self.assertRaisesRegex(ValueError, 'size bound'):
                release.fetch(response.geturl(), 2)
            response.read.assert_called_once_with(3)

    def test_release_upload_includes_archives_and_both_downstream_asset_sets(self):
        import fnmatch
        import re
        workflow = (ROOT / '.github/workflows/github-release.yml').read_text()
        linux_upload = workflow.split('name: release-linux-${{ matrix.arch }}', 1)[1].split('retention-days:', 1)[0]
        patterns = re.findall(r'^            (dist/[^\n]+)$', linux_upload, re.M)
        for arch in ('amd64', 'arm64'):
            name = release.asset_name(VERSION, arch)
            self.assertTrue(any(fnmatch.fnmatch('dist/' + name, pattern) for pattern in patterns))
            self.assertEqual(workflow.count('"plurx-tray-${VERSION}-linux-' + arch + '.tar.gz"'), 2)

    def test_release_container_binds_binary_version_to_resolved_source(self):
        workflow = (ROOT / '.github/workflows/github-release.yml').read_text()
        desktop = workflow.split('name: Build the optional desktop against', 1)[1].split('name: Package the standalone host tray', 1)[0]
        self.assertIn('--env PLURX_BUILD_SHA', desktop)
        self.assertIn('--env VERSION', desktop)
        self.assertIn('test "$(target-desktop/release/plurx-tray --version)" = "plurx-tray $VERSION ($PLURX_BUILD_SHA)"', desktop)

    def test_native_cohort_rejects_binary_stamp_that_contradicts_metadata(self):
        module = types.SimpleNamespace(stage=mock.Mock(return_value={'version': VERSION, 'source_commit': COMMIT, 'tray': '/candidate/plurx-tray'}))
        args = types.SimpleNamespace(build_tray=False, tray_release=None)
        with mock.patch.object(runtime, 'source_identity', return_value=(COMMIT, VERSION)), mock.patch.object(runtime, 'tray_release_module', return_value=module), mock.patch.object(runtime, 'tray_runtime_dependencies'), mock.patch.object(runtime, 'output', return_value=f'plurx-tray {VERSION} (unknown)'), mock.patch.object(runtime, 'build_context') as build:
            with self.assertRaisesRegex(ValueError, 'differs from its artifact identity'):
                runtime.prepare(self.root, args, 'deb')
            build.assert_not_called()
