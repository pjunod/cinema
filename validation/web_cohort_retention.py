"""Authenticated retention of the one reviewed main-lane whole web cohort.

A retained receipt describes an original execution, never current test events.
Unknown dependency/recipe shapes require closure review before any execution.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import sys
import zipfile

from validation import main_preflight_adoption as adoption
from validation import main_unit_receipts as main_receipts
from validation import python_unit_receipts as receipts
from validation import worker_control_resume as transport

require = receipts.require
WORKFLOW = '.github/workflows/main-fast-lane.yml'
ADAPTER = 'validation/web_cohort_retention.py'
OUTPUT = 'target/validation/web-cohort-retention'
JOB, JOB_ID = 'fast web syntax gate', 'web_compile'
PRODUCER = 'fbbf4078d48ef894dcd1c154f5e404ac8656a99a'
ORIGIN_RUN, ORIGIN_JOB, ORIGIN_TASK = 5964, 57771, 23006
ORIGIN_LOG_SHA256 = '5c99fda5f1b086222032888286cef6f9463a3c067fc63a8044037691395fe6b8'
BASELINE = 'tests/web/tsc-baseline.tsv'
RUNTIME = {'node':'v22.23.4','npm':'10.9.9','git':'git version 2.55.0','typescript_lock':'6.0.3'}
POLICY_VERSION = 2
SCRIPTS_RESOLVER_FAMILIES = {'fbbf4078d48ef894dcd1c154f5e404ac8656a99a': '48743b916c2a75d4e0e9531a52abff0b9f318d726be407f84d5fe1cc760e3a99', '087e836d369eb2b8f94ec1d0454ea6c157612df9': '72f0accba4f82129287fb12cb8b53c5739cf49f4dff530eb6d64bc464be6ec61'}
PYTHON_POLICY = 'python3 stdlib planning, summary and loopback HTTP fixtures v1'
NODE_PACKAGE_SCOPES = ('crates/package.json', 'crates/plurxd/package.json', 'crates/plurxd/src/package.json', 'crates/plurxd/src/web/core/package.json', 'crates/plurxd/src/web/detail/package.json', 'crates/plurxd/src/web/layouts/package.json', 'crates/plurxd/src/web/package.json', 'crates/plurxd/src/web/pages/package.json', 'crates/plurxd/src/web/player/package.json', 'crates/plurxd/src/web/types/package.json', 'package.json', 'scripts/cq-native-shipping/package.json', 'scripts/package.json', 'tests/package.json', 'tests/playback/continuous-quality/package.json', 'tests/playback/package.json', 'tests/vendor/package.json', 'tests/web/package.json')
NODE_PACKAGE_PINS = {}
PYTHON_CACHE_POLICY = {'kind':'owned-absent-prefix-no-bytecode-v1','prefix':'owned output/python-cache','write_bytecode':False}
CACHE_ROOTS = ('scripts','tests/playback')
MAX_CACHE_FILES, MAX_CACHE_BYTES = 128, 16*1024*1024
MAX_FILES, MAX_BLOB, MAX_SOURCE = 1500, 32*1024*1024, 128*1024*1024
MAX_LOG, MAX_ZIP, MAX_JSON = 4*1024*1024, 4*1024*1024, 1024*1024
ROOTS = ('Makefile','Cargo.toml','tests/web','tests/playback','crates/plurxd/src',
    'tools/web-types/package.json','tools/web-types/package-lock.json',
    'docs/clients/WEB-SHELL-LAYOUT.md','docs/clients/PLAYER-INPUT-CONTRACT.md',
    'docs/clients/PLAYBACK-SURFACE-CONTRACT.md','crates/plurx-core/tests/fixtures',
    'tests/contracts','tests/sharing/protocol-cases.json','tests/Images/cover.jpg',
    'tests/ui-structure.golden','THIRD-PARTY-NOTICES.md')
COMMANDS = (
    "scripts/js-check\ngit ls-files -z 'crates/plurxd/src/web/*.js' | xargs -0 -n1 node --check",
    'node tests/web/jsconfig-generated.test.js\nscripts/web-types --base "$PR_BASE_SHA"\nnode tests/web/player-typedef.test.js',
    'make web-unit-check',
)
LEGACY_JOB = '''  web_compile:
    name: fast web syntax gate
    needs: [scope, preflight]
    if: needs.scope.outputs.web_layout == 'true'
    runs-on: [self-hosted, Linux, X64, lab, general]
    # web-unit-check measured 103 s serial on two cores; the rest is checkout,
    # node and the pinned tsc.
    timeout-minutes: 10
    steps:
      - uses: https://data.forgejo.org/actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4
        with:
          persist-credentials: false
          # --base reads the baseline at the merge-base with the target.
          fetch-depth: 0
      - uses: https://data.forgejo.org/actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020 # v4
        with:
          node-version: "22"
      - name: Parse every shipped JavaScript surface
        run: |
          scripts/js-check
          git ls-files -z 'crates/plurxd/src/web/*.js' | xargs -0 -n1 node --check
      # TypeScript's checker over the same rows, against tests/web/
      # tsc-baseline.tsv: the counts must match exactly, so a new or raised
      # diagnostic fails and so does one that fell without the baseline being
      # lowered. --base also refuses a count the pull request raised in the
      # TSV without a new `# accepted` line for that key. The script runs
      # `npm ci` into tools/web-types from its lockfile (one package, no
      # dependencies) when the pinned tsc is not already there.
      - name: Type-check the web shell against its baseline
        env:
          PR_BASE_SHA: ${{ github.event.pull_request.base.sha }}
        run: |
          node tests/web/jsconfig-generated.test.js
          scripts/web-types --base "$PR_BASE_SHA"
          node tests/web/player-typedef.test.js
      # Every Node web and playback test, the same list `make web-check` runs
      # minus its two real-browser scripts. Until 2026-10-04 a pull request ran
      # two of them here and the rest ran only when someone typed make; a stale
      # assertion sat red on main for two days that way.
      - name: Run every Node web and playback test
        env:
          TMPDIR: /tmp
        run: make web-unit-check

'''
SHAPE_FAMILIES = {'fbbf4078d48ef894dcd1c154f5e404ac8656a99a': '97f338f93705406847d3987b80bf2f0fa8402755a4d1b80003f2434d5d44b814', '9569dd03dcbd4a84b408078f90601d075fe726a9': '4bddf7fb49972695846029e71e65c488052eefbd39a52e747b17b9c86dd28c6c'}
HELPERS = ('scripts/js-check', 'scripts/web-types', 'scripts/web-jsconfig', 'scripts/playback-lab', 'scripts/playback-frame-clock.js', 'scripts/continuous-quality-qualification', 'scripts/player-contract-table', 'scripts/ui-baseline', 'scripts/subtitle-readiness-browser-check', 'scripts/cq-native-shipping/check-evidence.js', 'tests/vendor/acorn.js', 'scripts/playback-startup-lab')
WRAPPER = '''      - name: Retain or execute the reviewed whole web cohort
        env:
          GITHUB_TOKEN: ${{ github.token }}
          PR_BASE_SHA: ${{ github.event.pull_request.base.sha }}
        run: python3 -m validation.web_cohort_retention --fresh-on-new-inputs
      - name: Preserve whole web cohort evidence
        if: always()
        uses: https://data.forgejo.org/forgejo/upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e # v4
        with:
          name: web-cohort-${{ github.run_id }}-${{ github.run_attempt }}
          path: target/validation/web-cohort-retention/
          if-no-files-found: error
          retention-days: 90

'''


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value,sort_keys=True,separators=(',',':')).encode()


def child_environment():
    secrets = {'GITHUB_TOKEN','GH_TOKEN','FORGEJO_TOKEN','GITEA_TOKEN','WEB_COHORT_API_TOKEN',
        'ACTIONS_RUNTIME_TOKEN','ACTIONS_ID_TOKEN_REQUEST_TOKEN'}
    return {key:value for key,value in os.environ.items() if key not in secrets}


def fresh_environment(prefix):
    env=child_environment()
    env.update(PYTHONPYCACHEPREFIX=str(prefix),PYTHONDONTWRITEBYTECODE='1')
    return env


def verify_fresh_cache(workspace,prefix):
    require(prefix.is_absolute() and not prefix.exists() and not prefix.is_symlink()
        and prefix.parent.is_dir() and prefix.parent.stat().st_uid == os.geteuid()
        and not any(p.is_symlink() for p in prefix.parents),
        'Fresh web cache prefix must be reserved, absent and nonaliased')
    code=('import sys;print(sys.implementation.name);print(sys.version.split()[0]);'
          'print(sys.pycache_prefix);print(int(sys.dont_write_bytecode))')
    observed=subprocess.check_output(['python3','-c',code],cwd=workspace,text=True,timeout=10,
        env=fresh_environment(prefix)).splitlines()
    require(len(observed)==4 and observed[0] and re.fullmatch(r'3\.[0-9]+\.[0-9]+',observed[1])
        and observed[2]==str(prefix) and observed[3]=='1' and not prefix.exists(),
        'Fresh web Python cache isolation is unsupported')
    return {'implementation':observed[0],'version':observed[1],'cache_policy':PYTHON_CACHE_POLICY}


def git(workspace,*args,limit=MAX_SOURCE):
    try:
        data = subprocess.check_output(['git',*args],cwd=workspace,timeout=20,
            stderr=subprocess.DEVNULL,env=child_environment())
    except (OSError,subprocess.SubprocessError):
        raise receipts.ReceiptError('Required web Git source unavailable') from None
    require(len(data) <= limit,'Web Git source exceeds bound')
    return data


def web_job(raw):
    text = raw.decode('utf-8')
    jobs = list(re.finditer(r'(?m)^  ([A-Za-z0-9_]+):\s*$',text))
    found = [i for i,m in enumerate(jobs) if m[1] == JOB_ID]
    require(len(found) == 1,'Web job shape missing/ambiguous')
    i = found[0]
    return text[jobs[i].start():jobs[i+1].start() if i+1 < len(jobs) else len(text)]


def adapted_job():
    return LEGACY_JOB[:LEGACY_JOB.index('      - name: Parse every shipped JavaScript surface\n')] + WRAPPER


def workflow_identity(workspace,commit):
    block = web_job(git(workspace,'show',commit+':'+WORKFLOW,limit=128*1024))
    require(block in (LEGACY_JOB,adapted_job()),'Unsupported web job/command/environment shape; closure review required')
    return {'sha256':digest(block.encode()),'kind':'legacy' if block == LEGACY_JOB else 'v1'}


def selectors(workspace,commit=None):
    if commit is None:
        require(not workspace.is_symlink(),'Web workspace is a symlink')
        with os.scandir(workspace) as children:
            names = [child.name for _,child in zip(range(513),children)]
    else:
        rows = git(workspace,'ls-tree','-z',commit,limit=32*1024).split(b'\0')
        names = [row.split(b'\t',1)[1].decode() for row in rows if row]
    require(len(names) <= 512,'Web Make selector inventory exceeds bound')
    require(all(name == 'Makefile' or name.casefold() not in ('makefile','gnumakefile') for name in names),
        'Unsupported web Make selector; closure review required')


def node_package_scope(workspace,commit):
    """Finite repository-local ancestor configs; no host/package crawler."""
    present = {}
    raw = git(workspace,'ls-tree','-z',commit,'--',*NODE_PACKAGE_SCOPES,limit=64*1024)
    for record in raw.split(b'\0'):
        if not record:continue
        header,name = record.split(b'\t',1);mode,kind,blob = header.decode().split();path = name.decode('utf-8')
        require(path in NODE_PACKAGE_SCOPES and path not in present and kind == 'blob'
            and mode in ('100644','100755'),'Unsupported web Node package scope type')
        require(int(git(workspace,'cat-file','-s',blob,limit=32)) <= MAX_JSON,'Web package configuration exceeds bound')
        data = git(workspace,'cat-file','blob',blob,limit=MAX_JSON)
        present[path] = {'mode':mode,'blob':blob,'sha256':digest(data)}
    require(present == NODE_PACKAGE_PINS,'Unsupported web Node package scope; closure review required')
    return [{'path':path,'identity':present.get(path)} for path in NODE_PACKAGE_SCOPES]


def live_node_packages(workspace):
    for relative in NODE_PACKAGE_SCOPES:
        location = workspace;present = None
        for index,part in enumerate(relative.split('/')):
            location = location/part
            try: mode = location.lstat().st_mode
            except FileNotFoundError:break
            if index < len(relative.split('/'))-1:
                require(stat.S_ISDIR(mode),'Unsupported live web package ancestor type')
                continue
            require(stat.S_ISREG(mode),'Unsupported live web Node package scope type')
            require(location.stat().st_size <= MAX_JSON,'Live web package configuration exceeds bound')
            data = location.read_bytes()
            present = {'mode':'100755' if mode & 0o111 else '100644',
                'blob':hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest(),'sha256':digest(data)}
        require(present == NODE_PACKAGE_PINS.get(relative),
            'Unsupported live web Node package scope; closure review required')


def live_cache_paths(workspace):
    """Only bounded real cache artifacts, never sibling import selectors.

    Their bytes are not certified: fresh execution uses a separate absent
    prefix and cannot consume these caches. Retention executes no helper.
    """
    allowed=set();count=total=0
    for root in CACHE_ROOTS:
        parent=workspace
        for part in root.split('/'):
            parent=parent/part
            require(not parent.is_symlink(),'Web cache search root contains a symlink')
        cache=parent/'__pycache__'
        try:mode=cache.lstat().st_mode
        except FileNotFoundError:continue
        require(stat.S_ISDIR(mode),'Unsupported web cache directory type')
        with os.scandir(cache) as children:
            for child in children:
                count += 1;info=child.stat(follow_symlinks=False);total += info.st_size
                require(count <= MAX_CACHE_FILES and total <= MAX_CACHE_BYTES
                    and stat.S_ISREG(info.st_mode) and child.name.endswith('.pyc'),
                    'Unsupported web cache member/bound')
                allowed.add((Path(root)/'__pycache__'/child.name).as_posix())
    return allowed


def live_inputs(workspace):
    selectors(workspace);live_node_packages(workspace)
    caches=live_cache_paths(workspace)
    for options in (('--others','--exclude-standard'),('--others','--ignored','--exclude-standard')):
        untracked=git(workspace,'ls-files','-z',*options,'--',*ROOTS,*HELPERS,limit=512*1024)
        require(all(path.decode('utf-8') in caches for path in untracked.split(b'\0') if path),
            'Untracked/ignored web closure input; clean reviewed checkout required')
    require(not any(os.environ.get(name) for name in ('MAKEFILES','MAKEFLAGS','GNUMAKEFLAGS',
        'NODE_OPTIONS','NODE_PATH','BASH_ENV','ENV','PYTHONPATH','PYTHONSTARTUP')),
        'Unsupported web interpreter configuration; closure review required')


def script_resolver_layout(workspace,commit):
    """Only top-level selector names/types/modes; unrelated bodies stay free."""
    raw = git(workspace,'ls-tree','-z',commit+':scripts',limit=64*1024)
    rows = []
    for record in raw.split(b'\0'):
        if not record:continue
        header,name = record.split(b'\t',1);mode,kind,_ = header.decode().split()
        rows.append({'name':name.decode('utf-8'),'mode':mode,'kind':kind})
    require(len(rows) <= 512,'Web script resolver namespace exceeds bound')
    rows.sort(key=lambda row:row['name'])
    require(digest(canonical(rows)) in SCRIPTS_RESOLVER_FAMILIES.values(),
        'Unsupported web script resolver namespace; closure review required')
    return rows


def live_script_resolvers(workspace,commit):
    expected = script_resolver_layout(workspace,commit);rows=[]
    directory = workspace/'scripts'
    require(not directory.is_symlink(),'Web script resolver directory is a symlink')
    with os.scandir(directory) as children:
        for index,child in enumerate(children):
            require(index < 513,'Live web script resolver namespace exceeds bound')
            mode = child.stat(follow_symlinks=False).st_mode
            # A normal cache directory is not itself an import selector. The
            # pinned sources never import __pycache__; do not delete its files.
            if child.name == '__pycache__' and stat.S_ISDIR(mode):continue
            require(stat.S_ISDIR(mode) or stat.S_ISREG(mode),'Unsupported live web script resolver type')
            rows.append({'name':child.name,'mode':'040000' if stat.S_ISDIR(mode) else ('100755' if mode & 0o111 else '100644'),
                'kind':'tree' if stat.S_ISDIR(mode) else 'blob'})
    require(sorted(rows,key=lambda row:row['name']) == expected,
        'Unsupported live web script resolver namespace; closure review required')


def shape_row(row):
    path = row['path']
    return path in ('Makefile','Cargo.toml',*HELPERS,'tools/web-types/package.json','tools/web-types/package-lock.json') or (
        path.startswith(('tests/web/','tests/playback/'))) or (
        path.startswith('crates/plurxd/src/web/') and path.endswith(('.js','.mjs','.cjs','.ts','.html','.json')))


def manifest(workspace,commit,base):
    receipts.sha(commit); receipts.sha(base)
    selectors(workspace,commit)
    resolver = script_resolver_layout(workspace,commit)
    packages = node_package_scope(workspace,commit)
    rows, contents, total = [], {}, 0
    raw = git(workspace,'ls-tree','-r','-z',commit,'--',*ROOTS,*HELPERS,limit=512*1024)
    for record in raw.split(b'\0'):
        if not record: continue
        header,name = record.split(b'\t',1); mode,kind,blob = header.decode().split(); path = name.decode('utf-8')
        require(mode in ('100644','100755') and kind == 'blob','Web closure contains symlink/submodule/unknown mode')
        require(path and not path.startswith('/') and '..' not in path.split('/') and ':' not in path
            and not any(c in path for c in '\\\r\n\0') and path not in contents,'Unsafe/duplicate web source path')
        require(len(rows) < MAX_FILES,'Web source member bound exceeded')
        size = int(git(workspace,'cat-file','-s',blob,limit=32))
        require(0 <= size <= MAX_BLOB and total+size <= MAX_SOURCE,'Web source byte bound exceeded')
        data = git(workspace,'cat-file','blob',blob,limit=MAX_BLOB)
        require(len(data) == size,'Web source blob size differs')
        total += size; contents[path] = data
        rows.append({'path':path,'mode':mode,'blob':blob,'sha256':digest(data)})
    rows.sort(key=lambda row:row['path'])
    require(set(HELPERS) <= set(contents) and BASELINE in contents and 'Makefile' in contents,'Incomplete web closure')
    require(digest(canonical([row for row in rows if shape_row(row)])) in SHAPE_FAMILIES.values(),
        'Unsupported web executable/recipe/dependency shape; closure review required')
    lock = receipts.bounded_json(contents['tools/web-types/package-lock.json'])
    require(lock['packages']['node_modules/typescript']['version'] == RUNTIME['typescript_lock'], 'Web locked TypeScript differs')
    merge = git(workspace,'merge-base',commit,base,limit=64).decode().strip(); receipts.sha(merge)
    baseline = git(workspace,'show',merge+':'+BASELINE,limit=MAX_JSON)
    return {'files':rows,'baseline':{'event_base':base,'merge_base':merge,'sha256':digest(baseline)},
        'runtime_contract':RUNTIME,'script_resolver_sha256':digest(canonical(resolver)),'node_package_scope_sha256':digest(canonical(packages)),'policy_sha256':policy_fingerprint()}


def policy_fingerprint():
    return digest(canonical({'version':POLICY_VERSION,'roots':ROOTS,'helpers':HELPERS,'shape_families':SHAPE_FAMILIES,
        'runtime':RUNTIME,'python_policy':PYTHON_POLICY,'fresh_python_cache':PYTHON_CACHE_POLICY,'cache_roots':CACHE_ROOTS,
        'cache_bounds':[MAX_CACHE_FILES,MAX_CACHE_BYTES],'node_package_scopes':NODE_PACKAGE_SCOPES,'node_package_pins':NODE_PACKAGE_PINS,'script_resolver_families':SCRIPTS_RESOLVER_FAMILIES,'commands':COMMANDS,'legacy_job':digest(LEGACY_JOB.encode()),
        'adapter_job':digest(adapted_job().encode()),'baseline':'event-base+merge-base+bytes',
        'host':'Linux/X64 host; python3 stdlib and fixed Make recipe compatibility v1'}))


def execution_key(inputs):
    return digest(canonical(inputs))


def runtime():
    require(sys.platform == 'linux' and platform.machine() in ('x86_64','AMD64'), 'Web host must be Linux/X64')
    actual = {name:subprocess.check_output(command,text=True,env=child_environment(),timeout=10).strip()
        for name,command in (('node',['node','--version']),('npm',['npm','--version']),('git',['git','--version']))}
    require(actual == {k:v for k,v in RUNTIME.items() if k != 'typescript_lock'},'Unsupported web concrete runtime family')
    return {'job_recorded':actual,'typescript':{'declared_locked':RUNTIME['typescript_lock'],'current_binary_observed':None,'current_qualification_execution':False},
        'policy_compatible':{'host':'Linux/X64 host executor','python':PYTHON_POLICY,'make':'fixed reviewed recipe'},
        'current_diagnostics':{'python':{'implementation':platform.python_implementation(),'version':platform.python_version()},'make':subprocess.check_output(['make','--version'],
            text=True,env=child_environment(),timeout=10).splitlines()[0]},
        'historical_not_recorded':['OS distribution','kernel','Python version','Python cache policy','Make version','immutable host/image identity']}


def current_context(workspace):
    require(os.environ.get('GITHUB_JOB') == JOB_ID and os.environ.get('GITHUB_RUN_ATTEMPT','1') == '1'
        and os.environ.get('GITHUB_EVENT_NAME') == 'pull_request','Web adapter needs its initial ready PR job')
    with Path(os.environ['GITHUB_EVENT_PATH']).open('rb') as stream: raw = stream.read(128*1024+1)
    require(len(raw) <= 128*1024,'Web event exceeds bound')
    event = receipts.bounded_json(raw); pull,repo = event['pull_request'],event['repository']
    require(repo['full_name'] == os.environ['GITHUB_REPOSITORY'] and event['number'] == pull['number'], 'Web current repository/PR differs')
    current = git(workspace,'rev-parse','HEAD',limit=64).decode().strip()
    require(current == pull['head']['sha'] == os.environ['GITHUB_SHA'] and not git(workspace,'diff','--name-only','HEAD'),
        'Web checkout/head changed')
    root = os.environ['GITHUB_API_URL']
    require(root.startswith(os.environ['GITHUB_SERVER_URL'].rstrip('/')+'/'),'Web API origin differs')
    api = receipts.API(root,os.environ['GITHUB_REPOSITORY'],os.environ.get('GITHUB_TOKEN'))
    rid = receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    scope = main_receipts.identity(api,event['number'],current); run = api.get('/actions/runs/'+str(rid))
    require(run.get('id') == rid and run.get('commit_sha') == current and adoption.bind_event(run,scope) == event,
        'Web current API event/head differs')
    live = api.get('/pulls/'+str(scope['pr']))
    require(live['head']['sha'] == current and live['base']['sha'] == pull['base']['sha'] and live['draft'] is False
        and live['state'] == 'open' and live['base']['ref'] == 'main'
        and live['head']['repo']['id'] == live['base']['repo']['id'] == repo['id'],'Web current PR moved/closed/drafted')
    jobs = [job for job in api.pages('/actions/runs/'+str(rid)+'/jobs') if job.get('name') == JOB]
    require(len(jobs) == 1,'Web current job missing/ambiguous')
    check_job(jobs[0],scope,rid,False)
    require(jobs[0]['status'] in ('running','in_progress'),'Web current job is not active')
    return api,scope,current,run,jobs[0],pull['base']['sha']


def check_job(job,scope,rid,successful=True):
    require(type(job.get('id')) is int and job['id'] > 0 and job.get('name') == JOB
        and type(job.get('repo_id')) is int and job['repo_id'] == scope['repository']
        and type(job.get('run_id')) is int and job['run_id'] == rid
        and type(job.get('attempt')) is int and job['attempt'] == 1
        and type(job.get('task_id')) is int and job['task_id'] > 0,'Web job repository/run/task/attempt differs')
    if successful: require(job.get('status') == 'success','Web prior job did not succeed')


def checkout(raw,commit,job):
    require(len(raw) <= MAX_LOG,'Web historical log exceeds bound')
    lines = main_receipts.log_lines(raw)
    header = '[command]/usr/bin/git log -1 --format=%H'
    positions = [i for i,line in enumerate(lines) if line == header]
    require(len(positions) == 1 and positions[0]+1 < len(lines) and lines[positions[0]+1] == commit,
        'Web pinned checkout final HEAD differs')
    banners = [line for line in lines if re.fullmatch(r'Runner [0-9a-f-]+ \(version:[^)]*\) received task '+
        str(job['task_id'])+r' of job web_compile, triggered by event: pull_request',line)]
    require(len(banners) == 1,'Web job log task differs')
    require(lines and lines[-1] == '🏁  Job succeeded','Web job log incomplete/failed')
    # The pinned checkout reports Git both during initialization and Post.
    # Bind each observation to its real action section, never to an arbitrary
    # number of version strings a test could print elsewhere in the log.
    action = 'https://data.forgejo.org/actions/checkout@11d5960a326750d5838078e36cf38b85af677262'
    def unique(line):
        found = [i for i,value in enumerate(lines) if value == line]
        require(len(found) == 1,'Web runtime section missing/ambiguous')
        return found[0]
    main = unique('⭐ Run Main '+action)
    info = unique('##[group]Getting Git version info')
    clean = unique('::remove-matcher owner=checkout-git::')
    environment = unique('##[group]Environment details')
    post = unique('⭐ Run Post '+action)
    finished = [i for i,line in enumerate(lines) if line.strip() == '✅  Success - Post '+action]
    require(len(finished) == 1,'Web checkout Post section missing/ambiguous')
    commands = [i for i,line in enumerate(lines) if line == '[command]/usr/bin/git version']
    versions = [i for i,line in enumerate(lines) if line.startswith('git version ')]
    require(len(commands) == 2 and versions == [i+1 for i in commands]
        and all(lines[i] == RUNTIME['git'] for i in versions)
        and main < info and commands[0] == info+2
        and re.fullmatch(r"Working directory is '[^'\r\n]+'",lines[info+1]) is not None
        and lines[commands[0]+2] == '##[endgroup]'
        and commands[0]+2 < positions[0] < clean < environment < post
        and post < commands[1] <= post+16 and commands[1]+1 < finished[0] < len(lines)-1,
        'Web checkout Git runtime sections differ')
    require(environment+3 < len(lines) and lines[environment+1] == 'node: '+RUNTIME['node']
        and lines[environment+2] == 'npm: '+RUNTIME['npm']
        and [i for i,line in enumerate(lines) if line.startswith(('node: ','npm: '))]
            == [environment+1,environment+2]
        and any(lines[i] == '##[endgroup]' for i in range(environment+3,min(environment+9,post))),
        'Web Node runtime section differs')
    require(lines[0] == banners[0] and sum(line.startswith('Runner ') for line in lines) == 1,
        'Web trusted runner preamble differs')
    return lines


def authenticate_job(api,scope,workspace,rid,jid):
    run = api.get('/actions/runs/'+str(receipts.positive(rid))); adoption.bind_event(run,scope)
    require(run.get('id') == rid,'Web prior run differs')
    jobs = [job for job in api.pages('/actions/runs/'+str(rid)+'/jobs') if job.get('id') == jid]
    require(len(jobs) == 1,'Web prior job missing/ambiguous'); job = jobs[0]; check_job(job,scope,rid)
    raw = api.bytes('/actions/jobs/'+str(receipts.positive(jid))+'/logs')
    lines = checkout(raw,run['commit_sha'],job)
    event = adoption.bind_event(run,scope)
    inputs = manifest(workspace,run['commit_sha'],event['pull_request']['base']['sha'])
    return run,job,raw,lines,inputs


def legacy_result(api,scope,workspace):
    require(scope['repository'] == 1 and scope['pr'] == 1263,'Known web origin belongs to another repository/PR')
    run,job,raw,lines,inputs = authenticate_job(api,scope,workspace,ORIGIN_RUN,ORIGIN_JOB)
    require(run['commit_sha'] == PRODUCER and job['task_id'] == ORIGIN_TASK
        and digest(raw) == ORIGIN_LOG_SHA256,'Known web origin source/log/task differs')
    require(workflow_identity(workspace,PRODUCER)['kind'] == 'legacy','Known web producer recipe differs')
    return {'kind':'legacy','run':ORIGIN_RUN,'job':ORIGIN_JOB,'attempt':1,'commit':PRODUCER,
        'log_sha256':digest(raw),'input_key':execution_key(inputs),
        'origin_observed_tools':{'node':'v22.23.4','npm':'10.9.9','git':'git version 2.55.0','typescript':'6.0.3'},
        'historical_not_recorded':['OS distribution','kernel','Python version','Python cache policy','Make version','immutable host/image identity']},raw


def unpack(data):
    require(len(data) <= MAX_ZIP,'Web archive exceeds bound')
    result = {}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        require(len(archive.infolist()) == 2,'Web archive member set differs')
        for info in archive.infolist():
            require(info.filename in ('receipt.json','web-cohort.log') and info.filename not in result
                and stat.S_IFMT(info.external_attr >> 16) in (0,stat.S_IFREG) and not info.flag_bits & 1
                and info.compress_type in (zipfile.ZIP_STORED,zipfile.ZIP_DEFLATED), 'Unsafe/duplicate web archive member')
            bound = MAX_JSON if info.filename == 'receipt.json' else MAX_LOG
            require(0 <= info.file_size <= bound and info.file_size <= max(info.compress_size,1)*250,
                'Web archive member size/ratio exceeds bound')
            with archive.open(info) as stream: raw = stream.read(bound+1)
            require(len(raw) == info.file_size,'Web archive member size differs'); result[info.filename] = raw
    return receipts.bounded_json(result['receipt.json']),result['web-cohort.log']


def fresh_log(raw,key):
    require(len(raw) <= MAX_LOG,'Web cohort log exceeds bound')
    lines = raw.decode('utf-8').splitlines()
    expected = ['WEB-COHORT-START '+key]
    for i in range(len(COMMANDS)): expected += [f'WEB-COHORT-COMMAND {i} START',f'WEB-COHORT-COMMAND {i} PASS']
    expected += ['WEB-COHORT-TYPESCRIPT Version 6.0.3','WEB-COHORT-FINISH '+key+' 0']
    require([line for line in lines if line.startswith('WEB-COHORT-')] == expected,'Web cohort command sequence incomplete/ambiguous')


def v1_result(api,scope,workspace,rid,jid,current,*,root_only=False):
    run,job,raw,lines,inputs = authenticate_job(api,scope,workspace,rid,jid)
    require(workflow_identity(workspace,run['commit_sha'])['kind'] == 'v1','Web v1 producer job shape differs')
    require(git(workspace,'show',run['commit_sha']+':'+ADAPTER,limit=256*1024) ==
        git(workspace,'show',current+':'+ADAPTER,limit=256*1024),'Web adapter compatibility unknown')
    name = f'web-cohort-{rid}-1'
    matches = [a for a in api.pages('/actions/artifacts',{'name':name}) if a.get('name') == name and a.get('run_id') == rid and not a.get('expired')]
    require(len(matches) == 1,'Web cohort artifact missing/ambiguous'); artifact = matches[0]
    aid,size = receipts.positive(artifact['id']),receipts.positive(artifact['size_in_bytes'])
    require(size <= MAX_ZIP,'Web cohort archive exceeds bound')
    sha = transport.upload_transaction_sha256(lines,name,aid,size)
    data = api.bytes('/actions/artifacts/'+str(aid)+'/zip')
    require(len(data) == size and digest(data) == sha,'Web cohort uploaded archive digest/size differs')
    receipt,log = unpack(data)
    require(set(receipt) == {'schema_version','mode','commit','tree','run','job','attempt','scope','inputs','input_key',
        'policy_sha256','adapter_sha256','web_job','runtime','python_cache_execution','origin','log_sha256','current_test_events'},'Web receipt field set differs')
    key = execution_key(inputs)
    require(type(receipt['schema_version']) is int and receipt['schema_version'] == 1
        and type(receipt['run']) is int and type(receipt['job']) is int
        and receipt['runtime']['job_recorded'] == {k:v for k,v in RUNTIME.items() if k != 'typescript_lock'}
        and receipt['runtime']['policy_compatible']['python'] == PYTHON_POLICY
        and isinstance(receipt['runtime']['current_diagnostics']['python']['implementation'],str)
        and receipt['runtime']['current_diagnostics']['python']['implementation']
        and re.fullmatch(r'3\.[0-9]+\.[0-9]+',receipt['runtime']['current_diagnostics']['python']['version']) is not None
        and receipt['runtime']['typescript'] == ({'declared_locked':'6.0.3','current_binary_observed':'Version 6.0.3','current_qualification_execution':True}
            if receipt['mode'] == 'fresh_execution' else {'declared_locked':'6.0.3','current_binary_observed':None,'current_qualification_execution':False})
        and receipt['commit'] == run['commit_sha'] and receipt['tree'] ==
        git(workspace,'rev-parse',run['commit_sha']+'^{tree}',limit=64).decode().strip()
        and receipt['run'] == rid and receipt['job'] == jid and type(receipt['attempt']) is int and receipt['attempt'] == 1
        and receipt['scope'] == scope and receipt['inputs'] == inputs and receipt['input_key'] == key
        and receipt['policy_sha256'] == policy_fingerprint() and receipt['adapter_sha256'] == digest(git(workspace,'show',current+':'+ADAPTER,limit=256*1024))
        and receipt['web_job'] == workflow_identity(workspace,run['commit_sha']) and receipt['log_sha256'] == digest(log),
        'Web receipt API/source/policy/input binding differs')
    marker = 'WEB-COHORT-RECEIPT '+digest(canonical(receipt))
    require(lines.count(marker) == 1,'Web receipt not bound to authenticated job log')
    if receipt['mode'] == 'fresh_execution':
        cache=receipt['python_cache_execution']
        require(isinstance(cache,dict) and set(cache)=={'implementation','version','cache_policy'}
            and isinstance(cache['implementation'],str) and cache['implementation']
            and re.fullmatch(r'3\.[0-9]+\.[0-9]+',cache['version']) is not None
            and cache['cache_policy']==PYTHON_CACHE_POLICY,'Web fresh Python cache policy differs')
        require(receipt['origin'] is None and receipt['current_test_events'] is True,'Web fresh origin/event claim differs')
        fresh_log(log,key)
        return {'kind':'v1_fresh','run':rid,'job':jid,'attempt':1,'commit':run['commit_sha'],
            'log_sha256':digest(log),'input_key':key,'artifact_id':aid,'artifact_sha256':sha,'job_log_sha256':digest(raw)},log
    require(not root_only and receipt['mode'] == 'retained_prior_execution' and receipt['current_test_events'] is False
        and receipt['python_cache_execution'] is None,
        'Web retained origin must point directly to real execution')
    pointer = receipt['origin']; require(isinstance(pointer,dict),'Web retained origin missing')
    if pointer.get('kind') == 'legacy': result,original = legacy_result(api,scope,workspace)
    else:
        require(pointer.get('kind') == 'v1_fresh' and (pointer.get('run'),pointer.get('job')) != (rid,jid), 'Web retained origin cycle/chain')
        result,original = v1_result(api,scope,workspace,pointer['run'],pointer['job'],current,root_only=True)
    require(result == pointer and original == log and result['input_key'] == key,'Web retained original provenance/input differs')
    return result,original


def matching_result(api,scope,workspace,current,current_run,key):
    # This protocol recognizes the explicit legacy origin and v1 producers.
    # Older heads without cohort evidence are outside it, not proven passes
    # or source inequalities. Never parse them with today's closure family.
    runs = api.pages('/actions/runs',{'workflow_id':'main-fast-lane.yml',
        'ref':f"refs/pull/{scope['pr']}/head"},'workflow_runs')
    seen = set()
    for listed in runs:
        rid = receipts.positive(listed['id'])
        if rid in seen or rid == current_run or rid == ORIGIN_RUN or listed.get('prettyref') != '#'+str(scope['pr']): continue
        seen.add(rid)
        run = api.get('/actions/runs/'+str(rid)); event = adoption.bind_event(run,scope,ready=False)
        require(run.get('id') == rid and run.get('commit_sha') == listed.get('commit_sha'),'Web history metadata changed')
        artifacts = [item for item in api.pages('/actions/runs/'+str(rid)+'/artifacts')
            if re.fullmatch(r'web-cohort-'+str(rid)+r'-[1-9][0-9]*',str(item.get('name','')))]
        workflow = git(workspace,'show',run['commit_sha']+':'+WORKFLOW,limit=128*1024)
        if not artifacts and b'validation.web_cohort_retention' not in workflow:
            continue
        require(workflow_identity(workspace,run['commit_sha'])['kind'] == 'v1',
            'Eligible web producer has unsupported recipe; closure review required')
        require(git(workspace,'show',run['commit_sha']+':'+ADAPTER,limit=256*1024) ==
            git(workspace,'show',current+':'+ADAPTER,limit=256*1024),
            'Eligible web producer adapter compatibility unknown; closure review required')
        jobs = [j for j in api.pages('/actions/runs/'+str(rid)+'/jobs') if j.get('name') == JOB]
        if not artifacts and len(jobs) == 1 and jobs[0].get('status') == 'skipped' and type(jobs[0].get('task_id')) is int and jobs[0]['task_id'] == 0:
            require(jobs[0].get('repo_id') == scope['repository'] and jobs[0].get('run_id') == rid,
                'Never-started web producer identity differs')
            continue
        adoption.bind_event(run,scope)
        inputs = manifest(workspace,run['commit_sha'],event['pull_request']['base']['sha'])
        if execution_key(inputs) != key: continue
        require(len(jobs) == 1 and jobs[0].get('status') == 'success',
            'Matching web history is incomplete/ambiguous; no fallback execution')
        require(len(artifacts) == 1 and artifacts[0].get('name') == f'web-cohort-{rid}-1'
            and type(artifacts[0].get('run_id')) is int and artifacts[0]['run_id'] == rid
            and artifacts[0].get('expired') is False,
            'Matching web cohort artifact missing/invalid/ambiguous; no fallback execution')
        return v1_result(api,scope,workspace,rid,jobs[0]['id'],current)
    return None


def write_receipt(output,receipt):
    receipts.atomic_json(output/'receipt.json',receipt)
    print('WEB-COHORT-RECEIPT '+digest(canonical(receipt)),flush=True)


def run_gate(workspace,context,output,actual_runtime,fresh_on_new_inputs=False):
    api,scope,current,run,job,base = context
    require(workflow_identity(workspace,current)['kind'] == 'v1','Current web job lacks reviewed adapter')
    live_inputs(workspace);live_script_resolvers(workspace,current)
    inputs = manifest(workspace,current,base); key = execution_key(inputs)
    origin_inputs = manifest(workspace,PRODUCER,'9bcfa9ca1f792d214122762981c84c85ba5b3e64')
    original = legacy_result(api,scope,workspace) if scope['repository'] == 1 and scope['pr'] == 1263 and key == execution_key(origin_inputs) else matching_result(api,scope,workspace,current,run['id'],key)
    receipt = {'schema_version':1,'mode':'retained_prior_execution' if original else 'fresh_execution',
        'commit':current,'tree':git(workspace,'rev-parse',current+'^{tree}',limit=64).decode().strip(),
        'run':run['id'],'job':job['id'],'attempt':1,'scope':scope,'inputs':inputs,'input_key':key,
        'policy_sha256':policy_fingerprint(),'adapter_sha256':digest(git(workspace,'show',current+':'+ADAPTER,limit=256*1024)),
        'web_job':workflow_identity(workspace,current),'runtime':actual_runtime,'python_cache_execution':None,'origin':None,'log_sha256':None,'current_test_events':False}
    # Reauthenticate the ready head and live selector/source boundary immediately
    # before either applicability emission or the first web command.
    again = current_context(workspace)
    require(again[2] == current and again[3]['id'] == run['id'] and again[4]['id'] == job['id']
        and again[4]['task_id'] == job['task_id'] and again[5] == base,'Current web API/source drifted')
    live_inputs(workspace);live_script_resolvers(workspace,current)
    require(manifest(workspace,current,base) == inputs and not git(workspace,'diff','--name-only','HEAD'),'Current web source changed before decision')
    if original:
        receipt['origin'],raw = original
        require(receipt['origin']['input_key'] == key,'Web retained applicability differs')
        (output/'web-cohort.log').write_bytes(raw)
    else:
        require(fresh_on_new_inputs,'New web inputs require explicit fresh execution policy')
        receipt['current_test_events'] = True
        cache_prefix=output.absolute()/'python-cache'
        receipt['python_cache_execution']=verify_fresh_cache(workspace,cache_prefix)
        path = output/'web-cohort.log'
        with path.open('xb') as stream:
            def marker(value): stream.write((value+'\n').encode()); stream.flush(); os.fsync(stream.fileno())
            marker('WEB-COHORT-START '+key)
            # Persist identity before execution; interruption is never a complete pass.
            write_receipt(output,receipt)
            for i,command in enumerate(COMMANDS):
                marker(f'WEB-COHORT-COMMAND {i} START')
                env = fresh_environment(cache_prefix); env.pop('PR_BASE_SHA',None)
                if i == 1: env['PR_BASE_SHA'] = base
                if i == 2: env['TMPDIR'] = '/tmp'
                completed = subprocess.run(['bash','-euo','pipefail','-c',command],cwd=workspace,env=env,stdout=stream,stderr=subprocess.STDOUT)
                require(completed.returncode == 0,'Fresh whole web cohort failed; no retention/fallback')
                require(path.stat().st_size <= MAX_LOG,'Fresh web log exceeds bound')
                marker(f'WEB-COHORT-COMMAND {i} PASS')
            version = subprocess.check_output(['node','tools/web-types/node_modules/typescript/bin/tsc','--version'],
                cwd=workspace,text=True,timeout=10,env=fresh_environment(cache_prefix)).strip()
            require(version == 'Version 6.0.3','Fresh web TypeScript binary differs from locked compiler')
            marker('WEB-COHORT-TYPESCRIPT '+version)
            receipt['runtime']['typescript'] = {'declared_locked':'6.0.3','current_binary_observed':version,'current_qualification_execution':True}
            marker('WEB-COHORT-FINISH '+key+' 0')
        require(not cache_prefix.exists() and not cache_prefix.is_symlink(),
            'Fresh web Python cache prefix was populated or aliased')
        raw = path.read_bytes(); fresh_log(raw,key)
    live_inputs(workspace);live_script_resolvers(workspace,current)
    require(git(workspace,'rev-parse','HEAD',limit=64).decode().strip() == current
        and not git(workspace,'diff','--name-only','HEAD') and manifest(workspace,current,base) == inputs,
        'Web source changed during cohort; no receipt acceptance')
    receipt['log_sha256'] = digest(raw)
    write_receipt(output,receipt)
    print('Web cohort '+receipt['mode']+'; original provenance preserved; current test events='+str(receipt['current_test_events']),flush=True)
    return receipt


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fresh-on-new-inputs',action='store_true'); args = parser.parse_args(argv)
    workspace = Path.cwd().resolve(); context = current_context(workspace); actual = runtime()
    output = workspace/OUTPUT
    require(not any(p.is_symlink() for p in (output,*output.parents)),'Web output contains symlink')
    output.parent.mkdir(parents=True,exist_ok=True)
    require(not output.exists(),'Web decision already attempted; no duplicate fallback')
    output.mkdir(mode=0o700)
    run_gate(workspace,context,output,actual,args.fresh_on_new_inputs)


if __name__ == '__main__':
    try: main()
    except (receipts.ReceiptError,KeyError,TypeError,ValueError,OSError,zipfile.BadZipFile):
        # Never expose token-bearing response objects or uncontrolled exception text.
        print('Whole web cohort refused: evidence/source/runtime/recipe authentication failed; no fallback execution',file=sys.stderr)
        raise SystemExit(1)
