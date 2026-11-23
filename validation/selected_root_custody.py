"""Prove complete source-equal Linux root custody outcomes without replaying them."""
from __future__ import annotations

import ast

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r

MODULE = 'test_source_custody_recovery.'
ROOT_CASE = MODULE + 'RecoveryHelperTests.test_root_preserves_nonroot_data_owner_for_daemon_replay'
INPUTS = ('tests/operations/test_source_custody_recovery.py', 'scripts/source-custody-recovery')


def prepare(operations, candidate):
    paths = {path: retention.git_bytes(candidate, path) for path in INPUTS}
    r.require(all(raw is not None for raw in paths.values()), 'Root recovery source unavailable')
    tests = ast.parse(paths[INPUTS[0]])
    name = ROOT_CASE.rsplit('.', 1)[-1]
    bodies = [node for node in ast.walk(tests) if isinstance(node, ast.FunctionDef) and node.name == name]
    guard = ast.dump(ast.parse('os.geteuid() == 0', mode='eval').body)
    r.require(len(bodies) == 1 and any(
        isinstance(node, ast.Call) and ast.unparse(node.func) == 'unittest.skipUnless'
        and node.args and ast.dump(node.args[0]) == guard for node in bodies[0].decorator_list
    ), 'Missing original actual-root guard in ownership test')

    names = {name for name in operations['inventory'] if name.startswith(MODULE)}
    records = {name: operations['retained_source_records'].get('operations:' + name) for name in sorted(names)}
    r.require(ROOT_CASE in records and all(isinstance(row, dict) and set(row) == {'run', 'commit'}
                                          for row in records.values()),
              'Root custody requires authenticated positives for every current method')
    root_origin = records[ROOT_CASE]
    r.require(all(row == root_origin for row in records.values()),
              'Root custody must originate from the one actual root execution domain')
    r.require(all(retention.git_bytes(root_origin['commit'], path) == raw for path, raw in paths.items()),
              'Root custody helper or case input differs at its actual origin')
    from validation.nextest_shard_continuation import IMAGE_DIGEST
    return {'image_sha256': IMAGE_DIGEST, 'source_sha256': {path: retention.digest(raw) for path, raw in paths.items()},
            'root_case': ROOT_CASE, 'records': records}



def coverage_shape(value):
    from validation.nextest_shard_continuation import IMAGE_DIGEST
    r.require(isinstance(value, dict) and set(value) == {'image_sha256', 'source_sha256', 'root_case', 'records'}
              and value['image_sha256'] == IMAGE_DIGEST and value['root_case'] == ROOT_CASE,
              'Unknown selected root custody coverage')
    sources = value['source_sha256']
    r.require(isinstance(sources, dict) and set(sources) == set(INPUTS)
              and all(isinstance(sha, str) and retention.DIGEST.fullmatch(sha) for sha in sources.values()),
              'Missing selected root custody source witnesses')
    records = value['records']
    r.require(isinstance(records, dict) and ROOT_CASE in records
              and all(isinstance(name, str) and name.startswith(MODULE)
                      and isinstance(row, dict) and set(row) == {'run', 'commit'}
                      for name, row in records.items()), 'Missing complete selected root custody outcomes')
    root_origin = records[ROOT_CASE]
    r.require(all(row == root_origin for row in records.values()),
              'Root custody coverage mixes execution domains')
    r.positive(root_origin['run'])
    r.sha(root_origin['commit'])
