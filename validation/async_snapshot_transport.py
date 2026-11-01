"""Private bounded snapshot byte transport; stored bytes confer no proof credit.

Wire records and authenticated origin selection belong to the controller. These
helpers accept an already validated owned directory and recheck its lease before
filesystem operations. Reservation callbacks must enforce the admitted physical
budget; successful reservations remain charged while immutable CAS bytes exist.
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import stat
import uuid
from collections.abc import Callable, Iterator

CHUNK_BYTES = 1 << 20
_DIGEST = re.compile(r'[0-9a-f]{64}\Z')


class SnapshotTransportError(ValueError):
    """Unsafe path, changed source, corrupt bytes, or invalid chunk length."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise SnapshotTransportError(message)


def _identity(info: os.stat_result) -> tuple[int, int, int, int, int]:
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def _regular(fd: int, device: int) -> os.stat_result:
    info = os.fstat(fd)
    _require(stat.S_ISREG(info.st_mode), 'snapshot member is not a regular file')
    _require(info.st_dev == device and info.st_uid == os.geteuid(),
             'snapshot member device/owner differs')
    _require(info.st_nlink == 1 and info.st_mode & 0o077 == 0,
             'snapshot member must be private and have one link')
    return info


class OwnedDirectory:
    """Open directory capability beneath the controller's validated lease root.

    The callback validates the actual source-bound storage lease, including its
    expiry and device. This class does not manufacture lease authority. Keep the
    capability open for one bounded operation; replacement of its path fails.
    """

    def __init__(self, path: Path, lease_root: Path, check_lease: Callable[[], None]):
        self.path = Path(path)
        self.lease_root = Path(lease_root)
        self.check_lease = check_lease
        self.fd = -1
        check_lease()
        _require(self.path.is_absolute() and self.lease_root.is_absolute(),
                 'owned paths must be absolute')
        _require(self.path != self.lease_root and self.lease_root in self.path.parents,
                 'owned directory must be below lease root')
        current = self.lease_root
        root_info = current.lstat()
        _require(stat.S_ISDIR(root_info.st_mode), 'lease root is not a directory')
        for component in self.path.relative_to(self.lease_root).parts:
            current = current / component
            info = current.lstat()
            _require(stat.S_ISDIR(info.st_mode) and info.st_dev == root_info.st_dev
                     and info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0,
                     'owned directory is symlinked, shared, or on another device')
        _require(self.path.resolve() == self.path and self.lease_root.resolve() == self.lease_root,
                 'owned directory path is not canonical')
        self.fd = os.open(self.path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.info = os.fstat(self.fd)
        self.validate()

    def validate(self) -> None:
        self.check_lease()
        _require(self.fd >= 0, 'owned directory is closed')
        info = self.path.lstat()
        _require(stat.S_ISDIR(info.st_mode) and (info.st_dev, info.st_ino)
                 == (self.info.st_dev, self.info.st_ino), 'owned directory was replaced')
        _require(info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0,
                 'owned directory ownership/mode changed')

    def close(self) -> None:
        if self.fd >= 0:
            os.close(self.fd)
            self.fd = -1

    def __enter__(self) -> OwnedDirectory:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()


def iter_chunks(directory: OwnedDirectory, name: str) -> Iterator[tuple[int, bytes, str]]:
    """Read one fixed-size chunk at a time; reject mutation across the stream."""
    _require(Path(name).name == name and name not in ('', '.', '..'), 'invalid snapshot filename')
    directory.validate()
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory.fd)
    try:
        before = _regular(fd, directory.info.st_dev)
        offset = 0
        while offset < before.st_size:
            directory.validate()
            wanted = min(CHUNK_BYTES, before.st_size - offset)
            data = bytearray()
            while len(data) < wanted:
                block = os.read(fd, wanted - len(data))
                _require(bool(block), 'snapshot truncated during read')
                data.extend(block)
            value = bytes(data)
            yield offset // CHUNK_BYTES, value, hashlib.sha256(value).hexdigest()
            offset += wanted
        _require(not os.read(fd, 1) and _identity(os.fstat(fd)) == _identity(before),
                 'snapshot changed during read')
        directory.validate()
    finally:
        os.close(fd)


def _verify_bytes(directory: OwnedDirectory, digest: str, length: int, maximum: int) -> None:
    """Rehash existing CAS bytes before reuse; presence is never authentication."""
    _require(bool(_DIGEST.fullmatch(digest)) and type(length) is int
             and 0 < length <= maximum, 'invalid content identity/length')
    directory.validate()
    fd = os.open(digest, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory.fd)
    try:
        before = _regular(fd, directory.info.st_dev)
        _require(before.st_size == length, 'CAS chunk length differs')
        hashed = hashlib.sha256()
        remaining = length
        while remaining:
            block = os.read(fd, min(65536, remaining))
            _require(bool(block), 'CAS chunk truncated')
            hashed.update(block)
            remaining -= len(block)
        _require(not os.read(fd, 1) and hashed.hexdigest() == digest
                 and _identity(os.fstat(fd)) == _identity(before), 'CAS chunk corrupt or changed')
        path_info = os.stat(digest, dir_fd=directory.fd, follow_symlinks=False)
        _require(_identity(path_info) == _identity(before), 'CAS chunk path replaced')
        directory.validate()
    finally:
        os.close(fd)


def _store_bytes(directory: OwnedDirectory, data: bytes,
                 reserve: Callable[[int], None], release: Callable[[int], None], maximum: int) -> tuple[str, int, bool]:
    """Install immutable bytes exclusively; return digest, length, newly stored.

    Reserve before materializing a temporary copy. Publication uses an exclusive
    hard link then removes the temporary name; no existing destination is ever
    overwritten. Crashes may leave a charged temporary file for explicit recovery.
    """
    _require(type(data) is bytes and 0 < len(data) <= maximum, 'invalid immutable content bytes')
    digest = hashlib.sha256(data).hexdigest()
    directory.validate()
    try:
        _verify_bytes(directory, digest, len(data), maximum)
        return digest, len(data), False
    except FileNotFoundError:
        pass
    reserve(len(data))
    temporary = '.chunk-' + uuid.uuid4().hex
    fd = -1
    installed = False
    created = False
    try:
        directory.validate()
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=directory.fd)
        created = True
        view = memoryview(data)
        while view:
            written = os.write(fd, view)
            _require(written > 0, 'CAS temporary write made no progress')
            view = view[written:]
        os.fsync(fd)
        os.close(fd)
        fd = -1
        directory.validate()
        try:
            os.link(temporary, digest, src_dir_fd=directory.fd,
                    dst_dir_fd=directory.fd, follow_symlinks=False)
            installed = True
        except FileExistsError:
            pass
        os.unlink(temporary, dir_fd=directory.fd)
        created = False
        os.fsync(directory.fd)
        _verify_bytes(directory, digest, len(data), maximum)
        return digest, len(data), installed
    finally:
        if fd >= 0:
            os.close(fd)
        if created:
            os.unlink(temporary, dir_fd=directory.fd)
            os.fsync(directory.fd)
        if not installed:
            release(len(data))

def verify_chunk(directory: OwnedDirectory, digest: str, length: int) -> None:
    _verify_bytes(directory, digest, length, CHUNK_BYTES)


def store_chunk(directory: OwnedDirectory, data: bytes, reserve, release):
    return _store_bytes(directory, data, reserve, release, CHUNK_BYTES)


# Closed operational records are defined in the source-owned adjunct schema.
# These framing functions do not import the controller or execute source helpers.
def canonical(value: object) -> bytes:
    import json
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False,
                      allow_nan=False).encode('utf-8')


def content_ref(kind: str, data: bytes) -> dict:
    return {'kind': kind, 'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data)}


def scope_digest(binding: dict, domain: str) -> str:
    return hashlib.sha256(canonical({key: binding[key] for key in
        ('source', 'session_id', 'resource_plan_sha256', 'initial_image_sha256')} |
        {'domain': domain})).hexdigest()


class ContentInventory:
    """Disk-backed content and edge inventory on an already granted connection.

    The caller reserves the publication_pack_scratch database before opening it.
    CAS files use the independently deducted cas_bytes pool. This inventory records
    bytes, never authentication; only reconstructed native receipt lineage grants
    coverage in PublicationLedger.
    """
    def __init__(self, database, cas: OwnedDirectory, reserve, release):
        self.db = database
        self.cas = cas
        self.reserve = reserve
        self.release = release
        self.db.executescript('''
            CREATE TABLE IF NOT EXISTS content(
                kind TEXT,sha256 TEXT,bytes INTEGER,path TEXT,PRIMARY KEY(kind,sha256));
            CREATE TABLE IF NOT EXISTS edges(
                owner_kind TEXT,owner_sha256 TEXT,ordinal INTEGER,kind TEXT,
                sha256 TEXT,bytes INTEGER,PRIMARY KEY(owner_kind,owner_sha256,ordinal));
            CREATE TABLE IF NOT EXISTS members(
                unit_ordinal INTEGER,member_path TEXT,bytes INTEGER,sha256 TEXT,
                kind TEXT,content_sha256 TEXT,PRIMARY KEY(unit_ordinal,member_path));
            CREATE TABLE IF NOT EXISTS levels(
                level INTEGER,position INTEGER,reference BLOB,summary BLOB,
                PRIMARY KEY(level,position));
        ''')
        self.db.commit()

    def put(self, kind: str, data: bytes, children=()) -> dict:
        maximum = CHUNK_BYTES if kind == 'snapshot_chunk' else 8 * CHUNK_BYTES
        sha, length, _ = _store_bytes(self.cas, data, self.reserve, self.release, maximum)
        reference = {'kind': kind, 'sha256': sha, 'bytes': length}
        prior = self.db.execute('SELECT bytes FROM content WHERE kind=? AND sha256=?',
                                (kind, sha)).fetchone()
        _require(prior is None or prior == (length,), 'conflicting content identity')
        self.db.execute('INSERT OR IGNORE INTO content VALUES(?,?,?,?)',
                        (kind, sha, length, str(self.cas.path / sha)))
        for ordinal, child in enumerate(children):
            self.db.execute('INSERT OR IGNORE INTO edges VALUES(?,?,?,?,?,?)',
                            (kind, sha, ordinal, child['kind'], child['sha256'], child['bytes']))
        self.db.commit()
        return reference

    def bytes(self, reference: dict) -> bytes:
        _require(set(reference) == {'kind', 'sha256', 'bytes'}, 'invalid ContentRef')
        row = self.db.execute('SELECT bytes FROM content WHERE kind=? AND sha256=?',
                              (reference['kind'], reference['sha256'])).fetchone()
        _require(row == (reference['bytes'],), 'content not present in inventory')
        _verify_bytes(self.cas, reference['sha256'], reference['bytes'],
                      CHUNK_BYTES if reference['kind'] == 'snapshot_chunk' else 8 * CHUNK_BYTES)
        fd = os.open(reference['sha256'], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                     dir_fd=self.cas.fd)
        try:
            data = bytearray()
            while len(data) < reference['bytes']:
                block = os.read(fd, min(65536, reference['bytes'] - len(data)))
                _require(bool(block), 'content disappeared during read')
                data.extend(block)
            _require(not os.read(fd, 1) and content_ref(reference['kind'], bytes(data)) == reference,
                     'content changed after verification')
            return bytes(data)
        finally:
            os.close(fd)

    def _level_row(self, level, position, reference, summary):
        self.db.execute('INSERT INTO levels VALUES(?,?,?,?)',
                        (level, position, canonical(reference), canonical(summary)))

    def snapshot_tree(self, directory: OwnedDirectory, name: str, binding: dict, domain: str):
        """Construct bounded 256-way manifest; stream leaf chunks and disk levels."""
        import json
        self.db.execute('DELETE FROM levels')
        scope = scope_digest(binding, domain)
        whole = hashlib.sha256()
        total = 0
        count = 0
        entries = []
        first = 0
        position = 0
        for ordinal, data, _sha in iter_chunks(directory, name):
            reference = self.put('snapshot_chunk', data)
            entries.append({'ordinal': ordinal, 'offset': total, 'bytes': len(data), 'content': reference})
            whole.update(data)
            total += len(data)
            count += 1
            if len(entries) == 256:
                node = {'schema_version': 2, 'kind': 'snapshot_manifest_node', 'scope_sha256': scope,
                        'level': 0, 'first_chunk': first, 'chunk_count': len(entries),
                        'offset': entries[0]['offset'], 'bytes': sum(row['bytes'] for row in entries),
                        'entries': entries}
                reference = self.put(node['kind'], canonical(node), [row['content'] for row in entries])
                self._level_row(0, position, reference, {key: node[key] for key in
                    ('first_chunk', 'chunk_count', 'offset', 'bytes')})
                position += 1
                first = count
                entries = []
        if entries:
            node = {'schema_version': 2, 'kind': 'snapshot_manifest_node', 'scope_sha256': scope,
                    'level': 0, 'first_chunk': first, 'chunk_count': len(entries),
                    'offset': entries[0]['offset'], 'bytes': sum(row['bytes'] for row in entries),
                    'entries': entries}
            reference = self.put(node['kind'], canonical(node), [row['content'] for row in entries])
            self._level_row(0, position, reference, {key: node[key] for key in
                ('first_chunk', 'chunk_count', 'offset', 'bytes')})
            position += 1
        if not count:
            self.db.commit()
            return None, 0, 0, whole.hexdigest()
        level = 0
        while position > 1:
            cursor = self.db.execute('SELECT reference,summary FROM levels WHERE level=? ORDER BY position', (level,))
            position = 0
            while True:
                rows = cursor.fetchmany(256)
                if not rows:
                    break
                children = [json.loads(row[0]) for row in rows]
                summaries = [json.loads(row[1]) for row in rows]
                node = {'schema_version': 2, 'kind': 'snapshot_manifest_node', 'scope_sha256': scope,
                        'level': level + 1, 'first_chunk': summaries[0]['first_chunk'],
                        'chunk_count': sum(row['chunk_count'] for row in summaries),
                        'offset': summaries[0]['offset'], 'bytes': sum(row['bytes'] for row in summaries),
                        'entries': [{**summary, 'child': child} for summary, child in zip(summaries, children)]}
                reference = self.put(node['kind'], canonical(node), children)
                self._level_row(level + 1, position, reference, {key: node[key] for key in
                    ('first_chunk', 'chunk_count', 'offset', 'bytes')})
                position += 1
            level += 1
        result = self.db.execute('SELECT reference FROM levels WHERE level=? AND position=0', (level,)).fetchone()
        self.db.commit()
        return json.loads(result[0]), count, total, whole.hexdigest()

    def keyed_tree(self, kind, binding, domain, rows):
        """Build evidence/transfer tree from strictly sorted streamed leaf rows."""
        import json
        _require(kind in ('evidence_manifest_node', 'transfer_manifest_node'), 'unknown keyed manifest kind')
        self.db.execute('DELETE FROM levels')
        scope = scope_digest(binding, domain)
        previous = None
        position = 0
        entries = []
        keys = []

        def publish(level, group, group_keys, ordinal):
            count = len(group) if level == 0 else sum(row['item_count'] for row in group)
            node = {'schema_version': 1, 'kind': kind, 'scope_sha256': scope, 'level': level,
                    'first_key': group_keys[0] if level == 0 else group[0]['first_key'],
                    'last_key': group_keys[-1] if level == 0 else group[-1]['last_key'], 'item_count': count,
                    'entries': group}
            children = ([row['chunks'] for row in group if row['chunks'] is not None]
                        if level == 0 and kind == 'evidence_manifest_node' else
                        [row['content'] for row in group] if level == 0 else [row['child'] for row in group])
            reference = self.put(kind, canonical(node), children)
            self._level_row(level, ordinal, reference, {key: node[key] for key in
                ('first_key', 'last_key', 'item_count')})

        for key, row in rows:
            _require(type(key) is str and (previous is None or previous < key), 'manifest keys duplicate/unsorted')
            previous = key
            keys.append(key)
            entries.append(row)
            if len(entries) == 256:
                publish(0, entries, keys, position)
                position += 1
                entries, keys = [], []
        if entries:
            publish(0, entries, keys, position)
            position += 1
        _require(position > 0, 'empty manifest is not representable')
        level = 0
        while position > 1:
            cursor = self.db.execute('SELECT reference,summary FROM levels WHERE level=? ORDER BY position', (level,))
            position = 0
            while True:
                group = cursor.fetchmany(256)
                if not group:
                    break
                branch = [{**json.loads(summary), 'child': json.loads(reference)} for reference, summary in group]
                publish(level + 1, branch, [branch[0]['first_key']] +
                        ([branch[-1]['last_key']] if len(branch) > 1 else []), position)
                position += 1
            level += 1
        reference = json.loads(self.db.execute('SELECT reference FROM levels WHERE level=? AND position=0', (level,)).fetchone()[0])
        self.db.commit()
        return reference


class PublicationLedger:
    """Finite dispatch and authenticated coverage state on a granted owned DB.

    ContentInventory is not an authority input. Call authenticate only after exact
    native task/upload/ZIP/member validation and source-bound lineage validation.
    Transaction rollback prevents partially credited batch coverage.
    """
    def __init__(self, database):
        self.db = database
        self.db.executescript('''
            CREATE TABLE IF NOT EXISTS coverage(kind TEXT,sha256 TEXT,bytes INTEGER,PRIMARY KEY(kind,sha256));
            CREATE TABLE IF NOT EXISTS queue(kind TEXT,sha256 TEXT,bytes INTEGER,PRIMARY KEY(kind,sha256));
            CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY,value BLOB);
            CREATE TABLE IF NOT EXISTS expanded(kind TEXT,sha256 TEXT,PRIMARY KEY(kind,sha256));
            CREATE TABLE IF NOT EXISTS dispatches(
                ordinal INTEGER PRIMARY KEY,operation TEXT,intent_sha256 TEXT,run_id INTEGER,status TEXT);
        ''')
        self.db.commit()

    def coverage(self):
        hashed = hashlib.sha256(b'[')
        count = 0
        for kind, sha, size in self.db.execute('SELECT kind,sha256,bytes FROM coverage ORDER BY kind,sha256'):
            if count:
                hashed.update(b',')
            hashed.update(canonical({'kind': kind, 'sha256': sha, 'bytes': size}))
            count += 1
        hashed.update(b']')
        return hashed.hexdigest(), count

    def authenticate(self, references):
        with self.db:
            for reference in references:
                row = self.db.execute('SELECT bytes FROM coverage WHERE kind=? AND sha256=?',
                                      (reference['kind'], reference['sha256'])).fetchone()
                _require(row is None or row == (reference['bytes'],), 'coverage identity conflicts')
                self.db.execute('INSERT OR IGNORE INTO coverage VALUES(?,?,?)',
                                (reference['kind'], reference['sha256'], reference['bytes']))
        return self.coverage()

    def reserve_dispatch(self, operation, intent_sha256, maximum_units):
        _require(operation in ('execute', 'publish', 'complete'), 'unknown dispatch operation')
        self.db.execute('BEGIN IMMEDIATE')
        try:
            ordinal = self.db.execute('SELECT coalesce(max(ordinal)+1,0) FROM dispatches').fetchone()[0]
            _require(ordinal < maximum_units, 'immutable dispatch allowance exhausted')
            self.db.execute('INSERT INTO dispatches VALUES(?,?,?,NULL,?)',
                            (ordinal, operation, intent_sha256, 'reserved'))
            self.db.commit()
            return ordinal
        except BaseException:
            self.db.rollback()
            raise


STRUCTURED_KINDS = ('publication_intent', 'catalog_backup', 'verifier_backup',
                    'snapshot_manifest_node', 'evidence_manifest_node')


def child_refs(kind: str, raw: bytes) -> list[dict]:
    """Closure children named by one authenticated structured object.

    Leaf payload kinds (snapshot_chunk, planned_execution_result) have none. A
    transfer manifest node is deliberately outside the publication closure.
    """
    import json
    value = json.loads(raw)
    _require(canonical(value) == raw, 'noncanonical structured content')
    if kind == 'publication_intent':
        rows = [value['catalog_root'], value['verifier_root'], value['execution_result'], value['evidence_root']]
        return [row for row in rows if row is not None]
    if kind in ('catalog_backup', 'verifier_backup'):
        return [value['manifest_root']]
    if kind == 'snapshot_manifest_node':
        return [row['child'] if value['level'] else row['content'] for row in value['entries']]
    if kind == 'evidence_manifest_node':
        if value['level']:
            return [row['child'] for row in value['entries']]
        return [row['chunks'] for row in value['entries'] if row['chunks'] is not None]
    raise SnapshotTransportError('unstructured content kind has no children: ' + kind)


def expand_with_resolver(ledger: 'PublicationLedger', resolve) -> int:
    """Grow the required closure from authenticated bytes only; return new entries.

    Children of covered structured content are queued so the receiver recomputes
    the complete closure itself. Uncovered content is never parsed, so a sender's
    claim about a graph confers nothing until its parent bytes are authenticated.
    """
    added = 0
    while True:
        rows = ledger.db.execute(
            "SELECT c.kind,c.sha256,c.bytes FROM coverage c LEFT JOIN expanded e "
            "ON e.kind=c.kind AND e.sha256=c.sha256 WHERE e.sha256 IS NULL AND c.kind IN (%s) "
            "ORDER BY c.kind,c.sha256 LIMIT 256" % ','.join('?' * len(STRUCTURED_KINDS)),
            STRUCTURED_KINDS).fetchall()
        if not rows:
            return added
        for kind, sha, size in rows:
            raw = resolve({'kind': kind, 'sha256': sha, 'bytes': size})
            for child in child_refs(kind, raw):
                before = ledger.db.total_changes
                ledger.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)',
                                  (child['kind'], child['sha256'], child['bytes']))
                added += ledger.db.total_changes - before
            ledger.db.execute('INSERT OR IGNORE INTO expanded VALUES(?,?)', (kind, sha))
        ledger.db.commit()


def closure_complete(ledger: 'PublicationLedger') -> bool:
    """Every queued identity is covered and every covered structured object expanded."""
    if next(missing_content(ledger), None) is not None:
        return False
    return ledger.db.execute(
        "SELECT 1 FROM coverage c LEFT JOIN expanded e ON e.kind=c.kind AND e.sha256=c.sha256 "
        "WHERE e.sha256 IS NULL AND c.kind IN (%s) LIMIT 1" % ','.join('?' * len(STRUCTURED_KINDS)),
        STRUCTURED_KINDS).fetchone() is None


def coverage_after(ledger: 'PublicationLedger', references) -> tuple[str, int]:
    """Coverage digest/count after adding `references`, without committing them."""
    ledger.db.execute('CREATE TEMP TABLE IF NOT EXISTS candidate(kind TEXT,sha256 TEXT,bytes INTEGER,PRIMARY KEY(kind,sha256))')
    ledger.db.execute('DELETE FROM candidate')
    ledger.db.execute('INSERT INTO candidate SELECT kind,sha256,bytes FROM coverage')
    for reference in references:
        row = ledger.db.execute('SELECT bytes FROM candidate WHERE kind=? AND sha256=?',
                                (reference['kind'], reference['sha256'])).fetchone()
        _require(row is None or row == (reference['bytes'],), 'authenticated coverage collision')
        ledger.db.execute('INSERT OR IGNORE INTO candidate VALUES(?,?,?)',
                          (reference['kind'], reference['sha256'], reference['bytes']))
    hashed = hashlib.sha256(b'[')
    count = 0
    for kind, sha, size in ledger.db.execute('SELECT kind,sha256,bytes FROM candidate ORDER BY kind,sha256'):
        if count:
            hashed.update(b',')
        hashed.update(canonical({'kind': kind, 'sha256': sha, 'bytes': size}))
        count += 1
    hashed.update(b']')
    ledger.db.execute('DROP TABLE candidate')
    return hashed.hexdigest(), count


def seed_coverage(ledger: 'PublicationLedger', references) -> None:
    """Credit exact predecessor-lineage identities (already authenticated batches)."""
    with ledger.db:
        for reference in references:
            ledger.db.execute('INSERT OR IGNORE INTO coverage VALUES(?,?,?)',
                              (reference['kind'], reference['sha256'], reference['bytes']))


class AuthenticatedResolver:
    """Resolve content bytes only through authenticated native batch locators.

    `database` is the controller dependency index whose native_locators rows are
    inserted solely after the exact API/upload/ZIP/member transaction and lineage
    checks. Local presence, a matching hash elsewhere, or a parent attestation is
    never authority. Every returned byte string is rehashed against the ContentRef.
    """
    def __init__(self, database, parts_root: Path):
        self.db = database
        self.parts_root = Path(parts_root)

    def __call__(self, reference: dict) -> bytes:
        _require(set(reference) == {'kind', 'sha256', 'bytes'} and bool(_DIGEST.fullmatch(reference['sha256'])) and
                 bool(re.fullmatch('[a-z][a-z0-9_]*', reference['kind'])) and type(reference['bytes']) is int
                 and 0 < reference['bytes'] <= 8 * CHUNK_BYTES, 'invalid ContentRef')
        name = 'content/' + reference['kind'] + '/' + reference['sha256']
        row = self.db.execute('SELECT artifact_id FROM native_locators WHERE member_path=? AND sha256=? AND bytes=? '
                              'ORDER BY batch_receipt_sha256 LIMIT 1',
                              (name, reference['sha256'], reference['bytes'])).fetchone()
        _require(row is not None, 'content has no authenticated native locator')
        base = self.parts_root / str(row[0])
        _require(base.is_dir() and not base.is_symlink() and base.resolve() == base, 'authenticated part directory unsafe')
        fd = os.open(base / name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            info = os.fstat(fd)
            _require(stat.S_ISREG(info.st_mode) and info.st_size == reference['bytes'], 'authenticated member length differs')
            data = bytearray()
            while len(data) < reference['bytes']:
                block = os.read(fd, min(65536, reference['bytes'] - len(data)))
                _require(bool(block), 'authenticated member truncated')
                data.extend(block)
            _require(not os.read(fd, 1) and content_ref(reference['kind'], bytes(data)) == reference,
                     'authenticated member bytes differ from ContentRef')
            return bytes(data)
        finally:
            os.close(fd)


def restore_image(root: dict, resolve, destination: OwnedDirectory, name: str, reserve, release):
    """Reconstruct every byte of a catalog/verifier root from authenticated content.

    `resolve` must authenticate ContentRef lineage before returning exact bytes.
    The destination reservation covers the complete new file before creation.
    No partial target becomes visible, and a failed restore never grants credit.
    """
    kind = root['kind']
    _require(kind in ('catalog_backup', 'verifier_backup') and root['schema_version'] == 2,
             'obsolete snapshot root')
    size_key, hash_key = (('catalog_bytes', 'catalog_sha256') if kind == 'catalog_backup'
                          else ('work_db_bytes', 'work_db_sha256'))
    _require(root['chunk_bytes_limit'] == CHUNK_BYTES and type(root[size_key]) is int and root[size_key] > 0 and
             root['chunk_count'] == (root[size_key] + CHUNK_BYTES - 1) // CHUNK_BYTES,
             'snapshot chunk geometry differs')
    return restore_tree(root['manifest_root'], root['chunk_count'], root[size_key], root[hash_key],
                        scope_digest(root, 'catalog' if kind == 'catalog_backup' else 'verifier'),
                        resolve, destination, name, reserve, release)


def restore_tree(manifest_root: dict, chunk_count: int, expected_size: int, expected_hash: str, scope: str,
                 resolve, destination: OwnedDirectory, name: str, reserve, release):
    """Stream one complete chunk tree into a fresh owned file; verify EOF and hash."""
    import json
    root = {'manifest_root': manifest_root, 'chunk_count': chunk_count}
    _require(Path(name).name == name and name not in ('', '.', '..'), 'invalid restore filename')
    reserve(expected_size)
    temporary = '.restore-' + uuid.uuid4().hex
    fd = -1
    created = False
    installed = False
    whole = hashlib.sha256()
    next_chunk = 0
    offset = 0

    def load(reference):
        data = resolve(reference)
        _require(type(data) is bytes and len(data) <= 8 * CHUNK_BYTES and
                 content_ref(reference['kind'], data) == reference, 'resolved content mismatch')
        return data

    def walk(reference, expected_level=None):
        nonlocal next_chunk, offset
        _require(reference['kind'] == 'snapshot_manifest_node', 'wrong snapshot child kind')
        node = json.loads(load(reference))
        _require(set(node) == {'schema_version', 'kind', 'scope_sha256', 'level', 'first_chunk',
                              'chunk_count', 'offset', 'bytes', 'entries'} and
                 node['schema_version'] == 2 and node['kind'] == 'snapshot_manifest_node' and
                 node['scope_sha256'] == scope and type(node['level']) is int and
                 0 <= node['level'] <= 16 and (expected_level is None or node['level'] == expected_level),
                 'snapshot node scope/level differs')
        _require(type(node['entries']) is list and 1 <= len(node['entries']) <= 256 and
                 node['first_chunk'] == next_chunk and node['offset'] == offset,
                 'snapshot range is not contiguous')
        first, start = next_chunk, offset
        for entry in node['entries']:
            if node['level']:
                _require(set(entry) == {'first_chunk', 'chunk_count', 'offset', 'bytes', 'child'},
                         'invalid snapshot branch')
                before_chunk, before_offset = next_chunk, offset
                walk(entry['child'], node['level'] - 1)
                _require(entry == {'first_chunk': before_chunk, 'chunk_count': next_chunk - before_chunk,
                                  'offset': before_offset, 'bytes': offset - before_offset,
                                  'child': entry['child']}, 'snapshot branch summary differs')
            else:
                _require(set(entry) == {'ordinal', 'offset', 'bytes', 'content'} and
                         entry['ordinal'] == next_chunk and entry['offset'] == offset and
                         entry['content']['kind'] == 'snapshot_chunk', 'invalid snapshot leaf')
                data = load(entry['content'])
                wanted = min(CHUNK_BYTES, expected_size - offset)
                _require(wanted > 0 and len(data) == wanted == entry['bytes'],
                         'snapshot short/intermediate chunk or excess bytes')
                view = memoryview(data)
                while view:
                    written = os.write(fd, view)
                    _require(written > 0, 'restore write made no progress')
                    view = view[written:]
                whole.update(data)
                offset += len(data)
                next_chunk += 1
        _require(node['chunk_count'] == next_chunk - first and node['bytes'] == offset - start,
                 'snapshot node summary differs')

    try:
        destination.validate()
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=destination.fd)
        created = True
        walk(root['manifest_root'])
        _require(offset == expected_size and next_chunk == root['chunk_count'] and
                 whole.hexdigest() == expected_hash, 'restored whole-image digest/EOF differs')
        os.fsync(fd)
        os.close(fd)
        fd = -1
        destination.validate()
        os.link(temporary, name, src_dir_fd=destination.fd, dst_dir_fd=destination.fd,
                follow_symlinks=False)
        installed = True
        os.unlink(temporary, dir_fd=destination.fd)
        created = False
        os.fsync(destination.fd)
        return destination.path / name
    finally:
        if fd >= 0:
            os.close(fd)
        if created:
            os.unlink(temporary, dir_fd=destination.fd)
            os.fsync(destination.fd)
        if not installed:
            release(expected_size)


class EvidencePlan:
    """Preseal logical member reservations, independent of native upload parts.

    A granted publication_pack_scratch connection owns the inventory. The caller
    deducts snapshot_spool_bytes before every copy and retains charges on success.
    No native locator is chosen here or exposed to M3 MemberRef2.
    """
    def __init__(self, directory: OwnedDirectory, database, reserve, release):
        self.directory_cap = directory
        self.root = directory.path
        self.db = database
        self.reserve_bytes = reserve
        self.release_bytes = release
        self.db.execute('CREATE TABLE members(path TEXT PRIMARY KEY,bytes INTEGER,digest TEXT)')
        self.db.commit()

    def directory(self, ordinal=0):
        _require(ordinal == 0, 'evidence spool has no physical part ordinal')
        self.directory_cap.validate()
        return self.root

    def reserve(self, name, maximum_bytes, role):
        _safe_member(name)
        _require(type(maximum_bytes) is int and 0 < maximum_bytes <= 8 * CHUNK_BYTES,
                 'metadata reservation exceeds existing bound')
        self.reserve_bytes(maximum_bytes)
        try:
            self.db.execute('INSERT INTO members VALUES(?,?,?)', (name, maximum_bytes, 'reserved:' + role))
            self.db.commit()
        except BaseException:
            self.release_bytes(maximum_bytes)
            raise

    def add_file(self, name, source, prefer=0):
        _require(prefer == 0, 'logical evidence cannot select a native slot')
        _safe_member(name)
        self.directory_cap.validate()
        source = Path(source)
        info = source.lstat()
        _require(stat.S_ISREG(info.st_mode) and not source.is_symlink() and
                 info.st_size <= 256 * CHUNK_BYTES, 'evidence member type/size differs')
        old = self.db.execute('SELECT bytes,digest FROM members WHERE path=?', (name,)).fetchone()
        reserved = old is not None and old[1].startswith('reserved:')
        if old is not None and not reserved:
            _require(old == (info.st_size, _file_hash(source)), 'conflicting logical member')
            _require(_file_hash(self.root / name) == old[1], 'existing logical spool changed')
            return
        if reserved:
            _require(info.st_size <= old[0], 'sealed metadata exceeds reservation')
        else:
            self.reserve_bytes(info.st_size)
        target = self.root / name
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        _require(target.parent.resolve() == target.parent and not target.exists(), 'evidence target aliases existing path')
        incoming = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        outgoing = -1
        created = False
        hashed = hashlib.sha256()
        try:
            before = os.fstat(incoming)
            _require(stat.S_ISREG(before.st_mode) and _identity(before) == _identity(info),
                     'evidence source changed before copying')
            outgoing = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            created = True
            copied = 0
            while True:
                data = os.read(incoming, CHUNK_BYTES)
                if not data:
                    break
                copied += len(data)
                _require(copied <= before.st_size, 'evidence source grew')
                hashed.update(data)
                view = memoryview(data)
                while view:
                    written = os.write(outgoing, view)
                    _require(written > 0, 'evidence copy made no progress')
                    view = view[written:]
            _require(copied == before.st_size and _identity(before) == _identity(os.fstat(incoming)),
                     'evidence source changed during copy')
            os.fsync(outgoing)
            os.close(outgoing)
            outgoing = -1
            self.db.execute('INSERT OR REPLACE INTO members VALUES(?,?,?)', (name, copied, hashed.hexdigest()))
            self.db.commit()
            parent = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(parent)
            finally:
                os.close(parent)
            created = False
            if reserved and copied < old[0]:
                self.release_bytes(old[0] - copied)
        finally:
            os.close(incoming)
            if outgoing >= 0:
                os.close(outgoing)
            if created:
                target.unlink()
                if not reserved:
                    self.release_bytes(info.st_size)

    def fulfill(self, name, source):
        row = self.db.execute('SELECT digest FROM members WHERE path=?', (name,)).fetchone()
        _require(row is not None and row[0].startswith('reserved:'), 'unreserved future member')
        self.add_file(name, source)

    def preflight(self, pending=False):
        if not pending:
            _require(self.db.execute("SELECT 1 FROM members WHERE digest LIKE 'reserved:%' LIMIT 1").fetchone() is None,
                     'unfulfilled logical metadata')
        self.directory_cap.validate()

    def close(self):
        # The owning DatabaseGrant retains charges; controller closes its DB.
        self.directory_cap.close()


def _safe_member(name):
    _require(type(name) is str and re.fullmatch(
        r'[A-Za-z0-9_-][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_-][A-Za-z0-9_.-]*)*', name) is not None
        and all(part not in ('.', '..') for part in name.split('/')), 'unsafe member path')
    return name


def _file_hash(path):
    hashed = hashlib.sha256()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        _require(stat.S_ISREG(info.st_mode), 'hash input is not regular')
        while True:
            data = os.read(fd, CHUNK_BYTES)
            if not data:
                break
            hashed.update(data)
        _require(_identity(info) == _identity(os.fstat(fd)), 'hash input mutated')
        return hashed.hexdigest()
    finally:
        os.close(fd)


def snapshot_root(inventory: ContentInventory, binding, execution_origin, checkpoint,
                  descriptor, directory: OwnedDirectory, name, *, predecessor_root=None,
                  verifier=False):
    """Construct a physical root only after exact descriptor/whole-file checks."""
    domain = 'verifier' if verifier else 'catalog'
    kind = domain + '_backup'
    size_key, hash_key = (('work_db_bytes', 'work_db_sha256') if verifier else
                          ('catalog_bytes', 'catalog_sha256'))
    cp = checkpoint['record']
    _require(descriptor['source'] == binding['source'] == cp['source'] and
             descriptor['session_id'] == binding['session_id'] == cp['session_id'] and
             descriptor['unit_ordinal'] == binding['unit_ordinal'] and
             descriptor['resource_plan_sha256'] == binding['resource_plan_sha256'] == cp['resource_plan_sha256'] and
             descriptor['initial_image_sha256'] == binding['initial_image_sha256'] == cp['initial_image_id'] and
             descriptor['checkpoint_sha256'] == hashlib.sha256(canonical(checkpoint)).hexdigest() and
             descriptor['catalog_commit'] == cp['committed_transaction'] and
             descriptor['catalog_logical_digest'] == cp['catalog_logical_digest'] and
             descriptor['semantic_manifest_root'] == cp['page_root'], 'snapshot descriptor binding differs')
    manifest, count, size, sha = inventory.snapshot_tree(directory, name, binding, domain)
    _require(manifest is not None and size == descriptor[size_key] and sha == descriptor[hash_key],
             'snapshot backup descriptor/full file differ')
    root = {**binding, 'schema_version': 2, 'kind': kind, 'execution_origin': execution_origin,
            'checkpoint_sha256': descriptor['checkpoint_sha256'],
            'previous_checkpoint_sha256': descriptor['previous_checkpoint_sha256'],
            'predecessor_root': predecessor_root, 'catalog_commit': descriptor['catalog_commit'],
            'catalog_logical_digest': descriptor['catalog_logical_digest'],
            'semantic_manifest_root': descriptor['semantic_manifest_root'],
            'snapshot_boundary': 'post_verifier_commit_pre_upload' if verifier else 'post_checkpoint_pre_upload',
            'format': 'sqlite3', 'backup_method': 'sqlite_backup_api_v1', 'chunk_bytes_limit': CHUNK_BYTES,
            'chunk_count': count, 'manifest_root': manifest, size_key: size, hash_key: sha}
    if verifier:
        root.update({key: descriptor[key] for key in ('verifier_schema_sha256', 'verifier_algorithm_version',
            'verifier_commit', 'verifier_logical_digest', 'previous_verifier_progress_sha256', 'progress_sha256')})
    _require(descriptor['snapshot_boundary'] == root['snapshot_boundary'] and
             descriptor['format'] == root['format'] and descriptor['backup_method'] == root['backup_method'],
             'snapshot backup mechanism differs')
    reference = inventory.put(kind, canonical(root), [manifest])
    return root, reference


def evidence_root(inventory: ContentInventory, binding, plan: EvidencePlan):
    """Bind all final full evidence members; no native locator enters a seal."""
    plan.preflight()

    def leaves():
        for name, size, sha in plan.db.execute('SELECT path,bytes,digest FROM members ORDER BY path'):
            target = plan.root / _safe_member(name)
            with OwnedDirectory(target.parent, plan.directory_cap.lease_root,
                                plan.directory_cap.check_lease) as parent:
                chunks, _count, actual_size, actual_sha = inventory.snapshot_tree(
                    parent, target.name, binding, 'evidence/' + name)
            _require((actual_size, actual_sha) == (size, sha), 'sealed evidence changed')
            member = {'unit_ordinal': binding['unit_ordinal'], 'member_path': name, 'bytes': size, 'sha256': sha}
            inventory.db.execute('INSERT INTO members VALUES(?,?,?,?,?,?)',
                (binding['unit_ordinal'], name, size, sha, None if chunks is None else chunks['kind'],
                 None if chunks is None else chunks['sha256']))
            yield name, {'member': member, 'chunks': chunks}
    # Tree construction shares the disk levels scratch with snapshot_tree. Store
    # the leaf index first, then build its tree from a second streaming DB cursor.
    for _key, _row in leaves():
        pass
    inventory.db.commit()

    def stored():
        for name, size, sha, kind, content_sha in inventory.db.execute(
                'SELECT member_path,bytes,sha256,kind,content_sha256 FROM members WHERE unit_ordinal=? ORDER BY member_path',
                (binding['unit_ordinal'],)):
            chunks = None
            if kind is not None:
                length = inventory.db.execute('SELECT bytes FROM content WHERE kind=? AND sha256=?',
                                              (kind, content_sha)).fetchone()
                _require(length is not None, 'evidence chunk root missing')
                chunks = {'kind': kind, 'sha256': content_sha, 'bytes': length[0]}
            yield name, {'member': {'unit_ordinal': binding['unit_ordinal'], 'member_path': name,
                                   'bytes': size, 'sha256': sha}, 'chunks': chunks}
    return inventory.keyed_tree('evidence_manifest_node', binding, 'evidence_index', stored())


def iter_keyed_manifest(reference, resolve, binding, domain, kind):
    """Validate exact sorted tree summaries while yielding one leaf at a time."""
    import json
    scope = scope_digest(binding, domain)
    previous = None

    def walk(current, expected_level=None):
        nonlocal previous
        raw = resolve(current)
        _require(type(raw) is bytes and len(raw) <= 8 * CHUNK_BYTES and
                 content_ref(kind, raw) == current, 'keyed manifest content differs')
        node = json.loads(raw)
        _require(canonical(node) == raw and set(node) == {'schema_version', 'kind', 'scope_sha256',
                 'level', 'first_key', 'last_key', 'item_count', 'entries'} and
                 node['schema_version'] == 1 and node['kind'] == kind and node['scope_sha256'] == scope and
                 type(node['level']) is int and 0 <= node['level'] <= 16 and
                 (expected_level is None or node['level'] == expected_level) and
                 type(node['entries']) is list and 1 <= len(node['entries']) <= 256,
                 'keyed manifest schema/scope/level differs')
        first, last, count = None, None, 0
        for entry in node['entries']:
            if node['level']:
                _require(set(entry) == {'first_key', 'last_key', 'item_count', 'child'},
                         'keyed branch fields differ')
                yield from walk(entry['child'], node['level'] - 1)
                summary = summaries.pop()
                _require(summary == {key: entry[key] for key in ('first_key', 'last_key', 'item_count')},
                         'keyed branch summary differs')
                key_first, key_last, amount = entry['first_key'], entry['last_key'], entry['item_count']
            else:
                if kind == 'evidence_manifest_node':
                    _require(set(entry) == {'member', 'chunks'} and set(entry['member']) ==
                             {'unit_ordinal', 'member_path', 'bytes', 'sha256'}, 'evidence leaf fields differ')
                    key = _safe_member(entry['member']['member_path'])
                    _require(entry['member']['unit_ordinal'] == binding['unit_ordinal'], 'evidence unit differs')
                else:
                    _require(set(entry) == {'content', 'part_ordinal', 'member_path'} and
                             type(entry['part_ordinal']) is int and 0 <= entry['part_ordinal'] < 32,
                             'transfer leaf fields differ')
                    _safe_member(entry['member_path'])
                    key = canonical([entry['content']['kind'], entry['content']['sha256']]).decode()
                _require(previous is None or previous < key, 'keyed leaves duplicate or unsorted')
                previous = key
                key_first = key_last = key
                amount = 1
                yield entry
            _require(last is None or last < key_first, 'keyed branch overlap')
            first = key_first if first is None else first
            last = key_last
            count += amount
        _require((node['first_key'], node['last_key'], node['item_count']) == (first, last, count),
                 'keyed node summary differs')
        summaries.append({'first_key': first, 'last_key': last, 'item_count': count})

    summaries = []
    yield from walk(reference)
    _require(len(summaries) == 1, 'keyed manifest traversal incomplete')


PLANNED_KEYS = {'schema_version', 'planned_work_unit', 'checkpoint', 'initial_image', 'session',
                'artifacts', 'stage_complete'}
ACCEPTED_KEYS = {'schema_version', 'work_unit', 'checkpoint', 'initial_image', 'session',
                 'artifacts', 'stage_complete'}


def planned_result(execution_result: dict, execution_origin: dict) -> dict:
    """Deterministic PlannedResult: the ordinary seven keys with an empty root list.

    The bytes are hashed into the publication intent before any receipt exists, so
    they can contain neither artifact roots nor any later hash (no result/ZIP/intent
    cycle). The original execution origin is a source-owned execute dispatch.
    """
    _require(set(execution_result) == ACCEPTED_KEYS, 'ordinary result fields differ')
    unit = execution_result['work_unit']
    _require(type(unit) is dict and unit['artifact_roots'] == [] and
             execution_origin['operation'] == 'execute' and execution_origin['attempt'] == 1 and
             execution_origin['mode'] == 'unit', 'planned result already claims artifacts or has no original execution')
    _require(all(unit[key] == execution_origin[key] for key in ('run', 'job', 'task', 'attempt', 'mode')),
             'planned WorkUnit is not the original execution task')
    planned = {key: value for key, value in execution_result.items() if key != 'work_unit'}
    planned['planned_work_unit'] = unit
    return planned


def accepted_result(planned: dict, execution_origin: dict, original_receipts, reference) -> dict:
    """Reconstruct the single ordinary result from authenticated original roots.

    `original_receipts` are the ArtifactReceipt envelopes of the direct original
    execution upload in original part order. Later publishers' receipts are never
    accepted here: each must carry the original run/job/task identity. `reference`
    is the controller's existing envelope reference function (id + transport hash).
    """
    _require(type(planned) is dict and set(planned) == PLANNED_KEYS, 'planned result fields differ')
    unit = dict(planned['planned_work_unit'])
    _require(unit['artifact_roots'] == [], 'preupload result already claims artifact authority')
    _require((unit['run'], unit['job'], unit['task'], unit['attempt']) ==
             (execution_origin['run'], execution_origin['job'], execution_origin['task'], 1),
             'planned WorkUnit differs from original execution')
    refs = []
    for receipt in original_receipts:
        record = receipt['record']
        _require((record['run'], record['job'], record['task'], record['attempt']) ==
                 (execution_origin['run'], execution_origin['job'], execution_origin['task'], 1) and
                 record['unit_ordinal'] == unit['unit_ordinal'],
                 'publisher receipt relabeled original execution')
        refs.append(reference(receipt, record['artifact_id']))
    _require(0 < len(refs) <= 32 and len({row['id'] for row in refs}) == len(refs),
             'original direct artifacts absent, duplicate or excessive')
    unit['artifact_roots'] = refs
    result = {key: value for key, value in planned.items() if key != 'planned_work_unit'}
    result['work_unit'] = unit
    _require(set(result) == ACCEPTED_KEYS, 'accepted result fields differ')
    return result


def prepare_publication(inventory: ContentInventory, ledger: PublicationLedger, binding,
                        execution_origin, execution_result, catalog_root, verifier_root, evidence):
    """Freeze the planned result, then intent; no postupload receipt hash cycle."""
    planned = planned_result(execution_result, execution_origin)
    unit = execution_result['work_unit']
    result_ref = inventory.put('planned_execution_result', canonical(planned))
    cp = execution_result['checkpoint']
    prior = unit['expected_prior_checkpoint']
    intent = {**binding, 'schema_version': 1, 'kind': 'publication_intent',
              'execution_origin': execution_origin,
              'previous_checkpoint': None if prior is None else {'kind': 'Checkpoint', 'id': prior, 'sha256': prior},
              'candidate_checkpoint': {'kind': 'Checkpoint', 'id': hashlib.sha256(canonical(cp)).hexdigest(),
                                       'sha256': hashlib.sha256(canonical(cp)).hexdigest()},
              'catalog_root': catalog_root, 'verifier_root': verifier_root,
              'execution_result': result_ref, 'evidence_root': evidence}
    dependencies = [catalog_root, result_ref, evidence]
    if verifier_root is not None:
        dependencies.append(verifier_root)
    intent_ref = inventory.put('publication_intent', canonical(intent), dependencies)
    ledger.db.execute('DELETE FROM queue')
    ledger.db.execute('INSERT INTO queue VALUES(?,?,?)',
                      (intent_ref['kind'], intent_ref['sha256'], intent_ref['bytes']))
    # Expand required closure on disk. No global content list or SQLite payload
    # cache is a substitute for each authenticated content identity.
    while True:
        before = ledger.db.total_changes
        for kind, sha, _size in ledger.db.execute('SELECT kind,sha256,bytes FROM queue ORDER BY kind,sha256'):
            for child_kind, child_sha, size in inventory.db.execute(
                    'SELECT kind,sha256,bytes FROM edges WHERE owner_kind=? AND owner_sha256=? ORDER BY ordinal',
                    (kind, sha)):
                ledger.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)', (child_kind, child_sha, size))
        if ledger.db.total_changes == before:
            break
    sha, count = ledger.coverage()
    pending = {'schema_version': 1, 'kind': 'pending_publication', 'intent': intent_ref,
               'previous_batch': None, 'coverage_sha256': sha, 'coverage_count': count, 'next_batch_ordinal': 0}
    ledger.db.execute('INSERT OR REPLACE INTO state VALUES(?,?)', ('pending', canonical(pending)))
    ledger.db.commit()
    return intent, pending


def missing_content(ledger: PublicationLedger):
    for kind, sha, size in ledger.db.execute('''
        SELECT q.kind,q.sha256,q.bytes FROM queue q LEFT JOIN coverage c
        ON c.kind=q.kind AND c.sha256=q.sha256
        WHERE c.sha256 IS NULL ORDER BY (q.kind!='publication_intent'),q.kind,q.sha256'''):
        yield {'kind': kind, 'sha256': sha, 'bytes': size}


def resume_publication(ledger: PublicationLedger, pending, batch, authenticated_contents):
    """Commit exact validated batch coverage; incomplete transport never advances."""
    _require(batch['schema_version'] == 1 and batch['kind'] == 'transfer_batch_receipt' and
             batch['intent_sha256'] == pending['intent']['sha256'] and
             batch['previous_batch'] == pending['previous_batch'] and
             batch['batch_ordinal'] == pending['next_batch_ordinal'] and
             batch['acceptance'] is False and batch['proof_checkpoint_advanced'] is False,
             'publication chain skips or claims proof credit')
    before_sha, before_count = ledger.coverage()
    _require((before_sha, before_count) == (pending['coverage_sha256'], pending['coverage_count']) and
             batch['coverage_before_sha256'] == before_sha, 'publication before-coverage differs')
    # Authenticate within one transaction including the advertised after digest.
    ledger.db.execute('BEGIN IMMEDIATE')
    try:
        for reference in authenticated_contents:
            old = ledger.db.execute('SELECT bytes FROM coverage WHERE kind=? AND sha256=?',
                (reference['kind'], reference['sha256'])).fetchone()
            _require(old is None or old == (reference['bytes'],), 'authenticated coverage collision')
            ledger.db.execute('INSERT OR IGNORE INTO coverage VALUES(?,?,?)',
                (reference['kind'], reference['sha256'], reference['bytes']))
        after_sha, after_count = ledger.coverage()
        _require((after_sha, after_count) == (batch['coverage_after_sha256'], batch['coverage_count']),
                 'publication advertised after-coverage differs')
        _require(batch['purpose'] == 'activate' or after_count > before_count,
                 'transfer-only batch made no authenticated progress')
        reference = content_ref('transfer_batch_receipt', canonical(batch))
        result = {**pending, 'previous_batch': reference, 'coverage_sha256': after_sha,
                  'coverage_count': after_count, 'next_batch_ordinal': pending['next_batch_ordinal'] + 1}
        ledger.db.execute('INSERT OR REPLACE INTO state VALUES(?,?)', ('pending', canonical(result)))
        ledger.db.commit()
        return result
    except BaseException:
        ledger.db.rollback()
        raise


def finish_publication(ledger: PublicationLedger, intent, pending, activation, planned,
                       original_receipts, reference, validate_prefix):
    """Return the ordinary accepted result exactly once, only after closure checks.

    The parent has already authenticated terminal-success execution/publisher
    origins and every native locator. `validate_prefix` performs the existing
    stage-appropriate Rust/M3/verifier/TLC checks on reconstructed full images
    and the restored-validation evidence named by the activation.
    """
    _require(next(missing_content(ledger), None) is None, 'publication payload closure incomplete')
    _require(activation['schema_version'] == 1 and activation['kind'] == 'publication_activation' and
             activation['intent_sha256'] == pending['intent']['sha256'] and
             activation['final_batch'] == pending['previous_batch'] and activation['activated'] is True and
             activation['acceptance'] is False and activation['execution_origin'] == intent['execution_origin'] and
             all(activation[key] == intent[key] for key in ('source', 'session_id', 'unit_ordinal',
                 'resource_plan_sha256', 'initial_image_sha256', 'previous_checkpoint', 'candidate_checkpoint',
                 'catalog_root', 'verifier_root', 'execution_result')), 'activation/intent identity differs')
    _require(ledger.db.execute('SELECT 1 FROM state WHERE key=?', ('activated:' + pending['intent']['sha256'],)).fetchone()
             is None, 'publication already activated')
    _require(content_ref('planned_execution_result', canonical(planned)) == intent['execution_result'],
             'planned execution result changed')
    result = accepted_result(planned, intent['execution_origin'], original_receipts, reference)
    _require(result['stage_complete'] == activation['stage_complete'] and
             result['work_unit']['stage'] == activation['stage'],
             'activation stage differs from frozen result')
    validate_prefix(result, activation['validations'])
    ledger.db.execute('INSERT INTO state VALUES(?,?)',
                      ('activated:' + pending['intent']['sha256'], canonical(activation)))
    ledger.db.commit()
    return result


def native_batch(inventory: ContentInventory, ledger: PublicationLedger, binding, part_plan, *, only=None):
    """Pack one bounded native batch, reserving exact source-bounded tree space.

    The caller gives an existing native PartPlan (32 slots, 300 members, 512MiB).
    Transport manifest nodes are not themselves coverage or proof evidence.
    `only` is the closed conformance recipe's offer set of content identities; it
    restricts which genuine required payload is offered, never the slot/size caps.
    """
    import math
    # Every actual transfer leaf has these fixed generated path widths. This is
    # an encoded metadata bound, not a source-state or SQLite packing theorem.
    maximum_integer = (1 << 63) - 1
    worst_ref = {'kind': 'x' * 64, 'sha256': 'f' * 64, 'bytes': maximum_integer}
    worst_key = canonical([worst_ref['kind'], worst_ref['sha256']]).decode()
    leaf = {'content': worst_ref, 'part_ordinal': 31,
            'member_path': 'content/' + 'x' * 64 + '/' + 'f' * 64}
    branch = {'first_key': worst_key, 'last_key': worst_key, 'item_count': maximum_integer,
              'child': worst_ref}
    node_header = {'schema_version': 1, 'kind': 'transfer_manifest_node', 'scope_sha256': 'f' * 64,
                   'level': maximum_integer, 'first_key': worst_key, 'last_key': worst_key,
                   'item_count': maximum_integer}
    metadata_bound = max(len(canonical({**node_header, 'entries': [entry] * 256}))
                         for entry in (leaf, branch))
    maximum_leaves = 32 * 300
    maximum_nodes = 0
    while maximum_leaves > 1:
        maximum_leaves = math.ceil(maximum_leaves / 256)
        maximum_nodes += maximum_leaves
    maximum_nodes = max(1, maximum_nodes)
    for ordinal in range(maximum_nodes):
        part_plan.reserve('transfer/node-' + str(ordinal) + '.json', metadata_bound, 'transfer_manifest')
    selected = 0
    for reference in missing_content(ledger):
        if only is not None and (reference['kind'], reference['sha256']) not in only:
            continue
        _require(len(reference['kind']) <= 64 and bool(re.fullmatch('[a-z][a-z0-9_]*', reference['kind'])),
                 'content kind is not a registered generated transport name')
        name = 'content/' + reference['kind'] + '/' + reference['sha256']
        # Ask the actual bounded allocator before copying; capacity refusal ends
        # this batch, while corrupt bytes and every other failure remain fatal.
        can_fit = False
        for part in range(part_plan.slots):
            count, size = part_plan.db.execute(
                'SELECT count(*),coalesce(sum(bytes),0) FROM members WHERE part=?', (part,)).fetchone()
            if count < 299 and size + reference['bytes'] <= 512 * CHUNK_BYTES - 16 * CHUNK_BYTES:
                can_fit = True
                break
        if not can_fit:
            break
        inventory.bytes(reference)
        part_plan.add_file(name, inventory.cas.path / reference['sha256'])
        selected += 1
    _require(selected > 0, 'resource_blocked: native transfer cannot fit any required content')

    def leaves():
        # Generated names sort exactly as the canonical [kind,sha] key order for
        # the registered lowercase identifier vocabulary.
        for name, part, size, sha in part_plan.db.execute(
                "SELECT path,part,bytes,digest FROM members WHERE path LIKE 'content/%' ORDER BY path"):
            _prefix, kind, identity = name.split('/')
            _require(sha == identity, 'CAS/native member digest differs')
            reference = {'kind': kind, 'sha256': sha, 'bytes': size}
            yield canonical([kind, sha]).decode(), {'content': reference, 'part_ordinal': part, 'member_path': name}
    transfer = inventory.keyed_tree('transfer_manifest_node', binding, 'transfer_index', leaves())
    # Enumerate only this tree using a disk queue; no unbounded manifest array.
    ledger.db.execute('DELETE FROM state WHERE key LIKE ?', ('transfer-node:%',))
    ledger.db.execute('INSERT INTO state VALUES(?,?)', ('transfer-node:' + transfer['sha256'], canonical(transfer)))
    position = 0
    while True:
        row = ledger.db.execute("SELECT key,value FROM state WHERE key LIKE 'transfer-node:%' AND key NOT LIKE 'transfer-node:done:%' ORDER BY key LIMIT 1").fetchone()
        if row is None:
            break
        import json
        reference = json.loads(row[1])
        raw = inventory.bytes(reference)
        node = json.loads(raw)
        if node['level']:
            for child in node['entries']:
                ledger.db.execute('INSERT OR IGNORE INTO state VALUES(?,?)',
                                  ('transfer-node:' + child['child']['sha256'], canonical(child['child'])))
        _require(position < maximum_nodes and len(raw) <= metadata_bound, 'actual transfer metadata exceeds source bound')
        name = 'transfer/node-' + str(position) + '.json'
        part_plan.add_bytes(name, raw)
        ledger.db.execute('DELETE FROM state WHERE key=?', (row[0],))
        ledger.db.execute('INSERT INTO state VALUES(?,?)', ('transfer-node:done:' + reference['sha256'], row[1]))
        position += 1
    for ordinal in range(position, maximum_nodes):
        # Remove unused metadata reservations, never already materialized bytes.
        name = 'transfer/node-' + str(ordinal) + '.json'
        part_plan.db.execute("DELETE FROM members WHERE path=? AND digest LIKE 'reserved:%'", (name,))
    part_plan.db.commit()
    ledger.db.commit()
    part_plan.preflight()
    return transfer


CONFORMANCE_CASE = 'native_snapshot_three_batch_v1'


def conformance_case_sha256(registration_sha256: str, session_id: str, anchor_intent_sha256: str) -> str:
    return hashlib.sha256(canonical({'registration_sha256': registration_sha256, 'session_id': session_id,
        'case_id': CONFORMANCE_CASE, 'anchor_intent_sha256': anchor_intent_sha256})).hexdigest()


def conformance_chunks(catalog_root: dict, verifier_root: dict, resolve) -> list[dict]:
    """First two distinct genuine snapshot_chunk identities of both complete manifests.

    Returns every distinct chunk identity sorted by canonical [kind,sha256]. The
    walk streams the complete bounded trees and substitutes nothing; a closure with
    fewer than two distinct chunks is not schedulable (the caller reports it).
    """
    import json
    seen = {}
    for root, domain in ((catalog_root, 'catalog'), (verifier_root, 'verifier')):
        stack = [root['manifest_root']]
        while stack:
            reference = stack.pop()
            raw = resolve(reference)
            _require(content_ref('snapshot_manifest_node', raw) == reference, 'conformance manifest node differs')
            node = json.loads(raw)
            for entry in node['entries']:
                if node['level']:
                    stack.append(entry['child'])
                else:
                    content = entry['content']
                    _require(content['kind'] == 'snapshot_chunk', 'conformance leaf is not a snapshot chunk')
                    seen[content['sha256']] = content
    return [seen[key] for key in sorted(seen)]


def conformance_offer(batch_ordinal: int, chunks: list[dict]):
    """Offer set for one rehearsal batch, or None for the normal remainder packing.

    Batch zero offers exactly the first sorted chunk and batch one exactly the
    second as their sole new payload; batch two onward offers the remaining genuine
    closure. Transport-manifest/control framing is bounded metadata, not payload.
    """
    _require(len(chunks) >= 2, 'conformance case needs two distinct real snapshot chunks')
    if batch_ordinal in (0, 1):
        return {('snapshot_chunk', chunks[batch_ordinal]['sha256'])}
    return None


def check_conformance_payloads(chunks: list[dict], batches: list[list[tuple[str, str]]]) -> None:
    """Recompute the forced three-batch shape from authenticated per-batch payload.

    `batches` lists, in dispatch order, each terminal-successful transfer batch's
    new payload identities (kind, sha256) from its authenticated transfer manifest.
    """
    _require(len(chunks) >= 2 and len(batches) >= 3 and all(batches), 'fewer than three nonempty transfer batches')
    _require(batches[0] == [('snapshot_chunk', chunks[0]['sha256'])] and
             batches[1] == [('snapshot_chunk', chunks[1]['sha256'])], 'first two batches are not the two sole real chunks')
    flat = [identity for batch in batches for identity in batch]
    _require(len(flat) == len(set(flat)), 'conformance payload credited twice')
