"""Authenticate the three native XCTest invocations in the original full Apple job."""
import hashlib
import re

from validation import python_unit_receipts as receipts

SOURCE = '65905cc9ae76d428374d5a383246ed1d7d12d5c2'
JOB_LOG_SHA256 = 'fe34501de8d56b3370b35eecccbcd99a92109444e4a386c280ddbc20d6afdfac'
FORMAT = 'xctest-full-job-6713'
SIMULATORS = {
    'iphone': ('iPhone', 'iOS', 'iPhone-17-Pro', 1264, 0),
    'ipad': ('iPad', 'iOS', 'iPad-Pro-13-inch-M4-8GB', 1264, 1),
    'tvos': ('Apple TV', 'tvOS', 'Apple-TV-4K-3rd-generation-4K', 1252, 0),
}


def invocation_block(text, command):
    """A successful terminal must belong to this one unfiltered invocation."""
    lines = text.splitlines()
    anchors = [index for index, line in enumerate(lines) if line.strip() == command]
    receipts.require(len(anchors) == 1, 'Missing or duplicate Apple invocation')
    start = anchors[0]
    receipts.require(start > 0 and lines[start - 1] == 'Command line invocation:',
                     'Apple invocation has no original command boundary')
    stop = next((i for i in range(start + 1, len(lines))
                 if lines[i] == 'Command line invocation:'), len(lines))
    terminals = [(i, lines[i]) for i in range(start, stop)
                 if re.fullmatch(r'\*\* TEST EXECUTE [A-Z]+ \*\*', lines[i])]
    receipts.require(len(terminals) == 1 and terminals[0][1] == '** TEST EXECUTE SUCCEEDED **',
                     'Apple invocation did not reach its own successful terminal')
    return '\n'.join(lines[start:terminals[0][0] + 1])


def require_same_execution(candidate, paths, read_source):
    from validation.retained_lane_commands import scoped_workflow

    require = receipts.require
    require(not any(path.startswith('clients/apple/') for path in paths)
            and read_source(SOURCE, 'Makefile') == read_source(candidate, 'Makefile'),
            'Apple project or build recipe changed')
    workflows = [scoped_workflow(read_source(commit, '.github/workflows/ci.yml'), 'apple')
                 for commit in (SOURCE, candidate)]
    require(workflows[0] == workflows[1], 'Apple executor or workflow environment changed')


def outcomes(lane, suite, record, raw):
    from validation import promotion_unit_retention as retention

    require = receipts.require
    require(lane == 'apple' and suite['name'] in SIMULATORS and record['source_commit'] == SOURCE
            and record['origin']['kind'] == 'authenticated-ci'
            and record['origin']['identity'] == 'run:6713/job:63638'
            and hashlib.sha256(raw.encode()).hexdigest() == JOB_LOG_SHA256,
            'Unknown Apple source, suite or whole CI job bytes')
    text = re.sub(r'(?m)^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', raw)
    label, os_name, model, total, skipped = SIMULATORS[suite['name']]
    boot_ids = re.findall(r'^Monitoring boot status for plurx CI ' + re.escape(label)
                          + r' 6713-1 \(([0-9A-F-]{36})\)\.$', text, re.M)
    require(len(boot_ids) == 1 and '\nXcode 27.2\nBuild version 27B52\n' in text,
            'Apple simulator or native Xcode installation differs')
    boot = boot_ids[0]
    command = (f'/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild '
               f'-project plurx.xcodeproj -scheme plurx-{os_name} '
               f'-destination "platform={os_name} Simulator,id={boot}" '
               '-derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=NO test-without-building')
    require(record['origin']['command'] == command and record['environment'] == {
        'platform': os_name + ' Simulator', 'toolchain': 'Xcode 27.2 (27B52)',
        'features': 'plurx-' + os_name, 'runtime': f'{os_name} 26.5 / {model} / {boot}'},
        'Apple command or native simulator differs')
    block = invocation_block(text, command)
    require(block.count("Test Suite 'All tests' passed") == 1 and block.count('.xcresult') == 1
            and re.search(r"Test Suite 'All tests' passed[^\n]*\n[^\n]*Executed "
                          + str(total) + r' tests, with '
                          + ('1 test skipped and ' if skipped else '')
                          + r'0 failures \(0 unexpected\)', block),
            'Apple command lacks its native suite and result summary')
    starts = re.findall(r"Test Case '-\[\w+\.(\w+) (test\w+)\]' started\.", block)
    actual = retention.log_outcomes(block, 'xctest-class')
    require(len(starts) == len(actual) == total
            and {name + '/' + method for name, method in starts} == set(actual)
            and sum(value == 'ignored' for value in actual.values()) == skipped
            and set(actual.values()) <= {'pass', 'ignored'},
            'Apple invocation has incomplete or failed XCTest records')
    return actual


def authenticate(run, job, document):
    require = receipts.require
    require(run.get('workflow_id') == 'ci.yml' and run.get('trigger_event') == 'workflow_dispatch'
            and run.get('commit_sha') == SOURCE and job.get('run_id') == 6713
            and job.get('id') == 63638 and job.get('task_id') == 26548
            and job.get('name') == 'Apple iOS + tvOS tests' and job.get('status') == 'success'
            and job.get('attempt') == 1, 'Apple job identity, attempt or status differs')
    event = receipts.bounded_json(run['event_payload'].encode())
    inputs = event['inputs']
    require(event['ref'] == 'refs/heads/' + document['branch']
            and inputs['promotion_pr'] == str(document['pr'])
            and inputs['promotion_head_sha'] == SOURCE, 'Apple job belongs to another promotion')
