"""Pure web source/runtime guards used by validation.web_unit_receipts.

No historical cohort consumer, execution controller, receipt writer or CLI.
"""

from __future__ import annotations
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import sys
from validation import main_preflight_adoption as adoption
from validation import main_unit_receipts as main_receipts
from validation import python_unit_receipts as receipts

require = receipts.require

JOB, JOB_ID = 'fast web syntax gate', 'web_compile'

BASELINE = 'tests/web/tsc-baseline.tsv'

RUNTIME = {'node':'v22.23.4','npm':'10.9.9','git':'git version 2.55.0','typescript_lock':'6.0.3'}

POLICY_VERSION = 3

SCRIPTS_RESOLVER_FAMILIES = {'fbbf4078d48ef894dcd1c154f5e404ac8656a99a': '48743b916c2a75d4e0e9531a52abff0b9f318d726be407f84d5fe1cc760e3a99', '087e836d369eb2b8f94ec1d0454ea6c157612df9': '72f0accba4f82129287fb12cb8b53c5739cf49f4dff530eb6d64bc464be6ec61', 'integrated-main-77e985-v1': '1ba18caa8750ee8b6f3a1b3aeaf5ed7954c531fd92faa7fb1ff548c2dca8caf2', 'integrated-main-28edf-v1': 'ca4337b7b6840903eba37aa11afec90ca10e59d7e34bf442346064cf0a80e945'}

PYTHON_POLICY = 'python3 stdlib planning, summary and loopback HTTP fixtures v1'

NODE_PACKAGE_SCOPES = ('crates/package.json', 'crates/plurxd/package.json', 'crates/plurxd/src/package.json', 'crates/plurxd/src/web/core/package.json', 'crates/plurxd/src/web/detail/package.json', 'crates/plurxd/src/web/layouts/package.json', 'crates/plurxd/src/web/package.json', 'crates/plurxd/src/web/pages/package.json', 'crates/plurxd/src/web/player/package.json', 'crates/plurxd/src/web/types/package.json', 'package.json', 'scripts/cq-native-shipping/package.json', 'scripts/package.json', 'tests/package.json', 'tests/playback/continuous-quality/package.json', 'tests/playback/package.json', 'tests/vendor/package.json', 'tests/web/package.json')

NODE_PACKAGE_PINS = {}

PYTHON_CACHE_POLICY = {'kind':'owned-absent-prefix-no-bytecode-v1','prefix':'owned output/python-cache','write_bytecode':False}

CACHE_ROOTS = ('scripts','tests/playback')

MAX_CACHE_FILES, MAX_CACHE_BYTES = 128, 16*1024*1024

MAX_FILES, MAX_BLOB, MAX_SOURCE = 1500, 32*1024*1024, 128*1024*1024

MAX_JSON = 1024*1024

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

SHAPE_FAMILIES = {'fbbf4078d48ef894dcd1c154f5e404ac8656a99a': '97f338f93705406847d3987b80bf2f0fa8402755a4d1b80003f2434d5d44b814', '9569dd03dcbd4a84b408078f90601d075fe726a9': '4bddf7fb49972695846029e71e65c488052eefbd39a52e747b17b9c86dd28c6c', 'integrated-main-94da-census-v1': 'fb5fd36641c7a21899f01dc9f20473c8085f794739c5de0c907eac251a9969ea', 'integrated-main-656a-v1': 'e11a97162c43e2637c83af099533a3024dc2302872d05e17f4791d0c80dfe35a', 'integrated-main-77e985-v1': 'ccdc1e9b04e7e77ef72d48c93b77481ade06d20d56dd044051daa6a16dc9a951', 'integrated-main-5ac9-v1': 'b905b36a381ff87c747f86687a419bbfaaa4b586efa176f76091752020a832ef', 'integrated-main-28edf-v1': '356a8799b84dd1941c02393b39af8d0c24f0a4b19b7dd5e359801abb9583fdb7', 'integrated-main-df275-v1': '0ce4c06155baadfb15bdb3898fb0985fa505a4990306707fef0673984240736b', 'integrated-main-eb322-v1': 'ebdcc67734310b61836be18ffe9c999aa30e0aae6ddf98fac8d30f530e787ab2'}

HELPERS = ('scripts/js-check', 'scripts/web-types', 'scripts/web-jsconfig', 'scripts/playback-lab', 'scripts/playback-frame-clock.js', 'scripts/continuous-quality-qualification', 'scripts/player-contract-table', 'scripts/ui-baseline', 'scripts/subtitle-readiness-browser-check', 'scripts/cq-native-shipping/check-evidence.js', 'tests/vendor/acorn.js', 'scripts/playback-startup-lab')

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
    return digest(canonical({'version': POLICY_VERSION, 'roots': ROOTS, 'helpers': HELPERS,
        'shape_families': SHAPE_FAMILIES, 'runtime': RUNTIME, 'python_policy': PYTHON_POLICY,
        'fresh_python_cache': PYTHON_CACHE_POLICY, 'cache_roots': CACHE_ROOTS,
        'cache_bounds': [MAX_CACHE_FILES, MAX_CACHE_BYTES], 'node_package_scopes': NODE_PACKAGE_SCOPES,
        'node_package_pins': NODE_PACKAGE_PINS, 'script_resolver_families': SCRIPTS_RESOLVER_FAMILIES,
        'commands': COMMANDS, 'baseline': 'authenticated-event-base+merge-base+bytes'}))

def runtime():
    require(sys.platform == 'linux' and platform.machine() in ('x86_64','AMD64'), 'Web host must be Linux/X64')
    actual = {name:subprocess.check_output(command,text=True,env=child_environment(),timeout=10).strip()
        for name,command in (('node',['node','--version']),('npm',['npm','--version']),('git',['git','--version']))}
    require(actual == {k:v for k,v in RUNTIME.items() if k != 'typescript_lock'},'Unsupported web concrete runtime family')
    return {'job_recorded':actual,'typescript':{'declared_locked':RUNTIME['typescript_lock'],'current_binary_observed':None,'current_qualification_execution':False},
        'policy_compatible':{'host':'Linux/X64 host executor','python':PYTHON_POLICY,'make':'fixed reviewed recipe'},
        'current_diagnostics':{'python':{'implementation':platform.python_implementation(),'version':platform.python_version()},'make':subprocess.check_output(['make','--version'],
            text=True,env=child_environment(),timeout=10).splitlines()[0]},
        'unrecorded':['OS distribution','kernel','Python version','Python cache policy','Make version','immutable host/image identity']}

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
    adoption.validate_live_pr(live,event,current)
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
