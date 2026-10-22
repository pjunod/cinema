"""Real Git object proofs over source-only copies; no Rust or host Git needed."""
import base64
import copy
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

from validation import rust_source_bundle as bundle


class RustSourceBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.git('init', '--object-format=sha1', '-q')
        self.git('config', 'user.name', 'Source fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        for name, content in [('a.c', b'before directory'), ('a/b', b'nested'),
                              ('a/z/c', b'deep'), ('a0', b'after directory'),
                              ('run', b'#!/bin/sh\nexit 0\n'), ('plain', b'payload\0bytes')]:
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        (self.repo / 'run').chmod(0o755)
        (self.repo / 'alias').symlink_to('a/b')
        self.git('add', '.')
        self.git('commit', '-qm', 'Source fixture')
        self.manifest = bundle.export(self.repo)
        self.source = self.root / 'source'
        self.source.mkdir()
        archive = self.git('archive', '--format=tar', 'HEAD')
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(self.source, filter='data')
        self.path = self.root / 'manifest.json'
        self.save()

    def git(self, *args):
        return subprocess.check_output(['git', '-c', 'core.hooksPath=/dev/null', '-C', str(self.repo), *args],
                                       stderr=subprocess.PIPE)

    def save(self, manifest=None):
        self.path.write_text(json.dumps(self.manifest if manifest is None else manifest))

    def test_nested_git_sorting_and_raw_commit_match_without_any_git_call(self):
        with mock.patch.object(bundle.subprocess, 'check_output', side_effect=AssertionError('host Git must not run')):
            observed = bundle.identity(self.source, self.path)
        self.assertEqual(observed, dict(commit=self.manifest['commit'], tree=self.manifest['tree'], tracked_clean=True))
        self.assertEqual(observed['tree'], self.git('rev-parse', 'HEAD^{tree}').decode().strip())
        self.assertFalse((self.source / '.git').exists())
        self.assertEqual(set(self.manifest), {'schema', 'commit', 'tree', 'raw_commit_base64', 'entries'})

    def test_missing_mutated_and_symlink_replaced_files_are_refused(self):
        original = (self.source / 'plain').read_bytes()
        for mutation in ('missing', 'bytes', 'symlink'):
            with self.subTest(mutation=mutation):
                path = self.source / 'plain'
                path.unlink()
                if mutation == 'bytes':
                    path.write_bytes(b'other')
                elif mutation == 'symlink':
                    path.symlink_to(self.repo / 'plain')
                with self.assertRaises(bundle.BundleError):
                    bundle.identity(self.source, self.path)
                if path.is_symlink() or path.exists():
                    path.unlink()
                path.write_bytes(original)

    def test_actual_symlink_bytes_and_parent_directory_type_are_bound(self):
        (self.source / 'alias').unlink()
        (self.source / 'alias').symlink_to('a0')
        with self.assertRaises(bundle.BundleError):
            bundle.identity(self.source, self.path)
        (self.source / 'alias').unlink()
        (self.source / 'alias').symlink_to('a/b')
        shutil.rmtree(self.source / 'a')
        (self.source / 'a').symlink_to(self.repo / 'a', target_is_directory=True)
        with self.assertRaises(bundle.BundleError):
            bundle.identity(self.source, self.path)

    def test_executable_mode_drift_in_either_direction_refuses(self):
        for name, mode, restore in [('run', 0o644, 0o755), ('plain', 0o755, 0o644), ('plain', 0o645, 0o644)]:
            with self.subTest(name=name, mode=mode):
                (self.source / name).chmod(mode)
                with self.assertRaises(bundle.BundleError):
                    bundle.identity(self.source, self.path)
                (self.source / name).chmod(restore)

    def test_unknown_source_and_bytecode_are_refused_but_generated_target_is_allowed(self):
        target = self.source / 'target/debug'
        target.mkdir(parents=True)
        (target / 'generated').write_bytes(b'not source')
        self.assertTrue(bundle.identity(self.source, self.path)['tracked_clean'])
        for name in ('unexpected.rs', '__pycache__/module.pyc'):
            path = self.source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('unknown')
            with self.subTest(name=name), self.assertRaises(bundle.BundleError):
                bundle.identity(self.source, self.path)
            path.unlink()
            if path.parent.name == '__pycache__':
                path.parent.rmdir()

    def test_git_content_is_forbidden_even_inside_generated_target(self):
        for name in ('.git', 'target/x/.git', 'target/.GIT'):
            path = self.source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('gitdir: forbidden')
            with self.subTest(name=name), self.assertRaises(bundle.BundleError):
                bundle.identity(self.source, self.path)
            path.unlink()

    def test_manifest_path_traversal_duplicates_gitlinks_and_collisions_refuse(self):
        entry = self.manifest['entries'][0]
        variants = []
        for name in ('../outside', '/absolute', 'a//b', 'a/./b', '.git/config', 'a/../b', 'a\\b'):
            manifest = copy.deepcopy(self.manifest)
            manifest['entries'][0]['path'] = name
            variants.append(manifest)
        manifest = copy.deepcopy(self.manifest)
        manifest['entries'].append(copy.deepcopy(entry))
        variants.append(manifest)
        for mode in ('160000', '040000', '100600'):
            manifest = copy.deepcopy(self.manifest)
            manifest['entries'][0]['mode'] = mode
            variants.append(manifest)
        manifest = copy.deepcopy(self.manifest)
        manifest['entries'].append(dict(path='a', mode='100644', blob=entry['blob']))
        variants.append(manifest)
        for manifest in variants:
            with self.subTest(entries=manifest['entries'][:1]), self.assertRaises(bundle.BundleError):
                self.save(manifest)
                bundle.identity(self.source, self.path)

    def test_raw_commit_tree_and_blob_identity_cannot_be_self_asserted(self):
        for field in ('commit', 'tree', 'raw_commit_base64', 'blob', 'malformed-base64'):
            manifest = copy.deepcopy(self.manifest)
            if field == 'malformed-base64':
                manifest['raw_commit_base64'] = 'not!base64'
            elif field == 'raw_commit_base64':
                raw = base64.b64decode(manifest[field])
                manifest[field] = base64.b64encode(raw + b'changed').decode()
            elif field == 'blob':
                manifest['entries'][0]['blob'] = '1' * 40
            else:
                manifest[field] = '1' * 40
            self.save(manifest)
            with self.subTest(field=field), self.assertRaises(bundle.BundleError):
                bundle.identity(self.source, self.path)

    def test_duplicate_manifest_keys_and_inside_manifest_are_refused(self):
        self.path.write_text('{"schema":1,"schema":1}')
        with self.assertRaises(bundle.BundleError):
            bundle.identity(self.source, self.path)
        inside = self.source / 'manifest.json'
        inside.write_text(json.dumps(self.manifest))
        with self.assertRaises(bundle.BundleError):
            bundle.identity(self.source, inside)

    def test_export_rejects_dirty_and_unknown_ignored_source(self):
        (self.repo / 'plain').write_bytes(b'dirty')
        with self.assertRaises(bundle.BundleError):
            bundle.export(self.repo)
        self.git('checkout', '--', 'plain')
        (self.repo / '.git/info/exclude').write_text('ignored-secret\n')
        (self.repo / 'ignored-secret').write_text('must never export')
        with self.assertRaises(bundle.BundleError):
            bundle.export(self.repo)


if __name__ == '__main__':
    unittest.main()
