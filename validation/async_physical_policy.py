"""Closed physical reservations for the independent paged proof harness.

Logical record quotas never estimate SQLite allocation. These are prospective
operational grants; exhausting one is an incomplete disposition, never proof.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3

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


class ReservationLedger:
    """One deducted owner/family ledger; children spend parent-held grants."""
    def __init__(self, policy, directory, *, source_map=None, _parent=None, _limits=None):
        self.source_map = load_source_map() if source_map is None else source_map
        self.policy = validate_physical_policy(policy, self.source_map)
        self.directory = Path(directory).resolve()
        require(self.directory.is_dir() and not Path(directory).is_symlink(), 'owned physical ledger root missing')
        self.parent = _parent
        self.limits = ({(f, k): v[k + '_bytes'] for f, v in policy['families'].items() for k in ('main', 'journal', 'temp')}
                       | {(p, 'bytes'): n for p, n in policy['pools'].items()}) if _limits is None else dict(_limits)
        self.used = {key: 0 for key in self.limits}
        self.reservations = []
        self.paths = {}
        self.parent_grants = []

    def reserve(self, owner, family_or_pool, component, bytes):
        integer(bytes)
        expected = FAMILY_OWNERS.get(family_or_pool, POOL_OWNERS.get(family_or_pool))
        require(owner == expected, 'physical reservation owner mismatch')
        key = family_or_pool, component
        require(key in self.limits, 'unknown physical reservation component')
        if self.used[key] + bytes > self.limits[key]:
            raise PhysicalIncomplete('physical reservation exhausted: ' + '/'.join(key))
        self.used[key] += bytes
        grant = Reservation(self, owner, key, bytes)
        self.reservations.append(grant)
        return grant

    def child(self, owner, allocations):
        """Deduct all child budgets before startup; never reconstruct full caps."""
        require(type(allocations) is dict and allocations, 'empty child physical grants')
        grants = []
        try:
            for (family, component), amount in allocations.items():
                grants.append(self.reserve(owner, family, component, amount))
        except BaseException:
            for grant in grants:
                grant.release_after_removal()
            raise
        child = ReservationLedger(self.policy, self.directory, source_map=self.source_map, _parent=self,
                                  _limits={g.key: g.bytes for g in grants})
        child.parent_grants = grants
        return child

    def check(self):
        for grant in self.reservations:
            grant.check()
        if self.parent is not None:
            for grant in self.parent_grants:
                require(not grant.released, 'parent physical grant released while child active')


class Reservation:
    def __init__(self, ledger, owner, key, amount):
        self.ledger, self.owner, self.key, self.bytes = ledger, owner, key, amount
        self.paths = set()
        self.released = False

    def track(self, path):
        require(not self.released, 'released physical reservation')
        path = _owned(path, self.ledger.directory)
        existing = self.ledger.paths.get(str(path))
        require(existing is None or existing is self, 'physical file has multiple owners')
        self.ledger.paths[str(path)] = self
        self.paths.add(path)
        if self.ledger.parent is not None:
            parents = [g for g in self.ledger.parent_grants if g.key == self.key]
            require(len(parents) == 1, 'missing deducted parent reservation')
            parents[0].track(path)
        return self

    def grow(self, bytes):
        integer(bytes)
        require(not self.released and bytes >= self.bytes, 'invalid physical reservation growth')
        increase = bytes - self.bytes
        if self.ledger.used[self.key] + increase > self.ledger.limits[self.key]:
            raise PhysicalIncomplete('prospective physical grant growth exhausted')
        self.ledger.used[self.key] += increase
        self.bytes = bytes

    def check(self):
        if self.released:
            return
        actual = 0
        for path in self.paths:
            if path.exists():
                require(not path.is_symlink() and path.is_file(), 'nonregular owned physical file')
                actual += path.stat().st_size
        if actual > self.bytes:
            raise PhysicalIncomplete('owned files exceed prospective physical grant')

    def release_after_removal(self):
        require(not self.released, 'physical reservation released twice')
        for path in self.paths:
            require(not path.exists() and not path.is_symlink(), 'surviving physical file retains its reservation')
        for parent in {p.parent for p in self.paths}:
            _fsync_directory(parent)
        self.ledger.used[self.key] -= self.bytes
        self.released = True


class DatabaseGrant:
    def __init__(self, ledger, family, path, ceiling, readonly):
        require(family in FAMILY_OWNERS, 'unknown physical database family')
        self.ledger, self.family = ledger, family
        self.path = _owned(path, ledger.directory)
        self.ceiling = integer(ceiling, positive=True)
        self.readonly = readonly
        require(type(readonly) is bool and self.ceiling <= ledger.policy['families'][family]['database_bytes'], 'database ceiling exceeds immutable policy')
        self.connection = None
        self.grants = []
        owner = FAMILY_OWNERS[family]
        try:
            self.main = ledger.reserve(owner, family, 'main', self.ceiling).track(self.path)
            self.grants.append(self.main)
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
                if not any(p.exists() or p.is_symlink() for p in grant.paths):
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
