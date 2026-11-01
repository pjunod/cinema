"""Closed physical reservations for the independent paged proof harness.

Logical record quotas never estimate SQLite allocation. These are prospective
operational grants; exhausting one is an incomplete disposition, never proof.
"""
from __future__ import annotations

import hashlib
import fcntl
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import sqlite3
import stat

MAXIMUM = (1 << 63) - 1
MIB = 1024 * 1024
FAMILY_OWNERS = {
    'catalog_main': 'catalog', 'verifier_main': 'verifier',
    'catalog_export_scratch': 'catalog_export',
    'verifier_continuation_scratch': 'verifier_continuation',
    'catalog_or_verifier_restore_scratch': 'snapshot_restore',
    'publication_pack_scratch': 'snapshot_transport',
    'authentication_index': 'provenance',
    'publication_pending_scratch': 'session_controller',
}
POOL_OWNERS = {
    'snapshot_spool_bytes': 'snapshot_transport',
    'restore_staging_bytes': 'snapshot_restore',
    'transfer_staging_bytes': 'snapshot_transport', 'cas_bytes': 'provenance',
    'source_runtime_scratch_bytes': 'session_controller',
    'control_bytes': 'session_controller',
}
SOURCE_MAP_PATH = Path(__file__).with_name('async_physical_table_map.json')


class PhysicalPolicyError(ValueError):
    """A policy, source map, or restored identity is invalid."""


class PhysicalIncomplete(RuntimeError):
    """An admitted physical reservation cannot accommodate this operation."""


def require(ok, message):
    if not ok:
        raise PhysicalPolicyError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def closed(value, fields, description):
    require(type(value) is dict and set(value) == set(fields), 'closed ' + description + ' differs')


def integer(value, *, positive=False):
    require(type(value) is int and (1 if positive else 0) <= value <= MAXIMUM, 'invalid physical byte/count integer')
    return value


def sha(value):
    require(type(value) is str and re.fullmatch('[0-9a-f]{64}', value) is not None, 'invalid physical SHA256')
    return value


def load_source_map(path=SOURCE_MAP_PATH):
    raw = Path(path).read_bytes()
    value = json.loads(raw)
    closed(value, ('schema_version', 'kind', 'sources', 'families'), 'physical source map')
    require(value['schema_version'] == 1 and value['kind'] == 'physical_table_map', 'physical source map version')
    require(set(value['families']) == set(FAMILY_OWNERS), 'physical source map family inventory')
    root = Path(__file__).resolve().parents[1]
    for name, expected in value['sources'].items():
        path = root / name
        require(path.is_file() and not path.is_symlink() and path.resolve().is_relative_to(root), 'unsafe physical source-map path')
        require(hashlib.sha256(path.read_bytes()).hexdigest() == sha(expected), 'physical source-map constructor drift: ' + name)
    return value


def validate_physical_policy(policy, source_map):
    closed(policy, ('schema_version', 'kind', 'table_map_sha256', 'families', 'pools'), 'physical policy')
    require(policy['schema_version'] == 3 and policy['kind'] == 'physical_policy', 'physical policy version')
    require(sha(policy['table_map_sha256']) == digest(source_map), 'physical policy/table-map digest differs')
    require(set(policy['families']) == set(FAMILY_OWNERS) and set(policy['pools']) == set(POOL_OWNERS), 'physical pool/family inventory')
    require(set(source_map['families']) == set(FAMILY_OWNERS), 'physical source-map family coverage')
    for family, owner in FAMILY_OWNERS.items():
        entry = policy['families'][family]
        closed(entry, ('owner', 'database_bytes', 'main_bytes', 'journal_bytes', 'temp_bytes'), family)
        require(entry['owner'] == owner, 'physical family owner differs: ' + family)
        for key in ('database_bytes', 'main_bytes', 'journal_bytes', 'temp_bytes'):
            integer(entry[key], positive=True)
        require(entry['main_bytes'] >= entry['database_bytes'], 'aggregate main reservation below individual database ceiling')
    for amount in policy['pools'].values():
        integer(amount, positive=True)
    return policy


def _normal_sql(sql):
    return re.sub(r'\s+', ' ', re.sub(r'\bIF\s+NOT\s+EXISTS\s+', '', sql, flags=re.I)).strip().rstrip(';')


def _role(source_map, role):
    matches = [(family, database) for family, databases in source_map['families'].items()
               for database in databases if database['database_role'] == role]
    require(len(matches) == 1, 'unknown/duplicate physical database role: ' + role)
    return matches[0]


def _schema_rows(source_map, role, tables, indexes):
    _, entry = _role(source_map, role)
    objects = entry['objects']
    explicit = {x['name']: x for x in objects}
    require(len(explicit) == len(objects), 'duplicate source-map database object')
    expected_tables = {x['name'] for x in objects if x['type'] == 'table'}
    auto = any('autoincrement' in x['ddl'].lower() for x in objects)
    if auto:
        expected_tables.add('sqlite_sequence')
    require(type(tables) is list and len(tables) == len(set(tables)) and set(tables) == expected_tables,
            'unknown/missing installed physical table: ' + role)
    explicit_indexes = {x['name'] for x in objects if x['type'] == 'index'}
    require(type(indexes) is list and len(indexes) == len(set(indexes)) and explicit_indexes <= set(indexes),
            'unknown/missing explicit installed physical index')
    rows = [[x['type'], x['name'], x['table'], _normal_sql(x['ddl'])] for x in objects]
    if auto:
        rows.append(['table', 'sqlite_sequence', 'sqlite_sequence', 'CREATE TABLE sqlite_sequence(name,seq)'])
    # Implicit indexes are engine objects of declared PK/UNIQUE constraints.
    # Their exact ordinal and column validation occurs in observe_geometry.
    for name in set(indexes) - explicit_indexes:
        match = re.fullmatch(r'sqlite_autoindex_(.+)_([1-9][0-9]*)', name)
        require(match is not None and match[1] in expected_tables and match[1] in explicit,
                'unknown implicit physical index')
        ddl = explicit[match[1]]['ddl']
        constraints = len(re.findall(r'\b(?:PRIMARY\s+KEY|UNIQUE)\b', ddl, re.I))
        require(int(match[2]) <= constraints, 'implicit index has no declared source constraint')
        rows.append(['index', name, match[1], None])
    return sorted(rows, key=lambda x: (x[0], x[1], x[2]))


def observe_geometry(connection, database_role, source_map):
    """Actual SQLite observation, callable only in authorized calibration."""
    actual = list(connection.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE type IN ('table','index') ORDER BY type,name,tbl_name"))
    tables = [x[1] for x in actual if x[0] == 'table']
    indexes = [x[1] for x in actual if x[0] == 'index']
    expected = _schema_rows(source_map, database_role, tables, indexes)
    normalized = [[kind, name, table, None if sql is None else _normal_sql(sql)] for kind, name, table, sql in actual]
    require(normalized == expected, 'installed physical DDL differs from source map')
    # Attribute each implicit index to an actual declared uniqueness constraint,
    # through SQLite's own constraint-origin and exact indexed column metadata.
    for table in tables:
        if table == 'sqlite_sequence':
            continue
        for row in connection.execute('PRAGMA index_list("' + table + '")'):
            if row[1].startswith('sqlite_autoindex_'):
                require(row[3] in ('pk', 'u') and row[2] == 1, 'implicit index is not a declared unique/PK constraint')
                columns = [r[2] for r in connection.execute('PRAGMA index_info("' + row[1] + '")')]
                require(columns and all(type(x) is str for x in columns), 'unmapped expression in implicit source index')
    return {'database_role': database_role, 'sqlite_source_id': connection.execute('SELECT sqlite_source_id()').fetchone()[0],
            'schema_sha256': digest(expected), 'page_size': connection.execute('PRAGMA page_size').fetchone()[0],
            'page_count': connection.execute('PRAGMA page_count').fetchone()[0], 'tables': tables, 'indexes': indexes}


def calibrate_physical_policy(policy, observed_geometry, observed_baseline, plan, *, source_map=None):
    source_map = load_source_map() if source_map is None else source_map
    validate_physical_policy(policy, source_map)
    require(type(observed_geometry) is dict and set(observed_geometry) == set(FAMILY_OWNERS), 'complete actual geometry family inventory required')
    closed(observed_baseline, ('source_workspace_bytes', 'existing_unpooled_bytes'), 'physical baseline')
    for value in observed_baseline.values():
        integer(value)
    require(observed_baseline['source_workspace_bytes'] <= policy['pools']['source_runtime_scratch_bytes'], 'source/runtime pool does not fit measured source baseline')
    for family, observations in observed_geometry.items():
        require(type(observations) is list and observations, 'missing actual physical family geometry')
        declared = {x['database_role'] for x in source_map['families'][family]}
        seen = set()
        empty_bytes = 0
        for geometry in observations:
            closed(geometry, ('database_role', 'sqlite_source_id', 'schema_sha256', 'page_size', 'page_count', 'tables', 'indexes'), 'physical geometry')
            role = geometry['database_role']
            require(role in declared and role not in seen, 'unknown/duplicate measured database role')
            seen.add(role)
            require(type(geometry['sqlite_source_id']) is str and geometry['sqlite_source_id'], 'unmeasured SQLite source identity')
            size, count = integer(geometry['page_size'], positive=True), integer(geometry['page_count'])
            require(size in (512, 1024, 2048, 4096, 8192, 16384, 32768, 65536), 'invalid measured SQLite page size')
            require(sha(geometry['schema_sha256']) == digest(_schema_rows(source_map, role, geometry['tables'], geometry['indexes'])), 'physical installed schema/map drift')
            effective = policy['families'][family]['database_bytes'] // size * size
            require(effective > 0 and count * size <= effective, 'empty physical database exceeds declared ceiling')
            empty_bytes += count * size
        require(seen == declared, 'unmeasured source-declared physical database role')
        require(empty_bytes <= policy['families'][family]['main_bytes'], 'simultaneous empty family databases exceed reservation')
    peak = (sum(v[k] for v in policy['families'].values() for k in ('main_bytes', 'journal_bytes', 'temp_bytes'))
            + sum(policy['pools'].values()) + observed_baseline['existing_unpooled_bytes']
            + 6 * integer(plan['total_retained_artifact_bytes'], positive=True)
            + 32 * MIB * (integer(plan['maximum_units'], positive=True) + 3))
    integer(peak, positive=True)
    require(peak <= plan['total_session_disk_bytes'], 'physical peak exceeds immutable session disk ceiling')
    return {'schema_version': 1, 'kind': 'physical_calibration', 'policy_sha256': digest(policy),
            'table_map_sha256': policy['table_map_sha256'], 'geometry': observed_geometry,
            'baseline': observed_baseline, 'peak_bytes': peak}


HEX64 = re.compile('[0-9a-f]{64}')
UUID_TEXT = re.compile('[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')
SAFE_SEGMENT = re.compile('[A-Za-z0-9_][A-Za-z0-9._-]{0,127}')
DB_COMPANIONS = ('-wal', '-shm', '-journal')
FAMILY_COMPONENTS = ('main', 'journal', 'temp')
GRANT_DIRECTORY = 'physical-grants'
TRANSFER_DIRECTORY = 'physical-transfers'


def safe_relative(value):
    """Owned-root-relative SafePath: no absolute, empty, dot, or dot-dot segment."""
    require(type(value) is str and 0 < len(value) <= 1024 and '\\' not in value and '\x00' not in value,
            'invalid physical SafePath')
    parts = value.split('/')
    require(len(parts) <= 32 and all(SAFE_SEGMENT.fullmatch(x) for x in parts), 'unsafe physical relative path')
    return PurePosixPath(value)


def _related(a, a_tree, b, b_tree):
    """Do two owned-root-relative claims alias one another (equal or nested subtree)?"""
    return (a == b or (a_tree and a in b.parents) or (b_tree and b in a.parents))


def _owned(path, root):
    path = Path(path)
    require(path.is_absolute() and path.resolve().is_relative_to(root) and not path.is_symlink(), 'unsafe physical reservation path')
    current = path.parent
    while current != root:
        require(not current.is_symlink(), 'symlink in physical storage parent')
        current = current.parent
    return path


def _fsync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _fsync_existing_ancestor(path, root):
    current = Path(path).parent
    while not current.exists() and current != root:
        current = current.parent
    _fsync_directory(current)


def process_start_ticks(pid):
    """Linux process start identity (field 22 of /proc/<pid>/stat), fail-closed."""
    require(type(pid) is int and pid > 0, 'invalid physical grant parent pid')
    try:
        raw = Path('/proc/%d/stat' % pid).read_text()
        tail = raw[raw.rindex(')') + 2:].split()
        return int(tail[19])
    except (OSError, ValueError, IndexError) as error:
        raise PhysicalPolicyError('parent process identity is unavailable/not live') from error


def component_key(family_or_pool, component):
    return family_or_pool, component


def expected_owner(family_or_pool):
    owner = FAMILY_OWNERS.get(family_or_pool, POOL_OWNERS.get(family_or_pool))
    require(owner is not None, 'unknown physical owner resource: ' + str(family_or_pool))
    return owner


class HeldLease:
    """The already-held owned-slot lease: an open descriptor and its owned-root-relative name.
    Any object exposing integer `fd` and `relative_path` is accepted by the ledger (duck typing).

    The ledger never opens a lease by filename. It only proves that the held
    descriptor and that name denote the same non-symlink regular file.
    """
    def __init__(self, fd, relative_path):
        require(type(fd) is int and fd >= 0, 'invalid held lease descriptor')
        self.fd, self.relative_path = fd, safe_relative(relative_path)


class Reservation:
    def __init__(self, ledger, owner, key, amount, reservation_id=None):
        self.ledger, self.owner, self.key, self.bytes = ledger, owner, key, amount
        self.reservation_id = secrets.token_hex(32) if reservation_id is None else reservation_id
        self.paths = set()
        self.subtrees = set()
        self.released = False
        self.delegated = False
        self.transfer = None
        self.allocation = None

    def _claim(self, path, recursive):
        require(not self.released, 'released physical reservation')
        require(not self.delegated and self.transfer is None, 'physical reservation is delegated/in transfer')
        path = self.ledger._claim(self, path, recursive)
        (self.subtrees if recursive else self.paths).add(path)
        return self

    def track(self, path):
        return self._claim(path, False)

    def track_subtree(self, path):
        return self._claim(path, True)

    def scope(self):
        """Exact claims as sorted (relative_path, recursive) pairs."""
        root = self.ledger.directory
        return sorted([(str(p.relative_to(root)), False) for p in self.paths]
                      + [(str(p.relative_to(root)), True) for p in self.subtrees])

    def surviving_files(self):
        found = []
        for path in sorted(self.paths):
            if path.is_symlink() or path.exists():
                found.append(path)
        for root in sorted(self.subtrees):
            if root.is_symlink():
                found.append(root)
            elif root.exists():
                for directory, directories, names in os.walk(root, followlinks=False):
                    found.extend(Path(directory) / n for n in names)
                    found.extend(Path(directory) / n for n in directories if (Path(directory) / n).is_symlink())
        return found

    def actual_bytes(self):
        actual = 0
        for path in self.surviving_files():
            require(not path.is_symlink() and path.is_file(), 'nonregular owned physical file')
            actual += path.stat().st_size
        return actual

    def grow(self, bytes):
        integer(bytes)
        require(not self.released and bytes >= self.bytes, 'invalid physical reservation growth')
        increase = bytes - self.bytes
        if self.ledger.used[self.key] + increase > self.ledger.limits[self.key]:
            raise PhysicalIncomplete('prospective physical grant growth exhausted')
        if self.allocation is not None and self.allocation.spent + increase > self.allocation.bytes:
            raise PhysicalIncomplete('physical growth exceeds child grant allocation')
        self.ledger.used[self.key] += increase
        if self.allocation is not None:
            self.allocation.spent += increase
        self.bytes = bytes

    def check(self):
        if self.released:
            return
        if self.actual_bytes() > self.bytes:
            raise PhysicalIncomplete('owned files exceed prospective physical grant')

    def release_after_removal(self):
        require(not self.released, 'physical reservation released twice')
        require(not self.delegated and self.transfer is None, 'delegated/in-transfer physical reservation cannot be released')
        require(not self.surviving_files(), 'surviving physical file retains its reservation')
        _fsync_after_removal(self)
        self.ledger._unclaim(self)
        self.ledger.used[self.key] -= self.bytes
        if self.allocation is not None:
            self.allocation.spent -= self.bytes
        self.released = True


def _fsync_after_removal(reservation):
    root = reservation.ledger.directory
    for path in reservation.paths | reservation.subtrees:
        _fsync_existing_ancestor(path, root)


class ReservationLedger:
    """One deducted owner/family ledger.

    Children never receive this object or the full policy; the parent deducts
    their allocations here before spawn and passes only a LocalPhysicalGrant1
    descriptor (see issue_child_grant/load_child_grant).
    """
    def __init__(self, policy, directory, *, source_map=None):
        self.source_map = load_source_map() if source_map is None else source_map
        self.policy = validate_physical_policy(policy, self.source_map)
        self._init_state(directory, ({(f, k): v[k + '_bytes'] for f, v in policy['families'].items() for k in FAMILY_COMPONENTS}
                                     | {(p, 'bytes'): n for p, n in policy['pools'].items()}))

    def _init_state(self, directory, limits):
        self.directory = Path(directory).resolve()
        require(self.directory.is_dir() and not Path(directory).is_symlink(), 'owned physical ledger root missing')
        self.limits = dict(limits)
        self.used = {key: 0 for key in self.limits}
        self.reservations = []
        self.by_id = {}
        self.claims = {}
        self.protected = {}
        self.grants = {}
        self.transfers = {}

    def reserve(self, owner, family_or_pool, component, bytes, *, reservation_id=None):
        integer(bytes)
        require(owner == FAMILY_OWNERS.get(family_or_pool, POOL_OWNERS.get(family_or_pool)), 'physical reservation owner mismatch')
        key = family_or_pool, component
        require(key in self.limits, 'unknown physical reservation component')
        require(reservation_id is None or (type(reservation_id) is str and HEX64.fullmatch(reservation_id)), 'invalid physical reservation id')
        require(reservation_id is None or reservation_id not in self.by_id, 'duplicate physical reservation id')
        self._authority()
        if self.used[key] + bytes > self.limits[key]:
            raise PhysicalIncomplete('physical reservation exhausted: ' + '/'.join(key))
        self.used[key] += bytes
        grant = Reservation(self, owner, key, bytes, reservation_id)
        while grant.reservation_id in self.by_id:  # 2**-256: never alias an id
            grant.reservation_id = secrets.token_hex(32)
        self.by_id[grant.reservation_id] = grant
        self.reservations.append(grant)
        return grant

    def remaining(self, family_or_pool, component):
        """Capacity not yet deducted for one resource component (delegation sizing)."""
        key = family_or_pool, component
        require(key in self.limits, 'unknown physical reservation component')
        return self.limits[key] - self.used[key]

    def _authority(self):
        """Root ledgers hold the process-local policy; nothing to re-verify."""

    def _conflicts(self, relative, recursive, owner=None):
        for text, (reservation, other_tree) in self.claims.items():
            if reservation is not owner and _related(relative, recursive, PurePosixPath(text), other_tree):
                return reservation
        for text in self.protected:
            if _related(relative, recursive, PurePosixPath(text), False):
                return self.protected[text]
        return None

    def _claim(self, reservation, path, recursive):
        path = _owned(path, self.directory)
        require(type(recursive) is bool, 'invalid physical claim recursion')
        relative = path.relative_to(self.directory)
        require(self._conflicts(relative, recursive, reservation) is None, 'physical file has multiple owners')
        self._attribute(reservation, relative, recursive)
        self.claims[str(relative)] = reservation, recursive
        return path

    def _attribute(self, reservation, relative, recursive):
        """Root: no sub-grant scope. ChildLedger binds the claim to one allocation."""

    def _unclaim(self, reservation):
        for text in [t for t, (r, _) in self.claims.items() if r is reservation]:
            del self.claims[text]

    def issue_child_grant(self, allocations, attachments, binding, held_lease):
        """Parent deducts every delegated allocation here BEFORE spawn; returns the sealed GrantHandle."""
        return _issue_child_grant(self, allocations, attachments, binding, held_lease)

    def held_reservation(self, entry):
        """The live, undelegated reservation that already holds exactly this allocation's claims.

        A writer child inherits such a file by passing the returned reservation_id in its
        allocation (see issue_child_grant); it is never released and reissued.
        """
        wanted = sorted((str(relative), recursive) for relative, recursive in _scope_claims(entry))
        for reservation in self.reservations:
            if (wanted and not reservation.released and not reservation.delegated and reservation.transfer is None
                    and (reservation.owner, reservation.key) == (entry['owner'], (entry['resource'], entry['component']))
                    and sorted(reservation.scope()) == wanted
                    and all(handle._control is not reservation for handle in self.grants.values())):
                return reservation
        return None

    def prepare_transfer(self, reservation_id, destination_owner, resource, component, new_path):
        return _prepare_transfer(self, reservation_id, destination_owner, resource, component, new_path)

    def commit_transfer_after_rename_fsync(self, handle, observed_device_inode):
        return _commit_transfer(self, handle, observed_device_inode)

    def abort_transfer(self, handle):
        return _abort_transfer(self, handle)

    def check(self):
        self._authority()
        for grant in self.reservations:
            grant.check()
        for handle in self.grants.values():
            handle.check()


class DatabaseGrant:
    def __init__(self, ledger, family, path, ceiling, readonly):
        require(family in FAMILY_OWNERS, 'unknown physical database family')
        self.ledger, self.family = ledger, family
        self.path = _owned(path, ledger.directory)
        self.ceiling = integer(ceiling, positive=True)
        self.readonly = readonly
        require(type(readonly) is bool, 'invalid read-only database flag')
        maximum = (ledger.policy['families'][family]['database_bytes'] if ledger.policy is not None
                   else ledger.limits.get((family, 'main'), 0))
        require(self.ceiling <= maximum, 'database ceiling exceeds immutable policy')
        self.connection = None
        self.grants = []
        owner = FAMILY_OWNERS[family]
        try:
            # Register each deduction before it can fail to claim its files so a
            # refusal can never leak charged capacity.
            self.main = ledger.reserve(owner, family, 'main', self.ceiling)
            self.grants.append(self.main)
            self.main.track(self.path)
            # Full worst-ceiling rollback/WAL overlap plus framing, independent
            # of row geometry. FILE work is a separate prospective reservation.
            self.journal = ledger.reserve(owner, family, 'journal', 2 * self.ceiling + MIB)
            self.grants.append(self.journal)
            for suffix in ('-wal', '-shm', '-journal'):
                self.journal.track(Path(str(self.path) + suffix))
            self.temp = ledger.reserve(owner, family, 'temp', 2 * self.ceiling)
            self.grants.append(self.temp)
        except BaseException:
            for grant in self.grants:
                # Existing files stay charged even on admission failure.
                if not grant.surviving_files():
                    grant.release_after_removal()
            raise

    def connect(self, *, factory=sqlite3.Connection):
        require(self.connection is None, 'physical database grant opened twice')
        if self.readonly:
            require(self.path.is_file(), 'immutable restored database missing')
            db = sqlite3.connect(self.path.as_uri() + '?mode=ro&immutable=1', uri=True, factory=factory)
        else:
            db = sqlite3.connect(self.path, factory=factory)
        self.connection = db
        try:
            size = db.execute('PRAGMA page_size').fetchone()[0]
            pages = db.execute('PRAGMA page_count').fetchone()[0]
            self.effective_pages = self.ceiling // size
            if self.effective_pages == 0 or pages > self.effective_pages:
                raise PhysicalIncomplete('restored/empty main database exceeds effective page ceiling')
            if not self.readonly:
                actual = db.execute('PRAGMA max_page_count=' + str(self.effective_pages)).fetchone()[0]
                require(actual == self.effective_pages, 'ineffective SQLite max_page_count')
                db.execute('PRAGMA temp_store=FILE')
            self.check()
            return db
        except BaseException:
            db.close()
            self.connection = None
            raise

    def prospective(self):
        self.check()
        # Entire ceiling journal/temp reservations were deducted before open,
        # and remain held through transaction rollback, export and close.
        return self

    def check(self):
        self.main.check()
        self.journal.check()
        self.temp.check()
        if self.connection is not None:
            size = self.connection.execute('PRAGMA page_size').fetchone()[0]
            pages = self.connection.execute('PRAGMA page_count').fetchone()[0]
            if pages > self.effective_pages or pages * size > self.ceiling:
                raise PhysicalIncomplete('main database exceeded effective physical page cap')
            if not self.readonly:
                require(self.connection.execute('PRAGMA max_page_count').fetchone()[0] == self.effective_pages,
                        'SQLite physical page cap changed')

    def retain(self):
        self.check()
        self.connection = None

    def release_after_removal(self):
        require(self.connection is None, 'cannot release open database grant')
        for grant in self.grants:
            grant.release_after_removal()


def reserve_database(ledger, family, path, requested_ceiling, readonly=False):
    return DatabaseGrant(ledger, family, path, requested_ceiling, readonly)


# ---------------------------------------------------------------------------
# LocalPhysicalGrant1: the parent/child seam (RESULT-GRANT ABI). The descriptor
# is local, transient, never an uploaded receipt, a ResourcePlan field or a
# WCTS request field. It confers capacity only; it never grants proof credit.
# ---------------------------------------------------------------------------
ENV_GRANT_FD = 'PLURX_ASYNC_PHYSICAL_GRANT_FD'
ENV_GRANT_SHA256 = 'PLURX_ASYNC_PHYSICAL_GRANT_SHA256'
ENV_LEASE_FD = 'PLURX_ASYNC_PHYSICAL_LEASE_FD'
GRANT_ENVIRONMENT = (ENV_GRANT_FD, ENV_GRANT_SHA256, ENV_LEASE_FD)
MAXIMUM_GRANT_BYTES = 4 * MIB
GRANT_FIELDS = ('schema_version', 'kind', 'grant_id', 'source', 'session_id', 'resource_plan_sha256', 'physical_policy_sha256',
                'owned_root', 'lease', 'parent', 'allocations', 'attachments')
ALLOCATION_FIELDS = ('reservation_id', 'owner', 'resource', 'component', 'bytes', 'relative_path', 'recursive')
BINDING_FIELDS = ('source', 'session_id', 'resource_plan_sha256', 'physical_policy_sha256')


def _strict_json(raw):
    def pairs(items):
        keys = [k for k, _ in items]
        require(len(keys) == len(set(keys)), 'duplicate key in physical grant descriptor')
        return dict(items)

    def refuse(_):
        raise PhysicalPolicyError('non-integer number in physical grant descriptor')
    try:
        return json.loads(raw.decode('utf-8'), object_pairs_hook=pairs, parse_float=refuse, parse_constant=refuse)
    except (UnicodeDecodeError, ValueError) as error:
        raise PhysicalPolicyError('physical grant descriptor is not strict JSON') from error


def _component_rules(resource, component):
    """(is_family, recursive_required) for one valid resource/component pair."""
    if resource in FAMILY_OWNERS:
        require(component in FAMILY_COMPONENTS, 'unknown physical family component')
        # main/journal name one database file; temp is accounting-only for the
        # anonymous FILE work of that database (non-recursive) or an owned scratch subtree.
        return True, (None if component == 'temp' else False)
    require(resource in POOL_OWNERS and component == 'bytes', 'unknown physical pool/component')
    return False, None


def _scope_claims(entry):
    """Resolved claims of one allocation/attachment: [(PurePosixPath, recursive)].

    The ownership rules resolve permitted DB companions inside their family:
    a journal grant names the main file and covers exactly the fixed
    -wal/-shm/-journal companion names; temp is the FILE-work subtree.
    """
    base = safe_relative(entry['relative_path'])
    is_family, _ = _component_rules(entry['resource'], entry['component'])
    if is_family and entry['component'] == 'journal':
        return [(base.with_name(base.name + suffix), False) for suffix in DB_COMPANIONS]
    if is_family and entry['component'] == 'temp' and not entry['recursive']:
        return []  # anonymous SQLite FILE work owns no addressable file, only prospective bytes
    return [(base, entry['recursive'])]


def validate_allocation(entry, *, attachment=False, policy=None):
    """Closed Allocation/Attachment entry; returns it unchanged."""
    closed(entry, ALLOCATION_FIELDS + (('access',) if attachment else ()), 'physical attachment' if attachment else 'physical allocation')
    require(type(entry['reservation_id']) is str and HEX64.fullmatch(entry['reservation_id']), 'invalid physical reservation id')
    require(entry['owner'] == expected_owner(entry['resource']), 'unknown/mismatched physical owner')
    is_family, recursive_required = _component_rules(entry['resource'], entry['component'])
    integer(entry['bytes'], positive=True)
    require(type(entry['recursive']) is bool, 'invalid physical recursion flag')
    require(not is_family or recursive_required is None or entry['recursive'] is recursive_required,
            'physical family component scope differs from ownership map')
    safe_relative(entry['relative_path'])
    if attachment:
        require(entry['access'] == 'read_only', 'attachments are read-only')
    if policy is not None:
        limit = (policy['families'][entry['resource']][entry['component'] + '_bytes'] if is_family
                 else policy['pools'][entry['resource']])
        require(entry['bytes'] <= limit, 'physical grant limit exceeds immutable policy')
    return entry


def _claims_overlap(left, right):
    return any(_related(a, at, b, bt) for a, at in left for b, bt in right)


def validate_grant_lists(allocations, attachments, *, policy=None):
    """Duplicate IDs, aliases and overlaps inside one grant are refused."""
    require(type(allocations) is list and type(attachments) is list and (allocations or attachments), 'empty physical grant')
    identifiers = set()
    for entry in allocations:
        validate_allocation(entry, policy=policy)
    for entry in attachments:
        validate_allocation(entry, attachment=True, policy=policy)
    for entry in allocations + attachments:
        require(entry['reservation_id'] not in identifiers, 'duplicate physical grant reservation id')
        identifiers.add(entry['reservation_id'])
    spans = [_scope_claims(entry) for entry in allocations]
    for i, left in enumerate(spans):
        for right in spans[i + 1:]:
            require(not _claims_overlap(left, right), 'overlapping physical grant allocations')
    for entry in attachments:
        # A writable allocation and a read-only attachment may not alias one object.
        require(not any(_claims_overlap(_scope_claims(entry), span) for span in spans), 'physical attachment aliases a writable allocation')


def _identity(stat_result):
    return {'device': stat_result.st_dev, 'inode': stat_result.st_ino}


def _regular_private(stat_result, description):
    require(stat.S_ISREG(stat_result.st_mode) and stat_result.st_nlink == 1
            and stat_result.st_uid == os.geteuid() and stat_result.st_mode & 0o077 == 0, description + ' is not a private regular file')


def _read_all(fd, limit):
    chunks, offset = [], 0
    while True:
        chunk = os.pread(fd, 1 << 20, offset)
        if not chunk:
            return b''.join(chunks)
        offset += len(chunk)
        require(offset <= limit, 'physical grant descriptor exceeds its bound')
        chunks.append(chunk)


def _private_directory(path):
    path.mkdir(mode=0o700, exist_ok=True)
    info = os.lstat(path)
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0,
            'physical control directory is not private')
    return path


def _write_private(path, raw):
    """Create-exclusive nofollow private file, fsynced, then directory fsynced."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        view = memoryview(raw)
        while view:
            view = view[os.write(fd, view):]
        os.fsync(fd)
    finally:
        os.close(fd)
    _fsync_directory(path.parent)


def _binding(value, *, policy=None):
    """Closed source/session/plan/policy binding; optional owned_root/parent are cross-checked by callers."""
    require(type(value) is dict, 'physical grant binding must be an object')
    closed({k: v for k, v in value.items() if k in BINDING_FIELDS}, BINDING_FIELDS, 'physical grant binding')
    require(set(value) <= set(BINDING_FIELDS) | {'owned_root', 'parent', 'physical_policy'}, 'unknown physical grant binding field')
    require(type(value['source']) is dict and value['source'], 'invalid physical grant source')
    require(type(value['session_id']) is str and UUID_TEXT.fullmatch(value['session_id']), 'invalid physical grant session')
    sha(value['resource_plan_sha256'])
    sha(value['physical_policy_sha256'])
    if policy is not None:
        require(value['physical_policy_sha256'] == digest(policy), 'physical grant policy digest differs')
    return value


def _lease_identity(root, relative, fd):
    """The held descriptor and the owned-root name denote the same non-symlink file."""
    held = os.fstat(fd)
    require(stat.S_ISREG(held.st_mode), 'held lease is not a regular file')
    named = os.lstat(root / relative)
    require(stat.S_ISREG(named.st_mode) and (named.st_dev, named.st_ino) == (held.st_dev, held.st_ino),
            'held lease descriptor does not name the owned-root lease path')
    return _identity(held)


class GrantHandle:
    """Parent-side handle for one issued child grant (never given to the child)."""
    def __init__(self, ledger, grant_id, raw, path, descriptor_fd, lease, reservations, control):
        self._ledger, self.grant_id, self._raw, self._path = ledger, grant_id, raw, path
        self._fd, self._lease, self.reservations, self._control = descriptor_fd, lease, reservations, control
        self.closed = False

    @property
    def descriptor_fd(self):
        require(not self.closed and self._fd >= 0, 'physical grant already reconciled/closed')
        return self._fd

    @property
    def fd(self):
        return self.descriptor_fd

    def close(self):
        """Release only the read-only descriptor handle; charges stay until reconcile_after_reap."""
        if self._fd >= 0:
            os.close(self._fd)
            self._fd = -1

    @property
    def sha256(self):
        return hashlib.sha256(self._raw).hexdigest()

    @property
    def descriptor(self):
        return json.loads(self._raw)

    def environment(self):
        """Exactly the three source-owned environment names (decimal FDs, canonical hash)."""
        require(not self.closed, 'physical grant already reconciled')
        return {ENV_GRANT_FD: str(self.descriptor_fd), ENV_GRANT_SHA256: self.sha256, ENV_LEASE_FD: str(self._lease.fd)}

    def pass_fds(self):
        return self.descriptor_fd, self._lease.fd

    def check(self):
        if not self.closed:
            for reservation in self.reservations:
                reservation.check()

    def reconcile_after_reap(self, child=None):
        """After the child is reaped: keep every surviving file charged, release the rest.

        Charges for surviving files are retained regardless of the child's exit
        status or reported output; only fully removed (and directory-fsynced)
        allocations are released. Exhaustion is raised after reconciliation.
        """
        require(not self.closed, 'physical grant already reconciled')
        # `child` may be a reaped exit status, or a Popen-like object that must already have exited.
        require(child is None or type(child) is int or (hasattr(child, 'poll') and child.poll() is not None),
                'child must be reaped before physical grant reconciliation')
        retained, released, overage = [], [], []
        for reservation in self.reservations:
            reservation.delegated = False
            if reservation.surviving_files():
                retained.append(reservation.reservation_id)
                if reservation.actual_bytes() > reservation.bytes:
                    overage.append(reservation.reservation_id)
            else:
                reservation.release_after_removal()
                released.append(reservation.reservation_id)
        self.close()
        self._path.unlink()
        _fsync_directory(self._path.parent)
        self._control.release_after_removal()
        self.closed = True
        del self._ledger.grants[self.grant_id]
        if overage:
            raise PhysicalIncomplete('child exceeded its physical grant: ' + ','.join(overage))
        return {'released': released, 'retained': retained}


def _require_delegable(ledger, held, item):
    """An allocation may delegate a held reservation only if it is exactly that live, unaliased claim."""
    require(not held.released and not held.delegated and held.transfer is None,
            'physical grant reservation id already in use')
    require(all(handle._control is not held for handle in ledger.grants.values()),
            'a grant descriptor reservation cannot be delegated')
    require((held.owner, held.key[0], held.key[1]) == (item['owner'], item['resource'], item['component']),
            'delegated reservation differs in owner/resource/component')
    require(item['bytes'] >= held.bytes, 'delegation cannot shrink a held reservation below its charge')
    require(sorted(held.scope()) == sorted((str(relative), recursive) for relative, recursive in _scope_claims(item)),
            'delegated reservation scope differs from its held claims')


def _issue_child_grant(self, allocations, attachments, binding, held_lease):
    """Deduct every delegated allocation in this one ledger BEFORE spawn, then seal the descriptor."""
    self._authority()
    policy = self.policy
    require(policy is not None, 'this ledger holds no policy to delegate from')
    _binding(binding, policy=policy)
    identity = os.lstat(self.directory)
    if 'owned_root' in binding:
        claimed = binding['owned_root']
        require(claimed == str(self.directory) or claimed == {'path': str(self.directory), **_identity(identity)},
                'physical grant binding owned root differs from the ledger root')
    if 'parent' in binding:
        require(binding['parent'] == {'pid': os.getpid(), 'start_ticks': process_start_ticks(os.getpid())},
                'physical grant binding parent differs from this process')
    held_lease = HeldLease(getattr(held_lease, 'fd', None), str(getattr(held_lease, 'relative_path', '')))
    lease = _lease_identity(self.directory, held_lease.relative_path, held_lease.fd)
    root = os.lstat(self.directory)
    require(stat.S_ISDIR(root.st_mode), 'owned root is not a directory')
    require(type(allocations) is list and all(type(x) is dict for x in allocations), 'physical allocations must be a list of objects')
    require(type(attachments) is list and all(type(x) is dict for x in attachments), 'physical attachments must be a list of objects')
    prepared, ids = [], set()
    for entry in allocations:
        item = dict(entry)
        if 'reservation_id' not in item:
            item['reservation_id'] = secrets.token_hex(32)
        prepared.append(item)
    attached = [dict(x) for x in attachments]
    validate_grant_lists(prepared, attached, policy=policy)
    # An allocation carrying the id of a reservation this ledger already holds DELEGATES it:
    # ownership moves to the grant in the one ledger, never through release and re-reserve.
    existing = {}
    for item in prepared:
        held = self.by_id.get(item['reservation_id'])
        if held is not None:
            _require_delegable(self, held, item)
            existing[item['reservation_id']] = held
        ids.add(item['reservation_id'])
    # Attachments reference an existing held parent reservation and confer no authority.
    for item in attached:
        source = self.by_id.get(item['reservation_id'])
        require(source is not None and not source.released and not source.delegated and source.transfer is None,
                'attachment names no live parent reservation')
        require((source.owner, source.key[0], source.key[1], source.bytes) == (item['owner'], item['resource'], item['component'], item['bytes'])
                and (item['relative_path'], item['recursive']) in source.scope(), 'attachment differs from its held reservation')
    # Conflicts against every claim this ledger already holds (other grants, DB files, lease).
    lease_relative = held_lease.relative_path
    self.protected[str(lease_relative)] = 'owned-slot lease'
    for item in prepared:
        owner = existing.get(item['reservation_id'])
        for relative, recursive in _scope_claims(item):
            require(self._conflicts(relative, recursive, owner) is None, 'physical grant aliases an owned file or another grant')
    deducted, grown = [], {}
    control = path = None
    deducted = []
    control = path = None
    try:
        for item in prepared:
            held = existing.get(item['reservation_id'])
            if held is None:
                deducted.append(self.reserve(item['owner'], item['resource'], item['component'], item['bytes'],
                                             reservation_id=item['reservation_id']))
                continue
            increase = item['bytes'] - held.bytes
            if self.used[held.key] + increase > self.limits[held.key]:
                raise PhysicalIncomplete('physical delegation growth exhausted: ' + '/'.join(held.key))
            self.used[held.key] += increase
            grown[held.reservation_id] = held.bytes
            held.bytes = item['bytes']
            deducted.append(held)
        for item, reservation in zip(prepared, deducted):
            if item['reservation_id'] not in existing:
                for relative, recursive in _scope_claims(item):
                    reservation._claim(self.directory / relative, recursive)
            reservation.delegated = True
        grant_id = secrets.token_hex(32)
        while grant_id in self.by_id or grant_id in self.grants or grant_id in ids:
            grant_id = secrets.token_hex(32)
        descriptor = {'schema_version': 1, 'kind': 'local_physical_grant', 'grant_id': grant_id, 'source': binding['source'],
                      'session_id': binding['session_id'], 'resource_plan_sha256': binding['resource_plan_sha256'],
                      'physical_policy_sha256': binding['physical_policy_sha256'],
                      'owned_root': {'path': str(self.directory), **_identity(root)},
                      'lease': {'relative_path': str(lease_relative), **lease},
                      'parent': {'pid': os.getpid(), 'start_ticks': process_start_ticks(os.getpid())},
                      'allocations': prepared, 'attachments': attached}
        raw = canonical(descriptor)
        require(len(raw) <= MAXIMUM_GRANT_BYTES, 'physical grant descriptor exceeds its bound')
        control = self.reserve('session_controller', 'control_bytes', 'bytes', len(raw))
        directory = _private_directory(self.directory / GRANT_DIRECTORY)
        path = directory / (grant_id + '.json')
        control.track(path)
        _write_private(path, raw)
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            _regular_private(os.fstat(fd), 'physical grant descriptor')
            require(_read_all(fd, MAXIMUM_GRANT_BYTES) == raw, 'physical grant descriptor read-back differs')
        except BaseException:
            os.close(fd)
            raise
    except BaseException:
        for reservation in reversed(deducted):
            reservation.delegated = False
            if reservation.reservation_id in existing:
                # A delegated held reservation goes back to the parent untouched: never released.
                if reservation.reservation_id in grown:
                    self.used[reservation.key] -= reservation.bytes - grown[reservation.reservation_id]
                    reservation.bytes = grown[reservation.reservation_id]
            elif not reservation.surviving_files():
                reservation.release_after_removal()
        if control is not None:
            if path is not None and os.path.lexists(path):
                path.unlink()
            if not control.surviving_files():
                control.release_after_removal()
        raise
    handle = GrantHandle(self, grant_id, raw, path, fd, held_lease, deducted, control)
    self.grants[grant_id] = handle
    return handle




class _GrantedAllocation:
    def __init__(self, entry):
        self.entry = entry
        self.key = entry['resource'], entry['component']
        self.bytes = entry['bytes']
        self.spent = 0
        self.claims = _scope_claims(entry)

    def covers(self, relative, recursive):
        for base, tree in self.claims:
            if relative == base and (tree or not recursive) or tree and base in relative.parents:
                return True
        return False


class ChildLedger(ReservationLedger):
    """Child view built ONLY from a verified grant; it never instantiates policy capacity.

    The child may suballocate inside its deducted allocations and cannot grow
    beyond them. Authority (live parent, owned root, lease) is re-checked on use.
    """
    def __init__(self, descriptor, raw_sha256, lease_fd, policy=None):
        directory = Path(descriptor['owned_root']['path'])
        limits = {}
        self.allocations = {}
        for entry in descriptor['allocations']:
            allocation = _GrantedAllocation(entry)
            self.allocations[entry['reservation_id']] = allocation
            limits[allocation.key] = limits.get(allocation.key, 0) + entry['bytes']
        self.attachments = {entry['reservation_id']: entry for entry in descriptor['attachments']}
        self.grant_id, self.grant_sha256, self.descriptor = descriptor['grant_id'], raw_sha256, descriptor
        self.lease_fd = lease_fd
        self.held_lease = HeldLease(lease_fd, descriptor['lease']['relative_path'])
        self.parent_identity = descriptor['parent']['pid'], descriptor['parent']['start_ticks']
        self.policy = policy
        self.source_map = None
        self._init_state(directory, limits)

    def _authority(self):
        pid, ticks = self.parent_identity
        require(process_start_ticks(pid) == ticks, 'physical grant outlived its parent')
        root = os.lstat(self.directory)
        require(stat.S_ISDIR(root.st_mode) and (root.st_dev, root.st_ino) == (self.descriptor['owned_root']['device'], self.descriptor['owned_root']['inode']),
                'owned root identity changed')
        require(_lease_identity(self.directory, self.held_lease.relative_path, self.lease_fd)
                == {'device': self.descriptor['lease']['device'], 'inode': self.descriptor['lease']['inode']}, 'lease identity changed')

    def _attribute(self, reservation, relative, recursive):
        if reservation.allocation is None:
            candidates = [a for a in self.allocations.values() if a.key == reservation.key and a.covers(relative, recursive)]
            require(candidates, 'physical path is outside the granted allocation scope')
            chosen = next((a for a in candidates if a.spent + reservation.bytes <= a.bytes), None)
            if chosen is None:
                raise PhysicalIncomplete('child grant allocation exhausted')
            chosen.spent += reservation.bytes
            reservation.allocation = chosen
        else:
            require(reservation.allocation.covers(relative, recursive), 'physical path is outside the granted allocation scope')

    def attachment(self, attachment_id):
        require(type(attachment_id) is str and attachment_id in self.attachments, 'unknown physical attachment')
        return self.attachments[attachment_id]


def load_child_grant(grant_fd, expected_sha256, lease_fd, expected_binding):
    """Verify every ABI check; the result is the only capacity the child owns.

    expected_binding is closed {source, session_id, resource_plan_sha256,
    physical_policy_sha256} and may additionally carry `owned_root` (absolute
    path) and `physical_policy` (the plan's policy, cross-checked against the
    digest so allocations beyond policy are refused).
    """
    require(type(expected_binding) is dict, 'physical grant binding is required')
    extras = {k: expected_binding[k] for k in ('owned_root', 'parent', 'physical_policy') if k in expected_binding}
    base = {k: v for k, v in expected_binding.items() if k not in extras}
    policy = extras.get('physical_policy')
    _binding(base, policy=policy)
    require(type(grant_fd) is int and type(lease_fd) is int and grant_fd >= 0 and lease_fd >= 0 and grant_fd != lease_fd,
            'invalid inherited physical descriptors')
    sha(expected_sha256)
    info = os.fstat(grant_fd)
    _regular_private(info, 'inherited physical grant descriptor')
    require(fcntl.fcntl(grant_fd, fcntl.F_GETFL) & os.O_ACCMODE == os.O_RDONLY, 'physical grant descriptor is writable')
    raw = _read_all(grant_fd, MAXIMUM_GRANT_BYTES)
    require(hashlib.sha256(raw).hexdigest() == expected_sha256, 'physical grant descriptor hash differs')
    descriptor = _strict_json(raw)
    require(type(descriptor) is dict and canonical(descriptor) == raw, 'physical grant descriptor is not canonical')
    closed(descriptor, GRANT_FIELDS, 'physical grant descriptor')
    require(descriptor['schema_version'] == 1 and descriptor['kind'] == 'local_physical_grant', 'physical grant version/kind')
    require(type(descriptor['grant_id']) is str and HEX64.fullmatch(descriptor['grant_id']), 'invalid physical grant id')
    for key in BINDING_FIELDS:
        require(descriptor[key] == base[key], 'physical grant binding differs: ' + key)
    validate_grant_lists(descriptor['allocations'], descriptor['attachments'], policy=policy)
    root, lease, parent = descriptor['owned_root'], descriptor['lease'], descriptor['parent']
    closed(root, ('path', 'device', 'inode'), 'physical owned root')
    closed(lease, ('relative_path', 'device', 'inode'), 'physical lease')
    closed(parent, ('pid', 'start_ticks'), 'physical parent identity')
    for value in (root['device'], root['inode'], lease['device'], lease['inode'], parent['start_ticks']):
        integer(value)
    integer(parent['pid'], positive=True)
    safe_relative(lease['relative_path'])
    require(type(root['path']) is str and Path(root['path']).is_absolute(), 'physical grant owned root differs')
    require('owned_root' not in extras or extras['owned_root'] in (root['path'], root), 'physical grant owned root differs from expectation')
    require('parent' not in extras or extras['parent'] == parent, 'physical grant parent differs from expectation')
    require(process_start_ticks(parent['pid']) == parent['start_ticks'], 'physical grant parent is not the live identity')
    named = os.lstat(root['path'])
    require(stat.S_ISDIR(named.st_mode) and (named.st_dev, named.st_ino) == (root['device'], root['inode']), 'owned root device/inode differs')
    require(_lease_identity(Path(root['path']), lease['relative_path'], lease_fd) == {'device': lease['device'], 'inode': lease['inode']},
            'inherited lease device/inode/path differs')
    for entry in descriptor['allocations'] + descriptor['attachments']:
        for relative, _ in _scope_claims(entry):
            require(relative != PurePosixPath(lease['relative_path']), 'physical grant aliases the lease')
    return ChildLedger(descriptor, expected_sha256, lease_fd, policy)


class ReadonlyDatabaseGrant:
    """An already-held restored database, checked without touching bytes or pragmas."""
    def __init__(self, ledger, family, path, attachment, page_size, page_count, ceiling):
        self.ledger, self.family, self.path, self.attachment = ledger, family, path, attachment
        self.page_size, self.page_count, self.ceiling_bytes = page_size, page_count, ceiling
        self.effective_pages = ceiling // page_size

    def reserve_ancillary(self, component, bytes):
        """Journal/temp work needs a SEPARATE allocation; a second main copy never exists."""
        require(component in ('journal', 'temp'), 'a read-only attachment never receives another main allocation')
        return self.ledger.reserve(FAMILY_OWNERS[self.family], self.family, component, bytes)

    def connect(self):
        self.ledger._authority()
        return sqlite3.connect(self.path.as_uri() + '?mode=ro&immutable=1', uri=True)


def validate_readonly_database(child_ledger, family, path, attachment_id):
    """Attach the already-held reservation; size/page ceilings only, no pragmas or writes."""
    require(isinstance(child_ledger, ChildLedger), 'a child grant ledger is required')
    child_ledger._authority()
    require(family in FAMILY_OWNERS and child_ledger.policy is not None, 'database family/policy unavailable to the child')
    entry = child_ledger.attachment(attachment_id)
    path = _owned(path, child_ledger.directory)
    relative = path.relative_to(child_ledger.directory)
    base = safe_relative(entry['relative_path'])
    require(relative == base or (entry['recursive'] and base in relative.parents), 'database is outside its attached reservation')
    # No second main allocation may alias an attached file.
    for allocation in child_ledger.allocations.values():
        require(not (allocation.key[1] == 'main' and any(_related(relative, False, b, t) for b, t in allocation.claims)),
                'attached database also has a writable main allocation')
    info = os.lstat(path)
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, 'attached database is not one regular file')
    require(info.st_size <= entry['bytes'], 'attached database exceeds its held reservation')
    for suffix in DB_COMPANIONS:
        require(not os.path.lexists(str(path) + suffix), 'attached database has unaccounted companion files')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        header = os.pread(fd, 100, 0)
    finally:
        os.close(fd)
    require(len(header) == 100 and header[:16] == b'SQLite format 3\x00', 'attached file is not a SQLite database')
    page_size = int.from_bytes(header[16:18], 'big')
    page_size = 65536 if page_size == 1 else page_size
    require(page_size in (512, 1024, 2048, 4096, 8192, 16384, 32768, 65536) and info.st_size > 0 and info.st_size % page_size == 0,
            'attached database page geometry is invalid')
    pages = info.st_size // page_size
    declared = int.from_bytes(header[28:32], 'big')
    require(declared == 0 or header[24:28] != header[92:96] or declared == pages, 'attached database page count differs from file size')
    ceiling = min(child_ledger.policy['families'][family]['database_bytes'], entry['bytes'])
    effective = ceiling // page_size
    if effective == 0 or pages > effective:
        raise PhysicalIncomplete('restored database exceeds its effective page ceiling')
    return ReadonlyDatabaseGrant(child_ledger, family, path, entry, page_size, pages, ceiling)


class TransferHandle:
    def __init__(self, ledger, transfer_id, source, destination, source_path, destination_path, identity, record):
        self.ledger, self.transfer_id, self.source, self.destination = ledger, transfer_id, source, destination
        self.source_path, self.destination_path, self.identity, self.record = source_path, destination_path, identity, record
        self.state = 'prepared'


def _prepare_transfer(self, reservation_id, destination_owner, resource, component, new_path):
    """Reserve the destination while the source stays charged; durably record both identities."""
    self._authority()
    source = self.by_id.get(reservation_id)
    require(source is not None and not source.released and not source.delegated and source.transfer is None,
            'transfer names no live undelegated reservation')
    require(destination_owner == expected_owner(resource), 'unknown/mismatched transfer destination owner')
    _component_rules(resource, component)
    require((resource, component) in self.limits, 'unknown transfer destination component')
    require(not source.subtrees and len(source.paths) == 1, 'a transfer moves exactly one physical file')
    (source_path,) = source.paths
    info = os.lstat(source_path)
    _regular_nonalias(info, 'transfer source')
    destination_path = _owned(new_path, self.directory)
    require(not os.path.lexists(destination_path), 'transfer destination already exists')
    require(destination_path.parent.is_dir() and os.stat(destination_path.parent).st_dev == info.st_dev,
            'transfer destination is not on the source filesystem')
    destination = self.reserve(destination_owner, resource, component, source.bytes)  # refusal leaves the prior root authoritative
    control = record_path = None
    try:
        destination.track(destination_path)
        transfer_id = secrets.token_hex(32)
        record = {'schema_version': 1, 'kind': 'physical_transfer', 'transfer_id': transfer_id, 'bytes': source.bytes,
                  'source': {'reservation_id': source.reservation_id, 'relative_path': str(source_path.relative_to(self.directory)), **_identity(info)},
                  'destination': {'reservation_id': destination.reservation_id, 'owner': destination_owner, 'resource': resource,
                                  'component': component, 'relative_path': str(destination_path.relative_to(self.directory))}}
        raw = canonical(record)
        control = self.reserve('session_controller', 'control_bytes', 'bytes', len(raw))
        record_path = _private_directory(self.directory / TRANSFER_DIRECTORY) / (transfer_id + '.json')
        control.track(record_path)
        _write_private(record_path, raw)
    except BaseException:
        if record_path is not None and os.path.lexists(record_path):
            record_path.unlink()
        if control is not None and not control.surviving_files():
            control.release_after_removal()
        destination.release_after_removal()
        raise
    source.transfer = destination.transfer = True
    handle = TransferHandle(self, transfer_id, source, destination, source_path, destination_path, (info.st_dev, info.st_ino),
                            (record_path, control))
    self.transfers[transfer_id] = handle
    return handle


def _regular_nonalias(info, description):
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, description + ' is not one regular unaliased file')


def _commit_transfer(self, handle, observed_device_inode):
    """One ownership after rename + directory fsync; never an unowned interval."""
    require(self.transfers.get(handle.transfer_id) is handle and handle.state == 'prepared', 'transfer is not prepared')
    require(type(observed_device_inode) is tuple and len(observed_device_inode) == 2
            and all(type(x) is int for x in observed_device_inode), 'invalid observed file identity')
    require(observed_device_inode == handle.identity, 'observed file identity differs from the prepared object')
    require(not os.path.lexists(handle.source_path), 'transfer source name still exists: aliases never create quota')
    info = os.lstat(handle.destination_path)
    _regular_nonalias(info, 'transfer destination')
    require((info.st_dev, info.st_ino) == handle.identity, 'transfer destination is not the prepared object')
    _fsync_directory(handle.destination_path.parent)
    _fsync_existing_ancestor(handle.source_path, self.directory)
    source, destination = handle.source, handle.destination
    source.transfer = destination.transfer = None
    source.paths.clear()  # the single object is now claimed by the destination alone
    source.release_after_removal()
    record_path, control = handle.record
    record_path.unlink()
    _fsync_directory(record_path.parent)
    control.release_after_removal()
    handle.state = 'committed'
    del self.transfers[handle.transfer_id]
    return destination


def _abort_transfer(self, handle):
    """Failure path: destination claim released, source remains charged and authoritative."""
    require(self.transfers.get(handle.transfer_id) is handle and handle.state == 'prepared', 'transfer is not prepared')
    require(not os.path.lexists(handle.destination_path), 'cannot abort after the destination name exists')
    handle.source.transfer = handle.destination.transfer = None
    handle.destination.release_after_removal()
    record_path, control = handle.record
    record_path.unlink()
    _fsync_directory(record_path.parent)
    control.release_after_removal()
    handle.state = 'aborted'
    del self.transfers[handle.transfer_id]


def locate_transfer(directory, record_path):
    """Crash reconciliation: where is the one object? Ambiguity keeps both charges (raises)."""
    directory = Path(directory).resolve()
    record = _strict_json(Path(record_path).read_bytes())
    closed(record, ('schema_version', 'kind', 'transfer_id', 'bytes', 'source', 'destination'), 'physical transfer record')
    source, destination = directory / record['source']['relative_path'], directory / record['destination']['relative_path']
    identity = record['source']['device'], record['source']['inode']
    found = []
    for name, path in (('source', source), ('destination', destination)):
        if os.path.lexists(path):
            info = os.lstat(path)
            _regular_nonalias(info, 'transfer ' + name)
            require((info.st_dev, info.st_ino) == identity, 'transfer ' + name + ' is not the recorded object')
            found.append(name)
    require(len(found) == 1, 'transfer location is ambiguous/lost; both reservations remain charged')
    return found[0]




def install_empty_role(connection, database_role, source_map=None):
    """Install the pinned exact empty schema, after a DatabaseGrant.connect.

    This is a calibration constructor, never an admission or fit receipt.
    """
    source_map = load_source_map() if source_map is None else source_map
    _, declaration = _role(source_map, database_role)
    require(not connection.execute("SELECT 1 FROM sqlite_schema WHERE type='table'").fetchone(), 'calibration constructor requires empty database')
    # Table source order preserves referenced table/index construction order;
    # all constraints/constructors remain in the reviewed map.
    for obj in declaration['objects']:
        connection.execute(obj['ddl'])
    connection.commit()
    return observe_geometry(connection, database_role, source_map)


def accounting_plan_inputs(plan):
    wrapper = json.loads(plan['source_bounds_utf8'])
    return {key: plan[key] for key in ('unit_byte_allowance', 'unit_record_allowance', 'maximum_units',
                                      'total_session_disk_bytes', 'total_retained_artifact_bytes',
                                      'resident_bytes', 'resident_rss_bytes', 'unit_predecessor_allowance')} | {
        'physical_policy_sha256': digest(wrapper['physical_policy']),
        'record_allowances_sha256': digest(wrapper['record_allowances']),
        'source_receipt_sha256': digest(wrapper['source_receipt'])}
