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
from validation import python_unit_receipts as r

LANES = {'rust', 'apple', 'android_jvm', 'android_device', 'web_preflight'}
COHORTS = {'rust': {'core', 'daemon', 'workspace-contracts'},
           'apple': {'iphone', 'ipad', 'tvos'},
           'android_jvm': {'jvm'}, 'android_device': {'device'},
           'web_preflight': {'node-contracts'}}
PROTOCOL_PATHS = {
    '.github/workflows/ci.yml', 'validation/qualification.py',
    'validation/promotion_unit_retention.py',
    'tests/validation/test_promotion_unit_retention.py',
    # Receipt adoption machinery decides what runs, not what a unit proves.
    'validation/main_preflight_adoption.py', 'validation/main_unit_receipts.py',
}
DIGEST = re.compile(r'[0-9a-f]{64}')
RETIRED_DAEMON = {
    'source': 'e52f1e3603a0d805db0f8acb6edca8eb883b1ea6',
    'log': 'e89367b0af7ace99368854e141cc9f36e08b546e09fd9da9845ba2a0d983bd96',
    'id': 'fragindex::ride_along_tests::dropping_the_build_future_mid_pass_removes_the_stage',
    'successor': 'fragindex::ride_along_tests::standalone_index_yields_to_foreground_and_removes_partial_stage',
}

# The source is the reviewed representative of the intact incoming PR1091
# capture; its actual execution commit remains unknown in the attestation.
# Keep the two still-applicable fragindex outcomes beside the retired copyseg
# outcome instead of cropping or relabeling that original three-case log.
RETIRED_DVH1_DAEMON = {
    'source': '4ec633fb7d024e0470d15b3f24614496bd2faecd',
    'log': '3819204d8c29836a5917535e27f8b59e39ca8cf109ce53052db1c57c2d5b9ceb',
    'id': 'copyseg::tests::captured_dvh1_completeness_publishes_and_refuses_retaining_copy',
    'successor': 'copyseg::tests::captured_dvh1_rolling_options_publish_and_reject_unproven_samples',
}



# Finite adoption of the intact successful Android job in promotion 5172.
# Its summary includes assumption skips: one command passed, not 78 named bodies.
ANDROID_DEVICE_5172 = {'source': '050b6ececbc9202492eb7e0675dc994f9f65686b',
 'log': '4a58a7353d0622a549c7e2eece27609b36a262d5322efc5ad5e57510a6bcadfe',
 'identity': 'run:5172/job:51191',
 'command': 'PLURX_ANDROID_SERIAL="emulator-${EMULATOR_PORT}" make '
            'android-instrumentation-run',
 'id': 'make android-instrumentation-run',
 'inputs': {'clients/android': '9693191eb6d9ff2ea2ef76223a949c432ffcc38f',
            '.github/actions/android-emulator-ready': '40a6f5c3a3a2d2ac57336fbe91bd32d05ad6ac1c',
            'crates/plurxd/src/web/offline-reader.html': '4b1caecca544e437e0a3ae92b3fd030ce8b73d7c433bd5864eee8a31dab1fc35',
            'crates/plurxd/src/web/offline-reader.js': '88c080b27b7470da9bd37598d64d868355bfa5a84ac15f6949561e5d095bf735',
            'crates/plurxd/src/web/reader.js': 'bf983e43fc293c8a84e2dbb66d12765c29f9451e6209d8c050c3aa36566d54b8',
            'workflow-global': '57af3bcff467a05fb7865f2d22b5c759a2833e403f7c922a72590ee38b7d9494',
            'workflow-device': 'cbd67038eb4e522bac5b9332b1edcdf6d2d2ae3fc2d249c6378d2823dd18387c',
            'make-android': 'a54c7c30021d36ebb437edb54a09cabc3c72cfd417285d814c6dc2916c58e20d',
            'make-variables': 'fb1f6e257a0a2b995c756a5d77e242faa38592be9164e21eaf1bb755a574196b'},
 'environment': {'platform': 'linux-x64',
                 'toolchain': {'declared_sdk': 36,
                               'compiler_version': None,
                               'java_version': None,
                               'image_sha256': None,
                               'apk_sha256': None},
                 'features': [],
                 'runtime': {'api': 36,
                             'target': 'android-tv',
                             'arch': 'x86',
                             'profile': 'tv_1080p',
                             'app_version': 173},
                 'source_scope': {'classes': 34,
                                  'test_declarations': 78,
                                  'basis': 'source declarations; named execution outcomes '
                                           'unknown'}}}


def android_device_tree(commit, path):
    return subprocess.check_output(['git', 'rev-parse', r.sha(commit) + ':' + path],
                                   text=True).strip()


def android_device_inputs(source, commit):
    paths = ('crates/plurxd/src/web/offline-reader.html',
             'crates/plurxd/src/web/offline-reader.js', 'crates/plurxd/src/web/reader.js')
    values = {path: android_device_tree(commit, path)
              for path in ('clients/android', '.github/actions/android-emulator-ready')}
    for path in paths:
        raw = source(commit, path)
        r.require(raw is not None, 'Missing Android command input')
        values[path] = digest(raw)
    workflow = source(commit, '.github/workflows/ci.yml')
    makefile = source(commit, 'Makefile')
    r.require(workflow is not None and makefile is not None, 'Missing Android command recipe')
    workflow, makefile = workflow.decode(), makefile.decode()
    patterns = {
        'workflow-global': (workflow, r'^env:\n(.*?)(?=^concurrency:)'),
        'workflow-device': (workflow, r'^  android_device:\n.*?(?=^  [A-Za-z_]+:|\Z)'),
        'make-android': (makefile, r'^# CI pre-pulls the image.*?(?=^\.PHONY: android-instrumentation\n)'),
    }
    for name, (text, pattern) in patterns.items():
        matches = list(re.finditer(pattern, text, re.M | re.S))
        r.require(len(matches) == 1, 'Ambiguous Android command recipe')
        values[name] = digest(matches[0][0].encode())
    values['make-variables'] = digest(('\n'.join(re.findall(
        r'^ANDROID_\w+ \?=.*$', makefile, re.M)) + '\n').encode())
    return values


def android_device_command_outcomes(raw):
    policy = ANDROID_DEVICE_5172
    r.require(digest(raw.encode()) == policy['log'], 'Unknown complete Android command capture')
    text = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', raw, flags=re.M)
    execution = '[command]/usr/bin/sh -c ' + policy['command']
    r.require(text.splitlines().count(execution) == 1
              and text.splitlines().count('OK (78 tests)') == 1
              and text.splitlines().count('Time: 133.708') == 1
              and text.splitlines().count('🏁  Job succeeded') == 1
              and text.index(execution) < text.index('Time: 133.708')
              < text.index('OK (78 tests)') < text.index('🏁  Job succeeded'),
              'Incomplete or ambiguous Android command result')
    # The complete raw digest binds all 37 native heading rows and the
    # interleaved probe output. Dot rows cannot establish named outcomes.
    return {policy['id']: 'pass'}


def android_device_record_contract(lane, suite, record, candidate, source):
    policy = ANDROID_DEVICE_5172
    r.require(lane == 'android_device' and suite['name'] == 'device'
              and suite['ids'] == [policy['id']] and len(suite['records']) == 1
              and 'aggregate' not in suite and 'compatible_environments' not in suite
              and record['source_commit'] == policy['source'] and record['sequence'] == 1
              and record['origin'] == {'kind': 'authenticated-ci',
                  'identity': policy['identity'], 'command': policy['command']}
              and suite['environment'] == record['environment'] == policy['environment']
              and record['outcomes'] == {policy['id']: 'pass'},
              'Android whole-command source, scope or environment differs')
    r.require(android_device_inputs(source, policy['source']) == policy['inputs']
              and android_device_inputs(source, candidate) == policy['inputs'],
              'Android whole-command consumed inputs changed')


def authenticate_android_device_command(run_id, job_id, job):
    r.require((run_id, job_id) == (5172, 51191)
              and job.get('status') == 'success' and job.get('attempt') == 1
              and type(job.get('attempt')) is int
              and job.get('name') == 'Android instrumented UI tests',
              'Android whole-command job identity or success differs')


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


def shared_blob(document, item):
    """Version two may deduplicate embedded evidence, never fetch another source."""
    if set(item) != {'sha256_ref'}:
        return item
    value = item['sha256_ref']
    r.require(document['version'] == 2 and isinstance(value, str)
              and DIGEST.fullmatch(value), 'Unknown shared evidence reference')
    logs = document['command_units']['logs']
    r.require(isinstance(logs, dict) and value in logs
              and logs[value].get('sha256') == value, 'Shared evidence unavailable or aliased')
    decoded_log(logs[value])
    return logs[value]


def expanded_document(document, objects=None):
    """Authenticate and validate one bounded view of the original signed bytes."""
    if document.get('version') != 2:
        return document
    from validation.unit_evidence_objects import Objects
    from validation import qualification_unit_continuation as commands
    lanes = document['lanes']
    units = document['command_units']
    r.require(isinstance(lanes, dict) and lanes and set(lanes) <= LANES
              and all(isinstance(slots, list) for slots in lanes.values())
              and sum(len(slots) for slots in lanes.values()) <= 8,
              'Unknown or oversized structural suite slots')
    r.require(isinstance(units, dict) and isinstance(units.get('commands'), list)
              and len(units['commands']) <= 512, 'Oversized structural command slots')
    objects = objects if objects is not None else Objects(units['logs'])
    if units.get('version') == 2:
        units = commands.expanded_commands(units, document['source_commit'], objects)
    else:
        r.require(not any(isinstance(item, dict) and 'sha256_ref' in item
                          for item in units['commands']),
                  'Version-one command objects cannot use references')
    expanded = {}
    for lane, slots in lanes.items():
        suites = [objects.resolve(slot, True) for slot in slots]
        names = [suite.get('name') for suite in suites]
        r.require(all(isinstance(name, str) and name in COHORTS[lane] for name in names)
                  and len(names) == len(set(names)), 'Unknown or repeated expanded suite')
        expanded[lane] = suites
    return {**document, 'lanes': expanded, 'command_units': units}


def xctest_result_rows(raw):
    """Read the native legacy xcresult hierarchy; provenance is checked separately."""
    try:
        tree = r.bounded_json(raw.encode())
    except (ValueError, RecursionError) as error:
        raise r.ReceiptError('Malformed native XCTest JSON') from error
    nodes, metadata = 0, set()
    def kind(value):
        header = value.get('_type') if isinstance(value, dict) else None
        return header.get('_name') if isinstance(header, dict) else None
    def scan(value, depth=0):
        nonlocal nodes
        nodes += 1
        r.require(nodes <= 65536 and depth <= 64, 'Native XCTest traversal limit')
        if isinstance(value, dict):
            if kind(value) == 'ActionTestMetadata':
                metadata.add(id(value))
            for child in value.values():
                scan(child, depth + 1)
        elif isinstance(value, list):
            for child in value:
                scan(child, depth + 1)
    scan(tree)
    def typed(value, name):
        r.require(isinstance(value, dict) and isinstance(value.get('_type'), dict)
                  and value['_type'].get('_name') == name, 'Unexpected native XCTest node type')
        return value
    def string(value):
        typed(value, 'String')
        r.require(set(value) == {'_type', '_value'} and isinstance(value['_value'], str),
                  'Invalid native XCTest string')
        return value['_value']
    def array(value):
        typed(value, 'Array')
        r.require(set(value) <= {'_type', '_values'} and isinstance(value.get('_values', []), list),
                  'Invalid native XCTest array')
        return value.get('_values', [])
    def empty_fields(value):
        for field in ('expectedFailures', 'failureSummaries'):
            if field in value:
                r.require(not array(value[field]), 'Native XCTest expected/failure metadata requires review')
    rows, visited = [], set()
    targets = {'plurx-iOSTests': 'plurx_iOSTests', 'plurx-tvOSTests': 'plurx_tvOSTests'}
    def group(value, target, parents=()):
        r.require(len(parents) < 16, 'Native XCTest group depth')
        if kind(value) == 'ActionTestSummaryGroup':
            empty_fields(value)
            name = string(value['name'])
            identifier = string(value['identifier'])
            expected = ({'All tests', 'Selected tests'} if not parents else
                        {target + '.xctest'} if len(parents) == 1 else None)
            r.require(name == identifier and
                      ((expected is not None and name in expected) or
                       (len(parents) == 2 and re.fullmatch(r'[A-Za-z_]\w*', name)))
                      and string(value['identifierURL']) ==
                      'test://com.apple.xcode/plurx/' + target + '/' + name.replace(' ', '%20'),
                      'Invalid native XCTest enclosing group')
            for child in array(value['subtests']):
                group(child, target, parents + (name,))
            return
        typed(value, 'ActionTestMetadata')
        visited.add(id(value))
        empty_fields(value)
        identifier = string(value['identifier'])
        match = re.fullmatch(r'([A-Za-z_]\w*)/(test\w+)\(\)', identifier)
        r.require(match is not None and len(parents) == 3 and parents[-1] == match[1]
                  and string(value['name']) == match[2] + '()'
                  and string(value['identifierURL']) ==
                  'test://com.apple.xcode/plurx/' + target + '/' + match[1] + '/' + match[2],
                  'Native XCTest target, name or URI disagreement')
        status = string(value['testStatus'])
        r.require(status in {'Success', 'Failure', 'Skipped'}, 'Unknown native XCTest status')
        scalar = typed(value['failureSummariesCount'], 'Int')
        r.require(set(scalar) == {'_type', '_value'}, 'Invalid native XCTest failure scalar')
        count = scalar.get('_value')
        r.require(isinstance(count, str) and re.fullmatch(r'0|[1-9]\d{0,6}', count)
                  and (int(count) > 0) == (status == 'Failure'),
                  'Native XCTest failure count contradicts status')
        rows.append((match[1] + '/' + match[2],
                     {'Success': 'pass', 'Failure': 'fail', 'Skipped': 'ignored'}[status]))
    typed(tree, 'ActionTestPlanRunSummaries')
    for summary in array(tree['summaries']):
        typed(summary, 'ActionTestPlanRunSummary')
        for testable in array(summary['testableSummaries']):
            typed(testable, 'ActionTestableSummary')
            target = string(testable['targetName'])
            r.require(target in targets and string(testable['name']) == target
                      and string(testable['identifierURL']) == 'test://com.apple.xcode/plurx/' + target,
                      'Unknown native XCTest target')
            empty_fields(testable)
            for value in array(testable['tests']):
                group(value, target)
    r.require(metadata == visited and rows, 'Native XCTest metadata outside declared hierarchy')
    return rows


def log_outcomes(log, format_name):
    if format_name == 'instrumentation-command-5172':
        return android_device_command_outcomes(log)
    outcomes = {}
    log = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', log, flags=re.M)
    if format_name == 'node-preflight-4954':
        from validation import retained_web_preflight as web
        return web.outcomes(log, git_bytes)
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
    elif format_name == 'xctest-class':
        rows = []
        for line in log.splitlines():
            # XCTest stdout can share a line with asynchronous stderr output.
            start = 0 if line.startswith('Test Case ') else line.find("Test Case '-[")
            if start < 0:
                continue
            line = line[start:]
            native = re.fullmatch(r"Test Case '-\[([^ ]+) ([^]]+)\]' (.+)", line)
            r.require(native is not None, 'Malformed native XCTest case line')
            name, method, terminal = native.groups()
            match = re.fullmatch(r'(ReceiptTests|plurx_iOSTests|plurx_tvOSTests)\.([A-Za-z_]\w*)', name)
            r.require(match is not None and re.fullmatch(r'test\w+', method),
                      'Unreviewed native XCTest namespace or selector')
            if terminal == 'started.':
                continue
            status = re.fullmatch(r'(passed|failed|skipped) \([0-9]+(?:\.[0-9]+)? seconds\)\.', terminal)
            r.require(status is not None, 'Unreviewed native XCTest terminal state')
            rows.append((match[2] + '/' + method,
                         {'passed': 'pass', 'failed': 'fail', 'skipped': 'ignored'}[status[1]]))
    elif format_name == 'xctest-result':
        rows = xctest_result_rows(log)
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
        if format_name in {'rust', 'rust-serial'}:
            name = name.removesuffix(' - should panic')
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
    keys = {'version', 'repository', 'pr', 'branch', 'base_sha', 'source_commit', 'lanes', 'windows_waiver'}
    if document.get('version') == 2:
        keys.add('command_units')
    r.require(set(document) == keys, 'Unknown retention proof fields')
    r.require(document['version'] in {1, 2} and isinstance(document['lanes'], dict)
              and document['lanes'] and set(document['lanes']) <= LANES,
              'Invalid retained lane inventory')
    frozen = r.sha(document['source_commit'])
    if document['version'] == 2:
        r.require(frozen == candidate, 'Command adoption binds the exact candidate')
        document = expanded_document(document)
    protocol_paths = set(PROTOCOL_PATHS)
    r.require(differences(frozen, candidate) <= protocol_paths,
              'Candidate changed beyond reviewed retention protocol')
    adopted = {}
    for lane, suites in document['lanes'].items():
        r.require(isinstance(suites, list) and suites, 'Empty retained suite')
        suite_names = set()
        lane_count = 0
        lane_retired = []
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
            passing_sequences = {}
            failure_sequences = {}
            retired_records = []
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
                if set(changes) == {'sha256_ref'}:
                    manifest = r.bounded_json(decoded_log(shared_blob(document, changes)).encode())
                    r.require(set(manifest) == {'source_commit', 'candidate', 'changes'}
                              and manifest['source_commit'] == prior and manifest['candidate'] == frozen,
                              'Shared applicability manifest source or candidate differs')
                    changes = manifest['changes']
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
                r.require(isinstance(outcomes, dict) and outcomes, 'Unknown actual unit outcome')
                r.require(record['format'] not in {'xctest-class', 'xctest-result'} or lane == 'apple',
                          'Native XCTest format outside Apple lane')
                raw_log = decoded_log(shared_blob(document, record['log']))
                if record['format'] == 'rust-serial':
                    r.require(re.search(r'--exact(?:\s|$)|--test-threads(?:=| )1(?:\s|$)',
                                        origin['command']),
                              'Split Rust outcomes require actual serial or exact execution')
                if lane == 'web_preflight' or record['format'] == 'node-preflight-4954':
                    from validation import retained_web_preflight as web
                    web.record_contract(lane, suite, record, frozen, raw_log, git_bytes, digest)
                if record['format'] == 'instrumentation-command-5172':
                    android_device_record_contract(lane, suite, record, frozen, source)
                actual_outcomes = log_outcomes(raw_log, record['format'])
                extras = set(outcomes) - set(ids)
                retired = None
                if extras:
                    r.require(document['version'] == 2 and lane == 'rust'
                              and suite['name'] == 'daemon' and record['format'] == 'rust',
                              'Unknown or failed retired original outcome')
                    # Resolve these finite policies here so existing controls
                    # still exercise the policy they patch at validation time.
                    matches = [policy for policy in (RETIRED_DAEMON, RETIRED_DVH1_DAEMON)
                               if prior == policy['source']
                               and digest(raw_log.encode()) == policy['log']
                               and extras == {policy['id']}
                               and outcomes[policy['id']] == 'pass'
                               and policy['successor'] in ids]
                    r.require(len(matches) == 1, 'Unknown or failed retired original outcome')
                    policy = matches[0]
                    r.require(not any(row['source_commit'] == policy['source']
                                      and row['log_sha256'] == policy['log']
                                      and row['test_id'] == policy['id'] for row in retired_records),
                              'Duplicate retired original execution')
                    retired = policy['id']
                    retired_records.append({'sequence': sequence, 'source_commit': prior,
                        'log_sha256': policy['log'], 'test_id': retired,
                        'outcome': 'pass', 'successor': policy['successor']})
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
                        failure_sequences[test_id] = sequence
                    if test_id == retired:
                        continue
                    if test_id not in invalidated:
                        results[test_id] = outcome
                        if outcome == 'pass':
                            passing_sequences[test_id] = sequence
            if aggregate:
                r.require(aggregate['baseline_sequence'] in sequences and
                          sum(record['sequence'] == aggregate['baseline_sequence'] for record in records) == 1,
                          'Aggregate cohort baseline execution missing')
            r.require(set(results) == set(ids) and all(value in {'pass', 'ignored'}
                      for value in results.values()), 'Incomplete or failed applicable unit universe')
            r.require(all(results[test_id] == 'pass' for test_id in failed_ids)
                      and (not aggregate or all(value == 'pass' for value in results.values())),
                      'A failed unit cannot be cleared by ignoring it')
            r.require(all(results.get(item['successor']) == 'pass'
                          and passing_sequences.get(item['successor'], 0)
                          > max(item['sequence'], failure_sequences.get(item['successor'], 0))
                          for item in retired_records),
                      'Retired original outcome needs a later applicable successor pass')
            lane_retired.extend(retired_records)
            lane_count += (aggregate['total'] if aggregate else
                           sum(value == 'pass' for value in results.values()))
        r.require(suite_names == COHORTS[lane], 'Incomplete canonical lane cohort footprint')
        adopted[lane] = {'mode': 'retained-unit-evidence', 'passed': lane_count,
                         'suites': sorted(suite_names),
                         'accounting': 'aggregate-cohort-derived' if any('aggregate' in s for s in suites) else 'literal-per-ID'}
        if any(record['format'] == 'instrumentation-command-5172'
               for suite in suites for record in suite['records']):
            adopted[lane].update(accounting='whole-command-success', commands_passed=1,
                reported_tests=78, named_body_outcomes=None, passed_body_count=None,
                ignored_body_count=None)
        if lane_retired:
            adopted[lane]['retired_original_passes'] = lane_retired
    waiver = document['windows_waiver']
    r.require(waiver is None or waiver == {
        'scope': 'windows_compile', 'authority': 'human-user',
        'reason': 'User explicitly waived Windows for this promotion'},
        'Unknown Windows waiver')
    if waiver:
        adopted['windows_compile'] = {'mode': 'explicit-human-waiver', **waiver}
    if document['version'] == 2:
        from validation import qualification_unit_continuation as commands
        adopted_commands = commands.validate(document['command_units'], candidate, existing=document['lanes'])
        return adopted, adopted_commands
    return adopted


def comment_evidence_bytes(api, pr, attachment_id, attachment_uuid, expected_digest):
    """Read private, writer-authenticated chunks through the runner's issue API."""
    repository = r.positive(api.get('')['id'])
    binding = {'version': 1, 'repository': repository, 'pr': pr,
               'attachment_id': attachment_id, 'attachment_uuid': attachment_uuid,
               'sha256': expected_digest}
    chunks = {}
    count = None
    writers = set()
    for comment in api.pages(f'/issues/{pr}/comments'):
        lines = [line.removeprefix('Promotion-Unit-Evidence-Chunk: ')
                 for line in comment['body'].splitlines()
                 if line.startswith('Promotion-Unit-Evidence-Chunk: ')]
        if not lines:
            continue
        r.require(len(lines) == 1, 'Ambiguous private evidence chunk comment')
        claim = r.bounded_json(lines[0].encode())
        if any(claim.get(key) != value for key, value in binding.items()):
            continue
        r.require(len(comment['body'].encode()) <= 65536
                  and set(claim) == set(binding) | {'index', 'count', 'gzip_base64'},
                  'Unknown or oversized private evidence chunk fields')
        writer = (r.positive(comment['user']['id']), comment['user']['login'])
        if writer not in writers:
            r.verify_attestor(api, {'repository': repository}, comment['user'])
            writers.add(writer)
        index, total, packed = claim['index'], claim['count'], claim['gzip_base64']
        r.require(type(total) is int and 0 < total <= 128
                  and type(index) is int and 0 <= index < total
                  and isinstance(packed, str) and 0 < len(packed) <= 60000
                  and (count is None or count == total) and index not in chunks,
                  'Duplicate, conflicting or invalid private evidence chunk')
        count = total
        chunks[index] = packed
        r.require(sum(len(value) for value in chunks.values()) <= r.MAX_BYTES,
                  'Private evidence encoded payload oversized')
    r.require(count is not None and set(chunks) == set(range(count)),
              'Private evidence comment chunks missing or incomplete')
    packed = ''.join(chunks[index] for index in range(count))
    return decoded_log({'gzip_base64': packed, 'sha256': expected_digest}).encode('utf-8')


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
    raw = comment_evidence_bytes(api, pr, attachment_id, attachment_uuid, expected_digest)
    r.require(len(raw) == item['size'], 'Private evidence attachment truncated or oversized')
    return raw


def runner_api(environment):
    return r.API(environment['GITHUB_API_URL'], environment['GITHUB_REPOSITORY'],
                 environment['GITHUB_TOKEN'])


def verify_identity(api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid):
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
    return document, pull, repository, attested


def verify(api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid,
           *, objects=None, live_logs=None):
    document, pull, repository, attested = verify_identity(
        api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid)
    document = expanded_document(document) if objects is None else expanded_document(document, objects)
    from validation import qualification_unit_continuation as commands
    from validation.unit_evidence_objects import LiveLogs
    runs, jobs_by_run = {}, {}
    live_logs = live_logs if live_logs is not None else LiveLogs()
    for lane, suites in document['lanes'].items():
        for suite in suites:
            for record in suite['records']:
                origin = record['origin']
                if origin['kind'] == 'authenticated-ci':
                    run_id, job_id = commands.authenticated_ids(origin['identity'])
                    if run_id not in runs:
                        runs[run_id] = api.get(f'/actions/runs/{run_id}')
                        jobs_by_run[run_id] = api.pages(f'/actions/runs/{run_id}/jobs')
                    actual_run = runs[run_id]
                    actual_jobs = jobs_by_run[run_id]
                    jobs = [job for job in actual_jobs if job['id'] == job_id]
                    r.require(actual_run['repository']['id'] == repository
                              and actual_run['commit_sha'] == record['source_commit']
                              and len(jobs) == 1 and jobs[0]['run_id'] == run_id
                              and jobs[0]['repo_id'] == repository
                              and jobs[0]['status'] in {'success', 'failure'}
                              and type(jobs[0]['attempt']) is int and jobs[0]['attempt'] == 1,
                              'Actual CI unit execution identity mismatch')
                    if lane == 'web_preflight':
                        from validation import retained_web_preflight as web
                        web.authenticate(run_id, job_id, jobs[0], git_bytes)
                    if record['format'] == 'instrumentation-command-5172':
                        authenticate_android_device_command(run_id, job_id, jobs[0])
                    raw_log = live_logs.get(api, run_id, job_id)
                    r.require(digest(raw_log) == shared_blob(document, record['log'])['sha256'],
                              'Authenticated CI log contradicts retained origin')
    command_units = None
    if document.get('version') == 2:
        from validation import qualification_unit_continuation as commands
        commands.authenticate(api, document['command_units'], document, repository, live_logs=live_logs)
        adopted, command_units = validate(document, candidate)
    else:
        adopted = validate(document, candidate)
    proof = {'version': 1, 'candidate_sha': candidate, 'base_sha': binding['base_sha'],
            'pull_request': pull['number'], 'sha256': expected_digest,
            'attestation_comments': sorted(attested), 'lanes': adopted,
            'attachment_id': attachment_id, 'attachment_uuid': attachment_uuid}
    if command_units is not None:
        proof['command_units'] = command_units
    return proof


def verify_enrollment(api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid):
    document = r.bounded_json(raw)
    if document.get('version') == 3:
        from validation.promotion_unit_bundle import verify_bundle
        return verify_bundle(api, raw, expected_digest, binding, candidate,
                             attachment_id, attachment_uuid)
    return verify(api, raw, expected_digest, binding, candidate, attachment_id, attachment_uuid)


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
    proof = verify_enrollment(api, raw, args.digest, binding, os.environ['GITHUB_SHA'],
                   args.attachment_id, args.attachment_uuid)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(proof, sort_keys=True) + '\n')
    print('retained_units=' + json.dumps(proof, separators=(',', ':')))
    for lane in sorted(LANES):
        print('retain_' + lane + '=' + str(lane in proof['lanes']).lower())
    for kind in ['validation', 'operations', 'store_contract', 'wal', 'android_jvm','topology','transport']:
        print('retain_' + kind + '_units=' + str(kind in proof.get('command_units', {})).lower())


if __name__ == '__main__':
    main()
