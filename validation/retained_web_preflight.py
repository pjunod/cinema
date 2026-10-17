"""The finite original two-script Node capture; no JavaScript is executed here."""
import json
import re
from validation import python_unit_receipts as receipts

SOURCE = '59440187d0ad16ceec7632f2461493112f3dfc60'
LOG = 'f5b438be3ec8c0b4e2fc0d496081503c44b69c6bb2afc0c1b2d96fda487ab45e'
SCRIPTS = {
    'tests/playback/player-input-contract.test.js':
        'a9e6c3743d95f1b565c490555be6d05658dc7e907d157219b547446f3d1ab101',
    'tests/web/player-dom.test.js':
        'a1cba7df487fa9ec7a1e3e69345cb9f93217d9425d1c4db1f265e8228200ea15',
}
COMMAND = '\n'.join('node ' + path for path in SCRIPTS)
CASE = 'overview groups dropped and total frames with buffer and interruptions'
FORMAT = 'node-preflight-4954'


def labels(read_source):
    """Read literal declarations from the original hash-bound static source."""
    first, second = [read_source(SOURCE, path).decode() for path in SCRIPTS]
    custom = [json.loads(name) for name in re.findall(r'^test\(("[^"\n]+"),', first, re.M)]
    passed = [json.loads(name).removeprefix('PASS ').removesuffix('\n') for name in
              re.findall(r'process\.stdout\.write\(("PASS [^"\n]+")\);', second)]
    native = [json.loads(name) for name in re.findall(r'^test\(("[^"\n]+"),', second, re.M)]
    receipts.require(len(custom) == 18 and len(set(custom)) == 18
                     and len(passed) == 6 and len(set(passed)) == 6 and native == [CASE],
                     'Original Node declaration inventory differs')
    return custom, passed


def outcomes(log, read_source):
    custom, passed = labels(read_source)
    lines = re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', log, flags=re.M).splitlines()
    start = [i for i, line in enumerate(lines) if line == 'ok - ' + custom[0]]
    receipts.require(len(start) == 1, 'Node capture start missing or repeated')
    end = [i for i in range(start[0], len(lines)) if lines[i].startswith('⭐ Run Post ')]
    receipts.require(end and lines[-1] == '🏁  Job succeeded'
                     and 'node: v22.23.3' in lines, 'Node capture executor terminal missing')
    window = lines[start[0]:end[0]]
    expected = ['ok - ' + name for name in custom] + ['PASS ' + name for name in passed]
    receipts.require(window[:24] == expected, 'Node custom outcomes incomplete or contradictory')
    terminal = '\n'.join(window[24:])
    pattern = (r'TAP version 13\n# Subtest: ' + re.escape(CASE) + r'\nok 1 - '
               + re.escape(CASE) + r"\n  ---\n  duration_ms: [0-9]+(?:\.[0-9]+)?"
               + r"\n  type: 'test'\n  \.\.\.\n1\.\.1\n# tests 1\n# suites 0\n# pass 1"
               + r'\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 0'
               + r'\n# duration_ms [0-9]+(?:\.[0-9]+)?')
    receipts.require(re.fullmatch(pattern, terminal), 'Node native TAP terminal incomplete or contradictory')
    # These are whole-script outcomes. Native case counts remain in the raw capture.
    return {path: 'pass' for path in SCRIPTS}


def record_contract(lane, suite, record, candidate, raw, read_source, digest):
    receipts.require(lane == 'web_preflight' and suite['name'] == 'node-contracts'
                     and set(suite['ids']) == set(SCRIPTS) and len(suite['records']) == 1
                     and record['source_commit'] == SOURCE and record['format'] == FORMAT
                     and record['origin'] == {'kind': 'authenticated-ci',
                         'identity': 'run:4954/job:49307', 'command': COMMAND}
                     and digest(raw.encode()) == LOG,
                     'Node preflight outside its finite original capture')
    environment = record['environment']
    receipts.require(suite['environment'] == environment, 'Node suite environment differs')
    receipts.require(environment == {'platform': 'linux', 'toolchain': 'Node 22.23.3',
                     'features': [], 'runtime': 'original full-CI Node preflight',
                     'binary_sha256': None, 'flags': None, 'per_script_exit': None},
                     'Node original environment unknowns changed')
    for path, sha in SCRIPTS.items():
        for commit in (SOURCE, candidate):
            source = read_source(commit, path)
            receipts.require(source is not None and digest(source) == sha,
                             'Node script inventory or body changed')


def authenticate(run_id, job_id, job, read_source):
    receipts.require((run_id, job_id) == (4954, 49307) and job['status'] == 'success',
                     'Node preflight original job was not successful')
    workflow = read_source(SOURCE, '.github/workflows/ci.yml').decode()
    step = ('      - name: Check the shared player input contract\n'
            '        run: |\n' + ''.join('          node ' + path + '\n' for path in SCRIPTS))
    receipts.require(workflow.count(step) == 1
                     and all(workflow.count('          node ' + path + '\n') == 1 for path in SCRIPTS),
                     'Original Node workflow command scope differs')
