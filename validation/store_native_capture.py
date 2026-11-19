"""Validate the two archived native Store continuations for promotion run 6713."""
import datetime
import json
import shlex
from pathlib import PurePosixPath

from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts

# Immutable, reviewed executor manifests. Neither the integer Cargo profile nor
# a read-only mount by itself authorizes adoption of any other native execution.
CAPTURES = {
    '5f10c0be1b457e069dad70d8ad0a6c7212d4b3eb':
        '9391b6249244b6868c4dd45c4f660b0cf721e6d1d9c90795304846560b6d36c1',
    '859ef58bc6656a090e860b3f7365b44c798266c0':
        'e58ba9e604268c893da3a8c9b2740cd5946dada64d995446b70bd005b6b7c184',
}
FIRST_SOURCE = '5f10c0be1b457e069dad70d8ad0a6c7212d4b3eb'
TARGETS = {'store_contract:1': 'store_contract', 'store_contract:2': 'schema_lineage_upgrade'}


def instant(value):
    return datetime.datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()


def bound(record, item, raw, proof):
    from validation import qualification_unit_continuation as q

    origin = record['origin']
    witness = origin['execution']
    source = record['source_commit']
    require = receipts.require
    require(item['name'] in TARGETS and origin['kind'] == 'attested-local'
            and source in CAPTURES and witness == {'mode': 'archived-store-native',
                'first_line': 1, 'capture': CAPTURES[source]}, 'Unknown archived Store execution')
    require(isinstance(proof, dict) and witness['capture'] in proof.get('logs', {}),
            'Archived Store manifest unavailable')
    encoded = retention.decoded_log(proof['logs'][witness['capture']]).encode()
    require(retention.digest(encoded) == CAPTURES[source], 'Archived Store manifest changed')
    capture = receipts.bounded_json(encoded)
    require(capture['source'] == source and capture['archive'] == q.source_archive_digest(source)
            and capture['compiler'].splitlines()[0] == q.RUSTC_VERSION,
            'Archived Store compiler or source differs')
    digest = record['log']['sha256_ref']
    require(digest in capture['logs'] and digest == retention.digest(raw.encode()),
            'Archived native Store output differs')
    case = capture['logs'][digest]
    target = TARGETS[item['name']]
    binary = capture['targets'][target]
    argv = case['argv']
    worker = capture['workers'][case['executor']]
    runtime = record['environment']['runtime']
    artifact = binary['artifact']
    fp = binary['fingerprint']
    control = capture['canonical_control']
    require(case['target'] == target and argv == [binary['path'], argv[1], '--exact', '--test-threads=1']
            and runtime['test_threads'] == 1 and runtime['rust_min_stack'] == 8388608
            and artifact['executable'] == argv[0] and artifact['target']['name'] == target
            and artifact['target']['kind'] == ['test'] and artifact['profile'] == runtime['profile']
            and artifact['features'] == record['environment']['features'] == json.loads(fp['features'])
            and fp['rustc'] == q.RUSTC_FINGERPRINT and fp['compile_kind'] == 0 and fp['rustflags'] == []
            and fp['profile'] == 5718297822855796632
            and control['artifact']['profile'] == runtime['profile']
            and control['fingerprint']['profile'] == q.TEST_PROFILE_FINGERPRINTS[0]
            and control['executor']['native_environment']['CARGO_INCREMENTAL'] == '0'
            and 'CARGO_INCREMENTAL' not in capture['incremental_source']['native_environment']
            and worker['native_environment']['RUST_MIN_STACK'] == '8388608'
            and worker['image'] == capture['image'] == control['executor']['image'],
            'Archived Store artifact, feature, runtime or profile control differs')
    require(origin['identity'] == worker['container'] and origin['command'] == shlex.join(argv)
            and case['exit'] == record['exit_code'] and retention.DIGEST.fullmatch(binary['before']),
            'Archived native Store process differs')
    if binary['after'] is None:
        # The first executable was replaced before a second hash could be
        # sampled. The original workers could only read the cache and source.
        # Each worker had exited before the next source build started.
        mounts = {m['Destination']: m for m in worker['mounts']}
        require(source == FIRST_SOURCE and target == 'store_contract'
                and worker['state']['Status'] == 'exited'
                and not mounts['/cache']['RW'] and not mounts['/src']['RW']
                and PurePosixPath(argv[0]).is_relative_to('/cache')
                and instant(worker['state']['StartedAt']) - 1 <= case['mtime']
                <= instant(worker['state']['FinishedAt'])
                < instant(capture['following_writer_started']),
                'Missing original read-only Store custody')
    else:
        require(binary['before'] == binary['after'], 'Archived Store executable changed')
    lines = raw.splitlines()
    require(lines[0] == 'COMMAND ' + json.dumps(argv, separators=(',', ':'))
            and lines[-1] == 'NATIVE_EXIT ' + str(case['exit']), 'Incomplete archived Store command')
    body = '\n'.join(lines[1:-1])
    outcomes = retention.log_outcomes(body, 'rust')
    require(record['coverage']['outcomes'] == outcomes and set(outcomes) == {argv[1]},
            'Archived Store selector or verdict differs')
    q.require_native_terminals(body, outcomes, case['exit'], origin['command'])
    return body
