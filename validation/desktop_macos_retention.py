"""One source-reconstructed desktop-macos-check adoption; never a native scheduler.

The known completed suite must retain or refuse. Only positively new inputs and
complete classified history can authorize the explicit fresh execution branch.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import urllib.error
import urllib.request
import zipfile

from validation import main_preflight_adoption as adoption
from validation import main_unit_receipts as main_receipts
from validation import python_unit_receipts as receipts
from validation import worker_control_resume as transport

require = receipts.require
WORKFLOW = '.github/workflows/main-fast-lane.yml'
PRODUCER = '36d25b2313852e4bbde9e7c7b43efa74de23f04c'
PRODUCER_TREE = '77a3cd25ae8117f001d09d7c59091cf73784235c'
PRODUCER_WORKFLOW = '41eb557438e184abaccad40f9c8cf6bbe22cfa1d69d5c875031f920e83d24eba'
ORIGIN_RUN, ORIGIN_JOB, ORIGIN_ARTIFACT = 5943, 57583, 4225
ORIGIN_PR, ORIGIN_REPOSITORY = 1263, 1
ORIGIN_ZIP_SHA = '9618566a1b9ec370eca2db5718f0142dfbcf66f98e7ca428b32d238150a3edcc'
ORIGIN_JOB_SHA = 'be0760040911104d265cf4c07e2d4624306bf968ebafb514736f3ccdc9709602'
JOB = 'Apple compile and native TV navigation'
LABELS = ['self-hosted','macOS','ARM64','lab','apple','xcode-27.1']
ROOTS = ('clients/server-tray/macos','Makefile','Cargo.toml','LICENSE','brand/export')
# Whole recipes are pinned: a new helper/dependency cannot escape ROOTS.
RECIPE_VERSION = 3
MAX_ROOT_ENTRIES, MAX_ROOT_BYTES = 512, 32*1024
MAKEFILE_FAMILY = (
    {'path':'Makefile','mode':'100644','blob':'917e9879ebac1006dc7267caffc3e08c2755b5a0',
     'sha256':'ffd8237d4bc75ad370ca7c7045e1565710035659f1929ce48ccf40531a221528'},
    {'path':'Makefile','mode':'100644','blob':'53f6271a48d8acfc96e394edf24a790f6c4d56af',
     'sha256':'5fd6f68451b172ce48f0093adf3bb32ee63644087264130a481206a43263ad64'},
)
CANONICAL_MAKEFILE = MAKEFILE_FAMILY[0]
APPLICABILITY_PROJECTION = 'exact-reviewed-make-pair-v1'
RECIPE_COMPATIBILITY = 'exact protected native workflow scope v1; exact reviewed Makefile pair v1'
MAX_WORKFLOW_BYTES, MAX_WORKFLOW_JOBS = 128*1024, 64
WORKFLOW_SCOPE_PINS = {
    'preamble':'4d7d1fba026f898d1f9f809414bd2a43e78744aab0ecda3f92e0ced06139742d',
    'paperwork':'56d0a2b952493c4f167a13941b3aebac1a20b44862cbcf3f5809e633701d0a31',
    'scope':'2ee2d0586a98a440b0e44800c44ba35286baad0e5c01d49808e62b7fdba3199d',
    'preflight':'054ad52683b711f4b286557d88f5f4999de0cfa20f3645043f484f2c9629c27c',
    'legacy_apple':'c6bd7e2361ac9055999c183f2f53b36b4a40fdf72b7cf1167a5990d271639e19',
    'adapted_apple':'519df6d3b88a2ba381cd1633379b1a040ea04ce962471c7783f124d8fd887194',
}
PROTECTED_DEPENDENCIES = ('paperwork','scope','preflight')
RECIPE_PINS = {
    'Cargo.toml':('100644','72af819e0c33f23e20387b12cd8b3cb72ec84ea56f14b957f101c8412315680f'),
    'clients/server-tray/macos/build':('100755','b7135b8d5de4ae53f86a5260965c72f257cd73ccfdac56477ee6688bd86456b0'),
    'clients/server-tray/macos/Package.swift':('100644','8e5ac76e4b9bece61bd85f3f92ecdceca2482bd99cfbd23bbdf4e0b6dee0c322'),
}
MAX_FILES, MAX_BLOB, MAX_SOURCE = 128, 256 * 1024, 2 * 1024 * 1024
MAX_ZIP, MAX_LOG, MAX_HISTORY = 16 * 1024, 64 * 1024, 100
MAX_V1_ZIP, MAX_V1_JSON, MAX_V1_TOTAL = 128 * 1024, 128 * 1024, 512 * 1024
MEMBER = 'desktop-macos-check.log'
TOOLCHAIN = ['Xcode 27.1','Build version 27A9275']
SWIFT = '/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/swift'
OUTPUT = 'target/validation/desktop-macos-retention'
COMMAND = ['make','desktop-macos-check']
LEGACY_CHECKOUT = '''  apple_compile:
    name: Apple compile and native TV navigation
    needs: [scope, preflight]
    if: needs.scope.outputs.apple == 'true' || needs.scope.outputs.desktop_macos == 'true'
    runs-on: [self-hosted, macOS, ARM64, lab, apple, xcode-27.1]
    # 32 native journeys take 81-163 s each (about 60 min, up to 87) after the compile.
    timeout-minutes: 120
    concurrency:
      group: plurx-apple-silicon-heavy
      cancel-in-progress: false
    env:
      DEVELOPER_DIR: /Applications/Xcode.app/Contents/Developer
      PATH: /opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
    steps:
      - uses: https://data.forgejo.org/actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4
        with:
          persist-credentials: false
'''
LEGACY_STEP = '''      - name: Build and test native macOS server tray
        if: needs.scope.outputs.desktop_macos == 'true'
        # Keep desktop XCTest namespaces out of mobile unit receipt parsing.
        # The complete native output is preserved independently, including failures.
        run: |
          mkdir -p target/validation
          make desktop-macos-check > target/validation/desktop-macos-check.log 2>&1
          echo "Native macOS server tray build and unit checks passed."
'''
ADAPTER_STEP = '''      - name: Retain or execute native macOS server tray evidence
        if: needs.scope.outputs.desktop_macos == 'true'
        env:
          GITHUB_TOKEN: ${{ github.token }}
        run: python3 -m validation.desktop_macos_retention --fresh-on-new-inputs
'''
LEGACY_UPLOAD = '          path: target/validation/desktop-macos-check.log\n'
ADAPTER_UPLOAD = '          path: target/validation/desktop-macos-retention\n'


RECEIPT_COMMON = frozenset(['schema_version', 'case', 'current_commit', 'current_tree', 'current_run', 'current_job', 'current_attempt', 'scope', 'native_execution_key', 'source_manifest_kind', 'current_source_manifest', 'expected_case_ids', 'toolchain_contract', 'adapter_environment', 'producer_workflow_sha256', 'consumer_workflow_sha256', 'recipe_compatibility', 'adapter_sha256', 'policy_sha256', 'build_product_digests', 'current_build_products_exist', 'checkout_attestation'])

def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',',':')).encode()


def child_environment():
    # Native children retain PATH/DEVELOPER_DIR but never inherit API credentials.
    names = {'GITHUB_TOKEN','GH_TOKEN','FORGEJO_TOKEN','GITEA_TOKEN','DESKTOP_NATIVE_API_TOKEN',
        'ACTIONS_RUNTIME_TOKEN','ACTIONS_ID_TOKEN_REQUEST_TOKEN'}
    return {key:value for key,value in os.environ.items() if key not in names}


def git(workspace, *args, limit=MAX_SOURCE):
    try:
        data = subprocess.check_output(['git',*args], cwd=workspace,
            stderr=subprocess.DEVNULL, timeout=15, env=child_environment())
    except (OSError,subprocess.SubprocessError):
        raise receipts.ReceiptError('Required native Git objects unavailable') from None
    require(len(data) <= limit, 'Native Git output exceeds bound')
    return data


def manifest(workspace, commit):
    require(isinstance(commit,str) and re.fullmatch('[0-9a-f]{40}',commit), 'Invalid native source commit')
    require(git(workspace,'cat-file','-t',commit).strip() == b'commit', 'Native source is not a commit')
    entries, contents, total = [], {}, 0
    for record in git(workspace,'ls-tree','-r','-z',commit,'--',*ROOTS).split(b'\0'):
        if not record: continue
        header, raw_path = record.split(b'\t',1)
        mode, kind, blob = header.decode('ascii').split()
        path = raw_path.decode('utf-8')
        require(mode in ('100644','100755') and kind == 'blob', 'Native closure contains symlink/submodule/unknown mode')
        require(path and not path.startswith('/') and ':' not in path and not any(c in path for c in '\r\n\0\\')
            and '..' not in path.split('/') and path not in contents, 'Unsafe/duplicate native source path')
        require(len(entries) < MAX_FILES, 'Native source member bound exceeded')
        size = int(git(workspace,'cat-file','-s',blob,limit=32))
        require(0 <= size <= MAX_BLOB and total + size <= MAX_SOURCE, 'Native source byte bound exceeded')
        data = git(workspace,'cat-file','blob',blob,limit=MAX_BLOB)
        require(len(data) == size, 'Native source blob size differs')
        total += size; contents[path] = data
        entries.append({'path':path,'mode':mode,'blob':blob,'sha256':digest(data)})
    entries.sort(key=lambda row:row['path'])
    paths = set(contents)
    require({'Makefile','Cargo.toml','LICENSE','clients/server-tray/macos/Package.swift',
        'clients/server-tray/macos/build'} <= paths and any(p.startswith('brand/export/') for p in paths),
        'Incomplete native source closure')
    supported_recipe(workspace,commit,entries,contents)
    return entries, contents


def supported_recipe(workspace,commit,entries,contents):
    """The fixed dependency-free recipe must still close over ROOTS."""
    roots = [row for row in git(workspace,'ls-tree','-z',commit,limit=MAX_ROOT_BYTES).split(b'\0') if row]
    require(len(roots) <= MAX_ROOT_ENTRIES, 'Native root selector inventory bound exceeded')
    for row in roots:
        header,raw_name = row.split(b'\t',1)
        mode,kind,blob = header.decode('ascii').split()
        name = raw_name.decode('utf-8')
        require(name == 'Makefile' or name.casefold() not in ('gnumakefile','makefile'),
            'Unsupported native root Make selector; closure review required: '+name)
    by_path = {row['path']:row for row in entries}
    require(by_path.get('Makefile') in MAKEFILE_FAMILY and digest(contents['Makefile']) == by_path['Makefile']['sha256'],
        'Unsupported native Makefile identity; closure review required')
    for path,(mode,sha) in RECIPE_PINS.items():
        row = by_path.get(path)
        require(row is not None and row['mode'] == mode and row['sha256'] == sha
            and digest(contents[path]) == sha, 'Unsupported native recipe/dependency bytes or mode; closure review required: '+path)
    prefix = 'clients/server-tray/macos/'
    for path in contents:
        if path.startswith(prefix):
            first = path.removeprefix(prefix).split('/',1)[0].casefold()
            require(not first.startswith('package@') and first not in ('package.resolved','.swiftpm'),
                'Unsupported native SwiftPM selector/configuration; closure review required')


def live_recipe_selectors(workspace):
    """Check actual names, avoiding case-insensitive Makefile alias probes."""
    for directory,package in ((workspace,False),(workspace/'clients/server-tray/macos',True)):
        require(not directory.is_symlink(), 'Native selector directory is a symlink; closure review required')
        with os.scandir(directory) as children:
            for index,child in enumerate(children):
                require(index < MAX_ROOT_ENTRIES, 'Native selector directory bound exceeded')
                name = child.name
                forbidden = (name.casefold().startswith('package@') or name.casefold() in ('package.resolved','.swiftpm')) if package else (
                    name != 'Makefile' and name.casefold() in ('gnumakefile','makefile'))
                require(not forbidden, 'Unsupported live native recipe selector; closure review required: '+name)


def case_inventory(contents):
    """Conservative declarations for this unchanged, dependency-free XCTest package."""
    expected = {}
    for path, raw in sorted(contents.items()):
        match = re.fullmatch(r'clients/server-tray/macos/Tests/(PlurxTrayCoreTests|PlurxTrayUITests)/[^/]+\.swift',path)
        if not match: continue
        module, owner = match[1], None
        text = raw.decode('utf-8')
        require(not re.search(r'^\s*(?:#if|extension\s|@Test\b)',text,re.M), 'Unsupported native test declaration shape')
        for line in text.splitlines():
            cls = re.fullmatch(r'(?:final )?class (\w+): XCTestCase \{',line)
            if cls: owner = cls[1]
            elif line == '}': owner = None
            test = re.fullmatch(r'    func (test\w+)\(\)(?: async)?(?: throws)? \{',line)
            if re.search(r'\bfunc\s+test\w+',line):
                require(test is not None and owner is not None, 'Unsupported native test method shape')
                identity = module+'.'+owner+'/'+test[1]
                require(identity not in expected, 'Duplicate native source test identity')
                expected[identity] = {'module':module,'class':owner,'method':test[1],'source':path}
    require(expected and len(expected) <= 128, 'Native source inventory empty/oversized')
    return expected


def transformed_workflow(legacy):
    require(digest(legacy) == PRODUCER_WORKFLOW, 'Unrecognized native producer workflow')
    text = legacy.decode('utf-8')
    for before, after in ((LEGACY_CHECKOUT,LEGACY_CHECKOUT+'          fetch-depth: 0\n'),
            (LEGACY_STEP,ADAPTER_STEP),(LEGACY_UPLOAD,ADAPTER_UPLOAD)):
        require(text.count(before) == 1, 'Native adapter delta anchor is not unique')
        text = text.replace(before,after,1)
    return text.encode()


def workflow_parts(raw):
    """Bounded plain block job map; protected YAML is never normalized."""
    require(isinstance(raw,bytes) and 0 < len(raw) <= MAX_WORKFLOW_BYTES,
        'Native workflow size/type unsupported')
    try: text = raw.decode('utf-8')
    except UnicodeDecodeError: raise receipts.ReceiptError('Native workflow is not UTF-8') from None
    require(not any(ch in text for ch in '\r\t\v\f\x85\u2028\u2029'),'Native workflow requires LF lines without tabs')
    lines = text.splitlines(keepends=True)
    markers = [i for i,line in enumerate(lines) if line in ('jobs:\n','jobs:')]
    require(len(markers) == 1,'Native workflow jobs marker missing/ambiguous')
    marker = markers[0];headers = []
    for i in range(marker+1,len(lines)):
        line = lines[i]
        if not line.strip() or line.lstrip(' ').startswith('#'): continue
        indent = len(line)-len(line.lstrip(' '))
        if indent == 2:
            match = re.fullmatch(r'  ([A-Za-z_][A-Za-z0-9_-]*):\n?',line)
            require(match is not None,'Unsupported native plain job-map key')
            require(match[1] not in {name for name,_ in headers},'Duplicate native workflow job')
            headers.append((match[1],i))
            require(len(headers) <= MAX_WORKFLOW_JOBS,'Native workflow job count exceeds bound')
        else:
            require(indent >= 4 and headers,'Unsupported native workflow top-level/indentation content')
    require(headers,'Native workflow job map is empty')
    blocks = {name:''.join(lines[i:headers[n+1][1] if n+1 < len(headers) else len(lines)]).encode()
        for n,(name,i) in enumerate(headers)}
    return ''.join(lines[:marker+1]).encode(),blocks


def workflow_scope(workspace,commit,legacy):
    require(isinstance(commit,str) and re.fullmatch('[0-9a-f]{40}',commit),'Invalid native workflow commit')
    require(digest(legacy) == PRODUCER_WORKFLOW,'Unrecognized native producer workflow')
    preamble,baseline = workflow_parts(legacy)
    adapted_preamble,adapted = workflow_parts(transformed_workflow(legacy))
    require(digest(preamble) == WORKFLOW_SCOPE_PINS['preamble'] and adapted_preamble == preamble
        and all(digest(baseline[name]) == WORKFLOW_SCOPE_PINS[name] for name in PROTECTED_DEPENDENCIES)
        and digest(baseline['apple_compile']) == WORKFLOW_SCOPE_PINS['legacy_apple']
        and digest(adapted['apple_compile']) == WORKFLOW_SCOPE_PINS['adapted_apple'],
        'Reviewed native workflow scope identities differ')
    current = git(workspace,'show',commit+':'+WORKFLOW,limit=MAX_WORKFLOW_BYTES)
    current_preamble,blocks = workflow_parts(current)
    require(current_preamble == preamble and all(name in blocks and blocks[name] == baseline[name]
        for name in PROTECTED_DEPENDENCIES),'Unsupported native workflow preamble/dependency scope')
    apple = blocks.get('apple_compile')
    require(apple in (baseline['apple_compile'],adapted['apple_compile']),
        'Unsupported native Apple job scope')
    return {'sha256':digest(current),'kind':'legacy' if apple == baseline['apple_compile'] else 'adapted'}


def workflow_identity(workspace,commit,legacy):
    # This remains actual complete bytes, never a scoped/canonical digest.
    return workflow_scope(workspace,commit,legacy)['sha256']


def unpack(data):
    require(0 < len(data) <= MAX_ZIP, 'Native archive size bound exceeded')
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            members = archive.infolist()
            require(len(members) == 1, 'Native archive must contain one log')
            member = members[0]
            require(member.filename == MEMBER and transport.safe_member(member.filename) == (MEMBER,) and not member.is_dir(), 'Unexpected native archive member')
            require(stat.S_IFMT(member.external_attr >> 16) in (0,stat.S_IFREG)
                and not member.flag_bits & 1 and member.compress_type in (zipfile.ZIP_STORED,zipfile.ZIP_DEFLATED),
                'Native archive special/encrypted/unsupported member')
            require(0 < member.file_size <= MAX_LOG, 'Native member size bound exceeded')
            with archive.open(member) as stream: raw = stream.read(MAX_LOG+1)
            require(len(raw) == member.file_size and len(raw) <= MAX_LOG, 'Native member expanded size differs')
            return raw
    except receipts.ReceiptError:
        raise
    except (zipfile.BadZipFile,RuntimeError,NotImplementedError,OSError):
        raise receipts.ReceiptError('Invalid native archive') from None


def parse_native(raw, expected, workspace):
    require(0 < len(raw) <= MAX_LOG, 'Native log size bound exceeded')
    text = raw.decode('utf-8'); lines = text.splitlines()
    require(not re.search(r'(^|\n)(?:error:|fatal error:|.*Test (?:Case|Suite).* (?:failed|skipped))',text), 'Native log contains failure/skip')
    markers = [i for i,line in enumerate(lines) if line.startswith('build: swift build ')]
    require(len(markers) == 2 and [lines[i] for i in markers] ==
        ['build: swift build -c release --arch arm64','build: swift build -c release --arch x86_64'],
        'Missing/duplicate/wrong native release architectures')
    command = 'swift test --package-path clients/server-tray/macos \\'
    require(lines.count(command) == 1, 'Missing/duplicate native test command')
    at = lines.index(command)
    require(markers[0] == 0 < markers[1] < at and
        lines[at+1] == '\t  --scratch-path "'+workspace+'/target/desktop-macos/tests"', 'Native scratch/command differs')
    for arch, begin, end in (('arm64',markers[0],markers[1]),('x86_64',markers[1],at)):
        segment = lines[begin:end]
        require(sum(bool(re.fullmatch(r'Build complete! \([0-9.]+ sec\)',line)) for line in segment) == 1
            and segment.count(workspace+'/target/desktop-macos/'+arch+'/Plurx Server.app') == 1,
            'Native release build incomplete')
    require(sum(line.startswith('Build complete! ') for line in lines) == 3, 'Native build phases incomplete/duplicated')
    first = next((i for i in range(at+2,len(lines)) if lines[i].startswith('Test Suite ')),None)
    require(first is not None and any(line.startswith('Build complete! ') for line in lines[at+2:first]), 'Native test build incomplete')
    stack, active, pending, started, passed, modules, leafs = [], None, None, [], [], [], []
    all_count = 0; tail = None
    for index in range(first,len(lines)):
        line = lines[index]
        if line == '◇ Test run started.': tail = index; break
        if pending is not None:
            match = re.fullmatch(r'\s*Executed ([0-9]+) tests?, with 0 failures \(0 unexpected\) in [0-9.]+ \([0-9.]+\) seconds',line)
            require(match is not None and int(match[1]) == pending, 'Native suite summary incomplete/failed')
            pending = None; continue
        suite = re.fullmatch(r"Test Suite '([^']+)' (started|passed) at [0-9:. -]+\.",line)
        if suite:
            name, status = suite.groups()
            require(active is None, 'Native suite interrupted a case')
            if status == 'started':
                if not stack: require(name == 'All tests', 'Unexpected native root suite')
                elif len(stack) == 1:
                    require(name in ('PlurxTrayCoreTests.xctest','PlurxTrayUITests.xctest'), 'Unexpected native module')
                    require(name not in modules, 'Duplicate native module'); modules.append(name)
                else:
                    require(len(stack) == 2 and any(v['class'] == name and v['module']+'.xctest' == stack[1][0]
                        for v in expected.values()), 'Unexpected native XCTest class')
                    require((stack[1][0],name) not in leafs, 'Duplicate native XCTest class'); leafs.append((stack[1][0],name))
                stack.append([name,[]])
            else:
                require(stack and stack[-1][0] == name, 'Native suite close order differs')
                _, cases = stack.pop(); pending = len(cases)
                require(cases, 'Empty native XCTest suite')
                if not stack: all_count += 1
            continue
        case = re.fullmatch(r"Test Case '-\[([A-Za-z0-9_]+)\.([A-Za-z0-9_]+) (test[A-Za-z0-9_]+)\]' (started\.|passed \([0-9.]+ seconds\)\.)",line)
        require(case is not None and len(stack) == 3, 'Unknown/failed/skipped native case or log line')
        identity = case[1]+'.'+case[2]+'/'+case[3]
        require(identity in expected and stack[1][0] == case[1]+'.xctest' and stack[2][0] == case[2], 'Native case identity/suite differs')
        if case[4] == 'started.':
            require(active is None and identity not in started, 'Duplicate/interleaved native case start')
            active = identity; started.append(identity)
        else:
            require(active == identity and identity not in passed, 'Native pass without unique start')
            passed.append(identity); active = None
            for node in stack: node[1].append(identity)
    require(tail is not None and not stack and active is None and pending is None and all_count == 2
        and set(started) == set(passed) == set(expected), 'Native exact case inventory incomplete')
    remainder = '\n'.join(lines[tail:]).rstrip('\n')
    zero = (r'◇ Test run started\.\n↳ Testing Library Version: 2084\n↳ Target Platform: arm64e-apple-macos14\.0\n'
        r'✔ Test run with 0 tests in 0 suites passed after [0-9.]+ seconds\.')
    require(re.fullmatch(zero+'\n'+zero,remainder), 'Unknown native Swift Testing tail')
    return {'started_once':sorted(started),'passed_once':sorted(passed),
        'module_counts':dict(Counter(v['module'] for v in expected.values())),
        'release_architectures':['arm64','x86_64'],'testing_library':'2084','reported_target':'arm64e-apple-macos14.0'}


def download_log(api, job_id):
    """The existing API origin/opener refuses redirects; cap this case's log read."""
    path = '/actions/jobs/'+str(receipts.positive(job_id))+'/logs'
    api.requests += 1
    require(api.requests <= 200, 'Native API request budget exhausted')
    request = urllib.request.Request(api.root+path, headers={
        'Authorization':'token '+api.token,'Accept':'text/plain'})
    try:
        with api.opener.open(request, timeout=15) as response: raw = response.read(MAX_LOG+1)
    except urllib.error.HTTPError as error:
        raise receipts.ReceiptHTTPError(error.code,path) from None
    except OSError:
        raise receipts.ReceiptError('Native job log unavailable') from None
    require(0 < len(raw) <= MAX_LOG, 'Native job log exceeds bound')
    return raw


def check_job(job, scope, run_id, *, successful=False):
    require(job.get('name') == JOB and type(job.get('repo_id')) is int and job.get('repo_id') == scope['repository']
        and type(job.get('run_id')) is int and job.get('run_id') == run_id and type(job.get('id')) is int and job['id'] > 0
        and type(job.get('attempt')) is int and job['attempt'] == 1
        and job.get('runs_on') == LABELS, 'Native job identity/attempt/runner contract differs')
    if successful:
        require(job.get('status') == 'success' and type(job.get('task_id')) is int
            and job['task_id'] > 0, 'Native job did not succeed')


def checkout_evidence(raw, commit, scope, job, *, complete=False):
    lines = main_receipts.log_lines(raw)
    deletion = [re.fullmatch(r"Deleting the contents of '([^']+)'",line) for line in lines]
    deletion = [m for m in deletion if m]
    require(len(deletion) == 1, 'Native checkout was not freshly initialized')
    workspace = deletion[0][1]
    require(workspace.startswith('/') and '\n' not in workspace, 'Invalid native checkout path')
    required = [f'[command]/usr/bin/git init {workspace}',
        f'[command]/usr/bin/git -c protocol.version=2 fetch --no-tags --prune --no-recurse-submodules --depth=1 origin +{commit}:refs/remotes/pull/{scope["pr"]}/head',
        f'[command]/usr/bin/git checkout --progress --force refs/remotes/pull/{scope["pr"]}/head',
        '[command]/usr/bin/git log -1 --format=%H',commit,*TOOLCHAIN,SWIFT]
    positions = []
    for line in required:
        require(lines.count(line) == 1, 'Native checkout/toolchain log evidence missing/duplicated')
        positions.append(lines.index(line))
    require(positions == sorted(positions), 'Native checkout/toolchain log order differs')
    require(any(re.fullmatch(r'Runner [0-9a-f-]+ \(version:[^)]*\) received task '+str(job['task_id'])+
        r' of job apple_compile, triggered by event: pull_request',line) for line in lines), 'Native task/event log binding differs')
    if complete: require(lines.count('🏁  Job succeeded') == 1, 'Native successful job log is incomplete')
    return lines, workspace


def applicability_inputs(entries):
    """Copy actual inputs; project only either exact reviewed Make row."""
    require(sum(row.get('path') == 'Makefile' for row in entries) == 1,
        'Native applicability requires one reviewed Makefile')
    projected = []
    for row in entries:
        if row.get('path') == 'Makefile':
            require(row in MAKEFILE_FAMILY, 'Unsupported native Makefile identity; closure review required')
            projected.append(dict(CANONICAL_MAKEFILE))
        else:
            projected.append(dict(row))
    return projected


def execution_key(entries, expected):
    return digest(canonical({'inputs':applicability_inputs(entries),'test_ids':sorted(expected),'command':COMMAND,
        'desktop_scope':True,'toolchain':TOOLCHAIN,'developer_dir':'/Applications/Xcode.app/Contents/Developer',
        'runner_labels':LABELS,'release_architectures':['arm64','x86_64']}))


def legacy_result(api, scope, workspace, current, run_id):
    """Only this reviewed legacy origin is accepted; a failed overall run is OK."""
    require(scope['repository'] == ORIGIN_REPOSITORY and scope['pr'] == ORIGIN_PR
        and run_id != ORIGIN_RUN, 'Known native origin is not this current same PR')
    prior = api.get('/actions/runs/'+str(ORIGIN_RUN))
    require(prior.get('id') == ORIGIN_RUN and prior.get('commit_sha') == PRODUCER
        and prior.get('status') in ('success','failure'), 'Known native run identity/result differs')
    adoption.bind_event(prior,scope)
    main_receipts.authenticate_run(scope,prior)
    jobs = api.pages('/actions/runs/'+str(ORIGIN_RUN)+'/jobs')
    selected = [j for j in jobs if j.get('name') == JOB]
    require(len(selected) == 1 and selected[0].get('id') == ORIGIN_JOB, 'Known native job missing/duplicated')
    job = selected[0]; check_job(job,scope,ORIGIN_RUN,successful=True)
    name = 'desktop-macos-check-5943-1'
    candidates = api.pages('/actions/artifacts',{'name':name})
    require(len(candidates) == 1, 'Known native artifact missing/ambiguous')
    artifact = candidates[0]
    require(artifact.get('id') == ORIGIN_ARTIFACT and artifact.get('run_id') == ORIGIN_RUN
        and artifact.get('name') == name and artifact.get('expired') is False
        and type(artifact.get('size_in_bytes')) is int and artifact['size_in_bytes'] == 3930,
        'Known native artifact metadata differs')
    log = download_log(api,ORIGIN_JOB)
    require(len(log) == 14852 and digest(log) == ORIGIN_JOB_SHA, 'Known native job log hash differs')
    lines, old_workspace = checkout_evidence(log,PRODUCER,scope,job,complete=True)
    upload = f'Artifact {name} has been successfully uploaded! Final size is 3930 bytes. Artifact ID is 4225'
    require(lines.count(upload) == 1 and lines.count('SHA256 hash of uploaded artifact zip is '+ORIGIN_ZIP_SHA) == 1
        and lines.count('Native macOS server tray build and unit checks passed.') == 1,
        'Known native command/upload receipt differs')
    data = transport.archive_bytes(api,ORIGIN_ARTIFACT,
        spec=transport.TransportSpec(max_archive_bytes=MAX_ZIP))
    require(len(data) == artifact['size_in_bytes'] and digest(data) == ORIGIN_ZIP_SHA,
        'Known native archive differs from authenticated upload')
    native = unpack(data)
    entries, contents = manifest(workspace,PRODUCER)
    require(git(workspace,'rev-parse',PRODUCER+'^{tree}').decode().strip() == PRODUCER_TREE,
        'Known native source tree differs')
    expected = case_inventory(contents)
    require(Counter(v['module'] for v in expected.values()) ==
        {'PlurxTrayCoreTests':61,'PlurxTrayUITests':14}, 'Known native source inventory differs')
    observed = parse_native(native,expected,old_workspace)
    return {'run':prior,'job':job,'artifact':artifact,'archive':data,'job_log':log,'tree':PRODUCER_TREE,
        'native_log':native,'manifest':entries,'expected':expected,'observed':observed,
        'checkout_path':old_workspace,'key':execution_key(entries,expected)}


def history_before_fresh(api, scope, workspace, current, current_run, current_key, legacy):
    """Complete bounded history is an absence proof only in this declared scope."""
    rows = {}
    queries = ({'ref':f'refs/pull/{scope["pr"]}/head'}, {'ref':'refs/heads/'+scope['branch']})
    for query in queries:
        for row in api.pages('/actions/runs',query,'workflow_runs'):
            identity = receipts.positive(row.get('id'))
            require(identity not in rows or canonical(rows[identity]) == canonical(row), 'Inconsistent duplicate native history')
            rows[identity] = row
            require(len(rows) <= MAX_HISTORY, 'Native history bound reached; absence unknown')
    require(current_run in rows and rows[current_run].get('commit_sha') == current, 'Current native history head differs/missing')
    if scope['pr'] == ORIGIN_PR:
        require(ORIGIN_RUN in rows, 'Known native origin missing from complete history')
    attempts = []
    for rid, listed in sorted(rows.items()):
        if rid == current_run: continue
        if listed.get('event') == 'pull_request' and listed.get('prettyref') != '#'+str(scope['pr']): continue
        require(listed.get('workflow_id') == 'main-fast-lane.yml',
            'Potential manual/unknown native producer requires explicit qualification review')
        prior = api.get('/actions/runs/'+str(rid))
        require(prior.get('id') == rid and prior.get('commit_sha') == listed.get('commit_sha'), 'Native history metadata changed')
        event = adoption.bind_event(prior,scope,ready=False)
        require(event['pull_request']['head']['ref'] == scope['branch'], 'Prior native branch is unknown')
        commit = prior['commit_sha']
        invocation = workflow_scope(workspace,commit,legacy)
        entries, contents = manifest(workspace,commit)
        key = execution_key(entries,case_inventory(contents))
        jobs = api.pages('/actions/runs/'+str(rid)+'/jobs')
        selected = [j for j in jobs if j.get('name') == JOB]
        if not selected:
            require(event['pull_request']['draft'] is True and prior.get('status') in ('success','failure','cancelled')
                and all(j.get('status') in ('success','failure','cancelled','skipped') for j in jobs),
                'Native attempt missing: absence is not nonexecution')
            attempts.append({'run':rid,'classification':'positively_not_started','basis':'recognized draft scope condition false'})
            continue
        seen = set()
        for job in selected:
            check_job(job,scope,rid)
            require(job['attempt'] not in seen, 'Duplicate native job attempt'); seen.add(job['attempt'])
            if job.get('status') == 'skipped' and job.get('task_id') == 0:
                classification = 'positively_not_started'
            else:
                require(job.get('status') in ('success','failure','cancelled') and job.get('task_id',0) > 0,
                    'Queued/running/unknown native attempt blocks fresh execution')
                # A source/recipe comparison must also bind actual checkout;
                # missing/truncated old logs cannot manufacture a different key.
                raw = download_log(api,job['id'])
                if invocation['kind'] == 'legacy':
                    lines, _ = checkout_evidence(raw,commit,scope,job)
                else:
                    require(git(workspace,'show',commit+':validation/desktop_macos_retention.py') ==
                        git(workspace,'show',current+':validation/desktop_macos_retention.py'), 'Prior native adapter compatibility unknown')
                    lines = main_receipts.log_lines(raw)
                    tree = git(workspace,'rev-parse',commit+'^{tree}').decode().strip()
                    checked_attestation(lines,commit,tree,rid,job,entries,key)
                require(lines and lines[-1] in ('🏁  Job succeeded','🏁  Job failed','🏁  Job cancelled'), 'Incomplete native history log')
                require(key != current_key, 'Matching complete/started/partial native history requires retention, never rerun')
                classification = 'different_native_key'
            attempts.append({'run':rid,'job':job['id'],'attempt':job['attempt'],'source_commit':commit,
                'native_key':key,'classification':classification})
    return {'queries':list(queries),'pagination_complete':True,'run_ids':sorted(rows),'attempts':attempts,
        'scope_limit':'same PR main-fast-lane plus branch query; potential manual/unknown producers block fresh'}


def current_job(api, scope, run_id):
    jobs = [j for j in api.pages('/actions/runs/'+str(run_id)+'/jobs') if j.get('name') == JOB]
    require(len(jobs) == 1, 'Current native job missing/ambiguous')
    check_job(jobs[0],scope,run_id)
    require(jobs[0].get('status') == 'running' and jobs[0].get('task_id',0) > 0,
        'Current native job is not the active task')
    return jobs[0]


def verify_toolchain():
    require(sys.platform == 'darwin' and os.environ.get('DEVELOPER_DIR') == '/Applications/Xcode.app/Contents/Developer',
        'Native current platform/toolchain environment differs')
    version = subprocess.check_output(['xcodebuild','-version'],text=True,env=child_environment()).splitlines()
    swift = subprocess.check_output(['xcrun','--find','swift'],text=True,env=child_environment()).strip()
    require(version == TOOLCHAIN and swift == SWIFT, 'Current native Xcode toolchain contract differs')
    return {'xcode_output':version,'swift_path':swift,'host_os_release':None,
        'compiler_binary_sha256':None,'sdk_inventory':None,
        'unrecorded_reason':'These values were not present in the original native evidence.'}


def output_directory(workspace):
    output = workspace/OUTPUT
    require(not any(p.is_symlink() for p in (output,*output.parents)), 'Native evidence output contains symlink')
    output.parent.mkdir(parents=True,exist_ok=True)
    require(not output.exists(), 'Native execution/adoption already attempted; no duplicate fallback')
    output.mkdir(mode=0o700)
    return output


def current_context(workspace):
    """Reuse API/event validators, with Git subprocesses receiving no API token."""
    require(os.environ.get('GITHUB_EVENT_NAME') == 'pull_request'
        and os.environ.get('GITHUB_RUN_ATTEMPT','1') == '1', 'Native adapter needs an initial ready PR run')
    path = Path(os.environ['GITHUB_EVENT_PATH'])
    with path.open('rb') as stream: raw = stream.read(128*1024+1)
    require(len(raw) <= 128*1024, 'Native current event exceeds bound')
    event = receipts.bounded_json(raw); repo = event['repository']; pull = event['pull_request']
    require(repo['full_name'] == os.environ['GITHUB_REPOSITORY'] and event['number'] == pull['number'],
        'Native current repository/PR differs')
    current = git(workspace,'rev-parse','HEAD').decode().strip()
    require(current == pull['head']['sha'] == os.environ['GITHUB_SHA']
        and not git(workspace,'diff','--name-only','HEAD'), 'Native current checkout/head changed')
    api_root = os.environ['GITHUB_API_URL']
    require(api_root.startswith(os.environ['GITHUB_SERVER_URL'].rstrip('/')+'/'), 'Native API origin differs')
    api = receipts.API(api_root,os.environ['GITHUB_REPOSITORY'],os.environ.get('GITHUB_TOKEN'))
    run_id = receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    current_run = api.get('/actions/runs/'+str(run_id))
    scope = main_receipts.identity(api,event['number'],current)
    require(current_run.get('id') == run_id and current_run.get('commit_sha') == current
        and adoption.bind_event(current_run,scope) == event, 'Native current API event/head differs')
    live = api.get('/pulls/'+str(scope['pr']))
    adoption.validate_live_pr(live,event,current)
    return api, scope, current, run_id


def recheck(workspace, current, run_id):
    api, scope, check, rid = current_context(workspace)
    require(check == current and rid == run_id and git(workspace,'rev-parse','HEAD').decode().strip() == current,
        'Current native head/run drifted')
    return api, scope


def run_gate(api, scope, workspace, current, run_id, output, toolchain, fresh_on_new_inputs=False):
    require(scope['repository'] == ORIGIN_REPOSITORY, 'Unsupported native repository')
    require(os.environ.get('GITHUB_JOB') == 'apple_compile', 'Native adapter must run in the Apple job')
    legacy = git(workspace,'show',PRODUCER+':'+WORKFLOW,limit=128*1024)
    require(digest(legacy) == PRODUCER_WORKFLOW, 'Known native workflow object differs')
    consumer_workflow = workflow_scope(workspace,current,legacy)
    require(consumer_workflow['kind'] == 'adapted', 'Current native job lacks reviewed adapter recipe')
    live_recipe_selectors(workspace)
    entries, contents = manifest(workspace,current); expected = case_inventory(contents)
    key = execution_key(entries,expected)
    origin_entries, origin_contents = manifest(workspace,PRODUCER)
    known_key = execution_key(origin_entries,case_inventory(origin_contents))
    job = current_job(api,scope,run_id)
    receipt = {'schema_version':1,'case':'desktop-macos-check','current_commit':current,
        'current_tree':git(workspace,'rev-parse',current+'^{tree}').decode().strip(),
        'current_run':run_id,'current_job':job['id'],'current_attempt':job['attempt'],'scope':scope,
        'native_execution_key':key,'source_manifest_kind':'reconstructed_from_authenticated_checkout',
        'current_source_manifest':entries,'expected_case_ids':sorted(expected),'toolchain_contract':toolchain,
        'adapter_environment':{'python':list(sys.version_info[:3]),'platform':sys.platform},
        'producer_workflow_sha256':PRODUCER_WORKFLOW,'consumer_workflow_sha256':consumer_workflow['sha256'],
        'recipe_compatibility':RECIPE_COMPATIBILITY,
        'adapter_sha256':digest(git(workspace,'show',current+':validation/desktop_macos_retention.py')),
        'policy_sha256':policy_fingerprint(),
        'build_product_digests':None,'current_build_products_exist':False}
    recheck(workspace,current,run_id)
    attestation = {'commit':current,'tree':receipt['current_tree'],'run':run_id,'job':job['id'],'attempt':1,
        'native_execution_key':key,'source_manifest_sha256':digest(canonical(entries)),
        'tracked_clean':True,'workspace':str(workspace)}
    receipt['checkout_attestation'] = attestation
    print('DESKTOP-NATIVE-CHECKOUT '+canonical(attestation).decode(),flush=True)
    if key == known_key and scope['pr'] == ORIGIN_PR:
        origin = legacy_result(api,scope,workspace,current,run_id)
    else:
        origin = matching_v1(api,scope,workspace,current,run_id,key,legacy)
    if origin is not None:
        require(origin['key'] == key and applicability_inputs(origin['manifest']) == applicability_inputs(entries), 'Native source closure differs after authentication')
        recheck(workspace,current,run_id)
        (output/'origin-artifact.zip').write_bytes(origin['archive'])
        (output/'origin-job.log').write_bytes(origin['job_log'])
        (output/MEMBER).write_bytes(origin['native_log'])
        metadata = {'run':origin['run'],'job':origin['job'],'artifact':origin['artifact']}
        receipts.atomic_json(output/'origin-metadata.json',metadata)
        receipt.update(decision='retained_prior_execution',execution_origin=origin_identity(origin),
            current_native_commands_executed=False,
            attached_metadata_sha256=digest((output/'origin-metadata.json').read_bytes()))
    else:
        require(fresh_on_new_inputs, 'New native inputs require the explicit fresh-on-new-inputs workflow policy')
        history = history_before_fresh(api,scope,workspace,current,run_id,key,legacy)
        recheck(workspace,current,run_id)
        live_recipe_selectors(workspace)
        launch_entries,launch_contents = manifest(workspace,current)
        require(launch_entries == entries and execution_key(launch_entries,case_inventory(launch_contents)) == key,
            'Native guarded source closure changed before execution')
        start = {'state':'started','commit':current,'run':run_id,'job':job['id'],'attempt':1,'native_execution_key':key}
        receipts.atomic_json(output/'fresh-start.json',start)
        print('DESKTOP-NATIVE-START '+canonical(start).decode(),flush=True)
        with (output/MEMBER).open('xb') as log:
            result = subprocess.run(COMMAND,cwd=workspace,stdout=log,stderr=subprocess.STDOUT,env=child_environment())
        finish = {**start,'state':'finished','exit_code':result.returncode}
        receipts.atomic_json(output/'fresh-finish.json',finish)
        print('DESKTOP-NATIVE-FINISH '+canonical(finish).decode(),flush=True)
        require(result.returncode == 0, 'Fresh native command failed; preserve its partial result, never replay')
        require((output/MEMBER).stat().st_size <= MAX_LOG, 'Fresh native log exceeds bound')
        with (output/MEMBER).open('rb') as stream: native = stream.read(MAX_LOG+1)
        observed = parse_native(native,expected,str(workspace))
        recheck(workspace,current,run_id)
        receipt.update(decision='fresh_execution',current_native_commands_executed=True,
            execution_origin={'commit':current,'tree':receipt['current_tree'],'run':run_id,'job':job['id'],
                'attempt':1,'native_execution_key':key,'native_log_sha256':digest(native)},
            history=history,observed=observed,native_log_sha256=digest(native),
            build_product_digests=None,current_build_products_exist=None)
    receipts.atomic_json(output/'receipt.json',receipt)
    print('Native desktop evidence decision: '+receipt['decision']+'; execution key '+key,flush=True)
    return receipt


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fresh-on-new-inputs',action='store_true')
    args = parser.parse_args(argv)
    try:
        workspace = Path(os.environ['GITHUB_WORKSPACE']).resolve()
        api, scope, current, run_id = current_context(workspace)
        output = output_directory(workspace)
        run_gate(api,scope,workspace,current,run_id,output,verify_toolchain(),args.fresh_on_new_inputs)
        return 0
    except (receipts.ReceiptError,OSError,KeyError,ValueError,TypeError,AttributeError,UnicodeError,subprocess.SubprocessError) as error:
        # HTTP errors use the existing token-safe exception; never print server data.
        message = str(error) if isinstance(error,receipts.ReceiptError) else type(error).__name__
        print('Native evidence refused: '+message,file=sys.stderr)
        return 1




def policy_fingerprint():
    return digest(canonical({'version':1,'origin_commit':PRODUCER,'origin_run':ORIGIN_RUN,
        'origin_job':ORIGIN_JOB,'origin_artifact':ORIGIN_ARTIFACT,'archive_sha256':ORIGIN_ZIP_SHA,
        'job_log_sha256':ORIGIN_JOB_SHA,'roots':ROOTS,'command':COMMAND,
        'workflow_sha256':PRODUCER_WORKFLOW,
        'workflow_scope':{'version':1,'grammar':'bounded-plain-job-map-LF-UTF8-v1','pins':WORKFLOW_SCOPE_PINS,
            'protected_dependencies':PROTECTED_DEPENDENCIES,'apple_job':'apple_compile','max_bytes':MAX_WORKFLOW_BYTES,'max_jobs':MAX_WORKFLOW_JOBS},'recipe_version':RECIPE_VERSION,'recipe_pins':RECIPE_PINS,
        'makefile_family':MAKEFILE_FAMILY,'canonical_makefile':CANONICAL_MAKEFILE,'applicability_projection':APPLICABILITY_PROJECTION,
        'forbidden_make_selectors':['GNUmakefile','makefile'],'forbidden_package_selectors':['Package@*','Package.resolved','.swiftpm'],
        'upload_section_limits':[32,16*1024],'live_selector_policy':{'actual_names':True,'casefold':True,'allow_root_spelling':'Makefile','directory_limit':MAX_ROOT_ENTRIES},'committed_selector_policy':{'nonrecursive_root_bytes':MAX_ROOT_BYTES,'root_entries':MAX_ROOT_ENTRIES,'casefold':True,'allow_root_spelling':'Makefile'},
        'limits':[MAX_FILES,MAX_BLOB,MAX_SOURCE,MAX_ZIP,MAX_LOG,MAX_HISTORY,MAX_V1_ZIP,MAX_V1_JSON,MAX_V1_TOTAL]}))


def origin_identity(result):
    """Stable identity projection; mutable API account/repository fields stay outside."""
    run, job, artifact = result['run'],result['job'],result['artifact']
    return {'commit':run['commit_sha'],'tree':result['tree'],'run':run['id'],'job':job['id'],
        'attempt':job['attempt'],'native_execution_key':result['key'],
        'artifact':artifact['id'],'archive_sha256':digest(result['archive']),
        'job_log_sha256':digest(result['job_log']),'native_log_sha256':digest(result['native_log']),
        'receipt_member_sha256':result.get('receipt_member_sha256'),
        'metadata_identity_sha256':digest(canonical({'run':{k:run[k] for k in
            ('id','commit_sha','workflow_id','event','prettyref','event_payload')},
            'job':{k:job[k] for k in ('id','run_id','repo_id','attempt','name','task_id','runs_on','status')},
            'artifact':{k:artifact[k] for k in ('id','run_id','name','size_in_bytes','expired')}}))}


def unpack_v1(data):
    require(0 < len(data) <= MAX_V1_ZIP, 'Native v1 archive exceeds bound')
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            infos = archive.infolist(); names = [m.filename for m in infos]
            fresh = {'receipt.json','fresh-start.json','fresh-finish.json',MEMBER}
            retained = {'receipt.json','origin-artifact.zip','origin-job.log','origin-metadata.json',MEMBER}
            require(len(names) == len(set(names)) and set(names) in (fresh,retained), 'Native v1 exact member schema differs')
            total, payload = 0, {}
            for member in infos:
                require(member.filename in (fresh|retained) and transport.safe_member(member.filename) == (member.filename,)
                    and not member.is_dir() and stat.S_IFMT(member.external_attr >> 16) in (0,stat.S_IFREG)
                    and not member.flag_bits & 1 and member.compress_type in (zipfile.ZIP_STORED,zipfile.ZIP_DEFLATED),
                    'Native v1 unsafe member')
                bound = MAX_LOG if member.filename in (MEMBER,'origin-job.log') else MAX_V1_JSON
                require(0 < member.file_size <= bound and total+member.file_size <= MAX_V1_TOTAL, 'Native v1 member/total size bound')
                with archive.open(member) as stream: raw = stream.read(bound+1)
                require(len(raw) == member.file_size and len(raw) <= bound, 'Native v1 expanded size differs')
                payload[member.filename] = raw; total += len(raw)
            return payload
    except receipts.ReceiptError:
        raise
    except (zipfile.BadZipFile,RuntimeError,NotImplementedError,OSError):
        raise receipts.ReceiptError('Invalid native v1 archive') from None


def upload_digest(lines,name,aid,size):
    """Bind one pinned uploader transaction; other job artifacts stay separate."""
    final = f'Artifact {name} has been successfully uploaded! Final size is {size} bytes. Artifact ID is {aid}'
    done = f'Artifact {name}.zip successfully finalized. Artifact ID {aid}'
    uploads = [i for i,line in enumerate(lines) if line.startswith('Artifact '+name+' has been successfully uploaded!')]
    require(len(uploads) == 1 and lines[uploads[0]] == final and lines.count(done) == 1,
        'Native v1 upload identity differs')
    end = uploads[0]
    starts = [i for i in range(end) if re.fullmatch(r'With the provided path, there will be [1-9][0-9]* files? uploaded',lines[i])]
    require(starts, 'Native v1 upload section start missing')
    start = starts[-1];section = lines[start:end+1]
    require(len(section) <= 32 and sum(len(line.encode())+1 for line in section) <= 16*1024,
        'Native v1 upload section bound exceeded')
    markers = ['Artifact name is valid!','Root directory input is valid!',
        'Beginning upload of artifact content to blob storage','Uploaded bytes '+str(size),
        'Finished uploading artifact content to blob storage!','Finalizing artifact upload',done,final]
    require(all(section.count(line) == 1 for line in markers), 'Native v1 upload section incomplete/ambiguous')
    positions = [section.index(line) for line in markers]
    hashes = [(i,m[1]) for i,line in enumerate(section)
        if (m := re.fullmatch(r'SHA256 hash of uploaded artifact zip is ([0-9a-f]{64})',line))]
    require(len(hashes) == 1 and sum(line.startswith('SHA256 hash of uploaded artifact zip is ') for line in section) == 1
        and positions == sorted(positions)
        and positions[4] < hashes[0][0] < positions[5]
        and sum(line.startswith('Artifact ') and ('successfully finalized.' in line or 'successfully uploaded!' in line)
            for line in section) == 2, 'Native v1 upload digest/order ambiguous')
    return hashes[0][1]


def v1_envelope(api,scope,workspace,run_id,job_id,current,legacy):
    prior = api.get('/actions/runs/'+str(receipts.positive(run_id)))
    require(prior.get('id') == run_id, 'Native v1 run ID differs')
    adoption.bind_event(prior,scope);main_receipts.authenticate_run(scope,prior)
    commit = prior['commit_sha']
    invocation = workflow_scope(workspace,commit,legacy)
    require(invocation['kind'] == 'adapted','Native v1 producer workflow unsupported')
    require(git(workspace,'show',commit+':validation/desktop_macos_retention.py') ==
        git(workspace,'show',current+':validation/desktop_macos_retention.py'), 'Native v1 adapter source compatibility differs')
    jobs = [j for j in api.pages('/actions/runs/'+str(run_id)+'/jobs') if j.get('name') == JOB]
    require(len(jobs) == 1 and (job_id is None or jobs[0].get('id') == job_id), 'Native v1 job missing/ambiguous')
    job = jobs[0];check_job(job,scope,run_id,successful=True)
    name = 'desktop-macos-check-'+str(run_id)+'-'+str(job['attempt'])
    artifacts = api.pages('/actions/artifacts',{'name':name})
    require(len(artifacts) == 1, 'Native v1 artifact missing/ambiguous')
    artifact = artifacts[0]
    require(artifact.get('run_id') == run_id and artifact.get('name') == name and artifact.get('expired') is False
        and type(artifact.get('size_in_bytes')) is int and 0 < artifact['size_in_bytes'] <= MAX_V1_ZIP,
        'Native v1 artifact metadata differs')
    aid = receipts.positive(artifact.get('id'));raw = download_log(api,job['id']);lines = main_receipts.log_lines(raw)
    archive_sha = upload_digest(lines,name,aid,artifact['size_in_bytes'])
    require(lines.count('🏁  Job succeeded') == 1, 'Native v1 job log incomplete')
    data = transport.archive_bytes(api,aid,spec=transport.TransportSpec(max_archive_bytes=MAX_V1_ZIP))
    require(len(data) == artifact['size_in_bytes'] and digest(data) == archive_sha, 'Native v1 archive/upload hash differs')
    # The pinned checkout action's final log -1 --format=%H output is bound
    # separately from the adapter's validated clean-HEAD manifest marker.
    heads = [i for i,line in enumerate(lines) if line == '[command]/usr/bin/git log -1 --format=%H']
    require(len(heads) == 1 and heads[0]+1 < len(lines) and lines[heads[0]+1] == commit,
        'Native v1 pinned checkout final HEAD record differs')
    require(all(lines.count(line) == 1 for line in TOOLCHAIN+[SWIFT])
        and heads[0]+1 < lines.index(TOOLCHAIN[0]) < lines.index(TOOLCHAIN[1]) < lines.index(SWIFT)
        < next(i for i,line in enumerate(lines) if line.startswith('Artifact '+name+' has been successfully uploaded!'))
        < lines.index('🏁  Job succeeded'), 'Native v1 toolchain/action/upload order differs')
    require(any(re.fullmatch(r'Runner [0-9a-f-]+ \(version:[^)]*\) received task '+str(job['task_id'])+
        r' of job apple_compile, triggered by event: pull_request',line) for line in lines), 'Native v1 task binding differs')
    return {'run':prior,'job':job,'artifact':artifact,'archive':data,'job_log':raw,'lines':lines,'workflow':invocation}


def checked_attestation(lines,commit,tree,run_id,job,entries,key):
    records = [line.removeprefix('DESKTOP-NATIVE-CHECKOUT ') for line in lines if line.startswith('DESKTOP-NATIVE-CHECKOUT ')]
    require(len(records) == 1, 'Native v1 clean checkout marker missing/duplicated')
    value = receipts.bounded_json(records[0].encode())
    require(type(value) is dict and set(value) == {'commit','tree','run','job','attempt','native_execution_key','source_manifest_sha256','tracked_clean','workspace'},
        'Native v1 checkout field schema differs')
    wanted = {'commit':commit,'tree':tree,'run':run_id,'job':job['id'],'attempt':1,'native_execution_key':key,
        'source_manifest_sha256':digest(canonical(entries)),'tracked_clean':True,'workspace':value['workspace']}
    require(canonical(value) == canonical(wanted) and isinstance(value['workspace'],str) and value['workspace'].startswith('/'),
        'Native v1 checkout identity differs')
    head = '[command]/usr/bin/git log -1 --format=%H'
    require(lines.count(head) == 1 and lines.index(head)+1 < len(lines) and lines[lines.index(head)+1] == commit
        and all(lines.count(line) == 1 for line in TOOLCHAIN+[SWIFT])
        and lines.index(head)+1 < lines.index(TOOLCHAIN[0]) < lines.index(TOOLCHAIN[1]) < lines.index(SWIFT)
        < lines.index('DESKTOP-NATIVE-CHECKOUT '+canonical(value).decode()), 'Native v1 action/toolchain/attestation order differs')
    return value


def v1_result(api,scope,workspace,run_id,job_id,current,legacy,expected_key,*,require_fresh=False):
    result = v1_envelope(api,scope,workspace,run_id,job_id,current,legacy)
    payload = unpack_v1(result['archive']);receipt = receipts.bounded_json(payload['receipt.json'])
    require(isinstance(receipt,dict), 'Native v1 receipt is not an object')
    fresh_fields = {'decision','current_native_commands_executed','execution_origin','history','observed','native_log_sha256'}
    retained_fields = {'decision','current_native_commands_executed','execution_origin','attached_metadata_sha256'}
    require(set(receipt) == RECEIPT_COMMON | (fresh_fields if receipt.get('decision') == 'fresh_execution' else retained_fields), 'Native v1 receipt field schema differs')
    prior,job = result['run'],result['job'];commit = prior['commit_sha']
    entries,contents = manifest(workspace,commit);expected = case_inventory(contents);key = execution_key(entries,expected)
    tree = git(workspace,'rev-parse',commit+'^{tree}').decode().strip()
    require(type(receipt.get('schema_version')) is int and receipt['schema_version'] == 1
        and receipt.get('case') == 'desktop-macos-check' and key == expected_key == receipt.get('native_execution_key')
        and receipt.get('current_commit') == commit and receipt.get('current_tree') == tree
        and receipt.get('current_run') == run_id and receipt.get('current_job') == job['id']
        and type(receipt.get('current_attempt')) is int and receipt['current_attempt'] == 1
        and canonical(receipt.get('scope')) == canonical(scope) and receipt.get('current_source_manifest') == entries
        and receipt.get('expected_case_ids') == sorted(expected)
        and receipt.get('adapter_sha256') == digest(git(workspace,'show',commit+':validation/desktop_macos_retention.py'))
        and receipt.get('policy_sha256') == policy_fingerprint()
        and receipt.get('producer_workflow_sha256') == PRODUCER_WORKFLOW
        and receipt.get('consumer_workflow_sha256') == result['workflow']['sha256']
        and receipt.get('source_manifest_kind') == 'reconstructed_from_authenticated_checkout'
        and receipt.get('recipe_compatibility') == RECIPE_COMPATIBILITY
        and receipt.get('build_product_digests') is None
        and isinstance(receipt.get('toolchain_contract'),dict)
        and receipt['toolchain_contract'].get('xcode_output') == TOOLCHAIN
        and receipt['toolchain_contract'].get('swift_path') == SWIFT,
        'Native v1 source/receipt/policy identity differs')
    tools = receipt['toolchain_contract']
    require(set(tools) == {'xcode_output','swift_path','host_os_release','compiler_binary_sha256','sdk_inventory','unrecorded_reason'}
        and all(tools[key] is None for key in ('host_os_release','compiler_binary_sha256','sdk_inventory'))
        and isinstance(tools['unrecorded_reason'],str), 'Native v1 invented/untyped environment identity')
    attestation = checked_attestation(result['lines'],commit,tree,run_id,job,entries,key)
    require(receipt.get('checkout_attestation') == attestation, 'Native v1 receipt/checkout marker differs')
    native = payload[MEMBER];observed = None
    result.update(native_log=native,manifest=entries,expected=expected,observed=observed,
        checkout_path=attestation['workspace'],key=key,tree=tree,receipt_member_sha256=digest(payload['receipt.json']))
    if receipt.get('decision') == 'fresh_execution':
        observed = parse_native(native,expected,attestation['workspace'])
        result['observed'] = observed
        require(set(payload) == {'receipt.json','fresh-start.json','fresh-finish.json',MEMBER}, 'Native fresh member schema differs')
        start = receipts.bounded_json(payload['fresh-start.json']);finish = receipts.bounded_json(payload['fresh-finish.json'])
        wanted = {'state':'started','commit':commit,'run':run_id,'job':job['id'],'attempt':1,'native_execution_key':key}
        require(canonical(start) == canonical(wanted) and canonical(finish) == canonical({**wanted,'state':'finished','exit_code':0})
            and type(start.get('attempt')) is int and type(finish.get('exit_code')) is int
            and receipt.get('current_native_commands_executed') is True
            and receipt.get('observed') == observed and receipt.get('native_log_sha256') == digest(native), 'Native fresh start/finish/outcomes differ')
        positions = [result['lines'].index('DESKTOP-NATIVE-CHECKOUT '+canonical(attestation).decode())]
        for prefix,value in [('DESKTOP-NATIVE-START ',start),('DESKTOP-NATIVE-FINISH ',finish)]:
            require(result['lines'].count(prefix+canonical(value).decode()) == 1, 'Native fresh real job marker differs')
            positions.append(result['lines'].index(prefix+canonical(value).decode()))
        require(positions == sorted(set(positions)), 'Native fresh start/finish order differs')
        root = {'commit':commit,'tree':tree,'run':run_id,'job':job['id'],'attempt':1,
            'native_execution_key':key,'native_log_sha256':digest(native)}
        require(canonical(receipt.get('execution_origin')) == canonical(root), 'Native fresh original execution differs')
        return result
    require(not require_fresh and receipt.get('decision') == 'retained_prior_execution'
        and receipt.get('current_native_commands_executed') is False
        and set(payload) == {'receipt.json','origin-artifact.zip','origin-job.log','origin-metadata.json',MEMBER},
        'Native origin is not a fresh root (retained-to-retained/cycle refused)')
    require(not any(line.startswith(('DESKTOP-NATIVE-START ','DESKTOP-NATIVE-FINISH ')) for line in result['lines']), 'Retained native job reports a fresh execution')
    pointer = receipt.get('execution_origin')
    require(type(pointer) is dict and pointer.get('run') != run_id, 'Native retained origin cycle')
    if pointer.get('run') == ORIGIN_RUN:
        original = legacy_result(api,scope,workspace,current,run_id)
        original['tree'] = PRODUCER_TREE
    else:
        original = v1_result(api,scope,workspace,receipts.positive(pointer.get('run')),
            receipts.positive(pointer.get('job')),current,legacy,key,require_fresh=True)
    require(canonical(pointer) == canonical(origin_identity(original)) and payload['origin-artifact.zip'] == original['archive']
        and payload['origin-job.log'] == original['job_log'] and native == original['native_log'],
        'Native retained original identity/hash/key/copy differs')
    copied = receipts.bounded_json(payload['origin-metadata.json'])
    require(isinstance(copied,dict) and set(copied) == {'run','job','artifact'}
        and receipt.get('attached_metadata_sha256') == digest(payload['origin-metadata.json']), 'Native retained metadata copy/hash differs')
    copied_result = {**original,**copied}
    require(origin_identity(copied_result) == origin_identity(original), 'Native retained metadata identity differs')
    return original


def matching_v1(api,scope,workspace,current,current_run,key,legacy):
    """A complete match may be retained before any absence/history decision."""
    seen = set()
    for page in range(1,5):
        data = api.get('/actions/runs',{'workflow_id':'main-fast-lane.yml',
            'ref':f'refs/pull/{scope["pr"]}/head','page':page,'limit':25})
        rows = data.get('workflow_runs')
        require(isinstance(rows,list) and len(rows) <= 25, 'Native candidate pagination differs')
        for row in rows:
            rid = receipts.positive(row.get('id'));require(rid not in seen, 'Duplicate native candidate run');seen.add(rid)
            if rid in (current_run,ORIGIN_RUN): continue
            if row.get('workflow_id') != 'main-fast-lane.yml' or row.get('prettyref') != '#'+str(scope['pr']): continue
            prior = api.get('/actions/runs/'+str(rid));adoption.bind_event(prior,scope,ready=False)
            commit = prior['commit_sha']
            if workflow_scope(workspace,commit,legacy)['kind'] != 'adapted': continue
            entries,contents = manifest(workspace,commit)
            if execution_key(entries,case_inventory(contents)) != key: continue
            jobs = [j for j in api.pages('/actions/runs/'+str(rid)+'/jobs') if j.get('name') == JOB and j.get('status') == 'success']
            if len(jobs) == 1:
                return v1_result(api,scope,workspace,rid,jobs[0]['id'],current,legacy,key)
        if len(rows) < 25: return None
    # No complete match within the selection bound proves no absence. The
    # separate complete-history guard must still decide whether fresh is safe.
    return None


if __name__ == '__main__':
    raise SystemExit(main())
