"""Authenticate complete native commands without claiming named test outcomes."""
import json
import re
import subprocess
from validation import python_unit_receipts as receipts

DEVICE_FORMAT = 'instrumentation-command'
WEB_FORMAT = 'node-preflight'
JVM_FORMAT = 'gradle-command'
FORMATS = {DEVICE_FORMAT, WEB_FORMAT, JVM_FORMAT}
JVM_COMMAND = 'BUILDX_CONFIG=target/validation/buildx make android-test'
JVM_ARGV = 'plurx-android-build ./gradlew --no-daemon testDebugUnitTest lintDebug'
DEVICE_COMMAND = 'PLURX_ANDROID_SERIAL="emulator-${EMULATOR_PORT}" make android-instrumentation-run'
WEB_SCRIPTS = ('tests/playback/player-input-contract.test.js', 'tests/web/player-dom.test.js')
WEB_COMMAND = '\n'.join('node ' + path for path in WEB_SCRIPTS)
WEB_INPUTS = (*WEB_SCRIPTS, 'tests/web/shell-source.js', 'scripts/player-contract-table',
              'tests/playback/player-input-contract.json', 'tests/playback/playback-info-fields.json',
              'tests/playback/playback-surface-contract.json',
              'docs/clients/PLAYER-INPUT-CONTRACT.md', 'docs/clients/PLAYBACK-SURFACE-CONTRACT.md',
              '.github/workflows/ci.yml')


SPECS = {
    DEVICE_FORMAT: {'lane': 'android_device', 'suite': 'device', 'command': DEVICE_COMMAND,
                    'ids': ['make android-instrumentation-run'], 'job': 'Android instrumented UI tests'},
    JVM_FORMAT: {'lane': 'android_jvm', 'suite': 'jvm', 'command': JVM_COMMAND,
                 'ids': ['make android-test'], 'job': 'Android JVM tests and lint'},
    WEB_FORMAT: {'lane': 'web_preflight', 'suite': 'node-contracts', 'command': WEB_COMMAND,
                 'ids': list(WEB_SCRIPTS), 'job': 'fast policy and contract preflight'},
}


def clean(raw):
    text = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', raw, flags=re.M)
    return re.sub(r'\x1b\[[0-9;]*[A-Za-z]', '', text)


def successful_terminal(lines):
    warning = '::warning::This Forgejo instance does not support job summaries. This job summary will be discarded.'
    receipts.require(lines.count('🏁  Job succeeded') == 1, 'Native executor success missing or repeated')
    index = lines.index('🏁  Job succeeded')
    receipts.require(lines[index + 1:] in ([], [warning]), 'Unexpected output after native executor success')
    return index


def device_summary(raw):
    text = clean(raw)
    execution = '[command]/usr/bin/sh -c ' + DEVICE_COMMAND
    lines = text.splitlines()
    successful_terminal(lines)
    summaries = list(re.finditer(r'^OK \(([1-9]\d*) tests\)$', text, re.M))
    receipts.require(lines.count(execution) == 1 and len(summaries) == 1
                     and lines.count('🏁  Job succeeded') == 1
                     and text.index(execution) < summaries[0].start() < text.index('🏁  Job succeeded')
                     and not re.search(r'^FAILURES!!!|^INSTRUMENTATION_FAILED:', text, re.M),
                     'Incomplete or contradictory native instrumentation command')
    return int(summaries[0][1])


def jvm_outcomes(raw):
    text = clean(raw)
    lines = [line.strip() for line in text.splitlines()]
    successful_terminal(lines)
    tasks = ['> Task :app:testDebugUnitTest', '> Task :app:lintDebug']
    successful = [i for i, line in enumerate(lines) if re.fullmatch(r'BUILD SUCCESSFUL in .+', line)]
    receipts.require(lines.count(JVM_ARGV) == 1 and all(lines.count(task) == 1 for task in tasks)
                     and len(successful) == 1 and lines.count('🏁  Job succeeded') == 1
                     and lines.index(JVM_ARGV) < lines.index(tasks[0])
                     < lines.index(tasks[1]) < successful[0] < lines.index('🏁  Job succeeded')
                     and not re.search(r'^BUILD FAILED|^FAILURE: Build failed', text, re.M),
                     'Native Gradle command did not execute test and lint tasks successfully')
    return {'make android-test': 'pass'}


def environment(raw, format_name):
    toolchain = None
    runtime = {DEVICE_FORMAT: 'native Android instrumentation command',
               JVM_FORMAT: 'native Gradle unit and lint command',
               WEB_FORMAT: 'native two-script Node preflight'}[format_name]
    if format_name == WEB_FORMAT:
        versions = set(re.findall(r'^node: v(\d+\.\d+\.\d+)$', clean(raw), re.M))
        receipts.require(len(versions) == 1, 'Native Node version missing or contradictory')
        toolchain = 'Node ' + versions.pop()
    return {'platform': 'linux', 'toolchain': toolchain, 'features': [], 'runtime': runtime,
            'binary_sha256': None, 'named_body_outcomes': None}


def web_outcomes(raw, commit, read_source):
    first, second = [read_source(commit, path).decode() for path in WEB_SCRIPTS]
    custom = [json.loads(name) for name in re.findall(r'^test\(("[^"\n]+"),', first, re.M)]
    passed = [json.loads(name).removeprefix('PASS ').removesuffix('\n') for name in
              re.findall(r'process\.stdout\.write\(("PASS [^"\n]+")\);', second)]
    native = [json.loads(name) for name in re.findall(r'^test\(("[^"\n]+"),', second, re.M)]
    receipts.require(custom and passed and native
                     and all(len(names) == len(set(names)) for names in (custom, passed, native)),
                     'Node source declarations are missing or ambiguous')
    lines = clean(raw).splitlines()
    successful_terminal(lines)
    starts = [i for i, line in enumerate(lines) if line == 'ok - ' + custom[0]]
    receipts.require(len(starts) == 1,
                     'Node command start or successful executor terminal missing')
    start = starts[0]
    ends = [i for i in range(start, len(lines)) if lines[i].startswith('⭐ Run Post ')]
    receipts.require(ends, 'Node command post boundary missing')
    window = lines[start:ends[0]]
    expected = ['ok - ' + name for name in custom] + ['PASS ' + name for name in passed]
    receipts.require(window[:len(expected)] == expected, 'Node custom outcomes differ from source')
    tail = window[len(expected):]
    subtests = [line.removeprefix('# Subtest: ') for line in tail if line.startswith('# Subtest: ')]
    terminals = [line for line in tail if re.match(r'^(?:not )?ok \d+ - ', line)]
    receipts.require(tail and tail[0] == 'TAP version 13' and subtests == native
                     and terminals == [f'ok {i} - {name}' for i, name in enumerate(native, 1)],
                     'Node native case names or outcomes differ from source')
    for summary in (f'1..{len(native)}', f'# tests {len(native)}', '# suites 0',
                    f'# pass {len(native)}', '# fail 0', '# cancelled 0', '# skipped 0', '# todo 0'):
        receipts.require(tail.count(summary) == 1, 'Node complete passing summary missing or repeated')
    receipts.require(all(sum(line.startswith(prefix) for line in tail) == 1
                         for prefix in ('# tests ', '# suites ', '# pass ', '# fail ',
                                        '# cancelled ', '# skipped ', '# todo ', '1..')),
                     'Contradictory Node terminal summaries')
    return {path: 'pass' for path in WEB_SCRIPTS}


def tree_id(commit, path):
    return subprocess.check_output(['git', 'rev-parse', receipts.sha(commit) + ':' + path],
                                   text=True).strip()


def record_contract(lane, suite, record, candidate, raw, read_source):
    from validation import promotion_unit_retention as retention
    device = record['format'] == DEVICE_FORMAT
    jvm = record['format'] == JVM_FORMAT
    spec = SPECS[record['format']]
    command, ids = spec['command'], spec['ids']
    receipts.require(lane == spec['lane'] and suite['name'] == spec['suite']
                     and suite['ids'] == ids and len(suite['records']) == 1
                     and 'aggregate' not in suite and 'compatible_environments' not in suite
                     and record['sequence'] == 1 and record['origin']['kind'] == 'authenticated-ci'
                     and record['origin']['command'] == command
                     and record['outcomes'] == dict.fromkeys(ids, 'pass')
                     and suite['environment'] == record['environment'] == environment(raw, record['format']),
                     'Native whole-command scope, source or chronology differs')
    prior = record['source_commit']
    if device or jvm:
        receipts.require(retention.android_device_inputs(read_source, prior)
                         == retention.android_device_inputs(read_source, candidate),
                         'Native instrumentation command consumed inputs changed')
        if jvm:
            for path in ('tests/contracts', 'tests/playback', 'tests/sharing',
                         'crates/plurx-core/tests/fixtures'):
                receipts.require(tree_id(prior, path) == tree_id(candidate, path),
                                 'Native Gradle consumed fixture tree changed: ' + path)
        workflow_raw = read_source(prior, '.github/workflows/ci.yml')
        receipts.require(workflow_raw == read_source(candidate, '.github/workflows/ci.yml'),
                         'Native Android workflow changed')
        workflow = workflow_raw.decode()
        receipts.require(len(re.findall(r'^ +' + re.escape(command) + r'$', workflow, re.M)) == (2 if device else 1),
                         'Native Android workflow command differs')
        if device:
            device_summary(raw)
        else:
            jvm_outcomes(raw)
    else:
        for path in WEB_INPUTS:
            before, after = read_source(prior, path), read_source(candidate, path)
            receipts.require(before is not None and before == after,
                             'Node preflight consumed input changed: ' + path)
        receipts.require(tree_id(prior, 'crates/plurxd/src/web')
                         == tree_id(candidate, 'crates/plurxd/src/web'),
                         'Node preflight consumed web source changed')
        workflow = read_source(prior, '.github/workflows/ci.yml').decode()
        receipts.require(all(workflow.count('            node ' + path + '\n') == 1 for path in WEB_SCRIPTS),
                         'Native Node workflow commands differ')
        web_outcomes(raw, prior, read_source)


def authenticate(run, job, record, document):
    name = SPECS[record['format']]['job']
    receipts.require(run.get('workflow_id') == 'ci.yml' and job.get('name') == name
                     and job.get('status') == 'success' and type(job.get('attempt')) is int
                     and job['attempt'] == 1 and run.get('trigger_event') == 'workflow_dispatch',
                     'Native whole-command CI workflow, job, attempt or success differs')
    event = receipts.bounded_json(run['event_payload'].encode())
    inputs = event['inputs']
    receipts.require(event['ref'] == 'refs/heads/' + document['branch']
                     and inputs['promotion_pr'] == str(document['pr'])
                     and inputs['promotion_head_sha'] == record['source_commit']
                     and run['commit_sha'] == record['source_commit'],
                     'Native commands did not run on this main-bound effort')
