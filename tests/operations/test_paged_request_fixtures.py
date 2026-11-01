"""Cross-language request shapes, pinned without running a quantum.

The controller (Python) writes private requests that the Rust catalog (`QuantumRequest`) and the
checker (Python) read. The shapes are closed on both sides, so they must change together. The
checked-in fixtures under fixtures/paged-requests/ are the one shared statement of them:

  * these fixtures assert the controller's real builders generate exactly the fixture bytes from
    fixed inputs (a builder that adds, drops or renames a field fails here), and
  * the Rust unit tests `controller_request_fixtures_parse_with_the_closed_request_shape` and
    `auxiliary_directories_match_the_controller_fixture` (async_driver_catalog.rs) parse the very
    same files with the real `QuantumRequest::read_shape`, so a field the Rust parser does not accept
    fails there.

The nested WorkUnit/ResourcePlan/Checkpoint records are opaque stand-ins here: their own schemas
are exercised by the record fixtures; this pins the request envelope and its paths.
"""
import json
import re
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / 'fixtures' / 'paged-requests'
PACKAGE = ROOT / 'verification' / 'async-driver-refinement'
sys.path.insert(0, str(PACKAGE))
import paged_codec as codec  # noqa: E402

from validation import async_refinement_provenance as provenance  # noqa: E402
from validation import async_refinement_session as session  # noqa: E402

SESSION = '00000000-0000-0000-0000-000000000001'
OUTPUT = Path('/owned') / SESSION / 'run-4242'
ACTIVATION = OUTPUT / 'activation'
PLAN = {'schema_version': 6, 'kind': 'ResourcePlan', 'record': {'plan_sha256': '1' * 64}}
IMAGE = {'schema_version': 6, 'kind': 'InitialImage', 'record': {'image_sha256': '2' * 64}}
PRIOR = {'schema_version': 6, 'kind': 'Checkpoint', 'record': {'checkpoint': 'prior'}}
ACCEPTED = {'schema_version': 6, 'kind': 'Checkpoint', 'record': {'checkpoint': 'accepted'}}
UNIT = {'repository': 'noirr/plurx', 'pr': 1263, 'event_base': 'b' * 40, 'workflow_id': 'async-proof-session.yml',
        'event_name': 'workflow_dispatch', 'mode': 'unit', 'run': 4242, 'job': 7, 'task': 8, 'attempt': 1,
        'stage': 'expand', 'unit_ordinal': 0}
REGISTRATION = {'source_files': {'verification/async-driver-refinement/PAGED-CONTRACT.md': '3' * 64},
                'runtime': {'java': {'path': '/opt/java/bin/java', 'sha256': '4' * 64},
                            'tlc': {'path': '/opt/tla/tla2tools.jar', 'sha256': '5' * 64}}}
STATE = {'resource_plan': PLAN, 'initial_image': IMAGE, 'checkpoint': PRIOR, 'next_dispatch_ordinal': 2,
         'verifier_progress_sha256': '6' * 64}


def fixture(name):
    return (FIXTURES / name).read_bytes()


class RequestFixtureTests(unittest.TestCase):
    def test_the_unit_execute_builder_generates_exactly_the_fixture_the_rust_parser_reads(self):
        request = session.unit_execute_request(STATE, {'schema_version': 6, 'kind': 'WorkUnit', 'record': UNIT},
                                               REGISTRATION, OUTPUT, 1800.5, 'execute')
        self.assertEqual(provenance.canonical(request), fixture('execute-request.json'))

    def test_the_catalog_restore_builder_generates_exactly_the_fixture_the_rust_parser_reads(self):
        origin = session.execution_origin(STATE, UNIT, 'execute')
        staged = session.restore_generation(ACTIVATION) / 'catalog.staged.sqlite'
        verifier_staged = session.restore_generation(ACTIVATION) / 'verifier.staged.sqlite'
        with mock.patch.object(session.time, 'monotonic', return_value=100.25):
            base = session.restore_request_base(STATE, REGISTRATION, UNIT, origin, IMAGE, ACCEPTED,
                                                OUTPUT / 'dependencies.sqlite',
                                                OUTPUT / 'evidence' / 'driver-graph.json', ACTIVATION)
        request = session.catalog_restore_request(base, staged, verifier_staged, ACCEPTED,
                                                  OUTPUT / 'dependencies.sqlite', ACTIVATION)
        self.assertEqual(provenance.canonical(request), fixture('restore-request.json'))
        # The catalog validator reads the verifier the same restore staged, never a not-yet-activated name.
        self.assertEqual(request['verifier_work_db'], str(verifier_staged))
        self.assertNotEqual(Path(request['verifier_work_db']).name, 'verifier.sqlite')

    def test_the_auxiliary_directory_is_derived_identically_by_the_controller_and_the_checker(self):
        pairs = json.loads(fixture('auxiliary-directories.json'))
        self.assertTrue(pairs)
        for pair in pairs:
            expected = Path(pair['auxiliary_directory'])
            self.assertEqual(session.auxiliary_directory(pair['output_dir']), expected)
            self.assertEqual(codec.auxiliary_directory(pair['output_dir']), expected)
            self.assertEqual(expected.parent, Path(pair['output_dir']).parent)  # a sibling, never inside
        self.assertEqual(session.AUXILIARY_SUFFIX, codec.AUXILIARY_SUFFIX)

    def test_the_fixture_fields_are_exactly_the_rust_closed_request_fields(self):
        source = (ROOT / 'crates/plurx-core/src/store/async_driver_catalog.rs').read_text()
        listed = re.search(r'pub fn read_shape.*?let base = \[(.*?)\];', source, re.S)
        self.assertIsNotNone(listed)
        rust = set(re.findall(r'"([a-z_0-9]+)"', listed.group(1)))
        for name in ('execute-request.json', 'restore-request.json'):
            self.assertEqual(set(json.loads(fixture(name))), rust, name)
        suffix = re.search(r'pub const AUXILIARY_SUFFIX: &str = "([^"]+)";', source)
        self.assertEqual(suffix.group(1), session.AUXILIARY_SUFFIX)

    def test_the_rust_and_python_runtime_pool_constants_are_the_same_policy(self):
        source = (ROOT / 'crates/plurx-core/src/store/async_driver_catalog.rs').read_text()

        def rust(name):
            match = re.search(r'pub const %s: u64 = ([0-9 *]+);' % name, source)
            self.assertIsNotNone(match, name)
            value = 1
            for factor in match.group(1).split('*'):
                value *= int(factor)
            return value
        physical = codec.physical_policy_module()
        self.assertEqual(rust('UNIT_LOG_RESERVE'), physical.UNIT_LOG_RESERVE)
        self.assertEqual(rust('CONTROL_OUTPUT_BYTES'), physical.CONTROL_OUTPUT_BYTES)
        self.assertEqual(rust('CONTROL_OUTPUT_DIRECTORIES'), physical.CONTROL_OUTPUT_DIRECTORIES)

    def test_no_producer_or_consumer_still_names_a_writer_file_inside_the_output_directory(self):
        # The named single-file claims moved beside the output tree; a path joined onto the output
        # directory would put an unclaimed file back inside the one recursive grant.
        checker = (PACKAGE / 'paged_checker.py').read_text()
        self.assertNotIn("output/'verifier-backup.sqlite'", checker)
        product = (ROOT / 'crates/plurx-core/src/store/async_driver_product.rs').read_text()
        self.assertNotIn('request.output_dir.join("catalog-backup.sqlite")', product)
        catalog = (ROOT / 'crates/plurx-core/src/store/async_driver_catalog.rs').read_text()
        self.assertNotIn('output.join("manifest-closure.sqlite")', catalog)
        controller = Path(session.__file__).read_text()
        self.assertNotIn("Path(output)/'verifier-backup.sqlite'", controller)
        self.assertNotIn("quantum/'catalog-backup.sqlite'", controller)


if __name__ == '__main__':
    unittest.main()
