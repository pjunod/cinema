"""Finite original local observations whose native transcripts survived a move.

Each digest covers the reviewed public-source projection, scope of the actual
selection, and references to the original private command, output and custody
record. No missing interpreter measurement or checkout metadata is inferred.
"""

import re

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts


# Complete, independently reviewed binding objects, not arbitrary writer input.
REVIEWED = {
    '83b08b2f4ffd7264dff398af9d9d23c050b5273fe37be2ece880c74704c0cbd3',  # historical Rust workflow selection
    'cf12f8dbb5e1a532d3c0ebc112d2fe45100a08d6e64084b9b7081854d1872ba0',  # Store original
    '17fdcaaf5cf710d6964f1bc028c109a15eb004ded620df54f0ec4f00fa81fc9e',  # Apple original
    '2248101c1b065e446b69546883afd7f01a36b0faa168958c2a66c2fd5e0c8fb0',  # integrated paperwork
    'f70c4716844ad5849583bf4314fbcaf3ee6f9df8426b35fc814610e8f91d0bab',  # test_retained_lane_commands, 3 source members
    'a157fc4d28a2d05e1d18c448c137003ca22e29647298b54b0261bace7e22dc89',  # test_retained_lane_commands, 1 source members
    'cbfbfe60203e81f54dd38ff4a02a86a91755995d6b46a010b3e1f89ee1354a09',  # test_retained_lane_commands, 3 source members
    '323a3ac16169c499dbaf36e6cc7a7efe330b9763f9a99c5f885590d6a03c806d',  # test_retained_lane_commands, 3 source members
    'a7e374b752318c8b0a367a4b7ae481005268119232e9860f3098167954defb1b',  # test_retained_lane_commands, 1 source members
    'c037ed39491c54bfeac44c77fd1289a2397de7b206b92017fc33d08ec2bf7aad',  # test_ci_janitor, 52 source members
    '01333d54e558d8281d099fc8bbf631d706dc25c8227c017ae4966be9d21bf782',  # test_effort_history_binding, 5 source members
    '17e9ba542e13f6d73633b81871273c505ef70461c937bab3222788fe2fe5f6be',  # test_effort_history_binding, 1 source members
    '439b1a49eeea43b34fd3a8a6d180732a6cbf997a5c1e1bb4d28333b31f7d9fb3',  # test_effort_history_binding, 2 source members
    '0063d8d417827b625442192428098d37fea0a98f45061f863ba16c4bc510a28f',  # test_ui_readiness_fixture, 1 source members
    '0d231a4d7c831f7b37545e9099d6e94d04912fd6d913d9203bf6d24ab5582e56',  # test_contracts, 1 source members
    '87f55667b4d0a082c885bc35505d36d9495774276587fee3501c7b657be4b8ad',  # test_publish_qualified_main, 6 source members
    '33027b5e2567909b5b7ff09033b69ec978e02e1d18997601ead0939384686da1',  # test_workspace_rust_continuation, 3 source members
    'cf2eedb780aa78584235dd8af2c59f806f7e728082170012dd9e5718d7533cda',  # test_workspace_rust_continuation, 1 source members
    '1de39229f997878488f1712fb6c9b27a531bff832427da91d193cc846b7cc3d0',  # test_workspace_rust_continuation, 2 source members
    'abe72f8e961efe4614744e8c76fe24b3191d3ee03ee1abde66240bd2b546c1ab',  # test_workspace_rust_continuation, 1 source members
    '6617331aff82c68af33853371c136fa8a7a2a5cc75867bbfe92657e7825647d6',  # test_contracts, 2 source members
}


def observed(proof, record, raw, kind, discovery):
    from validation.nonrust_unit_cohort import blob

    origin = record['origin']
    execution = origin['execution']
    reference = execution['reviewed_observation']
    data = blob(proof, reference)
    receipts.require(retention.digest(data) in REVIEWED, 'Unknown historical Python observation')
    witness = receipts.bounded_json(data)
    source = record['source_commit']
    receipts.require(kind in {'validation', 'operations'} and origin['kind'] == 'attested-local'
                     and set(execution) == {'mode', 'kind', 'reviewed_observation'}
                     and record['coverage'] == {'mode': 'reviewed-python-observation'}
                     and witness['kind'] == kind and witness['equivalent_source'] == source
                     and record['exit_code'] is None and origin['command'] == witness['command']
                     and witness['raw_sha256'] == retention.digest(raw)
                     and witness['interpreter_binary_sha256'] is None,
                     'Local observation source, measurement or command differs')
    blob(proof, witness['original_events'])
    for path, digest in witness['source_inputs'].items():
        receipts.require(retention.digest(retention.git_bytes(source, path) or b'') == digest,
                         'Reconstructed local Python input differs')
    ids = set(witness['covered_ids'])
    receipts.require(ids and len(ids) == len(witness['covered_ids'])
                     and ids <= set(discovery(source, kind)), 'Observed case absent from reconstructed source')
    text = raw.decode()
    if witness['output_mode'] == 'named':
        pattern = r'^\w+ \((?:tests\.' + kind + r'\.)?([^()\n]+)\) \.\.\. (ok|ERROR|FAIL|skipped)(?:\s|$)'
        rows = re.findall(pattern, text, re.M)
        names = [name for name, outcome in rows if outcome == 'ok']
        receipts.require(all(names.count(name) == 1 and not any(row[0] == name and row[1] != 'ok'
                             for row in rows) for name in ids), 'Literal passing named row unavailable')
        return {name: 'pass' for name in ids}
    if witness['output_mode'] == 'paperwork-selected':
        # The pinned script runs all four modules in one native subprocess
        # and suppresses its per-test output. Keep its aggregate granularity.
        guards = ('every doc under docs/ has an index row, and no reference names a missing docs/ path',
                  'docs stay under 40 KB, and the shrink-only list only shrinks',
                  'doc **Status:** headers make only possible lifecycle claims',
                  'STATUS.md stays short and indexes the sections moved out of it')
        receipts.require(all(text.splitlines().count('[ ok ] ' + guard) == 1 for guard in guards)
                         and re.search(r'^paperwork: 14/14 ok in [0-9.]+s .*$', text, re.M)
                         and ids == set(witness['selected_ids']), 'Original paperwork guards did not complete')
        return {name: 'opaque-pass' for name in ids}
    receipts.require(witness['output_mode'] == 'opaque-selected'
                     and re.findall(r'^Ran (\d+) tests? in [0-9.]+s\n\n([^\n]+)$', text, re.M)
                     == [(str(len(witness['selected_ids'])), 'OK')]
                     and ids <= set(witness['selected_ids'])
                     and len(witness['selected_ids']) == len(set(witness['selected_ids'])),
                     'Unidentified local command outcomes or selected membership differ')
    return {name: 'opaque-pass' for name in ids}
