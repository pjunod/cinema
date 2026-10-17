"""One locked registry-package adapter; never expands receipt-selected shell."""
from __future__ import annotations
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import tarfile
import tempfile
import tomllib
from contextlib import contextmanager
from validation import python_unit_receipts as receipts
from validation import promotion_unit_retention as retention

NAME = 'openraft'
VERSION = '0.9.25'
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'
ARCHIVE_SHA = 'a97014fb78acb77be3a40ac2da305f6dd3a6b243f3a908ace87d29b3972eaafd'
MANIFEST_SHA = '7281f7887dc9cb9fabbef72232beae37acdbdf506a2a204d24d91c7eddb3e104'
LOCK_SHA = 'baac66da5b7b06cf77a9c3e13e54ecf22026ab531a02b1646006b100d05fc8fa'
MANIFEST_VARIABLE = '$$OPENRAFT_MANIFEST'
SELECTOR = 'network::snapshot_transport::tests::test_chunked_reset_offset_if_snapshot_id_mismatch'
FEATURES = ['generic-snapshot-data']
MAX_ARCHIVE_BYTES = 1024 * 1024
MAX_SOURCE_BYTES = 8 * 1024 * 1024
MAX_MEMBERS = 4096
MAX_DEPTH = 64
WRAPPER = '@OPENRAFT_MANIFEST="$$( $(CARGO) metadata --locked --manifest-path vendor/hiqlite/Cargo.toml --format-version 1 | python3 -c \'import json, sys; data = json.load(sys.stdin); print(next(p["manifest_path"] for p in data["packages"] if p["name"] == "openraft" and p["version"] == "0.9.25"))\' )"; CARGO_TARGET_DIR="$(CURDIR)/target/openraft-regression" $(CARGO) test --locked --manifest-path "$$OPENRAFT_MANIFEST" --features generic-snapshot-data network::snapshot_transport::tests::test_chunked_reset_offset_if_snapshot_id_mismatch --lib -- --exact'


def source_descriptor(source):
    """Source constants bind the only supported external wrapper and package."""
    from validation.qualification_unit_continuation import make_commands
    makefile = retention.git_bytes(source, 'Makefile')
    receipts.require(makefile is not None and
                     make_commands(makefile.decode(), 'cluster-wal-check').count(WRAPPER) == 1,
                     'External package wrapper changed or unavailable')
    for path in ('Cargo.lock', 'vendor/hiqlite/Cargo.lock'):
        raw = retention.git_bytes(source, path)
        receipts.require(raw is not None, 'External package source lock unavailable')
        packages = tomllib.loads(raw.decode()).get('package', [])
        found = [p for p in packages if p.get('name') == NAME]
        receipts.require(len(found) == 1 and found[0].get('version') == VERSION
                         and found[0].get('source') == REGISTRY
                         and found[0].get('checksum') == ARCHIVE_SHA,
                         'External package version/source/checksum differs')
    return {'name': NAME, 'version': VERSION, 'registry': REGISTRY,
            'archive_sha256': ARCHIVE_SHA, 'manifest_sha256': MANIFEST_SHA,
            'own_lock_sha256': LOCK_SHA, 'wrapper_sha256': retention.digest(WRAPPER.encode()),
            'profile': {'opt_level': '0', 'debuginfo': 2, 'debug_assertions': True,
                        'overflow_checks': True, 'test': True},
            'features': FEATURES, 'target': {'name': NAME, 'kind': ['lib'], 'path': 'src/lib.rs'},
            'selector': SELECTOR, 'harness': ['--exact'],
            'adapter_sha256': retention.digest(Path(__file__).read_bytes())}


def archive_files(raw):
    receipts.require(isinstance(raw, bytes) and 0 < len(raw) <= MAX_ARCHIVE_BYTES
                     and hashlib.sha256(raw).hexdigest() == ARCHIVE_SHA,
                     'External package archive checksum or size differs')
    files = {}
    total = 0
    prefix = NAME + '-' + VERSION
    try:
        with tarfile.open(fileobj=io.BytesIO(raw), mode='r:gz') as archive:
            for ordinal, member in enumerate(archive):
                receipts.require(ordinal < MAX_MEMBERS, 'External package member limit')
                path = PurePosixPath(member.name)
                receipts.require(not path.is_absolute() and path.parts
                                 and path.parts[0] == prefix
                                 and all(p not in ('', '.', '..') for p in path.parts),
                                 'Unsafe external package archive path')
                if member.isdir():
                    continue
                receipts.require(member.isfile() and len(path.parts) > 1,
                                 'External package aliases or special files forbidden')
                name = str(PurePosixPath(*path.parts[1:]))
                total += member.size
                receipts.require(name not in files and 0 <= member.size <= MAX_SOURCE_BYTES
                                 and total <= MAX_SOURCE_BYTES, 'External package contents exceed bound')
                file = archive.extractfile(member)
                receipts.require(file is not None, 'External package member unavailable')
                content = file.read(member.size + 1)
                receipts.require(len(content) == member.size, 'External package member size differs')
                files[name] = content
    except (tarfile.TarError, EOFError, OSError) as error:
        raise receipts.ReceiptError('Malformed external package archive') from error
    receipts.require('Cargo.toml' in files and 'Cargo.lock' in files
                     and retention.digest(files['Cargo.toml']) == MANIFEST_SHA
                     and retention.digest(files['Cargo.lock']) == LOCK_SHA,
                     'External package manifest or own lock differs')
    manifest = tomllib.loads(files['Cargo.toml'].decode())
    receipts.require(manifest.get('package', {}).get('name') == NAME
                     and manifest['package'].get('version') == VERSION
                     and manifest.get('profile', {}) == {}
                     and manifest.get('lib', {}).get('path') == 'src/lib.rs'
                     and 'src/lib.rs' in files, 'External package target or default profile differs')
    return files


def metadata_package(data):
    receipts.require(isinstance(data, dict) and isinstance(data.get('packages'), list),
                     'External package metadata unavailable')
    found = [p for p in data['packages'] if isinstance(p, dict) and p.get('name') == NAME]
    receipts.require(len(found) == 1, 'External package metadata ambiguous')
    package = found[0]
    receipts.require(package.get('version') == VERSION and package.get('source') == REGISTRY
                     and package.get('id') == REGISTRY + '#' + NAME + '@' + VERSION,
                     'External package metadata version/source/identity differs')
    targets = [t for t in package.get('targets', []) if t.get('name') == NAME and t.get('kind') == ['lib']]
    receipts.require(len(targets) == 1, 'External package metadata target ambiguous')
    manifest = Path(package.get('manifest_path', ''))
    receipts.require(manifest.is_absolute() and manifest.name == 'Cargo.toml'
                     and manifest.parent.name == NAME + '-' + VERSION
                     and re.fullmatch(r'index\.crates\.io-[0-9a-f]{16}', manifest.parent.parent.name)
                     and manifest.parent.parent.parent.name == 'src'
                     and manifest.parent.parent.parent.parent.name == 'registry'
                     and Path(targets[0].get('src_path', '')) == manifest.parent / 'src/lib.rs',
                     'External package metadata cache path or target differs')
    return manifest


def bounded_file(path, limit):
    """Do not trust stat before a growing read or follow a replaced symlink."""
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'rb') as stream:
            before = os.fstat(stream.fileno())
            receipts.require(stat.S_ISREG(before.st_mode) and before.st_size <= limit,
                             'External package file type or size differs')
            raw = stream.read(limit + 1)
            after = os.fstat(stream.fileno())
            receipts.require(len(raw) <= limit and before.st_size == len(raw) == after.st_size,
                             'External package file grew or changed during bounded read')
            return raw
    except OSError as error:
        raise receipts.ReceiptError('External package file unavailable or replaced') from error


def check_extracted(root, files, *, registry_cache=False):
    """Compare every actual source file; no symlink or undeclared source allowed."""
    receipts.require(root.is_dir() and not root.is_symlink(), 'External package root differs')
    actual = set()
    stack = []
    count = 0
    try:
        stack.append((os.scandir(root), 0))
        while stack:
            iterator, depth = stack[-1]
            try:
                entry = next(iterator)
            except StopIteration:
                iterator.close()
                stack.pop()
                continue
            count += 1
            receipts.require(count <= MAX_MEMBERS and depth < MAX_DEPTH,
                             'External package extracted traversal limit')
            receipts.require(not entry.is_symlink(), 'External package source contains symlink')
            if entry.is_dir(follow_symlinks=False):
                if registry_cache and Path(entry.path) == root / 'target':
                    # Old standalone Cargo builds may leave this generated tree.
                    # It is neither copied nor compiled: owned source comes only
                    # from the checksum-authenticated archive.
                    receipts.require(not any(name.startswith('target/') for name in files),
                                     'External archive includes a build-output source')
                    continue
                stack.append((os.scandir(entry.path), depth + 1))
                continue
            name = str(Path(entry.path).relative_to(root))
            receipts.require(entry.is_file(follow_symlinks=False) and name not in actual,
                             'External package source special file')
            actual.add(name)
            if name in {'.cargo-ok', '.cargo-checksum.json'}:
                bounded_file(Path(entry.path), 1024 * 1024)
                continue
            receipts.require(name in files and bounded_file(Path(entry.path), len(files[name])) == files[name],
                             'External package extracted bytes changed')
    except OSError as error:
        raise receipts.ReceiptError('External package traversal unavailable') from error
    finally:
        for iterator, _ in stack:
            iterator.close()
    receipts.require(set(files) <= actual and actual <= set(files) | {'.cargo-ok', '.cargo-checksum.json'},
                     'External package extracted source footprint differs')


@contextmanager
def current_source(source, output):
    """Resolve locked metadata, authenticate cache bytes, and compile owned bytes."""
    descriptor = source_descriptor(source)
    metadata = receipts.bounded_json(output(['cargo', 'metadata', '--locked',
        '--manifest-path', 'vendor/hiqlite/Cargo.toml', '--format-version', '1'],
        timeout=120, limit=16 * 1024 * 1024).encode())
    manifest = metadata_package(metadata)
    registry = manifest.parent.parent.parent.parent
    crate = registry / 'cache' / manifest.parent.parent.name / (NAME + '-' + VERSION + '.crate')
    files = archive_files(bounded_file(crate, MAX_ARCHIVE_BYTES))
    check_extracted(manifest.parent, files, registry_cache=True)
    with tempfile.TemporaryDirectory(prefix='plurx-owned-openraft-unit-source-') as directory:
        root = Path(directory)
        for name, raw in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(raw)
        check_extracted(root, files)
        yield root / 'Cargo.toml', descriptor
        check_extracted(root, files)
