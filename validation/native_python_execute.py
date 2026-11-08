"""Run only current-source Python cases missing authenticated prior coverage."""

import os
import subprocess
import sys
import unittest
from pathlib import Path

from validation import nonrust_unit_cohort as cohort
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation import qualification_unit_continuation as commands


SELECTION = Path('target/validation/python-unit-selection.json')
KINDS = {'validation', 'operations'}


def prepare(api, descriptor, binding, candidate):
    receipts.require(set(descriptor) == {'sha256', 'attachment_id', 'attachment_uuid'},
                     'Invalid Python evidence descriptor')
    attachment = receipts.positive(descriptor['attachment_id'])
    sha, uuid = descriptor['sha256'], descriptor['attachment_uuid']
    raw = retention.attachment_bytes(api, binding['pull_request'], attachment, uuid, sha)
    document, _, repository, _ = retention.verify_identity(api, raw, sha, binding, candidate, attachment, uuid)
    receipts.require(set(document) == {'version', 'repository', 'pr', 'branch', 'base_sha',
                                       'source_commit', 'command_units'}
                     and document['version'] == 'native-python-continuation-v1'
                     and document['source_commit'] == candidate, 'Python evidence belongs to another source')
    proof = commands.expanded_commands(document['command_units'], candidate)
    items = proof['commands']
    receipts.require(len(items) == 2 and {item['name'] for item in items} == KINDS
                     and all(item['invocation'] == commands.COMMANDS[item['name']]
                             and item['protocol'] == 'source-cohort-v1'
                             and any(record['origin']['execution'].get('native_discovery') is True
                                     for record in item['records']) for item in items),
                     'Both native Python command origins are required')
    commands.authenticate(api, {**proof, 'version': 1}, document, repository)
    plans = {item['name']: cohort.validate(item, proof, candidate, commands.frozen_python_ids,
            commands.changed_inputs, commands.clean_log, commands.source_archive_digest,
            selection=True) for item in items}
    return {'source_commit': candidate, 'evidence_sha256': sha, 'suites': plans}


def run_selection(plan, tests):
    ids = [test.id() for test in tests]
    selected = set(plan['uncovered'])
    receipts.require(sorted(ids) == plan['inventory'] and len(ids) == len(set(ids))
                     and len(selected) == len(plan['uncovered']) and selected <= set(ids),
                     'Native test discovery differs from authenticated selection')
    print(f'Current discovery {len(ids)}, retained {len(ids) - len(selected)}, executing {len(selected)}',
          flush=True)
    for summary in plan['prior_discovery']:
        print('Original discovery', summary['native_discovered'], 'with', summary['native_skipped'],
              'skipped; individual result names unavailable', flush=True)
    if selected:
        suite = unittest.TestSuite(test for test in tests if test.id() in selected)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        receipts.require(result.wasSuccessful() and result.testsRun == len(selected),
                         'Selected Python cases did not complete successfully')


def main(kind):
    candidate = receipts.sha(os.environ['GITHUB_SHA'])
    receipts.require(subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == candidate,
                     'Python continuation checkout differs')
    if kind == 'prepare':
        receipts.require(all(os.environ.get('RETAIN_' + name.upper() + '_UNITS') != 'true' for name in KINDS),
                         'Conflicting whole and partial Python receipt')
        plan = prepare(retention.runner_api(os.environ),
                       receipts.bounded_json(os.environ['PYTHON_UNIT_EVIDENCE'].encode()),
                       receipts.bounded_json(os.environ['PROMOTION_BINDING'].encode()), candidate)
        receipts.atomic_json(SELECTION, plan)
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            print('selection_sha256=' + retention.digest(SELECTION.read_bytes()), file=output)
        return
    receipts.require(kind in KINDS, 'Unknown native Python suite')
    raw = SELECTION.read_bytes()
    receipts.require(retention.digest(raw) == os.environ['PYTHON_SELECTION_SHA256'],
                     'Python selection changed after authentication')
    plan = receipts.bounded_json(raw)
    receipts.require(plan['source_commit'] == candidate, 'Python selection belongs to another source')
    run_selection(plan['suites'][kind], receipts.discover(kind))


if __name__ == '__main__':
    main(sys.argv[1])
