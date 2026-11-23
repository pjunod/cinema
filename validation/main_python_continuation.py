"""Explicit manual reuse of the existing authenticated main-Python journal.

Original per-method source/run/provenance records are preserved. A reviewed
all-path impact manifest supplements method/local-fixture fingerprints for
changed imported implementation. Unknown applicability stops before bodies.
"""
from validation import main_unit_receipts as main
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r
from validation import nextest_shard_continuation as nextest
from validation.workspace_rust_continuation import source_gate_workflow_compatible

POLICY = 'main-python-journal-continuation-v1'
MAIN_ANCESTOR_POLICY = 'main-python-journal-continuation-v2'
NODE_PREFLIGHT_POLICY = 'main-python-journal-continuation-v3'
TYPED_POLICIES = {MAIN_ANCESTOR_POLICY, NODE_PREFLIGHT_POLICY}
PRODUCER_INPUTS = {'validation/main_unit_receipts.py': 'b4112a92f0dd6acb63937c837eeb899db76ab8d8dfad8025f12e7924622ac5f4', 'validation/main_preflight_adoption.py': '7741d9786e509de6c3feb8aa69602b065ee37e1ecd6b3236388f468c3dd51b51', 'validation/python_unit_receipts.py': 'fdbcd7ef5c93a47a7ee5b3dbf5a7c5a525e9b5f721a457f3b943fd836ee58a8e', '.github/workflows/main-fast-lane.yml': '3ce991087d99bc508988d674d37ca48eb99cec34ce0d18af4fc36ec7c0878e22'}



def require_producer(prior, candidate):
    for path, digest in PRODUCER_INPUTS.items():
        before, after = retention.git_bytes(prior, path), retention.git_bytes(candidate, path)
        if path == '.github/workflows/main-fast-lane.yml':
            before, after = nextest.projected_main_workflow(before), nextest.projected_main_workflow(after)
        r.require(before is not None and retention.digest(before) == digest and before == after,
                  'Original main-Python producer/runtime changed')


def authenticate_origins(api, scope, enclosing, runs, candidate):
    """An inherited pointer is not itself evidence of the original success."""
    by_run = {}
    for row in runs:
        by_run.setdefault(row['id'], []).append(row)
    journals = {enclosing['run']: enclosing}
    for key, source in enclosing['passes'].items():
        r.require(set(source) == {'run', 'commit'}, 'Unsupported inherited Python provenance')
        run, commit = r.positive(source['run']), r.sha(source['commit'])
        if run not in journals:
            listed = main.listed_run(by_run, run)
            r.require(listed['commit_sha'] == commit, 'Inherited origin source differs from actual run')
            artifacts = api.pages(f'/actions/runs/{run}/artifacts')
            finals = [row for row in artifacts if row['name'] == main.key(scope)]
            r.require(all(row['run_id'] == run for row in finals), 'Foreign inherited journal artifact')
            journals[run] = (main.journal_from_artifacts(api, scope, listed, finals) if finals
                             else main.journal_from_log(api, scope, listed))
        origin = journals[run]
        r.require(origin['commit'] == commit and origin['run'] == run
                  and origin['passes'].get(key) == {'run': run, 'commit': commit},
                  'Inherited Python pass has no authenticated direct-origin success')
    for origin in journals.values():
        require_producer(origin['commit'], candidate)
    return {run: journal['commit'] for run, journal in journals.items()}


def main_ancestor(source, base):
    import subprocess
    return subprocess.run(['git', 'merge-base', '--is-ancestor', r.sha(source), r.sha(base)],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def authenticate_main_journals(api, items, binding, candidate, repository):
    """Scoped positive records from an actually merged current-main ancestor."""
    r.require(isinstance(items, list) and 0 < len(items) <= 4,
              'Missing or oversized main-ancestor journal inventory')
    records, origins, runs_seen, ids_seen = [], {}, set(), set()
    for item in items:
        r.require(isinstance(item, dict) and set(item) == {
            'pr', 'branch', 'source_commit', 'run', 'journal_artifact', 'ids'},
            'Unknown main-ancestor journal descriptor')
        pr, run = r.positive(item['pr']), r.positive(item['run'])
        source = r.sha(item['source_commit'])
        ids = item['ids']
        r.require(run not in runs_seen and isinstance(ids, list) and ids
                  and len(ids) == len(set(ids)) and len(ids) <= r.MAX_TESTS
                  and all(isinstance(key, str) for key in ids) and not ids_seen.intersection(ids),
                  'Duplicate or unknown scoped main-ancestor identities')
        runs_seen.add(run); ids_seen.update(ids)
        pull = api.get(f'/pulls/{pr}')
        r.require(pull['number'] == pr and pull['state'] == 'closed' and pull['merged'] is True
                  and pull['head']['repo']['id'] == pull['base']['repo']['id'] == repository
                  and pull['head']['sha'] == source
                  and pull['head']['ref'] in {item['branch'], 'refs/pull/' + str(pr) + '/head'}
                  and pull['base']['ref'] == 'main' and main_ancestor(source, binding['base_sha'])
                  and main_ancestor(r.sha(pull['merge_commit_sha']), binding['base_sha']),
                  'Supplement is not an actual merged same-repository current-main ancestor')
        scope = {'repository': repository, 'pr': pr, 'branch': item['branch'],
                 'base': 'main', 'workflow': main.WORKFLOW}
        listed = api.pages('/actions/runs', {'workflow_id': main.WORKFLOW,
                            'ref': 'refs/pull/' + str(pr) + '/head'}, 'workflow_runs')
        matches = [row for row in listed if row['id'] == run]
        r.require(len(matches) == 1 and matches[0]['commit_sha'] == source
                  and matches[0]['status'] == 'success', 'Unknown or unsuccessful actual main-ancestor run')
        main.authenticate_run(scope, matches[0])
        job = main.preflight_job(api, scope, run)
        r.require(job['status'] == 'success', 'Main-ancestor preflight did not pass')
        artifacts = api.pages(f'/actions/runs/{run}/artifacts')
        finals = [row for row in artifacts if row['id'] == item['journal_artifact']]
        r.require(len(finals) == 1 and finals[0]['run_id'] == run, 'Foreign main-ancestor journal artifact')
        journal = main.journal_from_artifacts(api, scope, matches[0], finals)
        direct = {'run': run, 'commit': source}
        r.require(journal['run'] == run and journal['commit'] == source
                  and journal['complete'] is True and not journal['fixture_errors']
                  and all(value == direct for value in journal['passes'].values())
                  and set(ids) <= set(journal['passes']),
                  'Main supplement is partial, inherited, unknown or lacks original direct successes')
        require_producer(source, candidate)
        records.append({key: journal['passes'][key] for key in ids})
        origins[run] = source
    return records, origins


def origin_impacts(origin, candidate, changes, known):
    """Writer-reviewed imported-input impact starts at the ACTUAL execution."""
    r.require(isinstance(changes, dict) and set(changes) == retention.changed(origin, candidate),
              'Every actual-origin source change needs an impact witness')
    affected = set()
    for path, witness in changes.items():
        r.require(isinstance(witness, dict) and set(witness) == {'before', 'after', 'affected', 'reason'}
                  and isinstance(witness['reason'], str) and 0 < len(witness['reason'].strip()) <= 2048,
                  'Missing actual-origin imported-input impact reason')
        before, after = retention.git_bytes(origin, path), retention.git_bytes(candidate, path)
        r.require(witness['before'] == (retention.digest(before) if before is not None else None)
                  and witness['after'] == (retention.digest(after) if after is not None else None),
                  'Actual-origin impact hashes differ')
        ids = witness['affected']
        r.require(isinstance(ids, list) and len(ids) == len(set(ids)) and set(ids) <= known,
                  'Unknown actual-origin affected Python body')
        affected.update(ids)
    return affected


def prepare(api, record, binding, candidate, repository, *, inventories=None):
    fields = {'version', 'repository', 'pr', 'branch', 'base_sha', 'source_commit',
              'prior', 'run', 'journal_artifact', 'changes', 'source_impacts'}
    version = record.get('version')
    if version in TYPED_POLICIES:
        fields.add('main_journals')
    if version == NODE_PREFLIGHT_POLICY:
        fields.update({'node_preflight', 'formal_preflight', 'apple_preflight'})
    r.require(set(record) == fields and version in TYPED_POLICIES | {POLICY}
              and record['source_commit'] == candidate and binding['schema'] == 2,
              'Unknown main-Python continuation contract')
    prior, run = r.sha(record['prior']), r.positive(record['run'])
    require_producer(prior, candidate)
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
    origins = authenticate_origins(api, scope, journal, runs, candidate)
    supplements = []
    if version in TYPED_POLICIES:
        supplements, extra_origins = authenticate_main_journals(
            api, record['main_journals'], binding, candidate, repository)
        r.require(not set(origins).intersection(extra_origins), 'Supplement duplicates original run authority')
        origins.update(extra_origins)
    if inventories is None:
        inventories = {kind: sorted(test.id() for test in r.discover(kind)) for kind in r.SUITES}
    universe = {kind + ':' + name for kind, names in inventories.items() for name in names}
    original = set(journal['passes'])
    r.require(all(set(rows) <= universe for rows in supplements),
              'Main-ancestor scoped body left actual current inventory')
    changed = retention.changed(prior, candidate)
    r.require(set(record['changes']) == changed, 'Every changed Python input needs reviewed classification')
    invalidated = set()
    for path, witness in record['changes'].items():
        r.require(path in nextest.TEST_INPUTS or path in nextest.RUNTIME_INPUTS or path in retention.PROTOCOL_PATHS,
                  'Unreviewed Python implementation input')
        r.require(set(witness) == {'before', 'after', 'affected', 'reason'}
                  and isinstance(witness['reason'], str) and witness['reason'].strip(),
                  'Missing Python source impact reason')
        before, after = retention.git_bytes(prior, path), retention.git_bytes(candidate, path)
        r.require(witness['before'] == (retention.digest(before) if before is not None else None)
                  and witness['after'] == (retention.digest(after) if after is not None else None),
                  'Python source impact hashes differ')
        if path.startswith('.github/'):
            r.require(before is not None and after is not None
                      and ((path == '.github/workflows/ci.yml' and source_gate_workflow_compatible(before, after))
                           or (path == '.github/workflows/main-fast-lane.yml'
                               and nextest.projected_main_workflow(before) == nextest.projected_main_workflow(after))),
                      'Protected Python CI workflow changed')
        affected = witness['affected']
        r.require(isinstance(affected, list) and len(affected) == len(set(affected))
                  and set(affected) <= original | universe, 'Unknown Python affected body')
        invalidated.update(affected)
    r.require(isinstance(record['source_impacts'], dict)
              and set(record['source_impacts']) == set(origins.values()),
              'Every actual Python execution source needs its own impact manifest')
    invalid_by_origin = {origin: origin_impacts(origin, candidate, witnesses, original | universe)
                         for origin, witnesses in record['source_impacts'].items()}
    applicability = r.SourceApplicability(candidate)
    applicable = {key: value for key, value in journal['passes'].items()
                  if key in universe and key not in invalidated and key not in invalid_by_origin[value['commit']]
                  and applicability(key, value)}
    for rows in supplements:
        for key, value in rows.items():
            if key not in applicable and key not in invalid_by_origin[value['commit']] and applicability(key, value):
                applicable[key] = value
    # Each supplement is evaluated from its actual execution source, rather
    # than invalidated for changes already executed before that source.
    # Existing automatic discovery may run a body when history is unavailable.
    # Explicit no-replay continuation instead stops if applicability is unknown.
    r.require(not {key for key in applicability.refusals if key in universe and key not in invalidated and key not in applicable},
              'Python pass applicability is unknown; refusing broad replay')
    applicability.finish(applicable)
    result = {'source_commit': candidate, 'authenticated_origins': origins, 'suites': {kind: {
        'inventory': names, 'uncovered': [name for name in names if kind + ':' + name not in applicable],
        'prior_discovery': [], 'retained_source_records': {key: value for key, value in applicable.items()
                                                       if key.startswith(kind + ':')}}
        for kind, names in inventories.items()}}
    if version == NODE_PREFLIGHT_POLICY:
        from validation import selected_apple_preflight, selected_formal_preflight, selected_node_preflight
        result['node_preflight'] = selected_node_preflight.prepare(
            api, record['node_preflight'], record['main_journals'], candidate, repository)
        result['formal_preflight'] = selected_formal_preflight.prepare(
            api, record['formal_preflight'], scope, journal, candidate)
        result['apple_preflight'] = selected_apple_preflight.prepare(
            api, record['apple_preflight'], scope, binding, candidate)
    return result
