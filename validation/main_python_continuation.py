"""Explicit manual reuse of the existing authenticated main-Python journal.

Original per-method source/run/provenance records are preserved. A reviewed
all-path impact manifest supplements method/local-fixture fingerprints for
changed imported implementation. Unknown applicability stops before bodies.
"""
from validation import main_unit_receipts as main
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r
from validation import nextest_shard_continuation as nextest
from validation.workspace_rust_continuation import metadata_only_workflow

POLICY = 'main-python-journal-continuation-v1'
PRODUCER_INPUTS = {'validation/main_unit_receipts.py': 'b4112a92f0dd6acb63937c837eeb899db76ab8d8dfad8025f12e7924622ac5f4', 'validation/main_preflight_adoption.py': '7741d9786e509de6c3feb8aa69602b065ee37e1ecd6b3236388f468c3dd51b51', 'validation/python_unit_receipts.py': 'fdbcd7ef5c93a47a7ee5b3dbf5a7c5a525e9b5f721a457f3b943fd836ee58a8e', '.github/workflows/main-fast-lane.yml': '3ce991087d99bc508988d674d37ca48eb99cec34ce0d18af4fc36ec7c0878e22'}


def prepare(api, record, binding, candidate, repository):
    r.require(set(record) == {'version', 'repository', 'pr', 'branch', 'base_sha', 'source_commit',
              'prior', 'run', 'journal_artifact', 'changes'} and record['version'] == POLICY
              and record['source_commit'] == candidate and binding['schema'] == 2,
              'Unknown main-Python continuation contract')
    prior, run = r.sha(record['prior']), r.positive(record['run'])
    for path, digest in PRODUCER_INPUTS.items():
        before, after = retention.git_bytes(prior, path), retention.git_bytes(candidate, path)
        r.require(before is not None and retention.digest(before) == digest and before == after,
                  'Original main-Python producer/runtime changed')
    scope = {'repository': repository, 'pr': binding['pull_request'], 'branch': binding['head_ref'],
             'base': 'main', 'workflow': 'main-fast-lane.yml'}
    runs = api.pages('/actions/runs', {'workflow_id': main.WORKFLOW,
                     'ref': 'refs/pull/' + str(scope['pr']) + '/head'}, 'workflow_runs')
    origins = [row for row in runs if row['id'] == run]
    r.require(len(origins) == 1 and origins[0]['commit_sha'] == prior,
              'Missing original main-Python execution')
    artifacts = api.pages(f'/actions/runs/{run}/artifacts')
    matches = [row for row in artifacts if row['id'] == record['journal_artifact']]
    r.require(len(matches) == 1 and matches[0]['run_id'] == run, 'Foreign main-Python journal artifact')
    journal = main.journal_from_artifacts(api, scope, origins[0], matches)
    r.require(journal['complete'] is True and not journal['fixture_errors'],
              'Original Python journal did not complete cleanly')
    inventories = {kind: sorted(test.id() for test in r.discover(kind)) for kind in r.SUITES}
    universe = {kind + ':' + name for kind, names in inventories.items() for name in names}
    original = set(journal['passes'])
    changed = retention.changed(prior, candidate)
    r.require(set(record['changes']) == changed, 'Every changed Python input needs reviewed classification')
    invalidated = set()
    for path, witness in record['changes'].items():
        r.require(path in nextest.TEST_INPUTS or path in retention.PROTOCOL_PATHS,
                  'Unreviewed Python implementation input')
        r.require(set(witness) == {'before', 'after', 'affected', 'reason'}
                  and isinstance(witness['reason'], str) and witness['reason'].strip(),
                  'Missing Python source impact reason')
        before, after = retention.git_bytes(prior, path), retention.git_bytes(candidate, path)
        r.require(witness['before'] == (retention.digest(before) if before is not None else None)
                  and witness['after'] == (retention.digest(after) if after is not None else None),
                  'Python source impact hashes differ')
        if path.startswith('.github/'):
            r.require(path == '.github/workflows/ci.yml' and before is not None and after is not None
                      and metadata_only_workflow(before, after), 'Protected Python CI workflow changed')
        affected = witness['affected']
        r.require(isinstance(affected, list) and len(affected) == len(set(affected))
                  and set(affected) <= original | universe, 'Unknown Python affected body')
        invalidated.update(affected)
    applicability = r.SourceApplicability(candidate)
    applicable = {key: value for key, value in journal['passes'].items()
                  if key in universe and key not in invalidated and applicability(key, value)}
    # Existing automatic discovery may run a body when history is unavailable.
    # Explicit no-replay continuation instead stops if applicability is unknown.
    r.require(not {key for key in applicability.refusals if key in universe and key not in invalidated},
              'Python pass applicability is unknown; refusing broad replay')
    applicability.finish(applicable)
    return {'source_commit': candidate, 'suites': {kind: {
        'inventory': names, 'uncovered': [name for name in names if kind + ':' + name not in applicable],
        'prior_discovery': [], 'retained_source_records': {key: value for key, value in applicable.items()
                                                       if key.startswith(kind + ':')}}
        for kind, names in inventories.items()}}
