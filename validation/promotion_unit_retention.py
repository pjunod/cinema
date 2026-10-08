"""Verify attested unit evidence; never execute or manufacture unit outcomes."""
from __future__ import annotations
import argparse
import base64
import hashlib
from functools import lru_cache
import json
import os
from pathlib import Path
import re
import subprocess
import zlib
import xml.etree.ElementTree as ET
import urllib.parse
import urllib.request
import urllib.error
from validation import python_unit_receipts as r

LANES = {'rust', 'apple', 'android_jvm', 'android_device'}
COHORTS = {'rust': {'core', 'daemon', 'workspace-contracts'},
           'apple': {'iphone', 'ipad', 'tvos'},
           'android_jvm': {'jvm'}, 'android_device': {'device'}}
PROTOCOL_PATHS = {
    '.github/workflows/ci.yml', 'validation/qualification.py',
    'validation/promotion_unit_retention.py',
    'tests/validation/test_promotion_unit_retention.py',
    # Separately reviewed zero-execution receipt recovery, not unit inputs.
    'validation/main_preflight_adoption.py', 'validation/main_unit_receipts.py',
    'validation/main_source_skew4513.py', 'validation/main-source-skew4513.json',
    'tests/validation/test_main_source_skew4513.py',
}
DIGEST = re.compile(r'[0-9a-f]{64}')


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


@lru_cache(maxsize=4096)
def git_bytes(commit, path):
    r.sha(commit)
    r.require(isinstance(path, str) and path and not path.startswith('/')
              and all(part not in ('', '.', '..') for part in path.split('/')),
              'Unsafe evidence source path')
    result = subprocess.run(['git', 'show', commit + ':' + path], capture_output=True)
    if result.returncode:
        return None
    return result.stdout


@lru_cache(maxsize=256)
def changed(before, after):
    return frozenset(subprocess.check_output(
        ['git', 'diff', '--name-only', r.sha(before), r.sha(after)], text=True).splitlines())


def decoded_log(item):
    r.require(set(item) in ({'base64', 'sha256'}, {'gzip_base64', 'sha256'}),
              'Unknown retained log fields')
    if 'gzip_base64' in item:
        packed = base64.b64decode(item['gzip_base64'], validate=True)
        decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
        raw = decoder.decompress(packed, r.MAX_BYTES + 1)
        r.require(decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail,
                  'Retained compressed log incomplete or oversized')
    else:
        raw = base64.b64decode(item['base64'], validate=True)
    r.require(0 < len(raw) <= r.MAX_BYTES and DIGEST.fullmatch(item['sha256'])
              and digest(raw) == item['sha256'], 'Retained log digest mismatch')
    return raw.decode('utf-8')


def log_outcomes(log, format_name):
    outcomes = {}
    log = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', log, flags=re.M)
    if format_name == 'rust':
        rows = re.findall(r'^test (.+?) \.\.\. (ok|FAILED|ignored)(?:\s|,|$)', log, re.M)
        rows = [(name, {'ok': 'pass', 'FAILED': 'fail', 'ignored': 'ignored'}[status])
                for name, status in rows]
    elif format_name == 'rust-serial':
        summaries = list(re.finditer(r'^test result: (ok|FAILED)\. (\d+) passed; '
                                     r'(\d+) failed; (\d+) ignored;', log, re.M))
        r.require(summaries, 'Serial Rust harness summary missing')
        rows = []
        cursor = 0
        for summary in summaries:
            block = log[cursor:summary.start()]
            frames = list(re.finditer(r'^test (.+?) \.\.\. ', block, re.M))
            observed = []
            for index, frame in enumerate(frames):
                end = frames[index + 1].start() if index + 1 < len(frames) else len(block)
                statuses = re.findall(r'^(ok|FAILED|ignored)(?:,.*)?$', block[frame.end():end], re.M)
                r.require(len(statuses) == 1, 'Ambiguous or missing serial Rust outcome')
                observed.append((frame[1], {'ok': 'pass', 'FAILED': 'fail', 'ignored': 'ignored'}[statuses[0]]))
            counts = tuple(sum(value == outcome for _, value in observed)
                           for outcome in ('pass', 'fail', 'ignored'))
            expected = tuple(int(summary[index]) for index in (2, 3, 4))
            r.require(counts == expected and (summary[1] == 'ok') == (expected[1] == 0),
                      'Serial Rust named outcomes contradict harness summary')
            rows.extend(observed)
            cursor = summary.end()
        r.require(not re.search(r'^test .+? \.\.\. ', log[cursor:], re.M),
                  'Serial Rust trailing execution lacks terminal summary')
    elif format_name == 'xctest':
        rows = re.findall(r"Test Case '-\[([^ ]+) ([^]]+)\]' (passed|failed|skipped)", log)
        rows = [(name + '/' + method, {'passed': 'pass', 'failed': 'fail', 'skipped': 'ignored'}[status])
                for name, method, status in rows]
    elif format_name in {'gradle-summary', 'instrumentation-summary'}:
        if format_name == 'gradle-summary':
            rows = [(name + '/' + method, 'fail') for name, method in
                    re.findall(r'^(\w+) > (\w+) FAILED$', log, re.M)]
        else:
            rows = [(name.rsplit('.', 1)[-1] + '/' + method, 'fail') for method, name in
                    re.findall(r'^Error in (\w+)\(([^)]+)\):$', log, re.M)]
    elif format_name == 'instrumentation':
        rows = []
        fields = {}
        for line in log.splitlines():
            field = re.match(r'INSTRUMENTATION_STATUS: (class|test)=(.+)', line)
            if field:
                fields[field[1]] = field[2]
            code = re.match(r'INSTRUMENTATION_STATUS_CODE: (-?\d+)$', line)
            if code and int(code[1]) != 1:
                r.require(set(fields) == {'class', 'test'}, 'Incomplete instrumented ID')
                status = int(code[1])
                r.require(status in {0, -1, -2, -3, -4}, 'Unknown instrumentation outcome')
                rows.append((fields['class'].rsplit('.', 1)[-1] + '/' + fields['test'],
                             'pass' if status == 0 else 'ignored' if status in {-3, -4} else 'fail'))
                fields = {}
    elif format_name == 'junit':
        root = ET.fromstring(log)
        r.require(root.tag in {'testsuite', 'testsuites'}, 'Unexpected JUnit root')
        rows = [(node.attrib['classname'].rsplit('.', 1)[-1] + '/' + node.attrib['name'],
                 'fail' if node.find('failure') is not None or node.find('error') is not None
                 else 'ignored' if node.find('skipped') is not None else 'pass')
                for node in root.iter('testcase')]
    else:
        raise r.ReceiptError('Unsupported actual unit log format')
    for name, outcome in rows:
        r.require(name not in outcomes, 'Duplicate contradictory unit log outcome')
        outcomes[name] = outcome
    r.require(outcomes, 'No actual unit outcomes in retained log')
    return outcomes


def validate(document, candidate, source=git_bytes, differences=changed):
    """The authenticated writer attests dependency classification and local origin.

    Every changed input is enumerated and source-hashed. Its affected IDs must
    obtain applicable passing evidence; no unknown path or outcome is adopted.
    Raw logs, environment, inventory and original failures remain in the proof.
    """
    r.require(set(document) == {'version', 'repository', 'pr', 'branch', 'base_sha',
        'source_commit', 'lanes', 'windows_waiver'}, 'Unknown retention proof fields')
    r.require(document['version'] == 1 and isinstance(document['lanes'], dict)
              and document['lanes'] and set(document['lanes']) <= LANES,
              'Invalid retained lane inventory')
    frozen = r.sha(document['source_commit'])
    protocol_paths = set(PROTOCOL_PATHS)
    r.require(differences(frozen, candidate) <= protocol_paths,
              'Candidate changed beyond reviewed retention protocol')
    adopted = {}
    for lane, suites in document['lanes'].items():
        r.require(isinstance(suites, list) and suites, 'Empty retained suite')
        suite_names = set()
        lane_count = 0
        for suite in suites:
            r.require({'name', 'ids', 'environment', 'inventory', 'records'} <= set(suite)
                      and set(suite) <= {'name', 'ids', 'environment', 'inventory', 'records',
                                        'aggregate', 'compatible_environments'},
                      'Unknown retained suite fields')
            r.require(suite['name'] not in suite_names, 'Duplicate retained suite')
            suite_names.add(suite['name'])
            ids = suite['ids']
            r.require(isinstance(ids, list) and 0 < len(ids) <= r.MAX_TESTS
                      and len(ids) == len(set(ids))
                      and all(isinstance(i, str) and 0 < len(i) <= 1024 for i in ids),
                      'Invalid retained discovered ID universe')
            inventory = decoded_log(suite['inventory'])
            r.require(all(i in inventory.splitlines() for i in ids)
                      and set(inventory.splitlines()) == set(ids),
                      'Discovered universe contradicts retained inventory')
            environment = suite['environment']
            aggregate = suite.get('aggregate')
            if aggregate is not None:
                r.require(set(aggregate) == {'total', 'failed', 'baseline_sequence'}
                          and type(aggregate['total']) is int and aggregate['total'] > 0
                          and type(aggregate['failed']) is int and 0 < aggregate['failed'] <= aggregate['total'],
                          'Invalid aggregate cohort accounting')
            r.require(isinstance(environment, dict) and
                      {'platform', 'toolchain', 'features', 'runtime'} <= environment.keys(),
                      'Incomplete unit environment witness')
            compatible = suite.get('compatible_environments', [environment])
            r.require(isinstance(compatible, list) and 0 < len(compatible) <= 16
                      and all(isinstance(item, dict) and all(item.get(key) == value
                              for key, value in environment.items()) for item in compatible),
                      'Reviewed environments do not share the unit contract')
            results = {}
            failed_ids = set()
            sequences = set()
            records = suite['records']
            r.require(isinstance(records, list) and records, 'No retained unit records')
            for record in sorted(records, key=lambda item: item['sequence']):
                r.require(set(record) == {'sequence', 'source_commit', 'environment', 'origin',
                    'changes', 'outcomes', 'log', 'format'}, 'Unknown retained outcome fields')
                sequence = record['sequence']
                r.require(type(sequence) is int and sequence > 0 and sequence not in sequences,
                          'Ambiguous unit chronology')
                sequences.add(sequence)
                prior = r.sha(record['source_commit'])
                origin = record['origin']
                r.require(isinstance(origin, dict) and set(origin) == {'kind', 'identity', 'command'}
                          and origin['kind'] in {'attested-local', 'authenticated-ci'}
                          and isinstance(origin['identity'], str) and origin['identity']
                          and isinstance(origin['command'], str) and origin['command'],
                          'Missing actual unit execution origin')
                r.require(record['environment'] in compatible,
                          'Unit environment applicability mismatch')
                changes = record['changes']
                paths = differences(prior, frozen) - protocol_paths
                r.require(isinstance(changes, dict) and set(changes) == paths,
                          'Unclassified changed unit inputs')
                invalidated = set()
                for path, witness in changes.items():
                    r.require(set(witness) == {'before', 'after', 'affected_ids'},
                              'Unknown changed input witness fields')
                    old, new = source(prior, path), source(frozen, path)
                    r.require(witness['before'] == (digest(old) if old is not None else None)
                              and witness['after'] == (digest(new) if new is not None else None),
                              'Changed input source digest mismatch')
                    affected = witness['affected_ids']
                    r.require(isinstance(affected, list) and len(affected) == len(set(affected))
                              and set(affected) <= set(ids), 'Unknown affected unit identity')
                    invalidated.update(affected)
                outcomes = record['outcomes']
                r.require(isinstance(outcomes, dict) and outcomes and set(outcomes) <= set(ids),
                          'Unknown actual unit outcome')
                raw_log = decoded_log(record['log'])
                if record['format'] == 'rust-serial':
                    r.require(re.search(r'--exact(?:\s|$)|--test-threads(?:=| )1(?:\s|$)',
                                        origin['command']),
                              'Split Rust outcomes require actual serial or exact execution')
                actual_outcomes = log_outcomes(raw_log, record['format'])
                if aggregate and sequence == aggregate['baseline_sequence']:
                    r.require(record['format'] in {'gradle-summary', 'instrumentation-summary'},
                              'Unsupported aggregate cohort source')
                    summary_pattern = (r'(\d+) tests completed, (\d+) failed'
                        if record['format'] == 'gradle-summary'
                        else r'Tests run: (\d+),\s+Failures: (\d+)')
                    summaries = re.findall(summary_pattern, raw_log)
                    r.require(summaries and set(summaries) == {(str(aggregate['total']), str(aggregate['failed']))}
                              and len(actual_outcomes) == aggregate['failed']
                              and all(value == 'fail' for value in actual_outcomes.values()),
                              'Aggregate total/failure identities contradict raw runner log')
                r.require(actual_outcomes == outcomes,
                          'Raw log exact outcomes contradict receipt')
                for test_id, outcome in outcomes.items():
                    r.require(outcome in {'pass', 'fail', 'ignored'}, 'Invalid actual outcome')
                    if outcome == 'fail':
                        failed_ids.add(test_id)
                    if test_id not in invalidated:
                        results[test_id] = outcome
            if aggregate:
                r.require(aggregate['baseline_sequence'] in sequences and
                          sum(record['sequence'] == aggregate['baseline_sequence'] for record in records) == 1,
                          'Aggregate cohort baseline execution missing')
            r.require(set(results) == set(ids) and all(value in {'pass', 'ignored'}
                      for value in results.values()), 'Incomplete or failed applicable unit universe')
            r.require(all(results[test_id] == 'pass' for test_id in failed_ids)
                      and (not aggregate or all(value == 'pass' for value in results.values())),
                      'A failed unit cannot be cleared by ignoring it')
            lane_count += (aggregate['total'] if aggregate else
                           sum(value == 'pass' for value in results.values()))
        r.require(suite_names == COHORTS[lane], 'Incomplete canonical lane cohort footprint')
        adopted[lane] = {'mode': 'retained-unit-evidence', 'passed': lane_count,
                         'suites': sorted(suite_names),
                         'accounting': 'aggregate-cohort-derived' if any('aggregate' in s for s in suites) else 'literal-per-ID'}
    waiver = document['windows_waiver']
    r.require(waiver is None or waiver == {
        'scope': 'windows_compile', 'authority': 'human-user',
        'reason': 'User explicitly waived Windows for this promotion'},
        'Unknown Windows waiver')
    if waiver:
        adopted['windows_compile'] = {'mode': 'explicit-human-waiver', **waiver}
    return adopted


def attachment_bytes(api, pr, attachment_id, attachment_uuid, expected_digest):
    r.require(re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', attachment_uuid),
              'Invalid private attachment UUID')
    item = api.get(f'/issues/{r.positive(pr)}/assets/{r.positive(attachment_id)}')
    r.require(item['id'] == attachment_id and item['uuid'] == attachment_uuid
              and item['type'] == 'attachment' and item['name'] == expected_digest + '.json'
              and type(item['size']) is int and 0 < item['size'] <= r.MAX_BYTES,
              'Private evidence attachment metadata mismatch')
    api_origin = urllib.parse.urlsplit(api.root)
    url = urllib.parse.urlsplit(item['browser_download_url'])
    server = urllib.parse.urlsplit(os.environ['GITHUB_SERVER_URL'])
    r.require(server.scheme in {'http', 'https'} and server.netloc == api_origin.netloc
              and server.scheme == api_origin.scheme and not server.username and not server.password
              and url.scheme == server.scheme and url.netloc == server.netloc
              and not url.username and not url.password and not url.query and not url.fragment
              and url.path == server.path.rstrip('/') + '/attachments/' + attachment_uuid,
              'Private evidence download origin or UUID mismatch')
    request = urllib.request.Request(url.geturl(), headers={'Authorization': 'token ' + api.token})
    try:
        with api.opener.open(request, timeout=15) as response:
            raw = response.read(r.MAX_BYTES + 1)
    except urllib.error.HTTPError as error:
        raise r.ReceiptHTTPError(error.code, '/attachments/' + attachment_uuid) from None
    r.require(len(raw) == item['size'], 'Private evidence attachment truncated or oversized')
    return raw


def runner_api(environment):
    return r.API(environment['GITHUB_API_URL'], environment['GITHUB_REPOSITORY'],
                 environment['GITHUB_TOKEN'])


def verify(api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid):
    r.require(DIGEST.fullmatch(expected_digest) and digest(raw) == expected_digest,
              'Retention evidence enrollment digest mismatch')
    document = r.bounded_json(raw)
    pull = api.get('/pulls/' + str(r.positive(binding['pull_request'])))
    repository = r.positive(api.get('')['id'])
    r.require(pull['state'] == 'open' and pull.get('merged') is False
              and pull['head']['repo']['id'] == repository == pull['base']['repo']['id']
              and pull['head']['sha'] == candidate == binding['head_sha']
              and pull['base']['sha'] == binding['base_sha']
              and pull['head']['ref'] == binding['head_ref'] and pull['base']['ref'] == 'main'
              and document['repository'] == repository and document['pr'] == pull['number']
              and document['branch'] == binding['head_ref']
              and document['base_sha'] == binding['base_sha'],
              'Retention live promotion identity mismatch')
    claim = {'repository': repository, 'pr': pull['number'], 'sha256': expected_digest,
             'attachment_id': attachment_id, 'attachment_uuid': attachment_uuid}
    attested = []
    for comment in api.pages(f"/issues/{pull['number']}/comments"):
        lines = [line.removeprefix('Promotion-Unit-Evidence: ')
                 for line in comment['body'].splitlines()
                 if line.startswith('Promotion-Unit-Evidence: ')]
        if not lines:
            continue
        r.require(len(lines) == 1, 'Ambiguous retained evidence attestation')
        if r.bounded_json(lines[0].encode()) != claim:
            continue
        r.verify_attestor(api, {'repository': repository}, comment['user'])
        attested.append(r.positive(comment['id']))
    r.require(attested, 'Retained units await authenticated writer hash attestation')
    for suites in document['lanes'].values():
        for suite in suites:
            for record in suite['records']:
                origin = record['origin']
                if origin['kind'] == 'authenticated-ci':
                    match = re.fullmatch(r'run:(\d+)/job:(\d+)', origin['identity'])
                    r.require(match is not None, 'Malformed CI execution identity')
                    run_id, job_id = map(r.positive, match.groups())
                    actual_run = api.get(f'/actions/runs/{run_id}')
                    actual_jobs = api.pages(f'/actions/runs/{run_id}/jobs')
                    jobs = [job for job in actual_jobs if job['id'] == job_id]
                    r.require(actual_run['repository']['id'] == repository
                              and actual_run['commit_sha'] == record['source_commit']
                              and len(jobs) == 1 and jobs[0]['run_id'] == run_id
                              and jobs[0]['repo_id'] == repository
                              and jobs[0]['status'] in {'success', 'failure'}
                              and type(jobs[0]['attempt']) is int and jobs[0]['attempt'] == 1,
                              'Actual CI unit execution identity mismatch')
                    raw_log = api.bytes(f'/actions/jobs/{job_id}/logs')
                    r.require(digest(raw_log) == record['log']['sha256'],
                              'Authenticated CI log contradicts retained origin')
    adopted = validate(document, candidate)
    return {'version': 1, 'candidate_sha': candidate, 'base_sha': binding['base_sha'],
            'pull_request': pull['number'], 'sha256': expected_digest,
            'attestation_comments': sorted(attested), 'lanes': adopted,
            'attachment_id': attachment_id, 'attachment_uuid': attachment_uuid}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--digest', required=True)
    parser.add_argument('--binding', required=True)
    parser.add_argument('--attachment-id', required=True, type=int)
    parser.add_argument('--attachment-uuid', required=True)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    r.require(DIGEST.fullmatch(args.digest), 'Invalid retained evidence digest')
    binding = json.loads(args.binding)
    api = runner_api(os.environ)
    raw = attachment_bytes(api, binding['pull_request'], args.attachment_id,
                           args.attachment_uuid, args.digest)
    proof = verify(api, raw, args.digest, binding, os.environ['GITHUB_SHA'],
                   args.attachment_id, args.attachment_uuid)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(proof, sort_keys=True) + '\n')
    print('retained_units=' + json.dumps(proof, separators=(',', ':')))
    for lane in (*sorted(LANES), 'windows_compile'):
        print('retain_' + lane + '=' + str(lane in proof['lanes']).lower())


if __name__ == '__main__':
    main()
