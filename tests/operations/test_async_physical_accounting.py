"""Defined-only mutation fixtures for the physical accounting side of the checkpoint-capacity amendment.

Written for independent review; they have not been executed. Each case pins one
decision of the sealed capacity / result-grant ABIs (typed physical caps, one
ledger owner, deducted child grants, read-only restored attachment, single
ownership transfer) so that the previous whole-image/aggregate behaviour or a
weakened check makes it fail. Logical record quotas never estimate pages.
"""
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import signal
import stat
import struct
import sys
import tempfile
from types import SimpleNamespace
from unittest import mock
import unittest
import uuid

from validation import async_physical_policy as physical

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'verification' / 'async-driver-refinement'))
import paged_codec as codec  # noqa: E402

MIB = 1024 * 1024
PAGE = 4096
EMPTY_FIT = 256  # pages: the largest pinned empty schema (independent_verifier) is 169 pages, so the default ceiling must admit it


def owner_of(resource):
    return physical.FAMILY_OWNERS.get(resource, physical.POOL_OWNERS.get(resource))


def build_policy(source_map, *, database=EMPTY_FIT * PAGE, **pools):
    families = {name: {'owner': owner, **physical.minimum_family_policy(source_map, name, database)}
                for name, owner in physical.FAMILY_OWNERS.items()}
    pool_bytes = {name: 8 * MIB for name in physical.POOL_OWNERS}
    pool_bytes['source_runtime_scratch_bytes'] = 512 * MIB  # covers the output directories of a MIB-sized unit plan
    pool_bytes.update(pools)
    return {'schema_version': 3, 'kind': 'physical_policy', 'table_map_sha256': physical.digest(source_map),
            'families': families, 'pools': pool_bytes}


def alloc(resource, component, nbytes, path, recursive=False, **extra):
    return {'owner': owner_of(resource), 'resource': resource, 'component': component, 'bytes': nbytes,
            'relative_path': path, 'recursive': recursive, **extra}


def database_allocations(path='out/catalog.sqlite', resource='catalog_main', ceiling=3 * PAGE):
    return [alloc(resource, 'main', ceiling, path),
            alloc(resource, 'journal', 2 * ceiling + MIB, path),
            alloc(resource, 'temp', 2 * ceiling, path)]


class PhysicalBase(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / '.agent-proof-fixtures' / 'physical-accounting'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        (self.root / 'out').mkdir()
        (self.root / 'stage').mkdir()
        self.source_map = json.loads(physical.SOURCE_MAP_PATH.read_bytes())
        self.policy = build_policy(self.source_map)
        self.ledger = physical.ReservationLedger(self.policy, self.root, source_map=self.source_map)
        lease_path = self.root / 'lease'
        descriptor = os.open(lease_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.write(descriptor, b'lease')
        os.close(descriptor)
        self.lease_fd = os.open(lease_path, os.O_RDONLY)
        self.addCleanup(os.close, self.lease_fd)
        self.lease = SimpleNamespace(fd=self.lease_fd, relative_path='lease')
        self.binding = {'source': {'commit': '0' * 40}, 'session_id': str(uuid.UUID(int=1)),
                        'resource_plan_sha256': '1' * 64, 'physical_policy_sha256': physical.digest(self.policy)}
        self.expected = dict(self.binding, physical_policy=self.policy)
        self.forged = 0

    def issue(self, allocations, attachments=()):
        handle = self.ledger.issue_child_grant(list(allocations), list(attachments), self.binding, self.lease)
        self.addCleanup(handle.close)
        return handle

    def load(self, handle, **overrides):
        expected = dict(self.expected, **overrides)
        return physical.load_child_grant(handle.descriptor_fd, handle.sha256, self.lease_fd, expected)

    def forge(self, handle, mutate, *, mode=0o600, canonical_form=True):
        """A differently shaped descriptor with a matching hash, as a hostile parent could write."""
        value = handle.descriptor
        mutate(value)
        raw = physical.canonical(value) if canonical_form else json.dumps(value, indent=1).encode()
        self.forged += 1
        path = self.root / ('forged-%d.json' % self.forged)
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
        os.write(descriptor, raw)
        os.close(descriptor)
        os.chmod(path, mode)
        read_only = os.open(path, os.O_RDONLY)
        self.addCleanup(os.close, read_only)
        return read_only, hashlib.sha256(raw).hexdigest()

    def load_forged(self, handle, mutate, **kwargs):
        fd, sha = self.forge(handle, mutate, **kwargs)
        return physical.load_child_grant(fd, sha, self.lease_fd, self.expected)


class CalibrationHelpers:
    def role_connection(self, role):
        connection = sqlite3.connect(':memory:')
        self.addCleanup(connection.close)
        return connection, physical.install_empty_role(connection, role, self.source_map)

    def calibrate(self, policy, plan=None):
        geometry = {}
        for family, databases in self.source_map['families'].items():
            geometry[family] = [self.role_connection(d['database_role'])[1] for d in databases]
        plan = plan or {'unit_byte_allowance': MIB, 'total_retained_artifact_bytes': MIB, 'maximum_units': 2, 'total_session_disk_bytes': 1 << 40}
        return physical.calibrate_physical_policy(policy, geometry, {'source_workspace_bytes': 0, 'existing_unpooled_bytes': 0},
                                                  plan, source_map=self.source_map)


class PolicyAndCalibrationTests(CalibrationHelpers, PhysicalBase):

    def test_unknown_installed_table_and_index_are_refused(self):
        connection, _ = self.role_connection('catalog_manifest_closure')
        connection.execute('CREATE TABLE intruder(x)')
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'unknown/missing installed physical table'):
            physical.observe_geometry(connection, 'catalog_manifest_closure', self.source_map)
        connection.execute('DROP TABLE intruder')
        connection.execute('CREATE INDEX intruder_idx ON closure(done)')
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.observe_geometry(connection, 'catalog_manifest_closure', self.source_map)

    def test_installed_ddl_drift_from_the_source_map_is_refused(self):
        connection, _ = self.role_connection('catalog_manifest_closure')
        drifted = copy.deepcopy(self.source_map)
        entry = next(d for d in drifted['families']['catalog_export_scratch'] if d['database_role'] == 'catalog_manifest_closure')
        table = next(o for o in entry['objects'] if o['type'] == 'table')
        table['ddl'] = table['ddl'].replace('NOT NULL', 'NOT NULL DEFAULT 0', 1)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'installed physical DDL differs'):
            physical.observe_geometry(connection, 'catalog_manifest_closure', drifted)

    def test_source_map_constructor_drift_is_refused(self):
        sources = {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in self.source_map['sources']}
        fresh = dict(self.source_map, sources=sources)
        path = self.root / 'map.json'
        path.write_bytes(json.dumps(fresh).encode())
        path.chmod(0o600)
        self.assertEqual(physical.load_source_map(path)['sources'], sources)
        name = sorted(sources)[0]
        path.write_bytes(json.dumps(dict(fresh, sources=dict(sources, **{name: '0' * 64}))).encode())
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'constructor drift'):
            physical.load_source_map(path)

    def test_policy_digest_and_inventory_are_closed(self):
        physical.validate_physical_policy(self.policy, self.source_map)
        stale = dict(self.policy, table_map_sha256='0' * 64)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'digest differs'):
            physical.validate_physical_policy(stale, self.source_map)
        missing = copy.deepcopy(self.policy)
        del missing['pools']['cas_bytes']
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.validate_physical_policy(missing, self.source_map)
        wrong_owner = copy.deepcopy(self.policy)
        wrong_owner['families']['catalog_main']['owner'] = 'verifier'
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'owner differs'):
            physical.validate_physical_policy(wrong_owner, self.source_map)
        small_main = copy.deepcopy(self.policy)
        small_main['families']['catalog_main']['main_bytes'] = small_main['families']['catalog_main']['database_bytes'] - 1
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.validate_physical_policy(small_main, self.source_map)

    def test_calibration_matches_the_complete_actual_inventory_and_is_logical_independent(self):
        first = self.calibrate(self.policy)
        families = sum(v['main_bytes'] + v['journal_bytes'] + v['temp_bytes'] for v in self.policy['families'].values())
        pools = sum(self.policy['pools'].values())
        self.assertEqual(first['peak_bytes'], families + pools + 6 * MIB + 32 * MIB * 5)
        self.assertEqual(first['policy_sha256'], physical.digest(self.policy))
        # Logical record allowances never alter the physical identity or the peak.
        def plan_inputs(records):
            wrapper = {'physical_policy': self.policy, 'source_receipt': {}, 'record_allowances': {'Product': {'records': records, 'max_encoded_bytes': 1024}}}
            return physical.accounting_plan_inputs({'source_bounds_utf8': json.dumps(wrapper), 'unit_byte_allowance': 1, 'unit_record_allowance': 1,
                'maximum_units': 1, 'total_session_disk_bytes': 1, 'total_retained_artifact_bytes': 1, 'resident_bytes': 1,
                'resident_rss_bytes': 1, 'unit_predecessor_allowance': 1})
        low, high = plan_inputs(10), plan_inputs(10 ** 9)
        self.assertEqual(low['physical_policy_sha256'], high['physical_policy_sha256'])
        self.assertNotEqual(low['record_allowances_sha256'], high['record_allowances_sha256'])
        self.assertEqual(self.calibrate(self.policy)['peak_bytes'], first['peak_bytes'])

    def test_calibration_refuses_unfit_empty_databases_and_unmeasured_roles(self):
        tiny = build_policy(self.source_map, database=PAGE)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'empty physical database exceeds declared ceiling'):
            self.calibrate(tiny)
        geometry = {f: [self.role_connection(d['database_role'])[1] for d in dbs] for f, dbs in self.source_map['families'].items()}
        geometry['publication_pack_scratch'].pop()
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'unmeasured source-declared physical database role'):
            physical.calibrate_physical_policy(self.policy, geometry, {'source_workspace_bytes': 0, 'existing_unpooled_bytes': 0},
                {'unit_byte_allowance': MIB, 'total_retained_artifact_bytes': MIB, 'maximum_units': 1, 'total_session_disk_bytes': 1 << 40}, source_map=self.source_map)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'peak exceeds immutable session disk ceiling'):
            self.calibrate(self.policy, {'unit_byte_allowance': MIB, 'total_retained_artifact_bytes': MIB, 'maximum_units': 1, 'total_session_disk_bytes': 1024})


class SimultaneousLifetimeTests(CalibrationHelpers, PhysicalBase):
    """F8/F9: calibration reserves every declared simultaneous database lifetime; the map has no phantom."""
    DECLARED = {'catalog_main': 2, 'verifier_main': 2, 'catalog_export_scratch': 1, 'verifier_continuation_scratch': 1,
                'catalog_or_verifier_restore_scratch': 0, 'publication_pack_scratch': 3, 'authentication_index': 3,
                'publication_pending_scratch': 2}

    def test_the_map_declares_the_simultaneous_lifetime_of_every_database_role(self):
        self.assertEqual({f: physical.family_instances(self.source_map, f) for f in physical.FAMILY_OWNERS}, self.DECLARED)
        for databases in self.source_map['families'].values():
            for entry in databases:
                self.assertTrue(entry['role_lifetime']['holders'])

    def test_calibration_refuses_a_family_reservation_below_its_simultaneous_lifetimes(self):
        database = self.policy['families']['catalog_main']['database_bytes']
        self.calibrate(self.policy)  # exactly the declared minimum is admitted
        for family in physical.FAMILY_OWNERS:
            needed = physical.minimum_family_policy(self.source_map, family, database)
            for key in ('main_bytes', 'journal_bytes', 'temp_bytes'):
                policy = copy.deepcopy(self.policy)
                policy['families'][family][key] = needed[key] - 1
                with self.assertRaises(physical.PhysicalPolicyError, msg=family + '/' + key):
                    self.calibrate(policy)

    def test_the_one_database_journal_shape_that_failed_the_first_execute_job_is_no_longer_calibrated(self):
        old = copy.deepcopy(self.policy)
        database = old['families']['publication_pack_scratch']['database_bytes']
        old['families']['publication_pack_scratch'].update(main_bytes=2 * database, journal_bytes=2 * database + MIB, temp_bytes=2 * database)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'below its simultaneous database lifetimes: publication_pack_scratch'):
            self.calibrate(old)
        # What the old shape could not do: hold the pack job's second database.
        ledger = physical.ReservationLedger(old, self.root, source_map=self.source_map)
        physical.reserve_database(ledger, 'publication_pack_scratch', self.root / 'out' / 'old-0.sqlite', database)
        with self.assertRaisesRegex(physical.PhysicalIncomplete, 'publication_pack_scratch/journal'):
            physical.reserve_database(ledger, 'publication_pack_scratch', self.root / 'out' / 'old-1.sqlite', database)

    def test_a_calibrated_policy_holds_every_declared_database_of_a_family_and_not_one_more(self):
        for family, count in self.DECLARED.items():
            count = max(count, 1)
            database = self.policy['families'][family]['database_bytes']
            ledger = physical.ReservationLedger(self.policy, self.root, source_map=self.source_map)
            for index in range(count):
                physical.reserve_database(ledger, family, self.root / 'out' / ('%s-%d.sqlite' % (family, index)), database)
            before = dict(ledger.used)
            with self.assertRaises(physical.PhysicalIncomplete, msg=family):
                physical.reserve_database(ledger, family, self.root / 'out' / ('%s-extra.sqlite' % family), database)
            self.assertEqual(ledger.used, before, 'a refused extra database leaks no deduction')

    def test_every_map_constructor_resolves_to_a_definition_in_its_pinned_source(self):
        for family, databases in self.source_map['families'].items():
            for entry in databases:
                for obj in entry['objects']:
                    self.assertIn(obj['source_path'], self.source_map['sources'])
                    text = (ROOT / obj['source_path']).read_text()
                    self.assertTrue(physical.constructor_defined(obj['source_path'], obj['constructor'], obj['name'], text),
                                    '%s/%s: %s' % (entry['database_role'], obj['name'], obj['constructor']))

    def test_constructor_resolution_rejects_removed_or_renamed_definitions(self):
        provenance = 'validation/async_refinement_provenance.py'
        text = (ROOT / provenance).read_text()
        self.assertTrue(physical.constructor_defined(provenance, 'PartPlan.__init__', 'members', text))
        self.assertFalse(physical.constructor_defined(provenance, 'restore_catalog', 'chunks', text))  # removed by the capacity series
        self.assertFalse(physical.constructor_defined(provenance, 'PartPlan.restore_catalog', 'chunks', text))
        self.assertFalse(physical.constructor_defined(provenance, 'NoSuchClass.__init__', 'chunks', text))
        sql = 'verification/async-driver-refinement/paged-catalog.sql'
        self.assertFalse(physical.constructor_defined(sql, 'source SQL', 'not_a_table', (ROOT / sql).read_text()))
        rust = 'crates/plurx-core/src/store/async_driver_catalog/sqlite.rs'
        self.assertTrue(physical.constructor_defined(rust, 'SqliteStorage::open_inner', 'x', (ROOT / rust).read_text()))
        self.assertFalse(physical.constructor_defined(rust, 'SqliteStorage::removed_function', 'x', (ROOT / rust).read_text()))

    def test_a_phantom_database_role_cannot_load_or_calibrate(self):
        self.assertEqual(self.source_map['families']['catalog_or_verifier_restore_scratch'], [])
        phantom = copy.deepcopy(self.source_map)
        template = next(d for d in phantom['families']['catalog_export_scratch'])
        ghost = copy.deepcopy(template)
        ghost['database_role'] = 'snapshot_restore'
        for obj in ghost['objects']:
            obj['source_path'], obj['constructor'] = 'validation/async_refinement_provenance.py', 'restore_catalog'
        phantom['families']['catalog_or_verifier_restore_scratch'] = [ghost]
        phantom['sources'] = {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in phantom['sources']}
        path = self.root / 'phantom-map.json'
        path.write_bytes(json.dumps(phantom).encode())
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'constructor does not resolve'):
            physical.load_source_map(path)
        # And a measurement of a role the family does not declare is refused as unknown.
        geometry = {f: [self.role_connection(d['database_role'])[1] for d in dbs] for f, dbs in self.source_map['families'].items()}
        geometry['catalog_or_verifier_restore_scratch'] = [geometry['catalog_export_scratch'][0]]
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'unknown/duplicate measured database role'):
            physical.calibrate_physical_policy(self.policy, geometry, {'source_workspace_bytes': 0, 'existing_unpooled_bytes': 0},
                {'unit_byte_allowance': MIB, 'total_retained_artifact_bytes': MIB, 'maximum_units': 1, 'total_session_disk_bytes': 1 << 40}, source_map=self.source_map)

    def test_the_role_lifetime_declaration_is_closed(self):
        for mutate in (lambda role: role.pop('role_lifetime'), lambda role: role['role_lifetime'].update(simultaneous=0),
                       lambda role: role['role_lifetime'].update(simultaneous=True), lambda role: role['role_lifetime'].update(extra=1),
                       lambda role: role['role_lifetime'].update(holders='')):
            value = copy.deepcopy(self.source_map)
            mutate(value['families']['catalog_main'][0])
            value['sources'] = {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in value['sources']}
            path = self.root / 'lifetime-map.json'
            path.write_bytes(json.dumps(value).encode())
            with self.assertRaises(physical.PhysicalPolicyError):
                physical.load_source_map(path)


RUST_CATALOG = ROOT / 'crates' / 'plurx-core' / 'src' / 'store' / 'async_driver_catalog.rs'
PARITY = ROOT / 'tests' / 'operations' / 'fixtures' / 'physical-policy' / 'parity.json'


def rust_constant(name):
    """The value of `pub const NAME: u64 = a * b * c;` read from the Rust source text."""
    import re
    match = re.search(r'^pub const ' + name + r': u64 = ([0-9_ *]+);$', RUST_CATALOG.read_text(), re.M)
    assert match, name
    value = 1
    for factor in match[1].split('*'):
        value *= int(factor.replace('_', ''))
    return value


class RestoreStagingCalibrationTests(CalibrationHelpers, PhysicalBase):
    """N4: downloads, restores and rebuilt evidence charge restore_staging_bytes, so calibration must size it for them."""

    def plan(self, **overrides):
        return dict({'unit_byte_allowance': MIB, 'total_retained_artifact_bytes': 3 * MIB, 'maximum_units': 2,
                     'total_session_disk_bytes': 1 << 40}, **overrides)

    def required(self, policy, plan):
        families = policy['families']
        return (2 * plan['total_retained_artifact_bytes']
                + min(512 * MIB, plan['total_retained_artifact_bytes'])  # the largest ZIP of a job stays deducted
                + 2 * max(families['catalog_main']['database_bytes'], families['verifier_main']['database_bytes'])
                + plan['unit_byte_allowance'])

    def test_the_pool_must_cover_the_history_twice_two_databases_and_the_evidence_allowance(self):
        plan = self.plan()
        need = self.required(self.policy, plan)
        self.assertEqual(physical.restore_staging_requirement(self.policy, plan), need)
        exact = copy.deepcopy(self.policy)
        exact['pools']['restore_staging_bytes'] = need
        self.assertEqual(self.calibrate(exact, plan)['kind'], 'physical_calibration')
        short = copy.deepcopy(self.policy)
        short['pools']['restore_staging_bytes'] = need - 1
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'restore staging pool does not cover'):
            self.calibrate(short, plan)

    def test_each_term_of_the_inequality_is_binding(self):
        base = self.plan()
        floor = self.required(self.policy, base)
        for name, grown in (('total_retained_artifact_bytes', 2 * base['total_retained_artifact_bytes']),
                            ('unit_byte_allowance', 2 * base['unit_byte_allowance'])):
            plan = self.plan(**{name: grown})
            policy = copy.deepcopy(self.policy)
            policy['pools']['restore_staging_bytes'] = floor
            self.assertGreater(self.required(policy, plan), floor, name)
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'restore staging pool does not cover', msg=name):
                self.calibrate(policy, plan)
        # The larger of the catalog and verifier ceilings counts, twice.
        policy = copy.deepcopy(self.policy)
        policy['families']['verifier_main'] = {'owner': 'verifier', **physical.minimum_family_policy(
            self.source_map, 'verifier_main', 4 * self.policy['families']['catalog_main']['database_bytes'])}
        policy['pools']['restore_staging_bytes'] = floor
        self.assertEqual(self.required(policy, base) - floor, 2 * 3 * self.policy['families']['catalog_main']['database_bytes'])
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'restore staging pool does not cover'):
            self.calibrate(policy, base)
        policy['pools']['restore_staging_bytes'] = self.required(policy, base)
        self.calibrate(policy, base)

    def test_the_transient_zip_high_water_is_a_term_of_the_inequality(self):
        # The job-wide high-water ZIP reservation is min(ARTIFACT_BYTES, T): T itself while T is the smaller,
        # one 512 MiB artifact once the retained total is larger. Without the term the old sum is short by it.
        for retained, term in ((3 * MIB, 3 * MIB), (2048 * MIB, 512 * MIB)):
            plan = self.plan(total_retained_artifact_bytes=retained)
            old_sum = (2 * retained + 2 * max(self.policy['families']['catalog_main']['database_bytes'],
                                              self.policy['families']['verifier_main']['database_bytes'])
                       + plan['unit_byte_allowance'])
            need = physical.restore_staging_requirement(self.policy, plan)
            self.assertEqual(need - old_sum, term)
            policy = copy.deepcopy(self.policy)
            policy['pools']['restore_staging_bytes'] = old_sum
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'transient ZIP'):
                self.calibrate(policy, dict(plan, total_session_disk_bytes=1 << 60))

    def test_the_unadmitted_example_states_the_same_inequality_for_its_own_figures(self):
        example = json.loads((ROOT / 'verification' / 'async-driver-refinement' / 'paged-resource-plan.example.json').read_bytes())
        policy = json.loads(example['record']['source_bounds_utf8'])['physical_policy']
        illustrative = {'total_retained_artifact_bytes': 4 * example['record']['artifact_retained_bytes'],
                        'unit_byte_allowance': example['record']['artifact_extracted_bytes']}
        # The example's pool is exactly what the shared helper gives for its illustrative figures (not a hand figure).
        self.assertEqual(policy['pools']['restore_staging_bytes'], physical.restore_staging_requirement(policy, illustrative))


class PythonRustPolicyParityTests(unittest.TestCase):
    """N6: the Python calibration and the Rust plan validator admit the identical policy set.

    Both read tests/operations/fixtures/physical-policy/parity.json (the Rust test
    `physical_policy_arithmetic_matches_the_shared_vectors` asserts the same numbers), and the Rust
    constants are read here straight from the Rust source text.
    """

    def setUp(self):
        self.vectors = json.loads(PARITY.read_bytes())

    def test_the_constants_are_equal_in_python_the_rust_source_and_the_vectors(self):
        for name, value in self.vectors['constants'].items():
            self.assertEqual(rust_constant(name), value, 'Rust ' + name)
            self.assertEqual(getattr(physical, name), value, 'Python ' + name)

    def test_the_arithmetic_is_the_vectors_exactly(self):
        for row in self.vectors['runtime_pool_demand']:
            self.assertEqual(physical.runtime_pool_demand(row['unit_byte_allowance']), row['demand'])
        for row in self.vectors['lifetime_reservation']:
            self.assertEqual(physical.lifetime_reservation(row['live'], row['database_bytes']),
                             {k: row[k] for k in ('database_bytes', 'main_bytes', 'journal_bytes', 'temp_bytes')})
        for row in self.vectors['restore_staging_requirement']:
            policy = {'families': {'catalog_main': {'database_bytes': row['catalog_database_bytes']},
                                   'verifier_main': {'database_bytes': row['verifier_database_bytes']}}}
            plan = {'total_retained_artifact_bytes': row['total_retained_artifact_bytes'], 'unit_byte_allowance': row['unit_byte_allowance']}
            self.assertEqual(physical.restore_staging_requirement(policy, plan), row['required'])

    def test_the_rust_validator_applies_every_python_calibration_inequality(self):
        text = RUST_CATALOG.read_text()
        for needle in ('minimum_family_limits(&source_map, family, limits.database_bytes)',
                       'restore_staging_requirement(', 'runtime_pool_demand(number(body, "unit_byte_allowance")?)'):
            self.assertIn(needle, text)


class ParentIdentityTests(PhysicalBase):
    """F12: parent identity has an exact implementation on macOS as well as Linux (no /proc dependence)."""
    @staticmethod
    def kinfo(seconds, micros, pid):
        image = bytearray(648)
        struct.pack_into('=q', image, 0, seconds)
        struct.pack_into('=i', image, 8, micros)
        struct.pack_into('=i', image, 40, pid)
        return bytes(image)

    def test_darwin_kinfo_start_is_the_exact_microsecond_identity_of_that_pid(self):
        image = self.kinfo(1_790_000_000, 123_456, 4242)
        self.assertEqual(physical.parse_darwin_kinfo_start(image, 4242), 1_790_000_000_123_456)
        self.assertNotEqual(physical.parse_darwin_kinfo_start(self.kinfo(1_790_000_001, 123_456, 4242), 4242), 1_790_000_000_123_456)
        for bad in (self.kinfo(1_790_000_000, 123_456, 4243), self.kinfo(0, 0, 4242), self.kinfo(1, 1_000_000, 4242),
                    self.kinfo(1, -1, 4242), image[:43], b''):
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'parent process identity is unavailable'):
                physical.parse_darwin_kinfo_start(bad, 4242)

    def test_darwin_dispatch_reads_the_sysctl_image_and_never_proc(self):
        image = self.kinfo(1_790_000_000, 7, 4242)
        with mock.patch.object(sys, 'platform', 'darwin'), mock.patch.object(physical, '_darwin_kinfo', return_value=image) as sysctl, \
                mock.patch.object(Path, 'read_text', side_effect=AssertionError('the macOS path must not read /proc')):
            self.assertEqual(physical.process_start_ticks(4242), 1_790_000_000_000_007)
        sysctl.assert_called_once_with(4242)
        for bad in (0, -1, 2 ** 31, '4242', True):
            with self.assertRaises(physical.PhysicalPolicyError):
                physical.process_start_ticks(bad)

    @unittest.skipUnless(sys.platform == 'linux', 'reads the Linux /proc identity')
    def test_linux_identity_is_the_proc_start_time(self):
        raw = Path('/proc/self/stat').read_text()
        self.assertEqual(physical.process_start_ticks(os.getpid()), int(raw[raw.rindex(')') + 2:].split()[19]))

    def test_the_live_identity_is_stable_and_an_absent_process_is_refused(self):
        me = os.getpid()
        self.assertEqual(physical.process_start_ticks(me), physical.process_start_ticks(me))
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'parent process identity is unavailable'):
            physical.process_start_ticks(2 ** 31 - 1)

    def test_a_grant_verifies_through_the_macos_identity_and_a_recycled_pid_is_refused(self):
        starts = {'value': 1_790_000_000}
        with mock.patch.object(sys, 'platform', 'darwin'), \
                mock.patch.object(physical, '_darwin_kinfo', side_effect=lambda pid: self.kinfo(starts['value'], 5, pid)):
            handle = self.issue(database_allocations())
            child = self.load(handle)
            self.assertEqual(child.parent_identity, (os.getpid(), 1_790_000_000_000_005))
            starts['value'] += 1  # the same pid, started later: a different process
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'not the live identity'):
                self.load(handle)
            # Owned-root and lease identity stay exact device/inode comparisons on every platform.
            starts['value'] -= 1
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'owned root device/inode differs'):
                self.load_forged(handle, lambda v: v['owned_root'].update(inode=v['owned_root']['inode'] + 1))
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'lease device/inode/path differs'):
                self.load_forged(handle, lambda v: v['lease'].update(inode=v['lease']['inode'] + 1))


class LexicalOwnershipTests(PhysicalBase):
    def test_a_lexical_path_outside_the_root_is_refused_not_walked_forever(self):
        sibling = self.root.parent / ('lexical-sibling-' + uuid.uuid4().hex)
        sibling.mkdir()
        self.addCleanup(sibling.rmdir)
        lexical = sibling / '..' / self.root.name / 'out' / 'x.sqlite'  # resolves inside the root, never walks to it

        def hung(*_):
            raise AssertionError('physical._owned never terminated')
        previous = signal.signal(signal.SIGALRM, hung)
        signal.alarm(10)
        try:
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'not lexically under the owned root'):
                physical._owned(lexical, self.root)
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, previous)

    def test_issuing_a_grant_has_no_duplicated_initialisation(self):
        import ast
        tree = ast.parse(Path(physical.__file__).read_text())
        function = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == '_issue_child_grant')
        assigns = [ast.dump(statement) for statement in function.body if isinstance(statement, ast.Assign)]
        self.assertEqual(len(assigns), len(set(assigns)), 'a top-level initialisation statement is repeated')


class RestoredDatabaseCeilingTests(PhysicalBase):
    """F11: a restored database is checked against its effective page ceiling before its first open."""
    def restored(self, blob, name='stage/restored.sqlite'):
        path = self.root / name
        image = sqlite3.connect(path)
        image.execute('CREATE TABLE t(x BLOB)')
        image.execute('INSERT INTO t VALUES(zeroblob(%d))' % blob)
        image.commit()
        image.close()
        held = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', path.stat().st_size + PAGE)
        held.track(path)
        attachment = alloc('restore_staging_bytes', 'bytes', held.bytes, name, reservation_id=held.reservation_id, access='read_only')
        return path, attachment

    def budget(self, handle):
        budget = codec.Budget.__new__(codec.Budget)
        budget.physical_policy = self.policy
        budget.grant = self.load(handle)
        return budget

    def test_the_attached_restore_is_checked_before_open_and_an_oversize_database_is_incomplete(self):
        ceiling = self.policy['families']['verifier_main']['database_bytes']
        path, attachment = self.restored(blob=ceiling + 2 * PAGE)
        budget = self.budget(self.issue([], [attachment]))
        before = path.read_bytes()
        with mock.patch.object(sqlite3, 'connect', side_effect=AssertionError('the restored database must not be opened first')):
            with self.assertRaisesRegex(codec.Incomplete, 'effective page ceiling'):
                budget.restored_database('verifier_main', path)
        self.assertEqual(path.read_bytes(), before)

    def test_an_attached_restore_within_the_ceiling_reserves_no_second_main_file(self):
        path, attachment = self.restored(blob=3 * PAGE)
        budget = self.budget(self.issue([], [attachment]))
        used = dict(self.ledger.used)
        readonly = budget.restored_database('verifier_main', path)
        self.assertEqual(readonly.page_count, path.stat().st_size // PAGE)
        self.assertEqual(self.ledger.used, used)

    def test_a_restore_without_exactly_one_attached_reservation_is_incomplete(self):
        path, attachment = self.restored(blob=3 * PAGE)
        other = self.root / 'stage' / 'unattached.sqlite'
        other.write_bytes(path.read_bytes())
        budget = self.budget(self.issue([], [attachment]))
        with self.assertRaisesRegex(codec.Incomplete, 'no single held restore reservation'):
            budget.restored_database('verifier_main', other)
        with self.assertRaises(codec.Invalid):
            budget.restored_database('verifier_main', self.root.parent / 'outside.sqlite')

    def test_the_writers_own_database_is_checked_against_its_granted_ceiling_without_an_attachment(self):
        ceiling = 3 * PAGE
        path = self.root / 'out' / 'verifier.sqlite'
        image = sqlite3.connect(path)
        image.execute('CREATE TABLE t(x BLOB)')
        image.execute('INSERT INTO t VALUES(zeroblob(%d))' % (ceiling + 2 * PAGE))
        image.commit()
        image.close()
        budget = self.budget(self.issue(database_allocations('out/verifier.sqlite', 'verifier_main', ceiling)))
        with self.assertRaisesRegex(codec.Incomplete, 'effective physical page ceiling'):
            budget.restored_database('verifier_main', path, attached=False)
        small = self.root / 'out' / 'small.sqlite'
        image = sqlite3.connect(small)
        image.execute('CREATE TABLE t(x)')
        image.commit()
        image.close()
        self.assertIsNone(budget.restored_database('verifier_main', small, attached=False))

    def test_both_private_restore_validators_check_the_ceiling_before_opening(self):
        import ast
        tree = ast.parse((ROOT / 'verification' / 'async-driver-refinement' / 'paged_checker.py').read_text())
        functions = {n.name: n for n in tree.body if isinstance(n, ast.FunctionDef)}

        def lines(function, matches):
            return sorted(n.lineno for n in ast.walk(function) if isinstance(n, ast.Call) and matches(n))
        ceiling = lambda call: getattr(call.func, 'attr', '') == 'restored_database'
        opening = lambda call: getattr(call.func, 'attr', '') == 'connect'
        for name in ('validate_restore', 'validate_restored_verifier'):
            checks, opens = lines(functions[name], ceiling), lines(functions[name], opening)
            self.assertTrue(checks, name + ' never checks the restored database ceiling')
            if name == 'validate_restore':  # the catalog is opened by sqlite3.connect after the check
                self.assertLess(checks[0], opens[0], name + ' opens the restored database before checking its ceiling')
        # The catalog validator checks the staged verifier it names under the same rule: private_input_view is called
        # with a precheck that checks 'verifier_main', and a checkpoint without a verifier file never reaches it.
        views = [n for n in ast.walk(functions['validate_restore']) if isinstance(n, ast.Call)
                 and getattr(n.func, 'id', '') == 'private_input_view']
        self.assertEqual([[k.arg for k in call.keywords] for call in views], [['precheck']])
        verifier_checks = [n for n in ast.walk(functions['validate_restore']) if isinstance(n, ast.Call) and ceiling(n)
                           and getattr(n.args[0], 'value', None) == 'verifier_main']
        self.assertEqual(len(verifier_checks), 1)
        # The verifier validator runs the check as the precheck of the first open, before any read.
        view = functions['private_input_view']
        self.assertLess(lines(view, lambda c: getattr(c.func, 'id', '') == 'precheck')[0],
                        lines(view, lambda c: getattr(c.func, 'attr', '') == 'readonly_view')[0])
        # The unit preparation of the writer's own verifier database is not an attached restore.
        keywords = [k for n in ast.walk(functions['prepare_unit']) if isinstance(n, ast.Call)
                    and getattr(n.func, 'id', '') == 'validate_restored_verifier' for k in n.keywords]
        self.assertEqual([(k.arg, getattr(k.value, 'value', None)) for k in keywords], [('attached', False)])


class DatabaseCapTests(PhysicalBase):
    def test_ceiling_rounds_down_to_the_measured_page_size_and_is_effective(self):
        grant = physical.reserve_database(self.ledger, 'catalog_main', self.root / 'out' / 'a.sqlite', 3 * PAGE + 1)
        connection = grant.connect()
        self.addCleanup(connection.close)
        self.assertEqual(grant.effective_pages, 3)
        self.assertEqual(connection.execute('PRAGMA max_page_count').fetchone()[0], 3)

    def test_an_ineffective_page_cap_is_refused_before_any_transaction(self):
        class Lying(sqlite3.Connection):
            def execute(self, sql, *args):
                if sql.startswith('PRAGMA max_page_count='):
                    return super().execute('SELECT 4')
                return super().execute(sql, *args)
        grant = physical.reserve_database(self.ledger, 'catalog_main', self.root / 'out' / 'b.sqlite', 3 * PAGE)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'ineffective SQLite max_page_count'):
            grant.connect(factory=Lying)

    def test_restored_database_beyond_the_effective_ceiling_is_refused_unmodified(self):
        path = self.root / 'out' / 'restored.sqlite'
        image = sqlite3.connect(path)
        image.execute('CREATE TABLE t(x BLOB)')
        image.execute('INSERT INTO t VALUES(zeroblob(40000))')
        image.commit()
        image.close()
        before = path.read_bytes()
        grant = physical.reserve_database(self.ledger, 'catalog_main', path, 3 * PAGE)
        with self.assertRaises(physical.PhysicalIncomplete):
            grant.connect()
        self.assertEqual(path.read_bytes(), before)

    def test_journal_and_temp_boundaries_are_exact_and_refusal_leaks_nothing(self):
        for component, delta in (('journal_bytes', 0), ('temp_bytes', 0)):
            policy = copy.deepcopy(self.policy)
            ceiling = 3 * PAGE
            policy['families']['catalog_main']['journal_bytes'] = 2 * ceiling + MIB
            policy['families']['catalog_main']['temp_bytes'] = 2 * ceiling
            ledger = physical.ReservationLedger(policy, self.root, source_map=self.source_map)
            physical.reserve_database(ledger, 'catalog_main', self.root / 'out' / ('ok-%s.sqlite' % component), ceiling)
        for component in ('journal_bytes', 'temp_bytes'):
            policy = copy.deepcopy(self.policy)
            ceiling = 3 * PAGE
            policy['families']['catalog_main']['journal_bytes'] = 2 * ceiling + MIB
            policy['families']['catalog_main']['temp_bytes'] = 2 * ceiling
            policy['families']['catalog_main'][component] -= 1
            ledger = physical.ReservationLedger(policy, self.root, source_map=self.source_map)
            with self.assertRaises(physical.PhysicalIncomplete):
                physical.reserve_database(ledger, 'catalog_main', self.root / 'out' / ('short-%s.sqlite' % component), ceiling)
            self.assertEqual(sum(ledger.used.values()), 0, 'a refused database admission must not leak deductions')

    def test_ceiling_beyond_policy_is_refused(self):
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'exceeds immutable policy'):
            physical.reserve_database(self.ledger, 'catalog_main', self.root / 'out' / 'c.sqlite',
                                      self.policy['families']['catalog_main']['database_bytes'] + 1)


class ReservationTests(PhysicalBase):
    def test_copy_preallocation_fails_before_any_destination_exists(self):
        small = build_policy(self.source_map, restore_staging_bytes=PAGE)
        ledger = physical.ReservationLedger(small, self.root, source_map=self.source_map)
        with self.assertRaises(physical.PhysicalIncomplete):
            ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', PAGE + 1)
        self.assertEqual(ledger.used[('restore_staging_bytes', 'bytes')], 0)
        self.assertFalse((self.root / 'stage' / 'copy').exists())

    def test_release_only_after_the_owned_file_is_removed_and_fsynced(self):
        reservation = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', PAGE)
        path = self.root / 'stage' / 'copy'
        reservation.track(path)
        path.write_bytes(b'x' * 100)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'surviving physical file'):
            reservation.release_after_removal()
        self.assertEqual(self.ledger.used[('restore_staging_bytes', 'bytes')], PAGE)
        path.unlink()
        reservation.release_after_removal()
        self.assertEqual(self.ledger.used[('restore_staging_bytes', 'bytes')], 0)
        with self.assertRaises(physical.PhysicalPolicyError):
            reservation.release_after_removal()

    def test_growth_is_prospective_and_owned_files_cannot_exceed_the_reservation(self):
        reservation = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', 100)
        path = self.root / 'stage' / 'copy'
        reservation.track(path)
        path.write_bytes(b'x' * 100)
        reservation.check()
        path.write_bytes(b'x' * 101)
        with self.assertRaises(physical.PhysicalIncomplete):
            reservation.check()
        before = self.ledger.used[('restore_staging_bytes', 'bytes')]
        with self.assertRaises(physical.PhysicalIncomplete):
            reservation.grow(self.policy['pools']['restore_staging_bytes'] + 1)
        self.assertEqual(self.ledger.used[('restore_staging_bytes', 'bytes')], before)

    def test_owners_components_and_file_ownership_are_closed(self):
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'owner mismatch'):
            self.ledger.reserve('verifier', 'catalog_main', 'main', 1)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'unknown physical reservation component'):
            self.ledger.reserve('catalog', 'catalog_main', 'bytes', 1)
        first = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', PAGE)
        second = self.ledger.reserve('snapshot_transport', 'snapshot_spool_bytes', 'bytes', PAGE)
        first.track(self.root / 'stage' / 'one')
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'multiple owners'):
            second.track(self.root / 'stage' / 'one')
        tree = self.ledger.reserve('provenance', 'cas_bytes', 'bytes', PAGE)
        tree.track_subtree(self.root / 'stage' / 'tree')
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'multiple owners'):
            second.track(self.root / 'stage' / 'tree' / 'inside')
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'unsafe physical reservation path'):
            second.track(self.root.parent / 'escape')
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.reserve('catalog', 'catalog_main', 'main', 1, reservation_id=first.reservation_id)


class IssueGrantTests(PhysicalBase):
    def test_parent_deducts_before_spawn_and_seals_a_canonical_private_readonly_descriptor(self):
        before = self.ledger.remaining('catalog_main', 'main')
        handle = self.issue(database_allocations() + [alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/backup.sqlite')])
        self.assertEqual(self.ledger.remaining('catalog_main', 'main'), before - 3 * PAGE)
        self.assertEqual(self.ledger.used[('snapshot_spool_bytes', 'bytes')], PAGE)
        info = os.fstat(handle.descriptor_fd)
        self.assertTrue(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_mode & 0o077 == 0)
        self.assertEqual(fcntl.fcntl(handle.descriptor_fd, fcntl.F_GETFL) & os.O_ACCMODE, os.O_RDONLY)
        raw = os.pread(handle.descriptor_fd, 1 << 20, 0)
        self.assertEqual(raw, physical.canonical(handle.descriptor))
        self.assertEqual(handle.sha256, hashlib.sha256(raw).hexdigest())
        body = handle.descriptor
        self.assertEqual((body['schema_version'], body['kind'], body['parent']['pid']), (1, 'local_physical_grant', os.getpid()))
        self.assertEqual(body['parent']['start_ticks'], physical.process_start_ticks(os.getpid()))
        self.assertEqual(set(handle.environment()), {'PLURX_ASYNC_PHYSICAL_GRANT_FD', 'PLURX_ASYNC_PHYSICAL_GRANT_SHA256', 'PLURX_ASYNC_PHYSICAL_LEASE_FD'})
        self.assertEqual(handle.environment()['PLURX_ASYNC_PHYSICAL_LEASE_FD'], str(self.lease_fd))
        self.assertEqual(handle.pass_fds(), (handle.descriptor_fd, self.lease_fd))
        # The descriptor's own encoding is charged to the control pool until reconciliation.
        self.assertEqual(self.ledger.used[('control_bytes', 'bytes')], len(raw))

    def test_refusals_leave_no_deduction_and_no_descriptor(self):
        def refused(allocations, attachments=(), error=physical.PhysicalPolicyError, binding=None):
            used = dict(self.ledger.used)
            with self.assertRaises(error):
                self.ledger.issue_child_grant(allocations, list(attachments), binding or self.binding, self.lease)
            self.assertEqual(self.ledger.used, used)
            self.assertEqual(list((self.root / 'physical-grants').glob('*')) if (self.root / 'physical-grants').exists() else [], [])
        refused([alloc('snapshot_spool_bytes', 'bytes', self.policy['pools']['snapshot_spool_bytes'] + 1, 'out/b')])
        refused([alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/b'), alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/b')])
        refused([alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/b'), alloc('cas_bytes', 'bytes', PAGE, 'out/b')])
        refused([dict(alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/b'), owner='verifier')])
        refused([alloc('snapshot_spool_bytes', 'main', PAGE, 'out/b')])
        refused([alloc('no_such_pool', 'bytes', PAGE, 'out/b')])
        refused([alloc('snapshot_spool_bytes', 'bytes', PAGE, '../escape')])
        refused([alloc('snapshot_spool_bytes', 'bytes', PAGE, '/abs/path')])
        refused([alloc('snapshot_spool_bytes', 'bytes', PAGE, 'lease')])
        refused([alloc('catalog_main', 'main', PAGE, 'out/c.sqlite', True)])
        refused([alloc('snapshot_spool_bytes', 'bytes', 0, 'out/b')])
        refused([alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/b')], binding=dict(self.binding, physical_policy_sha256='0' * 64))
        refused([alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/b')], binding=dict(self.binding, session_id='not-a-uuid'))
        refused([], error=physical.PhysicalPolicyError)

    def test_grant_beyond_the_parents_remaining_deduction_is_refused_after_a_first_grant(self):
        pool = self.policy['pools']['snapshot_spool_bytes']
        self.issue([alloc('snapshot_spool_bytes', 'bytes', pool - PAGE, 'out/first')])
        used = dict(self.ledger.used)
        with self.assertRaises(physical.PhysicalIncomplete):
            self.ledger.issue_child_grant([alloc('snapshot_spool_bytes', 'bytes', 2 * PAGE, 'out/second')], [], self.binding, self.lease)
        self.assertEqual(self.ledger.used, used)

    def test_overlapping_independent_grants_and_owned_file_aliases_are_refused(self):
        self.issue([alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/tree', True)])
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([alloc('cas_bytes', 'bytes', PAGE, 'out/tree/inside')], [], self.binding, self.lease)
        owned = physical.reserve_database(self.ledger, 'verifier_main', self.root / 'out' / 'v.sqlite', 3 * PAGE)
        self.assertIsNotNone(owned)
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([alloc('cas_bytes', 'bytes', PAGE, 'out/v.sqlite')], [], self.binding, self.lease)

    def test_journal_grant_resolves_only_the_fixed_database_companions(self):
        self.issue(database_allocations())
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([alloc('cas_bytes', 'bytes', PAGE, 'out/catalog.sqlite-wal')], [], self.binding, self.lease)
        self.issue([alloc('cas_bytes', 'bytes', PAGE, 'out/catalog.sqlite-other')])

    def test_lease_must_be_the_held_owned_root_file(self):
        other = os.open(self.root / 'out', os.O_RDONLY)
        self.addCleanup(os.close, other)
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([alloc('cas_bytes', 'bytes', PAGE, 'out/c')], [], self.binding,
                                          SimpleNamespace(fd=other, relative_path='lease'))
        decoy = self.root / 'decoy'
        decoy.write_bytes(b'x')
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([alloc('cas_bytes', 'bytes', PAGE, 'out/c')], [], self.binding,
                                          SimpleNamespace(fd=self.lease_fd, relative_path='decoy'))


class ChildLoaderTests(PhysicalBase):
    def test_child_view_is_only_the_grant(self):
        handle = self.issue(database_allocations())
        child = self.load(handle)
        self.assertEqual(child.limits, {('catalog_main', 'main'): 3 * PAGE, ('catalog_main', 'journal'): 2 * 3 * PAGE + MIB,
                                        ('catalog_main', 'temp'): 2 * 3 * PAGE})
        with self.assertRaises(physical.PhysicalPolicyError):
            child.reserve('verifier', 'verifier_main', 'main', 1)
        with self.assertRaises(physical.PhysicalPolicyError):
            child.reserve('provenance', 'cas_bytes', 'bytes', 1)
        with self.assertRaises(physical.PhysicalIncomplete):
            child.reserve('catalog', 'catalog_main', 'main', 3 * PAGE + 1)
        spent = child.reserve('catalog', 'catalog_main', 'main', PAGE)
        with self.assertRaises(physical.PhysicalIncomplete):
            spent.grow(3 * PAGE + 1)

    def test_child_database_enforces_the_granted_ceiling_not_the_policy_ceiling(self):
        handle = self.issue(database_allocations(ceiling=3 * PAGE))
        child = self.load(handle)
        with self.assertRaises(physical.PhysicalIncomplete):
            physical.reserve_database(child, 'catalog_main', self.root / 'out' / 'catalog.sqlite', 4 * PAGE)
        grant = physical.reserve_database(child, 'catalog_main', self.root / 'out' / 'catalog.sqlite', 3 * PAGE)
        connection = grant.connect()
        self.addCleanup(connection.close)
        self.assertEqual(connection.execute('PRAGMA max_page_count').fetchone()[0], 3)
        # The first database consumed the whole granted main allocation; the policy ceiling (far larger) grants nothing more.
        with self.assertRaisesRegex(physical.PhysicalIncomplete, 'physical reservation exhausted: catalog_main/main'):
            physical.reserve_database(child, 'catalog_main', self.root / 'out' / 'elsewhere.sqlite', PAGE)

    def test_child_cannot_claim_a_path_outside_its_allocation(self):
        handle = self.issue([alloc('snapshot_spool_bytes', 'bytes', 2 * PAGE, 'out/spool.bin')])
        child = self.load(handle)
        reservation = child.reserve('snapshot_transport', 'snapshot_spool_bytes', 'bytes', PAGE)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'outside the granted allocation scope'):
            reservation.track(self.root / 'out' / 'other.bin')
        reservation.track(self.root / 'out' / 'spool.bin')

    def test_descriptor_identity_privacy_and_form_are_all_enforced(self):
        handle = self.issue(database_allocations())
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'hash differs'):
            physical.load_child_grant(handle.descriptor_fd, '0' * 64, self.lease_fd, self.expected)
        writable = os.open(self.root / 'physical-grants' / (handle.descriptor['grant_id'] + '.json'), os.O_RDWR)
        self.addCleanup(os.close, writable)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'writable'):
            physical.load_child_grant(writable, handle.sha256, self.lease_fd, self.expected)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'private regular file'):
            self.load_forged(handle, lambda v: None, mode=0o640)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'not canonical'):
            self.load_forged(handle, lambda v: None, canonical_form=False)
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.load_child_grant(handle.descriptor_fd, handle.sha256, handle.descriptor_fd, self.expected)

    def test_binding_parent_root_and_lease_identity_are_all_checked(self):
        handle = self.issue(database_allocations())
        for field, value in (('session_id', str(uuid.UUID(int=2))), ('resource_plan_sha256', '2' * 64), ('source', {'commit': '1' * 40})):
            with self.assertRaisesRegex(physical.PhysicalPolicyError, 'binding differs'):
                self.load(handle, **{field: value})
        with self.assertRaises(physical.PhysicalPolicyError):
            self.load(handle, physical_policy_sha256='0' * 64)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'owned root differs'):
            self.load(handle, owned_root=str(self.root / 'out'))
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'live identity'):
            self.load_forged(handle, lambda v: v['parent'].update(start_ticks=1))
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'parent process identity is unavailable/not live'):
            self.load_forged(handle, lambda v: v['parent'].update(pid=4194304))
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'owned root device/inode'):
            self.load_forged(handle, lambda v: v['owned_root'].update(inode=v['owned_root']['inode'] + 1))
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'inherited lease'):
            self.load_forged(handle, lambda v: v['lease'].update(inode=v['lease']['inode'] + 1))
        other = os.open(self.root / 'physical-grants', os.O_RDONLY)
        self.addCleanup(os.close, other)
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.load_child_grant(handle.descriptor_fd, handle.sha256, other, self.expected)

    def test_entries_beyond_policy_or_malformed_are_refused_even_with_a_valid_hash(self):
        handle = self.issue(database_allocations())
        too_big = self.policy['families']['catalog_main']['main_bytes'] + 1
        for mutate in (lambda v: v['allocations'][0].update(bytes=too_big),
                       lambda v: v['allocations'][0].update(owner='verifier'),
                       lambda v: v['allocations'][0].update(component='bytes'),
                       lambda v: v['allocations'][0].update(relative_path='../x'),
                       lambda v: v['allocations'][1].update(reservation_id=v['allocations'][0]['reservation_id']),
                       lambda v: v['allocations'][1].update(relative_path=v['allocations'][0]['relative_path'], component='main'),
                       lambda v: v['allocations'].append(dict(v['allocations'][0], reservation_id='9' * 64, resource='cas_bytes',
                                                              owner='provenance', component='bytes')),
                       lambda v: v.update(extra=1),
                       lambda v: v.update(schema_version=2)):
            with self.assertRaises(physical.PhysicalPolicyError):
                self.load_forged(handle, mutate)


class ReconcileTests(PhysicalBase):
    def test_surviving_files_stay_charged_and_the_rest_is_released_after_reap(self):
        handle = self.issue(database_allocations() + [alloc('snapshot_spool_bytes', 'bytes', PAGE, 'out/backup.sqlite')])
        (self.root / 'out' / 'catalog.sqlite').write_bytes(b'x' * 100)
        (self.root / 'out' / 'catalog.sqlite-wal').write_bytes(b'y')
        descriptor = self.root / 'physical-grants' / (handle.descriptor['grant_id'] + '.json')
        with self.assertRaises(physical.PhysicalPolicyError):
            handle.reconcile_after_reap(SimpleNamespace(poll=lambda: None))
        report = handle.reconcile_after_reap(0)
        self.assertEqual(len(report['retained']), 2, 'main and journal files survive')
        self.assertEqual(len(report['released']), 2)
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], 3 * PAGE)
        self.assertEqual(self.ledger.used[('catalog_main', 'journal')], 2 * 3 * PAGE + MIB)
        self.assertEqual(self.ledger.used[('catalog_main', 'temp')], 0)
        self.assertEqual(self.ledger.used[('snapshot_spool_bytes', 'bytes')], 0)
        self.assertEqual(self.ledger.used[('control_bytes', 'bytes')], 0)
        self.assertFalse(descriptor.exists())
        with self.assertRaises(physical.PhysicalPolicyError):
            handle.reconcile_after_reap(0)
        # The survivors are plain parent reservations again and release only after removal.
        survivor = next(r for r in self.ledger.reservations if r.key == ('catalog_main', 'main') and not r.released)
        self.assertFalse(survivor.delegated)
        with self.assertRaises(physical.PhysicalPolicyError):
            survivor.release_after_removal()

    def test_a_child_that_overspent_its_allocation_is_still_charged(self):
        handle = self.issue([alloc('snapshot_spool_bytes', 'bytes', 100, 'out/backup.sqlite')])
        (self.root / 'out' / 'backup.sqlite').write_bytes(b'z' * 101)
        with self.assertRaises(physical.PhysicalIncomplete):
            handle.reconcile_after_reap(1)
        self.assertEqual(self.ledger.used[('snapshot_spool_bytes', 'bytes')], 100)

    def test_a_delegated_reservation_cannot_be_released_while_the_child_may_run(self):
        handle = self.issue([alloc('snapshot_spool_bytes', 'bytes', 100, 'out/backup.sqlite')])
        delegated = handle.reservations[0]
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'delegated'):
            delegated.release_after_removal()


class ReadonlyAttachmentTests(PhysicalBase):
    def restored(self, name='stage/restored.sqlite', pages_blob=9000):
        path = self.root / name
        image = sqlite3.connect(path)
        image.execute('CREATE TABLE t(x BLOB)')
        image.execute('INSERT INTO t VALUES(zeroblob(%d))' % pages_blob)
        image.commit()
        image.close()
        held = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', path.stat().st_size + PAGE)
        held.track(path)
        attachment = alloc('restore_staging_bytes', 'bytes', held.bytes, name, reservation_id=held.reservation_id, access='read_only')
        return path, held, attachment

    def child_with(self, attachment, extra=()):
        handle = self.issue(list(extra), [attachment])
        return self.load(handle)

    def test_attaches_the_held_reservation_without_mutating_bytes_or_adding_a_main_allocation(self):
        path, held, attachment = self.restored()
        used = self.ledger.used[('catalog_main', 'main')]
        child = self.child_with(attachment, [alloc('catalog_main', 'temp', 2 * 3 * PAGE, 'out/scratch', True)])
        before = hashlib.sha256(path.read_bytes()).hexdigest()
        readonly = physical.validate_readonly_database(child, 'catalog_main', path, attachment['reservation_id'])
        self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), before)
        self.assertEqual((readonly.page_size, readonly.page_count), (PAGE, path.stat().st_size // PAGE))
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], used, 'no second main-file reservation exists')
        self.assertFalse(any(Path(str(path) + s).exists() for s in physical.DB_COMPANIONS))
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'second main allocation|never receives another main'):
            readonly.reserve_ancillary('main', 1)
        with self.assertRaises(physical.PhysicalPolicyError):
            readonly.reserve_ancillary('journal', 1)  # no journal allocation was granted for this attachment
        readonly.reserve_ancillary('temp', PAGE)
        connection = readonly.connect()
        self.addCleanup(connection.close)
        self.assertEqual(connection.execute('SELECT count(*) FROM t').fetchone(), (1,))
        self.assertTrue(held.paths)

    def test_oversized_restore_wrong_attachment_and_unaccounted_companions_are_refused(self):
        path, held, attachment = self.restored()
        child = self.child_with(attachment)
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.validate_readonly_database(child, 'catalog_main', path, '0' * 64)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'outside its attached reservation'):
            physical.validate_readonly_database(child, 'catalog_main', self.root / 'stage' / 'other.sqlite', attachment['reservation_id'])
        Path(str(path) + '-journal').write_bytes(b'x')
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'companion'):
            physical.validate_readonly_database(child, 'catalog_main', path, attachment['reservation_id'])
        Path(str(path) + '-journal').unlink()
        path.write_bytes(path.read_bytes() + b'\0')
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.validate_readonly_database(child, 'catalog_main', path, attachment['reservation_id'])

    def test_restore_beyond_the_effective_page_ceiling_is_incomplete(self):
        tight = build_policy(self.source_map, database=2 * PAGE)
        self.ledger = physical.ReservationLedger(tight, self.root, source_map=self.source_map)
        self.policy, self.expected = tight, dict(self.expected, physical_policy=tight)
        self.binding = dict(self.binding, physical_policy_sha256=physical.digest(tight))
        self.expected['physical_policy_sha256'] = self.binding['physical_policy_sha256']
        path, held, attachment = self.restored()
        child = self.child_with(attachment)
        with self.assertRaises(physical.PhysicalIncomplete):
            physical.validate_readonly_database(child, 'catalog_main', path, attachment['reservation_id'])

    def test_an_attachment_must_name_a_live_held_reservation_and_never_alias_a_writable_allocation(self):
        path, held, attachment = self.restored()
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([], [dict(attachment, bytes=attachment['bytes'] + 1)], self.binding, self.lease)
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([], [dict(attachment, reservation_id='7' * 64)], self.binding, self.lease)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'aliases a writable allocation'):
            self.ledger.issue_child_grant([alloc('catalog_main', 'main', PAGE, 'stage/restored.sqlite')], [attachment], self.binding, self.lease)
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([], [{k: v for k, v in attachment.items() if k != 'access'}], self.binding, self.lease)


class TransferTests(PhysicalBase):
    def staged(self):
        path = self.root / 'stage' / 'restored.sqlite'
        path.write_bytes(b'd' * 2000)
        held = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', 4096)
        held.track(path)
        return path, held

    def finish_rename(self, source, destination):
        info = os.lstat(source)
        os.rename(source, destination)
        for directory in {source.parent, destination.parent}:
            physical._fsync_directory(directory)
        return info.st_dev, info.st_ino

    def test_one_ownership_after_rename_and_directory_fsync_never_an_unowned_interval(self):
        source, held = self.staged()
        destination = self.root / 'out' / 'catalog.sqlite'
        handle = self.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'main', destination)
        self.assertEqual(self.ledger.used[('restore_staging_bytes', 'bytes')], 4096, 'source stays charged')
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], 4096, 'destination reserved before the rename')
        self.assertTrue(handle.record[0].exists(), 'both identities are durably recorded')
        record = json.loads(handle.record[0].read_bytes())
        self.assertEqual({k: record['source'][k] for k in ('owner', 'resource', 'component')},
                         {'owner': 'snapshot_restore', 'resource': 'restore_staging_bytes', 'component': 'bytes'},
                         'the record names the source side itself; crash reconciliation never assumes it')
        self.assertEqual({k: record['destination'][k] for k in ('owner', 'resource', 'component')},
                         {'owner': 'catalog', 'resource': 'catalog_main', 'component': 'main'})
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'still exists'):
            self.ledger.commit_transfer_after_rename_fsync(handle, (handle.identity[0], handle.identity[1]))
        identity = self.finish_rename(source, destination)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'differs from the prepared'):
            self.ledger.commit_transfer_after_rename_fsync(handle, (identity[0], identity[1] + 1))
        moved = self.ledger.commit_transfer_after_rename_fsync(handle, identity)
        self.assertEqual(moved.key, ('catalog_main', 'main'))
        self.assertEqual(self.ledger.used[('restore_staging_bytes', 'bytes')], 0)
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], 4096)
        self.assertEqual(self.ledger.used[('control_bytes', 'bytes')], 0)
        self.assertEqual(moved.paths, {destination})
        self.assertFalse(handle.record[0].exists())
        owners = [r for r in self.ledger.reservations if not r.released and destination in r.paths]
        self.assertEqual(owners, [moved], 'exactly one reservation owns the one physical file')

    def test_destination_capacity_failure_leaves_the_prior_root_authoritative(self):
        tight = build_policy(self.source_map)
        tight['families']['catalog_main']['main_bytes'] = tight['families']['catalog_main']['database_bytes']
        self.ledger = physical.ReservationLedger(tight, self.root, source_map=self.source_map)
        source, held = self.staged()
        big = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', 2 * tight['families']['catalog_main']['database_bytes'])
        big.track(self.root / 'stage' / 'big')
        (self.root / 'stage' / 'big').write_bytes(b'b')
        used = dict(self.ledger.used)
        with self.assertRaises(physical.PhysicalIncomplete):
            self.ledger.prepare_transfer(big.reservation_id, 'catalog', 'catalog_main', 'main', self.root / 'out' / 'big.sqlite')
        self.assertEqual(self.ledger.used, used)
        self.assertTrue(source.exists())
        self.assertEqual(self.ledger.transfers, {})

    def test_aliases_wrong_owners_and_existing_destinations_never_create_quota(self):
        source, held = self.staged()
        destination = self.root / 'out' / 'catalog.sqlite'
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'owner'):
            self.ledger.prepare_transfer(held.reservation_id, 'verifier', 'catalog_main', 'main', destination)
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'bytes', destination)
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.prepare_transfer('0' * 64, 'catalog', 'catalog_main', 'main', destination)
        destination.write_bytes(b'x')
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'already exists'):
            self.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'main', destination)
        destination.unlink()
        os.link(source, self.root / 'stage' / 'hardlink')
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'unaliased'):
            self.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'main', destination)
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], 0)

    def test_abort_releases_only_the_destination_claim(self):
        source, held = self.staged()
        handle = self.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'main', self.root / 'out' / 'catalog.sqlite')
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'main', self.root / 'out' / 'second.sqlite')
        self.ledger.abort_transfer(handle)
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], 0)
        self.assertEqual(self.ledger.used[('restore_staging_bytes', 'bytes')], 4096)
        self.assertEqual(physical.locate_transfer(self.root, handle.record[0]) if handle.record[0].exists() else 'source', 'source')

    def test_crash_reconciliation_finds_the_one_object_or_keeps_both_charged(self):
        source, held = self.staged()
        destination = self.root / 'out' / 'catalog.sqlite'
        handle = self.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'main', destination)
        self.assertEqual(physical.locate_transfer(self.root, handle.record[0]), 'source')
        self.finish_rename(source, destination)
        self.assertEqual(physical.locate_transfer(self.root, handle.record[0]), 'destination')
        os.link(destination, source)
        with self.assertRaises(physical.PhysicalPolicyError):
            physical.locate_transfer(self.root, handle.record[0])


class DelegationTests(PhysicalBase):
    """An allocation carrying a held reservation_id moves that reservation into the grant in place.

    The release-and-reissue version has an unowned interval (the claim and the charge disappear
    before the grant re-claims them); every case below fails on it.
    """
    def held_main(self, name='out/catalog.sqlite', size=PAGE, nbytes=3 * PAGE, owner='catalog', family='catalog_main'):
        path = self.root / name
        path.write_bytes(b'm' * size)
        held = self.ledger.reserve(owner, family, 'main', nbytes)
        held.track(path)
        return path, held

    def entry(self, held, name='out/catalog.sqlite', nbytes=None, **extra):
        return dict(alloc(held.key[0], held.key[1], held.bytes if nbytes is None else nbytes, name),
                    reservation_id=held.reservation_id, **extra)

    @staticmethod
    def forbidden(*_args, **_kwargs):
        raise AssertionError('released/unclaimed while being handed on: an unowned interval')

    def test_delegation_moves_ownership_with_no_release_unclaim_or_extra_charge(self):
        path, held = self.held_main()
        used, claims = dict(self.ledger.used), dict(self.ledger.claims)
        with mock.patch.object(physical.Reservation, 'release_after_removal', self.forbidden), \
                mock.patch.object(physical.ReservationLedger, '_unclaim', self.forbidden):
            handle = self.issue([self.entry(held)])
        self.assertIn(held.reservation_id, [r.reservation_id for r in handle.reservations])
        self.assertIs(self.ledger.by_id[held.reservation_id], held)
        self.assertTrue(held.delegated)
        self.assertEqual(held.bytes, 3 * PAGE)
        self.assertEqual(self.ledger.claims[str(path.relative_to(self.root))][0], held)
        self.assertEqual({k: v for k, v in self.ledger.used.items() if k != ('control_bytes', 'bytes')},
                         {k: v for k, v in used.items() if k != ('control_bytes', 'bytes')}, 'no second charge for the held file')
        self.assertEqual(set(claims) <= set(self.ledger.claims), True)
        child = self.load(handle)
        self.assertEqual(child.limits[('catalog_main', 'main')], 3 * PAGE)

    def test_the_held_charge_cannot_be_released_by_the_parent_while_delegated(self):
        path, held = self.held_main()
        self.issue([self.entry(held)])
        path.unlink()
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'delegated'):
            held.release_after_removal()
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([self.entry(held)], [], self.binding, self.lease)  # nor delegated twice

    def test_growth_beyond_the_held_charge_deducts_only_the_difference(self):
        path, held = self.held_main(nbytes=2 * PAGE)
        before = self.ledger.remaining('catalog_main', 'main')
        self.issue([self.entry(held, nbytes=3 * PAGE)])
        self.assertEqual(held.bytes, 3 * PAGE)
        self.assertEqual(self.ledger.remaining('catalog_main', 'main'), before - PAGE)

    def test_a_failed_issue_returns_the_held_reservation_untouched(self):
        path, held = self.held_main(nbytes=2 * PAGE)
        used = dict(self.ledger.used)
        too_big = self.policy['pools']['snapshot_spool_bytes']
        taker = alloc('snapshot_spool_bytes', 'bytes', too_big, 'out/spool')
        self.ledger.limits[('snapshot_spool_bytes', 'bytes')] = too_big - 1  # the second allocation fails after the first moved
        with mock.patch.object(physical.Reservation, 'release_after_removal', self.forbidden):
            with self.assertRaises(physical.PhysicalIncomplete):
                self.ledger.issue_child_grant([self.entry(held, nbytes=3 * PAGE), taker], [], self.binding, self.lease)
        self.assertEqual((held.bytes, held.delegated, held.released), (2 * PAGE, False, False))
        self.assertEqual(self.ledger.used, used, 'the growth is rolled back with the rest')
        self.assertEqual(self.ledger.claims[str(path.relative_to(self.root))][0], held)
        self.assertEqual(self.ledger.grants, {})

    def test_only_the_exact_live_undelegated_claim_can_be_delegated(self):
        path, held = self.held_main(nbytes=3 * PAGE)
        cases = [
            self.entry(held, nbytes=2 * PAGE),                                   # shrinking would uncharge bytes
            dict(self.entry(held), resource='verifier_main', owner='verifier'),  # another family
            dict(self.entry(held), component='journal'),                         # another component
            self.entry(held, name='out/other.sqlite'),                           # another scope
            dict(self.entry(held), recursive=True),                              # a subtree over a file
        ]
        for case in cases:
            used = dict(self.ledger.used)
            with self.assertRaises(physical.PhysicalPolicyError):
                self.ledger.issue_child_grant([case], [], self.binding, self.lease)
            self.assertEqual(self.ledger.used, used)
            self.assertFalse(held.delegated)
        # a reservation with no claim at all names nothing a child could write
        bare = self.ledger.reserve('catalog', 'catalog_main', 'main', PAGE)
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([self.entry(bare)], [], self.binding, self.lease)
        released_path = self.root / 'out' / 'gone.sqlite'
        gone = self.ledger.reserve('catalog', 'catalog_main', 'main', PAGE)
        gone.track(released_path)
        gone.release_after_removal()
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([dict(self.entry(gone, name='out/gone.sqlite'))], [], self.binding, self.lease)

    def test_a_grant_descriptor_reservation_and_a_reservation_in_transfer_are_not_delegable(self):
        handle = self.issue(database_allocations())
        control = handle._control
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([dict(alloc('control_bytes', 'bytes', control.bytes, 'physical-grants/x.json'),
                                                reservation_id=control.reservation_id)], [], self.binding, self.lease)
        path = self.root / 'stage' / 'restored.sqlite'
        path.write_bytes(b'd' * 100)
        staged = self.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', PAGE)
        staged.track(path)
        self.ledger.prepare_transfer(staged.reservation_id, 'catalog', 'catalog_main', 'main', self.root / 'out' / 'moved.sqlite')
        with self.assertRaises(physical.PhysicalPolicyError):
            self.ledger.issue_child_grant([dict(alloc('restore_staging_bytes', 'bytes', PAGE, 'stage/restored.sqlite'),
                                                reservation_id=staged.reservation_id)], [], self.binding, self.lease)

    def test_without_the_id_the_same_path_is_an_alias_and_is_refused(self):
        path, held = self.held_main()
        used = dict(self.ledger.used)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'aliases an owned file'):
            self.ledger.issue_child_grant([alloc('catalog_main', 'main', 3 * PAGE, 'out/catalog.sqlite')], [], self.binding, self.lease)
        self.assertEqual(self.ledger.used, used)

    def test_held_reservation_finder_matches_only_the_exact_claims(self):
        path, held = self.held_main()
        self.assertIs(self.ledger.held_reservation(alloc('catalog_main', 'main', PAGE, 'out/catalog.sqlite')), held)
        self.assertIsNone(self.ledger.held_reservation(alloc('catalog_main', 'main', PAGE, 'out/else.sqlite')))
        self.assertIsNone(self.ledger.held_reservation(alloc('verifier_main', 'main', PAGE, 'out/catalog.sqlite')))
        self.issue([self.entry(held)])
        self.assertIsNone(self.ledger.held_reservation(alloc('catalog_main', 'main', PAGE, 'out/catalog.sqlite')))

    def test_reconcile_returns_a_surviving_file_to_the_parent_still_charged_and_releases_a_removed_one(self):
        path, held = self.held_main()
        handle = self.issue([self.entry(held)])
        report = handle.reconcile_after_reap(0)
        self.assertEqual(report['retained'], [held.reservation_id])
        self.assertEqual((held.delegated, held.released), (False, False))
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], 3 * PAGE)
        self.assertIs(self.ledger.held_reservation(alloc('catalog_main', 'main', PAGE, 'out/catalog.sqlite')), held)
        again = self.issue([self.entry(held)])  # inherited again without any intervening release
        path.unlink()
        report = again.reconcile_after_reap(0)
        self.assertEqual(report['released'], [held.reservation_id])
        self.assertEqual(self.ledger.used[('catalog_main', 'main')], 0)

    def test_a_child_over_a_delegated_file_still_cannot_exceed_its_allocation(self):
        path, held = self.held_main()
        child = self.load(self.issue([self.entry(held)]))
        with self.assertRaises(physical.PhysicalIncomplete):
            child.reserve('catalog', 'catalog_main', 'main', 3 * PAGE + 1)
        child.reserve('catalog', 'catalog_main', 'main', 3 * PAGE)


class VerifierBudgetCapTests(unittest.TestCase):
    """paged_codec.Budget physical caps, without constructing a full admitted plan."""
    def budget(self, *, database=3 * PAGE, journal=None, temp=None, grant=None, pools=None):
        budget = codec.Budget.__new__(codec.Budget)
        family = {'database_bytes': database, 'main_bytes': database, 'journal_bytes': 2 * database + MIB if journal is None else journal,
                  'temp_bytes': 2 * database if temp is None else temp}
        budget.physical_policy = {'families': {'verifier_main': family, 'verifier_continuation_scratch': dict(family)},
                                  'pools': pools or {'snapshot_spool_bytes': 8 * MIB}}
        budget.grant = grant
        return budget

    def test_database_cap_rounds_down_is_effective_and_precedes_ddl(self):
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        self.assertEqual(self.budget(database=3 * PAGE + 17).enforce_database_cap(db), 3)
        self.assertEqual(db.execute('PRAGMA max_page_count').fetchone()[0], 3)
        db.execute('CREATE TABLE t(x BLOB)')
        with self.assertRaises(sqlite3.OperationalError) as caught:
            db.execute('INSERT INTO t VALUES(zeroblob(100000))')
        self.assertTrue(codec.is_physical_exhaustion(caught.exception))
        self.assertFalse(codec.is_physical_exhaustion(sqlite3.OperationalError('no such table')))

    def test_restored_verifier_database_beyond_the_ceiling_is_incomplete_and_unwritten(self):
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        db.execute('CREATE TABLE t(x BLOB)')
        db.execute('INSERT INTO t VALUES(zeroblob(40000))')
        # SQLite's compile-time default differs by version (1073741823 before 3.33, 4294967294 after).
        default = db.execute('PRAGMA max_page_count').fetchone()[0]
        with self.assertRaises(codec.Incomplete):
            self.budget(database=3 * PAGE).enforce_database_cap(db)
        self.assertEqual(db.execute('PRAGMA max_page_count').fetchone()[0], default)

    def test_journal_and_temp_reservations_must_cover_the_ceiling(self):
        for kwargs in ({'journal': 2 * 3 * PAGE + MIB - 1}, {'temp': 2 * 3 * PAGE - 1}):
            db = sqlite3.connect(':memory:')
            self.addCleanup(db.close)
            with self.assertRaises(codec.Invalid):
                self.budget(**kwargs).enforce_database_cap(db)

    def test_check_detects_a_changed_cap_and_exhausted_pages(self):
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        budget = self.budget()
        budget.enforce_database_cap(db)
        budget.check_database(db)
        db.execute('PRAGMA max_page_count=1000')
        with self.assertRaises(codec.Invalid):
            budget.check_database(db)

    def test_child_grant_narrows_every_limit_and_missing_families_are_incomplete(self):
        calls = []
        grant = SimpleNamespace(limits={('verifier_main', 'main'): 2 * PAGE, ('verifier_main', 'journal'): 5 * PAGE,
                                        ('verifier_main', 'temp'): 4 * PAGE, ('snapshot_spool_bytes', 'bytes'): 1000},
                                _authority=lambda: calls.append(1))
        budget = self.budget(database=10 * PAGE, grant=grant)
        narrowed = budget.family_limits('verifier_main')
        self.assertEqual((narrowed['database_bytes'], narrowed['journal_bytes'], narrowed['temp_bytes']), (2 * PAGE, 5 * PAGE, 4 * PAGE))
        self.assertEqual(budget.pool_limit('snapshot_spool_bytes'), 1000)
        with self.assertRaises(codec.Incomplete):
            budget.family_limits('verifier_continuation_scratch')
        with self.assertRaises(codec.Incomplete):
            budget.pool_limit('cas_bytes')
        self.assertTrue(calls, 'grant authority is re-verified on use')
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        with self.assertRaises(codec.Incomplete):
            budget.enforce_database_cap(db, 'verifier_continuation_scratch')
        # 2 pages of grant < 2*ceiling + 1 MiB journal needs: the granted journal is too small.
        with self.assertRaises(codec.Invalid):
            budget.enforce_database_cap(db)

    def test_a_child_without_an_explicit_grant_is_refused(self):
        saved = {k: os.environ.pop(k) for k in list(os.environ) if k.startswith('PLURX_ASYNC_PHYSICAL_')}
        self.addCleanup(os.environ.update, saved)
        with self.assertRaisesRegex(codec.Incomplete, 'explicit physical grant'):
            codec.Budget._environment_grant({'source': {}, 'session_id': '', 'plan_sha256': ''}, physical, {})


if __name__ == '__main__':
    unittest.main()
