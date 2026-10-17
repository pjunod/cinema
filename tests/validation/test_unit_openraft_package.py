"""Closed package/source and original native-capture refusal controls."""
import base64
import copy
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from contextlib import ExitStack, contextmanager
from unittest.mock import patch
from validation import python_unit_receipts as receipts
from validation import promotion_unit_retention as retention
from validation import qualification_unit_continuation as commands
from validation import unit_openraft_package as package


def blob(raw):
    return {'sha256': retention.digest(raw), 'base64': base64.b64encode(raw).decode()}


def packed(files, extra=None):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode='w:gz') as archive:
        for name, raw in files.items():
            member = tarfile.TarInfo('openraft-0.9.25/' + name)
            member.size = len(raw)
            archive.addfile(member, io.BytesIO(raw))
        if extra is not None:
            archive.addfile(extra)
    return stream.getvalue()


@contextmanager
def fixture():
    files = {'Cargo.toml': b'[package]\nname="openraft"\nversion="0.9.25"\n[lib]\npath="src/lib.rs"\n',
             'Cargo.lock': b'version=4\n', 'src/lib.rs': b'// exact source bytes\n'}
    raw = packed(files)
    checksum = retention.digest(raw)
    lock = ('[[package]]\nname="openraft"\nversion="0.9.25"\nsource="' + package.REGISTRY
            + '"\nchecksum="' + checksum + '"\n').encode()
    make = ('cluster-wal-check: ## finite fixture\n\t' + package.WRAPPER + '\n.PHONY: other\n').encode()
    sources = {'Makefile': make, 'Cargo.lock': lock, 'vendor/hiqlite/Cargo.lock': lock}
    with ExitStack() as patches:
        patches.enter_context(patch.object(package, 'ARCHIVE_SHA', checksum))
        patches.enter_context(patch.object(package, 'MANIFEST_SHA', retention.digest(files['Cargo.toml'])))
        patches.enter_context(patch.object(package, 'LOCK_SHA', retention.digest(files['Cargo.lock'])))
        patches.enter_context(patch.object(retention, 'git_bytes', side_effect=lambda source, path: sources.get(path)))
        yield files, raw, sources


class OpenraftPackageCase(unittest.TestCase):
    def test_source_descriptor_requires_exact_wrapper_and_both_unique_registry_locks(self):
        with fixture() as (_, _, sources):
            descriptor = package.source_descriptor('a' * 40)
            self.assertEqual(descriptor['profile']['debuginfo'], 2)
            self.assertEqual(descriptor['target'], {'name': 'openraft', 'kind': ['lib'], 'path': 'src/lib.rs'})
            for path in ('Makefile', 'Cargo.lock', 'vendor/hiqlite/Cargo.lock'):
                original = sources[path]
                changes = ([original.replace(b'$$OPENRAFT_MANIFEST', b'$$OTHER_MANIFEST'), original * 2]
                           if path == 'Makefile' else [original.replace(b'0.9.25', b'0.9.26'),
                             original.replace(package.REGISTRY.encode(), b'path+file:///tmp/other'),
                             original.replace(package.ARCHIVE_SHA.encode(), b'0' * 64), original * 2])
                for changed in changes:
                    sources[path] = changed
                    with self.subTest(path=path), self.assertRaises(receipts.ReceiptError):
                        package.source_descriptor('a' * 40)
                sources[path] = original

    def test_archive_authenticates_own_manifest_lock_target_and_bounded_members(self):
        with fixture() as (files, raw, _):
            self.assertEqual(package.archive_files(raw), files)
            with self.assertRaises(receipts.ReceiptError): package.archive_files(raw + b'changed')
            with patch.object(package, 'MAX_SOURCE_BYTES', 1), self.assertRaises(receipts.ReceiptError):
                package.archive_files(raw)
            with patch.object(package, 'MAX_MEMBERS', 1), self.assertRaises(receipts.ReceiptError):
                package.archive_files(raw)
            for name in ('Cargo.toml', 'Cargo.lock'):
                modified = {**files, name: files[name] + b'# changed\n'}
                archive = packed(modified)
                with patch.object(package, 'ARCHIVE_SHA', retention.digest(archive)), \
                        self.subTest(name=name), self.assertRaises(receipts.ReceiptError):
                    package.archive_files(archive)
            for name, kind in [('openraft-0.9.25/src/link', tarfile.SYMTYPE),
                               ('openraft-0.9.25/../escape', tarfile.REGTYPE),
                               ('openraft-0.9.25/Cargo.toml', tarfile.REGTYPE)]:
                member = tarfile.TarInfo(name); member.type = kind; member.linkname = 'src/lib.rs'
                archive = packed(files, member)
                with patch.object(package, 'ARCHIVE_SHA', retention.digest(archive)), \
                        self.subTest(name=name), self.assertRaises(receipts.ReceiptError):
                    package.archive_files(archive)

    def metadata(self, root):
        return {'packages': [{'name': 'openraft', 'version': '0.9.25', 'source': package.REGISTRY,
            'id': package.REGISTRY + '#openraft@0.9.25', 'manifest_path': str(root / 'Cargo.toml'),
            'targets': [{'name': 'openraft', 'kind': ['lib'], 'src_path': str(root / 'src/lib.rs')}]}]}

    def test_metadata_refuses_ambiguous_packages_foreign_source_and_target_or_cache_aliases(self):
        root = Path('/tmp/owned/registry/src/index.crates.io-1234567890abcdef/openraft-0.9.25')
        data = self.metadata(root)
        self.assertEqual(package.metadata_package(data), root / 'Cargo.toml')
        changes = [lambda d: d['packages'].append(copy.deepcopy(d['packages'][0])),
                   lambda d: d['packages'][0].update(version='0.9.26'),
                   lambda d: d['packages'][0].update(source='path+file:///tmp/foreign'),
                   lambda d: d['packages'][0].update(id=package.REGISTRY + '#foreign@0.9.25'),
                   lambda d: d['packages'][0]['targets'].append(copy.deepcopy(d['packages'][0]['targets'][0])),
                   lambda d: d['packages'][0]['targets'][0].update(src_path='/tmp/foreign/src/lib.rs'),
                   lambda d: d['packages'][0].update(manifest_path='/tmp/foreign/Cargo.toml')]
        for mutate in changes:
            modified = copy.deepcopy(data); mutate(modified)
            with self.subTest(mutation=mutate), self.assertRaises(receipts.ReceiptError):
                package.metadata_package(modified)

    def test_extracted_source_refuses_missing_changed_extra_files_and_symlinks(self):
        with fixture() as (files, _, _), tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, raw in files.items():
                path = root / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(raw)
            package.check_extracted(root, files)
            for name in files:
                path = root / name; path.write_bytes(files[name] + b'changed')
                with self.subTest(name=name), self.assertRaises(receipts.ReceiptError):
                    package.check_extracted(root, files)
                path.unlink()
                with self.assertRaises(receipts.ReceiptError): package.check_extracted(root, files)
                path.write_bytes(files[name])
            extra = root / 'unexpected.rs'; extra.write_bytes(b'unbound')
            with self.assertRaises(receipts.ReceiptError): package.check_extracted(root, files)
            extra.unlink(); extra.symlink_to('src/lib.rs')
            with self.assertRaises(receipts.ReceiptError): package.check_extracted(root, files)

    def test_current_source_authenticates_cache_then_yields_and_rechecks_owned_bytes(self):
        with fixture() as (files, raw, _), tempfile.TemporaryDirectory() as directory:
            registry = Path(directory) / 'registry'; index = 'index.crates.io-1234567890abcdef'
            root = registry / 'src' / index / 'openraft-0.9.25'
            for name, content in files.items():
                path = root / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(content)
            archive = registry / 'cache' / index / 'openraft-0.9.25.crate'
            archive.parent.mkdir(parents=True); archive.write_bytes(raw)
            (root / 'target').mkdir()
            (root / 'target/untrusted-generated.bin').write_bytes(b'generated only' * 1000)
            calls = []
            def output(argv, **limits):
                calls.append(argv)
                return json.dumps(self.metadata(root))
            with package.current_source('a' * 40, output) as (manifest, descriptor):
                self.assertNotEqual(manifest.parent, root)
                self.assertEqual(manifest.read_bytes(), files['Cargo.toml'])
                self.assertFalse((manifest.parent / 'target').exists())
                self.assertEqual(descriptor['archive_sha256'], retention.digest(raw))
                owned = manifest.parent
            self.assertFalse(owned.exists())
            self.assertEqual(calls[0], ['cargo', 'metadata', '--locked', '--manifest-path',
                                      'vendor/hiqlite/Cargo.toml', '--format-version', '1'])
            (root / 'src/lib.rs').write_bytes(b'changed')
            with self.assertRaises(receipts.ReceiptError):
                with package.current_source('a' * 40, output): self.fail('Unverified source yielded')
            (root / 'src/lib.rs').write_bytes(files['src/lib.rs'])
            with self.assertRaises(receipts.ReceiptError):
                with package.current_source('a' * 40, output) as (manifest, _):
                    (manifest.parent / 'src/lib.rs').write_bytes(b'changed after compilation')

    def test_original_native_capture_keeps_full_wrapper_profile_binary_and_scope(self):
        with fixture():
            source = commands.OPENRAFT_ORIGINAL_SOURCE
            descriptor = package.source_descriptor(source)
            command = package.WRAPPER.removeprefix('@').replace('$$( ', '$( ').replace('$(CARGO)',
                'scripts/require-test-count cargo').replace('$(CURDIR)', '/tmp/plurx-dv-repair/source')
            command = command.replace('$$OPENRAFT_MANIFEST', '$OPENRAFT_MANIFEST')
            binary = 'target/openraft-regression/debug/deps/openraft-813539703a553270'
            raw = ('Canonical WAL ordinal 237 on effort ' + source + '\n' + command + '\n'
                + '     Running unittests src/lib.rs (' + binary + ')\n\nrunning 1 test\ntest '
                + package.SELECTOR + ' ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; '
                + '0 measured; 196 filtered out; finished in 0.01s\n\n')
            observation = {'ordinal': 237, 'execution_command': command, 'canonical_command': package.WRAPPER,
                'source_commit': source, 'log_sha256': retention.digest(raw.encode()), 'exit_code': 0,
                'expected_pass_count': 1, 'terminal_success': True, 'features': 'generic-snapshot-data',
                'running_binaries': [binary]}
            binding = blob(json.dumps({'source_commit': source, 'branch': 'effort/dv-hdr-repair',
                'rustc': commands.RUSTC_VERSION, 'records': [observation]}).encode())
            artifacts = blob(json.dumps({'artifacts': [{'path': binary,
                'sha256': '4f41f4416a03c63c962512cd4fe35bb537e4a85c9767576fa3055de4b23712c4'}],
                'fingerprints': {'target/openraft-regression/debug/.fingerprint/openraft-813539703a553270/'
                    'test-lib-openraft.json': {'rustc': commands.RUSTC_FINGERPRINT,
                    'profile': commands.TEST_PROFILE_FINGERPRINTS[2], 'compile_kind': 0,
                    'rustflags': [], 'features': json.dumps(package.FEATURES)}}}).encode())
            record = {'source_commit': source, 'exit_code': 0,
                'environment': {'platform': 'linux-x64', 'toolchain': 'rust1.97.1',
                    'features': package.FEATURES, 'runtime': {'test_threads': 'libtest-default',
                    'rust_min_stack': None, 'target': 'x86_64-unknown-linux-gnu',
                    'profile': descriptor['profile'], 'rustc_version': commands.RUSTC_VERSION}},
                'origin': {'kind': 'attested-local', 'command': command, 'execution': {
                    'mode': 'local-openraft-original', 'first_line': 2, 'binding': binding, 'artifacts': artifacts}},
                'log': {'sha256_ref': retention.digest(raw.encode())},
                'coverage': {'first_line': 3, 'last_line': len(raw.splitlines()), 'format': 'rust'}}
            item = {'name': 'wal:237', 'invocation': package.WRAPPER}
            with patch.object(commands, 'OPENRAFT_ORIGINAL_LOG', retention.digest(raw.encode())), \
                    patch.object(commands, 'OPENRAFT_ORIGINAL_BINDING', binding['sha256']), \
                    patch.object(commands, 'OPENRAFT_ORIGINAL_ARTIFACTS', artifacts['sha256']):
                self.assertIn('running 1 test', commands.original_openraft_capture(record, item, raw))
                mutations = [lambda r: r.update(source_commit='a' * 40),
                    lambda r: r['environment']['runtime']['profile'].update(debuginfo=0),
                    lambda r: r['environment']['features'].append('foreign-feature'),
                    lambda r: r['coverage'].update(first_line=4),
                    lambda r: r['origin'].update(command='cargo test --other'),
                    lambda r: r['origin']['execution']['artifacts'].update(sha256='0' * 64)]
                for mutate in mutations:
                    changed = copy.deepcopy(record); mutate(changed)
                    with self.subTest(mutation=mutate), self.assertRaises(receipts.ReceiptError):
                        commands.original_openraft_capture(changed, item, raw)
                for text in (raw + 'test another ... ok\n', raw.replace(' ... ok', ' ... FAILED')):
                    with self.assertRaises(receipts.ReceiptError):
                        commands.original_openraft_capture(record, item, text)

    def test_current_compile_binds_owned_package_profile_features_and_discovery_without_bodies(self):
        with fixture():
            source = 'b' * 40
            descriptor = package.source_descriptor(source)
            manifest = Path('/tmp/owned-openraft-proof/Cargo.toml')
            identity = commands.rust_identity(package.WRAPPER)
            action = ('cargo', 'test', '--no-run', '--message-format=json', '--locked',
                      '--manifest-path', package.MANIFEST_VARIABLE,
                      '--features', 'generic-snapshot-data', '--lib')
            proof = {'mode': 'retained-complete-command-union',
                'validator': retention.digest(Path(commands.__file__).read_bytes()),
                'native_inventories': {'wal:237': {'features': package.FEATURES,
                    'ids_sha256': retention.digest(json.dumps([package.SELECTOR], separators=(',', ':')).encode()),
                    'external_package': descriptor}}}
            artifact = {'reason': 'compiler-artifact', 'executable': '/tmp/owned-target/deps/openraft-12345678',
                'target': {'name': 'openraft', 'kind': ['lib'], 'src_path': str(manifest.parent / 'src/lib.rs')},
                'package_id': 'path+' + manifest.parent.as_uri() + '#openraft@0.9.25',
                'features': package.FEATURES, 'profile': descriptor['profile']}
            @contextmanager
            def current(source, output):
                self.assertEqual(source, 'b' * 40)
                yield manifest, descriptor
            def output(argv, **limits):
                if argv == ['rustc', '-vV']:
                    return commands.RUSTC_VERSION + '\nhost: x86_64-unknown-linux-gnu\n'
                self.assertEqual(argv, [artifact['executable'], '--list', '--format=terse'])
                return package.SELECTOR + ': test\n'
            def run(candidate_proof, candidate_artifact):
                with patch.dict(commands.os.environ, {}, clear=True), \
                        patch.object(commands, 'compile_commands', return_value={action: [('wal:237', identity)]}), \
                        patch.object(package, 'current_source', side_effect=current), \
                        patch.object(commands, 'owned_output', side_effect=output), \
                        patch.object(commands, 'require_current_fingerprint') as fingerprint, \
                        patch.object(commands, 'owned_compile', return_value=json.dumps(candidate_artifact) + '\n') as compile:
                    commands.compile_adopted('wal', candidate_proof, source)
                    fingerprint.assert_called_once_with(candidate_artifact, descriptor['profile'])
                    argv = compile.call_args.args[0]
                    self.assertEqual(argv[:2], ('env', 'CARGO_TARGET_DIR=' + str(Path.cwd() / 'target/openraft-regression')))
                    self.assertIn(str(manifest), argv)
                    self.assertNotIn(package.MANIFEST_VARIABLE, argv)
            run(proof, artifact)
            for mutate in [lambda a: a.update(package_id='path+file:///tmp/foreign#openraft@0.9.25'),
                           lambda a: a['target'].update(src_path='/tmp/foreign/src/lib.rs'),
                           lambda a: a.update(features=['foreign-feature']),
                           lambda a: a['profile'].update(debuginfo=0)]:
                changed = copy.deepcopy(artifact); mutate(changed)
                with self.subTest(mutation=mutate), self.assertRaises(receipts.ReceiptError):
                    run(proof, changed)
            changed = copy.deepcopy(proof)
            changed['native_inventories']['wal:237']['external_package']['adapter_sha256'] = '0' * 64
            with self.assertRaises(receipts.ReceiptError): run(changed, artifact)

    def test_extracted_walk_stops_lazily_and_file_growth_reads_remain_bounded(self):
        from types import SimpleNamespace
        import stat
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            class Iterator:
                consumed = 0
                closed = False
                def __iter__(self): return self
                def __next__(self):
                    name = 'f' + str(self.consumed)
                    self.consumed += 1
                    return SimpleNamespace(path=str(root / name), is_symlink=lambda: False,
                        is_dir=lambda **kw: False, is_file=lambda **kw: True)
                def close(self): self.closed = True
            iterator = Iterator()
            with patch.object(package.os, 'scandir', return_value=iterator), \
                    patch.object(package, 'bounded_file', return_value=b'x'), \
                    patch.object(package, 'MAX_MEMBERS', 3), self.assertRaisesRegex(receipts.ReceiptError, 'traversal'):
                package.check_extracted(root, {'f' + str(i): b'x' for i in range(100)})
            self.assertEqual(iterator.consumed, 4)
            self.assertTrue(iterator.closed)
            node = root
            for _ in range(5):
                node = node / 'deep'; node.mkdir()
            with patch.object(package, 'MAX_DEPTH', 3), self.assertRaisesRegex(receipts.ReceiptError, 'traversal'):
                package.check_extracted(root, {})
        class Stream:
            requested = None
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def fileno(self): return 88
            def read(self, size):
                self.requested = size
                return b'abc'
        stream = Stream()
        with patch.object(package.os, 'open', return_value=88), \
                patch.object(package.os, 'fdopen', return_value=stream), \
                patch.object(package.os, 'fstat', side_effect=[SimpleNamespace(st_mode=stat.S_IFREG, st_size=2),
                    SimpleNamespace(st_mode=stat.S_IFREG, st_size=3)]), \
                self.assertRaisesRegex(receipts.ReceiptError, 'grew'):
            package.bounded_file(Path('/tmp/owned-growing-file'), 2)
        self.assertEqual(stream.requested, 3)
