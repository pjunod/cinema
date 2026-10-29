"""Registered, bounded schema-6 proof sessions and exact-head Main aggregation.

All remote authority is read from authenticated API producers and uploader logs.
The mutable controller journal is progress, never proof or a completion receipt.
"""
from __future__ import annotations

import argparse
import base64
import copy
from datetime import datetime, timezone
import fnmatch
import fcntl
import http.client
import json
import os
from pathlib import Path
import resource
import re
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.request
import urllib.parse
import urllib.error

from validation import async_refinement_provenance as p
from validation import main_preflight_adoption as adoption
from validation import main_unit_receipts
from validation import python_unit_receipts as receipts

STAGES = ('expand', 'semantic_verify', 'monitor_scc_verify', 'temporal_components',
          'backend_qualification', 'complete')
MAIN_JOBS = {'paperwork': 'paperwork', 'scope': 'fast-lane validation scope',
             'preflight': 'fast policy and contract preflight',
             'worker_control_proof': 'production worker controller proof',
             'rust_compile': 'fast Rust compile gate', 'web_compile': 'fast web syntax gate',
             'apple_compile': 'fast Apple compile gate', 'android_compile': 'fast Android compile gate'}
# Exact job names are read from the bound workflow, not guessed from these labels.
MAIN_ENV = {'paperwork': 'PAPERWORK_RESULT', 'scope': 'SCOPE_RESULT', 'preflight': 'PREFLIGHT_RESULT',
            'worker_control_proof': 'WORKER_CONTROL_RESULT', 'rust_compile': 'RUST_RESULT',
            'async_refinement': 'ASYNC_DRIVER_RESULT', 'web_compile': 'WEB_RESULT',
            'apple_compile': 'APPLE_RESULT', 'android_compile': 'ANDROID_RESULT'}
CONTROL_KEYS=('schema_version','registration','resource_plan','probe_run','unit_runs','attempt_runs',
              'resume_decisions','main_run','session_id')
NULL_PROGRESS = {'context': None, 'record_kind': None, 'after_key': None, 'fifo_ticket': None,
                 'component_id': None, 'family': None, 'case_index': 0}
REGISTRATION_KEYS = ('schema_version', 'source', 'repository', 'repository_name', 'pr', 'event_base',
                     'ref', 'workflow', 'event', 'modes', 'source_files', 'source_archive_sha256',
                     'accounting_method_sha256','accounting_method',
                     'storage', 'runtime', 'runner', 'route', 'expires_at', 'cases', 'main_workflow_sha256', 'upload_slot_count')



ROUTE_FILE='validation/runner-fleet.toml'
ROUTE_LABELS=['self-hosted','Linux','X64','lab','general','high-cpu','async-proof-owned']


def route_contract(root):
    fleet=tomllib.loads((Path(root)/ROUTE_FILE).read_text())
    route=fleet.get('async_proof_route')
    expected={'state':route.get('state') if type(route) is dict else None,
        'logical_slot':'gha-lab5-general-03','labels':ROUTE_LABELS,
        'mount_root':'/srv/plurx-async-proof',
        'container_bind':'/srv/plurx-async-proof:/srv/plurx-async-proof'}
    p.require(route==expected and route['state'] in ('provisioning_required','active'),'dedicated proof routing/provisioning contract differs')
    owners=[row for row in fleet['runners'] if row['name']==route['logical_slot']]
    p.require(len(owners)==1 and owners[0]['os']=='Linux' and owners[0]['arch']=='X64',
              'logical proof slot has no unique roster owner')
    installed=[row['name'] for row in fleet['runners'] if 'async-proof-owned' in row['labels']]
    p.require(installed==([] if route['state']=='provisioning_required' else [route['logical_slot']]),
              'planned/active proof route does not match installed roster labels')
    return route


def registration_route(reg,root):
    route=route_contract(root);binding=reg['route']
    p.closed(binding,('logical_slot','contract_sha256','runner_id','runner_uuid'))
    receipts.positive(binding['runner_id'])
    p.require(type(binding['runner_uuid']) is str and re.fullmatch(
        r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',binding['runner_uuid']) is not None,
        'actual scheduler runner UUID is absent')
    p.closed(reg['runner'],('name','os','arch'))
    p.require(binding['logical_slot']==route['logical_slot']
              and binding['contract_sha256']==p.file_sha(Path(root)/ROUTE_FILE)
              and reg['source_files'].get(ROUTE_FILE)==binding['contract_sha256']
              and reg['runner']['os']=='Linux' and reg['runner']['arch']=='X64'
              and type(reg['runner']['name']) is str and reg['runner']['name']
              and reg['storage']['mount_root']==route['mount_root'],
              'registration substitutes the dedicated proof route/storage owner')
    # The public roster name is a logical placement, never an actual API name.
    return route


def visible_runner_inventory(api):
    """Authenticate a complete visible eligible inventory, including offline slots."""
    rows=[];total=None
    for page in range(1,receipts.MAX_PAGES+1):
        batch=api.get('/actions/runners',{'visible':'true','page':page,'limit':50})
        headers=getattr(api,'response_headers',{})
        raw=headers.get('x-total-count','')
        p.require(type(batch) is list and len(batch)<=50 and re.fullmatch(r'[0-9]+',raw) is not None,
                  'complete visible runner inventory/count unavailable')
        observed=int(raw)
        p.require(total is None or total==observed,'runner inventory changed during pagination')
        total=observed;rows.extend(batch)
        p.require(len(rows)<=total,'runner inventory exceeds declared total')
        if len(rows)==total:return rows
        p.require(batch,'runner inventory ended before declared total')
    raise receipts.ReceiptError('Visible runner inventory pagination allowance exhausted')


def authenticate_runner_route(api,reg,root):
    route=registration_route(reg,root)
    p.require(route['state']=='active','dedicated proof route provisioning is not activated in source')
    rows=visible_runner_inventory(api);ids=set();uuids=set();carriers=[]
    for row in rows:
        p.require(type(row) is dict,'runner inventory row invalid')
        ident=receipts.positive(row.get('id'));uid=row.get('uuid');labels=row.get('labels')
        p.require(type(uid) is str and re.fullmatch(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',uid) is not None
                  and ident not in ids and uid not in uuids
                  and type(row.get('name')) is str and row['name']
                  and type(row.get('ephemeral')) is bool
                  and type(labels) is list and all(type(v) is str and v for v in labels)
                  and len(labels)==len(set(labels)) and row.get('status') in ('idle','active','offline'),
                  'duplicate/inconsistent visible runner inventory')
        ids.add(ident);uuids.add(uid)
        # Count every carrier of the reserved label, including offline/misconfigured ones.
        if 'async-proof-owned' in labels:carriers.append(row)
    p.require(len(carriers)==1,'dedicated proof route has no unique visible scheduler owner')
    row=carriers[0];binding=reg['route']
    p.require(set(route['labels'])<=set(row['labels']) and row['id']==binding['runner_id']
              and row['uuid']==binding['runner_uuid'] and row.get('name')==reg['runner']['name']
              and row['status'] in ('idle','active') and row.get('ephemeral') is False,
              'actual dedicated runner identity/availability differs from registration')
    return row


def utc(value):
    try:
        stamp = datetime.fromisoformat(value.replace('Z', '+00:00'))
        p.require(stamp.tzinfo is not None, 'timestamp lacks timezone')
        return stamp.timestamp()
    except (ValueError, TypeError, AttributeError):
        raise receipts.IntegrityError('Async session: malformed timestamp') from None


def registration(value, root, now=None):
    p.closed(value, REGISTRATION_KEYS)
    now = time.time() if now is None else now
    p.require(value['schema_version'] == 6 and type(value['schema_version']) is int
              and value['workflow'] == p.WORKFLOW and value['event'] == 'workflow_dispatch'
              and value['upload_slot_count'] == p.UPLOAD_SLOTS
              and value['modes'] == ['unit', 'complete'] and now < utc(value['expires_at']),
              'registration expired or unrecognized')
    schema = p.read(Path(root) / p.SCHEMA_PATH)
    p.validate(value['source'], schema['$defs']['SourceIdentity'], schema)
    for field in ('repository', 'pr'):
        receipts.positive(value[field])
    p.require(p.COMMIT.fullmatch(value['event_base']) and p.SHA.fullmatch(value['source_archive_sha256'])
              and isinstance(value['ref'], str) and value['ref']
              and isinstance(value['source_files'], dict) and value['source_files'],
              'registration lacks exact source closure')
    files = value['source_files']
    entries=subprocess.check_output(['git','ls-tree','-r','-z',value['source']['commit']],cwd=root,timeout=30)
    tracked=set()
    for entry in entries.split(b'\0'):
        if not entry:continue
        header,name=entry.split(b'\t',1);mode,kind,_=header.split()
        p.require(mode in (b'100644',b'100755') and kind==b'blob', 'source archive closure has symlink/submodule')
        tracked.add(name.decode('utf-8'))
    p.require(set(files)==tracked, 'source closure omits committed inputs')
    for name, sha in files.items():
        p.safe_source_path(name)
        p.require(p.SHA.fullmatch(sha), 'invalid registered source digest')
    p.require(p.digest(p.canonical(sorted(files.items()))) == value['source']['source_closure_sha256']
              and files.get('.github/workflows/' + p.WORKFLOW) == value['source']['workflow_sha256']
              and files.get('.github/workflows/main-fast-lane.yml') == value['main_workflow_sha256'],
              'workflow/source closure mismatch')
    proof = 'verification/async-driver-refinement/'
    names = ('PAGED-CONTRACT.md', 'paged-proof.schema.json', 'paged-catalog.sql',
             'finite-inventory.json', 'paged-outcomes.json', 'paged-catalog-transport.schema.json',
             'paged-verifier-continuation.schema.json','paged-verifier-state.sql',
             'source-bound-formulas.json','source-bound-receipt.schema.json','paged-control-carrier.schema.json')
    p.require(all(proof + n in files for n in names)
              and p.digest(p.canonical(sorted([[proof + n, files[proof + n]] for n in names])))
              == value['source']['proof_contract_sha256']
              and files[proof + 'finite-inventory.json'] == value['source']['finite_inventory_sha256'],
              'finite inventory/proof contract mismatch')
    p.require(set(value['cases']) == set(STAGES) and all(type(v) is list and len(v) == len(set(v))
              and all(isinstance(c, str) and c for c in v) for v in value['cases'].values()),
              'registered stage/cases are not closed')
    p.require(all(value['cases'][stage] for stage in STAGES), 'empty stage case registry')
    registered=p.read(Path(root)/'verification/async-driver-refinement/store-tests.json').get('session_protocol')
    p.require(type(registered) is dict and registered.get('schema_version')==1
              and registered.get('stages')==value['cases'], 'actual source-owned session case registry differs')
    p.require(value['accounting_method']==ACCOUNTING_METHOD
              and value['accounting_method_sha256']==accounting_method_digest(files)
              and all(p.file_sha(Path(root)/name)==files[name] for name in ACCOUNTING_METHOD_FILES), 'unregistered accounting method')
    p.require(p.digest(p.canonical(value['runtime'])) == value['source']['toolchain_sha256'],
              'runtime/toolchain identity differs')
    registration_route(value,root)
    storage_identity(value['storage'])
    for name in CAPABILITY_ACTION_FILES:
        p.require(files.get(name)==p.file_sha(Path(root)/name),'capability action source bytes differ')
    validate_workflow_protocol(root)
    return value


def live_pr(api, reg):
    live = api.get('/pulls/' + str(reg['pr']))
    p.require(live.get('number') == reg['pr'] and live.get('state') == 'open'
              and live.get('draft') is False and live.get('head', {}).get('sha') == reg['source']['commit']
              and live.get('head', {}).get('repo', {}).get('id') == reg['repository']
              and live.get('base', {}).get('repo', {}).get('id') == reg['repository']
              and live.get('base', {}).get('ref') == 'main'
              and live.get('base', {}).get('sha')==reg['event_base']
              and p.COMMIT.fullmatch(live.get('base', {}).get('sha', '')),
              'current PR is not ready/open at registered exact head')
    return live


def runtime_identity(root, reg):
    """Pinned executable hashes/versions, actual owned runner, no env assertions."""
    expected = reg['runtime']
    p.closed(expected, ('rustc', 'cargo', 'java', 'tlc', 'python','action_node'))
    actual = {}
    temporary=Path(root).parent/'runtime-identity-temp';temporary.mkdir(exist_ok=True)
    home=temporary/'home';home.mkdir(exist_ok=True)
    clean={'PATH':os.environ.get('PATH','/usr/bin:/bin'),'HOME':str(home),
        'RUSTUP_HOME':os.environ.get('RUSTUP_HOME',str(Path.home()/'.rustup')),
        'RUSTUP_TOOLCHAIN':'1.97.1','LANG':'C.UTF-8','TMPDIR':str(temporary),'TMP':str(temporary),'TEMP':str(temporary)}
    for name in ('rustc', 'cargo', 'java', 'python','action_node'):
        row = expected[name]
        p.closed(row, ('path', 'sha256', 'argv', 'version'))
        executable = Path(row['path'])
        p.require(executable.is_absolute() and executable.is_file() and not executable.is_symlink()
                  and p.file_sha(executable) == row['sha256'] and type(row['argv']) is list
                  and row['argv'] == (['-version'] if name == 'java' else ['--version']),
                  'runtime executable identity differs')
        if name in ('rustc','cargo'):
            selected=subprocess.run(['rustup','which','--toolchain','1.97.1',name],capture_output=True,text=True,check=True,timeout=15,cwd=root,env=clean)
            p.require(Path(selected.stdout.strip()).resolve()==executable.resolve(),
                      'runtime compiler pin names a shim or another selected toolchain')
        result = subprocess.run([str(executable), *row['argv']], capture_output=True,
                                text=True, check=True, timeout=15, cwd=root,env=clean)
        actual[name] = {**row, 'version': (result.stdout + result.stderr).strip()}
        p.require(actual[name] == row, 'runtime version differs')
    p.require(Path(sys.executable).resolve()==Path(actual['python']['path']).resolve()
              and p.file_sha(Path(sys.executable).resolve())==actual['python']['sha256'],
              'actual controller interpreter differs from pinned Python binary')
    p.require('1.97.1' in actual['rustc']['version'] and '1.97.1' in actual['cargo']['version'], 'Rust toolchain is not pinned1.97.1')
    tlc = expected['tlc']
    p.closed(tlc, ('path', 'sha256'))
    p.require(Path(tlc['path']).is_absolute() and p.file_sha(tlc['path']) == tlc['sha256'],
              'TLC binary differs')
    actual['tlc'] = tlc
    p.require(actual == expected, 'toolchain differs')
    runner = {'name': os.environ.get('RUNNER_NAME'), 'os': os.environ.get('RUNNER_OS'),
              'arch': os.environ.get('RUNNER_ARCH')}
    p.require(runner == reg['runner'] and all(isinstance(v, str) and v for v in runner.values()),
              'unregistered runner slot')
    return {'toolchain': actual, 'runner': runner}


def measured_resources(slot, identity, now=None):
    slot = Path(slot).resolve()
    p.require(slot.is_dir(), 'owned slot missing')
    usage = shutil.disk_usage(slot)
    cgroup = Path('/sys/fs/cgroup/memory.max')
    p.require(cgroup.is_file(), 'actual resident limit unavailable')
    memory = cgroup.read_text().strip()
    p.require(memory.isdecimal() and int(memory) > 0, 'finite RSS ceiling unavailable')
    current_path=Path('/sys/fs/cgroup/memory.current')
    p.require(current_path.is_file(),'actual cgroup resident usage unavailable')
    current=current_path.read_text().strip()
    p.require(current.isdecimal() and 0<int(current)<int(memory),'no measured cgroup resident headroom')
    resident=process_tree_rss(os.getpid())
    p.require(resident>0,'actual controller RSS unavailable')
    return {'slot': str(slot), 'free_disk_bytes': usage.free, 'rss_limit_bytes': int(memory),
            'cgroup_current_bytes':int(current),'rss_current_bytes':resident,'rss_available_bytes':int(memory)-int(current),
            'runner': identity['runner'], 'runtime_sha256': p.digest(p.canonical(identity['toolchain'])),
            'measured_at': datetime.fromtimestamp(time.time() if now is None else now,
                                                  timezone.utc).isoformat()}


def resource_plan(plan_envelope, reg, measurement, root, now=None):
    plan = p.envelope(plan_envelope, 'ResourcePlan', root)
    p.require(plan['admission'] == 'admitted' and plan['source'] == reg['source']
              and plan['plan_sha256'] == p.digest(p.transport_bytes(plan_envelope)),
              'unadmitted or changed plan')
    now = time.time() if now is None else now
    p.require(0 <= now - utc(measurement['measured_at']) <= 300
              and measurement['runner'] == reg['runner']
              and measurement['runtime_sha256'] == reg['source']['toolchain_sha256']
              and plan['total_session_disk_bytes'] <= measurement['free_disk_bytes']
              and plan['resident_rss_bytes'] <= measurement['rss_available_bytes']
              and plan['resident_bytes'] <= plan['resident_rss_bytes'], 'stale/inadequate live resources')
    p.require(plan['available_disk_receipt'] == {'kind': 'runtime_receipt', 'id': 'resources.json',
              'sha256': p.digest(p.canonical(measurement))}, 'disk receipt is not the actual measurement')
    bounds=operational_bounds(plan['source_bounds_utf8'],reg,root)
    p.require(bounds['measurement_sha256']==p.digest(p.canonical(measurement)),
              'overhead measurement binding differs')
    widths,finite=p.source_bound_receipt(bounds['source_receipt'],root,reg['source'],reg['source_files'],
                                        plan['total_session_disk_bytes'])
    allowances=bounds['record_allowances'];inventory=accounting_inventory(bounds['source_receipt'])
    p.require(set(allowances)==set(inventory),'operational accounting kind inventory differs')
    overhead=bounds['overhead']
    p.require(measurement.get('resource_overheads')==overhead
              and measurement.get('resource_method',{}).get('source_sha256')==reg['accounting_method_sha256']
              and measurement.get('resource_method',{}).get('accounting_inventory')==inventory
              and measurement.get('resource_method',{}).get('record_allowances')==allowances
              and measurement.get('resource_method',{}).get('source_bound_specification_sha256')==reg['source_files'][p.SOURCE_BOUND_SPEC]
              and measurement.get('resource_method',{}).get('plan_inputs')==accounting_plan_inputs(plan)
              and measurement.get('resource_method',{}).get('actual_prefix_reserved_at_late_export') is True,
              'unregistered/missing measured allocation method')
    allocated=sum(row['records']*row['max_encoded_bytes'] for row in allowances.values())
    peak=allocated+sum(overhead.values())
    # Combined backup sizes are operational reservations, not global source Bk.
    backup_bytes=measurement['resource_method']['maximum_combined_backup_bytes']
    chunks=(backup_bytes+p.MAX_MEMBER-1)//p.MAX_MEMBER
    p.require(chunks+measurement['resource_method']['minimum_fixed_prefix_parts']<=reg['upload_slot_count']
              and peak<=plan['total_session_disk_bytes']
              and plan['total_retained_artifact_bytes']<=plan['total_session_disk_bytes'],
              'combined peak/retention or native-slot operational allowance exceeded')
    return {'operational_peak_bytes':peak,'source_global_widths':widths,
            'finite_worst_case_expressions':finite,'record_allowances':allowances,
            'accounting_inventory':inventory}


def admit(reg, plan, probe, measurement, root, now=None):
    registration(reg, root, now)
    p.closed(probe, ('registration_sha256', 'source_archive_sha256', 'run', 'job', 'task', 'attempt',
                     'workflow', 'event', 'mode', 'runner', 'measurement_sha256', 'authenticated','finalizer_capability'))
    p.require(probe['finalizer_capability']=={'success':True,'failure_propagated':True,'post_failure_propagated':True,'same_task_slots':32,'acceptance':False},
              'actual terminal/live-log/finalizer capability is not established')
    p.require(probe['authenticated'] is True and probe['registration_sha256'] == p.digest(p.canonical(reg))
              and probe['source_archive_sha256'] == reg['source_archive_sha256']
              and probe['workflow'] == p.WORKFLOW and probe['event'] == 'workflow_dispatch'
              and probe['mode'] == 'unit' and probe['attempt'] == 1 and probe['runner'] == reg['runner']
              and probe['measurement_sha256'] == p.digest(p.canonical(measurement)),
              'exact candidate dispatch capability is not established')
    for key in ('run', 'job', 'task'):
        receipts.positive(probe[key])
    arithmetic = resource_plan(plan, reg, measurement, root, now)
    return {'schema_version': 6, 'registration': reg, 'resource_plan': plan,
            'capability': probe, 'measurement': measurement, 'arithmetic': arithmetic,
            'units': [], 'attempts': [], 'next_ordinal':0, 'session': None, 'checkpoint': None, 'initial_image': None,
            'stage': 'expand', 'progress': copy.deepcopy(NULL_PROGRESS), 'phase': 'admitted',
            'retained_bytes': 0, 'dispatched_runs': [], 'created_at': time.time() if now is None else now}


FINAL_INPUT_SCOPE = 'restored catalog/page identity correspondence; production replay and full semantic qualification remain separate'
FINAL_INPUT_EXECUTION = 'restore-semantic-execution.json'
FINAL_INPUT_LOG = 'restore-semantic.log'
FINAL_INPUT_LOG_PREFIX = b'ASYNC-READONLY-VALIDATION-LOG 1\n'


def dispatch_mode(state):
    """Only replay-authenticated current input evidence can select Main."""
    if state['stage']!='complete':return 'unit'
    current=p.digest(p.canonical(state['checkpoint']))
    return 'complete' if state.get('final_input_checkpoint_sha256')==current else 'unit'


def final_input_receipt(receipt,checkpoint,catalog_sha256):
    cp=checkpoint['record']
    expected={'schema_version':6,'source':cp['source'],'session_id':cp['session_id'],
        'checkpoint_sha256':p.digest(p.canonical(checkpoint)), 'catalog_commit':cp['committed_transaction'],
        'catalog_logical_digest':cp['catalog_logical_digest'],'manifest_root':cp['page_root'],
        'catalog_sha256':catalog_sha256,'counts':cp['counts'],'contexts':cp['contexts'],
        'complete':True,'acceptance':False,'scope':FINAL_INPUT_SCOPE}
    partial={**expected,'complete':False,'scope':'input_preparation'}
    p.require(type(receipt) is dict and type(receipt.get('complete')) is bool
              and p.canonical(receipt) in (p.canonical(expected),p.canonical(partial)),
              'final input readiness receipt is not exact current snapshot correspondence')
    return receipt['complete']


def unit_request(state, run, job, root):
    plan = p.envelope(state['resource_plan'], 'ResourcePlan', root)
    ordinal = state['next_ordinal']
    p.require(ordinal < plan['maximum_units'] and state['phase'] not in
              ('resource_blocked', 'semantic_invalid', 'complete', 'completion_rejected'),
              'session exhausted or terminal')
    reg = state['registration']
    cp = state['checkpoint']
    return {'schema_version': 6, 'kind': 'WorkUnit', 'record': {
        'session_id': plan['session_id'], 'unit_ordinal': ordinal,
        'expected_prior_checkpoint': p.digest(p.canonical(cp)) if cp else None,
        'source': reg['source'], 'pr': reg['pr'], 'repository': reg['repository'],
        'event_base': reg['event_base'], 'workflow_id': p.WORKFLOW, 'event_name': 'workflow_dispatch',
        'mode': dispatch_mode(state),
        'run': run, 'job': job['id'], 'task': job['task_id'], 'attempt': 1,
        'resource_plan_sha256': plan['plan_sha256'], 'runner_identity_utf8': p.canonical(reg['runner']).decode(),
        'status': 'running', 'artifact_roots': [], 'stage': state['stage'],
        'registered_cases': reg['cases'][state['stage']], 'progress': state['progress']}}


def advance(state, result, root):
    """One authenticated unit may advance exactly one ordered phase/cursor."""
    p.closed(result, ('schema_version', 'work_unit', 'checkpoint', 'initial_image', 'session',
                      'artifacts', 'stage_complete'))
    p.require(result['schema_version'] == 6 and type(result['stage_complete']) is bool,
              'invalid unit result')
    unit = result['work_unit']
    p.require(type(unit) is dict and 'record' not in unit,'current WorkUnit must be the closed record')
    p.validate(unit,p.read(Path(root)/p.SCHEMA_PATH)['$defs']['WorkUnit'],p.read(Path(root)/p.SCHEMA_PATH))
    plan = p.envelope(state['resource_plan'], 'ResourcePlan', root)
    reg = state['registration']
    p.require(unit['unit_ordinal'] == state['next_ordinal'] and unit['session_id'] == plan['session_id']
              and unit['source'] == reg['source'] and unit['resource_plan_sha256'] == plan['plan_sha256']
              and unit['pr'] == reg['pr'] and unit['repository'] == reg['repository']
              and unit['event_base'] == reg['event_base'] and unit['stage'] == state['stage']
              and unit['registered_cases'] == reg['cases'][state['stage']]
              and unit['expected_prior_checkpoint'] == (p.digest(p.canonical(state['checkpoint']))
                    if state['checkpoint'] else None), 'unit skips/splices session progress')
    p.require(unit['status'] in ('checkpointed_incomplete', 'closed_unqualified')
              and (unit['stage'] in ('semantic_verify','monitor_scc_verify','temporal_components','complete')
                   or unit['progress'] != state['progress'] or result['stage_complete']),
              'failed or nonprogressing unit cannot advance')
    preparation_only=(state['stage'] in ('semantic_verify','monitor_scc_verify','temporal_components','complete')
                      and result['stage_complete'] is False and result['checkpoint']==state['checkpoint'])
    if state['stage']=='complete':
        p.require(preparation_only and unit['mode']=='unit','final input unit cannot publish a new catalog checkpoint or qualify')
    checkpoint = p.envelope(result['checkpoint'], 'Checkpoint', root)
    initial = p.envelope(result['initial_image'], 'InitialImage', root)
    session = p.envelope(result['session'], 'Session', root)
    p.require(checkpoint['source'] == initial['source'] == session['source'] == reg['source']
              and checkpoint['session_id'] == session['session_id'] == plan['session_id']
              and checkpoint['initial_image_id'] == session['initial_image_id'] == initial['image_sha256']
              and checkpoint['resource_plan_sha256'] == session['resource_plan_sha256'] == plan['plan_sha256']
              and (preparation_only or (checkpoint['unit_ordinal'] == unit['unit_ordinal']
              and checkpoint['previous_checkpoint'] == unit['expected_prior_checkpoint']))
              and session['pr'] == reg['pr'] and session['repository'] == reg['repository']
              and session['event_base'] == reg['event_base'] and session['counts'] == checkpoint['counts'],
              'checkpoint/Init/session binding differs')
    prior = state['checkpoint']
    if prior:
        previous = prior['record']
        p.require(result['initial_image'] == state['initial_image']
                  and (checkpoint == previous if preparation_only else checkpoint['sequence'] == previous['sequence'] + 1)
                  and checkpoint['committed_transaction'] >= previous['committed_transaction']
                  and all(checkpoint['counts'][k] >= v for k, v in previous['counts'].items()
                          if k != 'frontier'), 'checkpoint rollback or changed actual Init')
    else:
        p.require(unit['unit_ordinal'] == 0 and state['stage'] == 'expand'
                  and checkpoint['sequence'] == 0, 'initial checkpoint is not first expansion')
    if state['stage'] != 'expand' or result['stage_complete']:
        p.require(checkpoint['counts']['frontier'] == 0 and len(checkpoint['contexts']) == 7
                  and len({c['context'] for c in checkpoint['contexts']}) == 7
                  and all(c['frontier'] == 0 and c['reachable_products'] == c['expanded_products']
                          for c in checkpoint['contexts']), 'global/context frontier not closed')
    p.require(result['session']==state['session'] if preparation_only else session['next_unit'] == unit['unit_ordinal'] + 1,
              'session next unit differs or preparation mutated publisher Session')
    new = copy.deepcopy(state)
    new['units'].append({'work_unit': {'schema_version': 6, 'kind': 'WorkUnit', 'record': unit},
                         'checkpoint': result['checkpoint'], 'artifacts': result['artifacts']})
    new.update(checkpoint=result['checkpoint'], initial_image=result['initial_image'],
               session=result['session'], progress=unit['progress'], phase=session['phase'])
    if result['stage_complete']:
        p.require(state['stage'] != 'complete', 'completion cannot use ordinary progress path')
        new['stage'] = STAGES[STAGES.index(state['stage']) + 1]
        new['progress'] = copy.deepcopy(NULL_PROGRESS)
    return new


def child_environment(reg, request, source, output):
    # Never copy os.environ: API tokens, checkout credentials and CI payloads
    # stay in the trusted controller. HOME is a fresh private child directory.
    home = Path(output) / 'child-home'
    home.mkdir(exist_ok=True)
    temporary=Path(output)/'child-temp';temporary.mkdir(exist_ok=True)
    compiler=os.environ.get('CARGO_TARGET_DIR')
    p.require(type(compiler) is str and Path(compiler).is_absolute()
              and Path(output).resolve() not in (Path(compiler).resolve(),*Path(compiler).resolve().parents),
              'separate owned compiler-cache slot is unavailable')
    env = {'PYTHONDONTWRITEBYTECODE':'1','PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(home),
           'LANG': 'C.UTF-8', 'TMPDIR':str(temporary),'TMP':str(temporary),'TEMP':str(temporary),
           'PLURX_ASYNC_PAGED_REQUEST': str(request),
           'CARGO_BUILD_JOBS': '1', 'CARGO_NET_OFFLINE': 'true',
           'CARGO_INCREMENTAL':'0','RUSTUP_TOOLCHAIN':'1.97.1',
           'CARGO_TARGET_DIR': str(Path(compiler).resolve())}
    cargo_home = Path(output).parent / 'child-cargo-home'
    cargo_home.mkdir(exist_ok=True)
    upstream = Path(os.environ.get('CARGO_HOME', str(Path.home()/'.cargo')))
    # Only content caches cross the boundary. Registry credentials/config and
    # controller HOME are not visible through CARGO_HOME.
    for name in ('registry', 'git'):
        cached = upstream / name
        target = cargo_home / name
        if cached.is_dir() and not target.exists():
            target.symlink_to(cached.resolve(), target_is_directory=True)
    env['CARGO_HOME'] = str(cargo_home)
    rustup = os.environ.get('RUSTUP_HOME', str(Path.home()/'.rustup'))
    p.require(Path(rustup).is_absolute(), 'toolchain home is not absolute')
    env['RUSTUP_HOME'] = rustup
    return env


def validate_completion_command(operation, command, source, request_path, mode):
    """Final preparation may read/verify the frozen catalog, never publish it."""
    checker=str(Path(source)/'verification/async-driver-refinement/paged_checker.py')
    runner=str(Path(source)/'scripts/check-async-driver-refinement')
    p.require(mode in ('unit','complete'),'invalid complete-stage mode')
    verbs={'validate_restore':('validate-restore','validate-restored-verifier')}
    if mode=='unit':verbs.update(unit=('prepare-unit',),finalize_unit=('finalize-unit',))
    else:verbs['completion_verify']=('complete',)
    allowed=[[sys.executable,checker,verb,'--request',str(request_path)]
             for verb in verbs.get(operation,())]
    if operation=='validate_restore':
        allowed.append([sys.executable,runner,'paged-unit','--request',str(request_path)])
    p.require(command in allowed,'completion stage permits only fixed independent preparation/verification and readonly restore commands')


def run_quantum(request_path, reg, source, output, command):
    request = p.read(request_path)
    operational=request.get('operation')=='catalog_calibration'
    if operational:
        p.closed(request,('operation','resource_plan','session_id','deadline_monotonic','output_dir'))
        session_id=request['session_id']
    else:
        unit=request['work_unit']
        p.require(type(unit) is dict and 'record' not in unit,'private request WorkUnit must be record')
        if unit['stage']=='complete':
            validate_completion_command(request['operation'],command,source,request_path,unit['mode'])
        p.require(unit['registered_cases']==reg['cases'][unit['stage']],'unregistered quantum cases')
        session_id=unit['session_id']
    import selectors
    import signal
    started = time.monotonic()
    deadline = min(started + 1800, request.get('deadline_monotonic', started + 1800))
    limit = request['resource_plan']['record']['resident_rss_bytes']
    child_address_limit=limit-process_tree_rss(os.getpid())
    p.require(child_address_limit>0,'no admitted resident allowance remains for quantum')
    def limits():
        resource.setrlimit(resource.RLIMIT_AS, (child_address_limit, child_address_limit))
        os.umask(0o077)
    log_path = Path(output) / ('quantum-' + str(time.time_ns()) + '.log')
    environment=child_environment(reg,request_path,source,output)
    if operational:
        environment.pop('PLURX_ASYNC_PAGED_REQUEST',None)
        environment['PLURX_ASYNC_CATALOG_CALIBRATION']=str(Path(output)/'catalog-calibration.json')
    process = subprocess.Popen(command, cwd=source, env=environment,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               preexec_fn=limits, start_new_session=True)
    size = 0
    last_guard=0
    try:
        with log_path.open('xb') as log, selectors.DefaultSelector() as poll:
            os.set_blocking(process.stdout.fileno(), False)
            poll.register(process.stdout, selectors.EVENT_READ)
            while poll.get_map() or process.poll() is None:
                p.require(time.monotonic() < deadline, 'unit deadline exhausted')
                if time.monotonic()-last_guard>=1:
                    p.require(process_tree_rss(process.pid)+resource.getrusage(resource.RUSAGE_SELF).ru_maxrss*1024<=limit,
                              'actual resident RSS allocation exhausted')
                    owned=Path(reg['storage']['root'])/session_id
                    p.require(directory_bytes(owned)<=request['resource_plan']['record']['total_session_disk_bytes'],
                              'actual owned session disk allocation exhausted')
                    last_guard=time.monotonic()
                for key, _ in poll.select(min(0.1, max(0.001, deadline-time.monotonic()))):
                    block = os.read(key.fd,65536)
                    if not block:
                        poll.unregister(key.fileobj)
                    else:
                        size += len(block)
                        p.require(size <= p.SMALL, 'unit execution log byte allowance exhausted')
                        log.write(block)
            p.require(process.wait() == 0, 'quantum failed; preceding checkpoint preserved')
    finally:
        try:os.killpg(process.pid,signal.SIGKILL)
        except ProcessLookupError:pass
        process.wait()
        process.stdout.close()
    verify_source_tree(Path(source),reg)
    if operational:return p.read(Path(output)/'catalog-calibration-execution.json')
    if request['operation']=='completion_verify':return p.read(Path(output)/'complete-verification.json')
    if request.get('operation') == 'validate_restore':
        return None
    if 'prepare-unit' in command:
        return p.read(Path(output)/'prepare-unit.json')
    if unit['stage'] == 'backend_qualification' and request['operation'] == 'unit':
        return p.read(Path(output) / 'backend-progress.json')
    return p.read(Path(output) / 'unit-result.json')


def main_conditions(results, affected):
    """The ordinary and completion producers execute this identical predicate."""
    p.require(set(results) == set(MAIN_ENV) and type(affected) is bool, 'Main result fields differ')
    p.require(results['paperwork'] == 'success' and results['worker_control_proof'] == 'success',
              'Main mandatory policy/worker proof failed')
    if affected:
        p.require(results['async_refinement'] == 'success', 'Main async proof is incomplete')
    p.require(all(v in ('success', 'skipped') for v in results.values()), 'Main selected job failed')
    return True


def ordinary_main():
    p.require(os.environ.get('GITHUB_EVENT_NAME') == 'pull_request', 'ordinary Main event differs')
    results = {key: os.environ.get(env) for key, env in MAIN_ENV.items()}
    affected = os.environ.get('ASYNC_DRIVER_AFFECTED')
    p.require(affected in ('true','false') or (affected == '' and results['rust_compile'] == 'skipped'),
              'missing Main async applicability for selected Rust lane')
    main_conditions(results, affected == 'true')


def workflow_jobs(path):
    # The workflow has a deliberately simple job header/name grammar. Refuse
    # names expressed through interpolation instead of accepting aliases.
    import re
    text = Path(path).read_text()
    jobs = {}
    current = None
    for line in text.splitlines():
        match = re.fullmatch(r'  ([a-zA-Z0-9_]+):', line)
        if match:
            current = match[1]
        match = re.fullmatch(r'    name: ([A-Za-z0-9 /_-]+)', line)
        if match and current:
            jobs[current] = match[1]
    p.require(set(MAIN_JOBS) <= set(jobs), 'current Main job inventory changed')
    return jobs




def transport_hash(lines, artifact):
    from validation.worker_control_resume import upload_transaction_sha256
    return upload_transaction_sha256(lines, artifact['name'], artifact['id'], artifact['size_in_bytes'])




def completion(state, verifier, main_receipts, fresh, unit, root):
    p.require(state['stage'] == 'complete' and state['phase'] == 'qualified',
              'session not globally qualified')
    checkpoint = state['checkpoint']['record']
    p.require(checkpoint['counts']['frontier'] == 0 and len(checkpoint['contexts']) == 7,
              'complete root lacks global closure')
    p.closed(verifier, ('manifest_root', 'counts', 'contexts', 'outcomes',
                        'backend_causal_numeric_eligibility', 'complete'))
    p.require(verifier['complete'] is True and verifier['manifest_root'] == checkpoint['page_root']
              and verifier['counts'] == checkpoint['counts'] and verifier['contexts'] == checkpoint['contexts'],
              'verified complete root differs from checkpoint')
    expected = p.read(Path(root) / 'verification/async-driver-refinement/paged-outcomes.json')
    families = expected.get('families', expected.get('outcomes'))
    p.require(type(families) is list, 'outcome registry format unsupported')
    expected_map = {f['family']: f['expected_result'] for f in families}
    outcomes = verifier['outcomes']
    p.require(len(outcomes) == 11 and {o['family'] for o in outcomes} == set(expected_map)
              and all(o['actual'] == o['expected'] == expected_map[o['family']] for o in outcomes),
              'eleven semantic outcome families not established')
    kinds = {r['kind'] for r in verifier['backend_causal_numeric_eligibility']}
    p.require({'backend_receipt', 'causal_receipt', 'numeric_receipt', 'eligibility_receipt'} <= kinds,
              'backend/causal/numeric/eligibility coverage missing')
    reg = state['registration']
    value = {'session_id': state['resource_plan']['record']['session_id'], 'source': reg['source'],
             'pr': reg['pr'], 'repository': reg['repository'], 'event_base': reg['event_base'],
             'manifest_root': verifier['manifest_root'], 'counts': verifier['counts'],
             'contexts': verifier['contexts'], 'outcomes': outcomes,
             'backend_causal_numeric_eligibility': verifier['backend_causal_numeric_eligibility'],
             'applicable_main_receipts': main_receipts, 'completion_unit': unit,
             'current_head_recheck': fresh, 'accepted': True}
    result = {'schema_version': 6, 'kind': 'Completion', 'record': value}
    p.envelope(result, 'Completion', root)
    return result


class SessionAPI(receipts.API):
    """New protocol's finite RPC allowance; ordinary receipt API is unchanged."""
    def __init__(self,*args,**kwargs):
        super().__init__(*args,**kwargs)
        self.session_requests=0
        self.maximum_session_requests=200
        self.deadline=time.monotonic()+1800
        self.storage=None

    def bind_storage(self,plan,reg,session_id):
        limit=plan['record']['total_session_disk_bytes']
        p.require(type(limit) is int and limit>0,'missing operational proof-storage allocation')
        remote=Path(reg['storage']['root'])/session_id
        local=Path(self.scratch).resolve().parent
        if remote in local.parents or remote==local:
            owned=validate_storage(reg['storage'],Path(__file__).resolve().parents[1])/session_id
        else:
            # Read-only external consumers use their explicitly owned output,
            # without pretending the producer mount exists on this host.
            p.require(local.is_dir() and local.stat().st_uid==os.getuid() and not local.is_symlink(),
                      'consumer output is not locally owned')
            owned=local
        self.storage=(owned,limit)
        self.storage_observed=0;self.storage_checked=0

    def guard_storage(self,additional):
        p.require(time.monotonic()<self.deadline,'session transport deadline exhausted')
        if self.storage is None:return
        root,limit=self.storage
        if time.monotonic()-self.storage_checked>=1:
            self.storage_observed=directory_bytes(root) if root.exists() else 0
            self.storage_checked=time.monotonic()
        p.require(type(additional) is int and additional>=0 and self.storage_observed+additional<=limit
                  and shutil.disk_usage(root if root.exists() else root.parent).free>=additional,
                  'actual owned proof-storage allocation/free disk exhausted')
        self.storage_observed+=additional

    def reserve_request(self):
        self.session_requests+=1
        p.require(self.session_requests<=self.maximum_session_requests and time.monotonic()<self.deadline,
                  'registered session RPC/time allowance exhausted')

    def bytes(self,path,query=None):
        self.reserve_request()
        p.require((path=='' or path.startswith('/')) and '..' not in path and len(path)<=1024
                  and re.fullmatch(r'[A-Za-z0-9_./-]*',path),'unsafe session API path')
        url=self.root+path
        if query:url+='?'+urllib.parse.urlencode(query)
        request=urllib.request.Request(url,headers={'Authorization':'token '+self.token,'Accept':'application/json'})
        data=bytearray()
        try:
            with self.opener.open(request,timeout=15) as incoming:
                self.response_headers={key.lower():value for key,value in incoming.headers.items()}
                p.require(hasattr(incoming,'read1'),'bounded session API reader unavailable')
                while block:=incoming.read1(64*1024):
                    p.require(time.monotonic()<self.deadline and len(data)+len(block)<=receipts.MAX_BYTES,
                              'session API response/time allowance exhausted')
                    data.extend(block)
        except urllib.error.HTTPError as error:
            raise receipts.ReceiptHTTPError(error.code,'/repos/'+self.repository+path) from None
        except (OSError,http.client.HTTPException) as error:
            raise receipts.ReceiptError(type(error).__name__+' at /repos/'+self.repository+path+'; evidence unavailable') from None
        return bytes(data)


def api_environment(scratch):
    p.require(os.environ.get('GITHUB_API_URL', '').startswith(os.environ.get('GITHUB_SERVER_URL', '') + '/'),
              'API origin differs')
    api = SessionAPI(os.environ['GITHUB_API_URL'], os.environ['GITHUB_REPOSITORY'], os.environ.get('GITHUB_TOKEN'))
    api.scratch = str(Path(scratch).resolve())
    Path(api.scratch).mkdir(parents=True, exist_ok=True)
    return api


def dispatch(api, state, control, root):
    """One bounded registered successor. A failed dispatch never advances state."""
    reg = state['registration']
    registration(reg, root)
    live_pr(api, reg)
    p.require(state['phase'] not in ('resource_blocked','semantic_invalid','completion_rejected','complete'),
              'terminal/incomplete resource disposition cannot dispatch another same-plan unit')
    p.require(state['next_ordinal'] < state['resource_plan']['record']['maximum_units'], 'unit budget exhausted')
    p.require(state.get('retry_authorized') is True,'unknown terminal failure cannot automatically dispatch a successor')
    mode = dispatch_mode(state)
    reject_prior_ordinal_dispatch(api,reg,control,None)
    inputs = {'mode': mode, 'operation': 'execute', 'registration_sha256': p.digest(p.canonical(reg)),
              'control': p.canonical(control).decode()}
    payload = p.canonical({'ref': reg['ref'], 'inputs': inputs})
    p.require(len(payload) <= p.SMALL, 'dispatch payload too large')
    path = '/actions/workflows/' + p.WORKFLOW + '/dispatches'
    request = urllib.request.Request(api.root + path, data=payload, method='POST',
              headers={'Authorization': 'token ' + api.token, 'Content-Type': 'application/json'})
    authenticate_runner_route(api,reg,root)
    with api.opener.open(request, timeout=30) as response:
        p.require(response.status in (200, 201, 204), 'dispatch was not accepted')
    # API acceptance is not run authentication/capability; controller polls
    # separately and admits only the exact event/registration/task result.
    return {'registration_sha256': inputs['registration_sha256'], 'mode': mode,
            'ordinal': state['next_ordinal'], 'expected_prior_checkpoint':
            p.digest(p.canonical(state['checkpoint'])) if state['checkpoint'] else None,
            'dispatched_at': datetime.now(timezone.utc).isoformat()}



def source_scope(root, event_base):
    """Execute the current bound selector, not a caller-supplied skip list."""
    text = subprocess.check_output([sys.executable, '-m', 'validation.ci_scope', '--event',
                                    'pull_request', '--base', event_base], cwd=root, text=True, timeout=60)
    selected = {}
    for line in text.splitlines():
        key, enabled = line.split('=', 1)
        p.require(key not in selected and enabled in ('true', 'false'), 'scope selector ambiguous')
        selected[key] = enabled == 'true'
    p.require({'rust', 'apple', 'desktop_macos', 'android_jvm', 'web_layout'} <= set(selected),
              'scope selector incomplete')
    return selected


def main_required(scope):
    return {'paperwork': True, 'scope': True, 'preflight': True, 'worker_control_proof': True,
            'rust_compile': scope['rust'], 'web_compile': scope['web_layout'],
            'apple_compile': scope['apple'] or scope['desktop_macos'],
            'android_compile': scope['android_jvm']}


def zip_inventory(path, max_members=p.MAX_MEMBERS, max_expanded=p.MAX_ARTIFACT):
    import stat
    import zipfile
    rows = []
    with zipfile.ZipFile(path) as archive:
        p.require(len(archive.infolist()) <= max_members, 'receipt member allowance exceeded')
        seen = set(); total = 0
        for info in archive.infolist():
            name = p.safe_path(info.filename)
            p.require(name not in seen and not info.is_dir() and not info.flag_bits & 1
                      and stat.S_IFMT(info.external_attr >> 16) in (0, stat.S_IFREG), 'receipt alias/link')
            seen.add(name); total += info.file_size
            p.require(total <= max_expanded and info.file_size <= p.MAX_MEMBER, 'receipt expansion bound')
            h = __import__('hashlib').sha256(); count = 0
            with archive.open(info) as stream:
                while block := stream.read(p.MIB):
                    count += len(block); h.update(block)
                    p.require(count <= info.file_size, 'receipt member expanded beyond size')
            p.require(count == info.file_size, 'receipt member truncated')
            rows.append({'path': name, 'bytes': count, 'sha256': h.hexdigest()})
    return rows


def authenticated_main_artifact(api, artifact, lines, output):
    path = p.artifact_bytes(api, artifact)
    try:
        p.require(p.file_sha(path) == transport_hash(lines, artifact), 'Main artifact upload/local differs')
        members = zip_inventory(path)
        p.unpack(path, output, members,storage_guard=api.guard_storage)
        return members
    finally:
        path.unlink(missing_ok=True)


def authenticate_main(api, reg, run_id, root, *, verified):
    """Execute actual Main conditions on individually authenticated producers."""
    import re
    run = api.get('/actions/runs/' + str(receipts.positive(run_id)))
    scope = {'pr': reg['pr'], 'repository': reg['repository']}
    event = adoption.bind_event(run, scope)
    p.require(run['commit_sha'] == reg['source']['commit']
              and event['pull_request']['base']['sha'] == reg['event_base'], 'Main raw event/source differs')
    adoption.validate_live_pr(api.get('/pulls/' + str(reg['pr'])), event, reg['source']['commit'])
    p.require(p.file_sha(Path(root) / '.github/workflows/main-fast-lane.yml') == reg['main_workflow_sha256'],
              'Main workflow bytes differ')
    names = workflow_jobs(Path(root) / '.github/workflows/main-fast-lane.yml')
    selected = source_scope(root, reg['event_base'])
    required = main_required(selected)
    jobs = api.pages('/actions/runs/' + str(run_id) + '/jobs')
    artifacts = api.pages('/actions/runs/' + str(run_id) + '/artifacts', field='artifacts')
    p.require(type(verified) is dict and verified.get('complete') is True
              and len(verified.get('contexts',[]))==7 and len(verified.get('outcomes',[]))==11,
              'Main aggregation requires actual independent complete verification')
    records = []; results = {'async_refinement': 'success'}
    logs = {}; contexts = {}
    for key, needed in required.items():
        matches = [j for j in jobs if j.get('name') == names[key]]
        p.require(len(matches) == 1, 'Main job missing/ambiguous')
        job = matches[0]
        p.require(type(job.get('attempt')) is int and job['attempt'] == 1
                  and job.get('run_id') == run_id and job.get('repo_id') == reg['repository'],
                  'Main task/attempt binding differs')
        if not needed:
            p.require(job.get('status') == 'skipped', 'Main skip contradicts current affected source')
            results[key] = 'skipped'
            continue
        p.require(job.get('status') == 'success', 'applicable Main condition not successful')
        receipts.positive(job.get('id')); receipts.positive(job.get('task_id'))
        raw = api.bytes('/actions/jobs/' + str(job['id']) + '/logs', {'attempt': 1})
        lines = main_unit_receipts.log_lines(raw)
        p.require(lines and re.fullmatch(r'Runner [0-9a-f-]+ \(version:[^)]*\) received task '
                  + str(job['task_id']) + ' of job ' + re.escape(key)
                  + ', triggered by event: pull_request', lines[0]), 'Main task log differs')
        marker = '[command]/usr/bin/git log -1 --format=%H'
        starts = [i for i, line in enumerate(lines) if line == marker]
        p.require(len(starts) == 1 and starts[0] + 1 < len(lines)
                  and lines[starts[0]+1] == reg['source']['commit'], 'Main checkout differs')
        logs[key] = lines; contexts[key] = job
        records.append({'condition': key, 'source': reg['source'], 'event_base': reg['event_base'],
                        'run': run_id, 'job': job['id'], 'task': job['task_id'], 'attempt': 1,
                        'status': 'success', 'log_sha256': p.digest(raw)})
        results[key] = 'success'
    # The original success of each exact workflow job binds its actual commands;
    # retained artifact validation below also preserves each independent family.
    full_scope = main_unit_receipts.identity(api, reg['pr'], reg['source']['commit'])
    python_artifacts = [a for a in artifacts if a.get('name') == main_unit_receipts.key(full_scope)]
    journal = main_unit_receipts.journal_from_artifacts(api, full_scope, run, python_artifacts)
    p.require(journal['complete'] is True, 'current Python receipt is incomplete')
    # Current successful preflight executed the strict origin/applicability
    # adapter itself. Require its final journal and exact start/final chain;
    # no historical fallback changes current integrity failures here.
    name = adoption.journal_key(scope)
    finals = [a for a in artifacts if a.get('name') == name]
    p.require(len(finals) == 1, 'current preflight journal missing')
    out = Path(api.scratch) / ('main-preflight-' + str(run_id))
    members = authenticated_main_artifact(api, finals[0], logs['preflight'], out)
    p.require(len(members) == 1, 'preflight journal member ambiguity')
    current_journal = p.read(out / members[0]['path'])
    env = adoption.environment()
    adoption.validate_journal(current_journal, scope, run, contexts['preflight'], env)
    p.require(current_journal['producer_blob'] == p.git(root, 'rev-parse', reg['source']['commit'] + ':validation/main_preflight_adoption.py')
              and current_journal['manifest_blob'] == p.git(root, 'rev-parse', reg['source']['commit'] + ':' + str(adoption.MANIFEST)),
              'preflight source/manifest differs')
    worker_due=worker_affected(root,reg['event_base'],reg['source']['commit'])
    marker='Worker controller proof inputs affected: '+str(worker_due).lower()
    p.require(logs['worker_control_proof'].count(marker)==1,'worker source applicability log differs')
    worker = [a for a in artifacts if a.get('name') == 'worker-control-proof-' + str(run_id) + '-1']
    if worker_due:
        p.require(len(worker) == 1, 'applicable worker proof receipt missing')
        worker_dir = Path(api.scratch) / ('main-worker-' + str(run_id))
        authenticated_main_artifact(api, worker[0], logs['worker_control_proof'], worker_dir)
        validate_worker_receipt(root, worker_dir)
    else:
        p.require(not worker,'unaffected worker job published unexpected proof artifact')
    if required['apple_compile']:
        native_result=authenticate_native(api, reg, run_id, artifacts, logs['apple_compile'], selected,root,contexts['apple_compile'])
        for record in records:
            if record['condition']=='apple_compile':record['native_receipts']=native_result
    main_conditions(results, True)
    live_pr(api, reg)
    return records


def validate_worker_receipt(root, directory):
    import importlib.machinery
    worker = importlib.machinery.SourceFileLoader('async_session_worker_validator',
                                                 str(Path(root) / 'scripts/check-worker-control')).load_module()
    value = p.read(Path(directory) / 'receipt.json')
    p.require(value.get('schema_version') == 1 and value.get('mode') == 'full'
              and value.get('acceptance') is True and value.get('outcome') == 'complete'
              and value.get('source') == worker.source_identity()
              and len(value.get('proofs', [])) == 6 and len(value.get('controls', [])) == 3
              and not value.get('target_errors'), 'current worker proof/source incomplete')
    worker.validate_staging(Path(directory), value['source_staging'])
    controls = p.read(Path(root) / 'verification/worker-control/controls.json')
    for index, result in enumerate([*value['proofs'], *value['controls']]):
        control = controls[index-6] if index >= 6 else None
        directory_case = Path(directory) / control['name'] if control else Path(directory)
        log = directory_case / (result['harness'].rsplit('::',1)[-1] + '.log')
        p.require(log.is_file() and p.file_sha(log) == result['log_sha256'], 'worker bound log differs')
        worker.validate_case_sources(directory_case, result['source'], control is not None)
        parsed = worker.verdict(log.read_text(), result['exit_code'], result['harness'],
                     control['assertion_marker'] if control else None,
                     control['allowed_assertions'] if control else None,
                     fmt_stdlib_sha256=result['tools'].get('fmt_stdlib_sha256'),
                     parser_attestation=result['parser_stdlib_attestation'])
        p.require(all(result.get(k) == v for k,v in parsed.items()), 'worker original verdict differs')


def authenticate_native(api, reg, run_id, artifacts, lines, selected, root, job):
    from validation import native_receipts as native
    from validation import desktop_macos_retention as desktop
    scope = {'repository': reg['repository'], 'pr': reg['pr']}
    adopted_name = 'native-adoption-' + str(run_id) + '-1'
    adoption_artifacts = [a for a in artifacts if a.get('name') == adopted_name]
    p.require(len(adoption_artifacts) == 1, 'native adoption provenance missing')
    directory = Path(api.scratch) / ('native-adoption-' + str(run_id))
    rows = authenticated_main_artifact(api, adoption_artifacts[0], lines, directory)
    p.require(len(rows) == 1, 'native adoption member ambiguous')
    report = p.read(directory / rows[0]['path'])
    p.closed(report,('version','scope','current_commit','current_run','adoptions','compatibility'))
    p.require(type(report['version']) is int and report['version']==1,'native report version differs')
    p.require(report.get('scope') == scope and report.get('current_commit') == reg['source']['commit']
              and report.get('current_run') == run_id and type(report.get('adoptions')) is dict
              and set(report['adoptions'])<= {'tv_navigation'}, 'native current TV-only provenance differs')
    result={}
    if selected['apple']:
        adopted = report['adoptions'].get('tv_navigation')
        if adopted:
            ident = receipts.positive(adopted['artifact'])
            artifact = api.get('/actions/artifacts/' + str(ident))
            confirmed = native.adopt(api, scope, reg['source']['commit'], run_id, [artifact],families=('tv_navigation',))
            p.require(confirmed.get('tv_navigation') == adopted, 'native adopted TV origin/applicability differs')
            result['tv_navigation']={'disposition':'adopted','original':adopted}
        else:
            name='tv-navigation-'+str(run_id)+'-1'
            matches=[a for a in artifacts if a.get('name')==name]
            p.require(len(matches)==1 and matches[0].get('run_id')==run_id
                      and matches[0].get('expired') is False,'current original TV artifact absent/foreign')
            artifact=matches[0];aid=receipts.positive(artifact['id'])
            _,archive_sha,_,_=p.upload_slice(lines,name,aid,artifact['size_in_bytes'])
            archive=p.artifact_bytes(api,artifact)
            try:
                p.require(p.file_sha(archive)==archive_sha,'current TV archive differs from full native upload transaction')
                with archive.open('rb') as stream:data=native.archive_member(stream,'receipt.json')
            finally:archive.unlink(missing_ok=True)
            native.verify_tv(p.decode(data),reg['source']['commit'])
            result['tv_navigation']={'disposition':'fresh_execution','commit':reg['source']['commit'],
                'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1,'artifact':aid,
                'archive_sha256':archive_sha,'receipt_sha256':p.digest(data)}
    if selected['desktop_macos']:
        result['desktop_macos']=desktop.authenticate_current_result(api,Path(root),reg['source']['commit'],
            run_id,job['id'],reg['repository'],reg['pr'],event_base=reg['event_base'])
    return result


OUTPUT_MARKER = 'ASYNC_SESSION_OUTPUT '


def artifact_name(session_id, ordinal, part):
    import re
    p.require(isinstance(session_id, str) and re.fullmatch(r'[0-9a-f-]{36}', session_id)
              and type(ordinal) is int and ordinal >= 0 and type(part) is int and 0 <= part < p.UPLOAD_SLOTS,
              'artifact name identity invalid')
    return f'async-proof-{session_id}-{ordinal}-part-{part}'


def output_metadata(reg, session_id, ordinal, plan, result_path, operation):
    count = plan.preflight()
    return {'schema_version': 1, 'registration_sha256': p.digest(p.canonical(reg)),
            'source_archive_sha256': reg['source_archive_sha256'], 'operation': operation,
            'session_id': session_id, 'unit_ordinal': ordinal, 'result_path': result_path,
            'parts': [{'ordinal': n, 'name': artifact_name(session_id, ordinal, n),
                       'members': plan.manifest(n)} for n in range(count)]}


def upload_outputs(metadata, staging):
    encoded = p.canonical(metadata)
    p.require(len(encoded) <= p.SMALL, 'bounded uploader metadata exceeded')
    p.write(Path(staging).parent/'upload-plan.json',metadata)
    if metadata['operation']=='execute':preflight_carrier(Path(staging).parent,metadata)
    print(OUTPUT_MARKER + encoded.decode(), flush=True)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
        stream.write('part_count=' + str(len(metadata['parts'])) + '\n')
        for part in metadata['parts']:
            n = part['ordinal']
            stream.write(f'part_{n}_name={part["name"]}\n')
            stream.write(f'part_{n}_path={Path(staging).resolve() / "parts" / str(n)}\n')


def authenticate_output(api, reg, run_id, mode, destination, root):
    run, job, event = p.producer(api, run_id, reg, mode, ('success',))
    if event['inputs']['operation']=='execute':
        bound=p.decode(event['inputs']['control'].encode())
        declared=p.envelope(bound['resource_plan'],'ResourcePlan',root)
        p.require(declared['source']==reg['source'],'current producer resource source differs')
        controller_memory_limit(bound['resource_plan'])
        api.bind_storage(bound['resource_plan'],reg,bound['session_id'])
    raw, lines = p.producer_log(api, run_id, job, 'complete' if mode == 'complete' else 'unit')
    matches = [line[len(OUTPUT_MARKER):] for line in lines if line.startswith(OUTPUT_MARKER)]
    p.require(len(matches) == 1, 'producer output inventory missing/ambiguous')
    metadata = p.decode(matches[0].encode())
    p.closed(metadata, ('schema_version', 'registration_sha256', 'source_archive_sha256',
                        'operation', 'session_id', 'unit_ordinal', 'result_path', 'parts'))
    p.require(metadata['schema_version'] == 1 and metadata['registration_sha256'] == p.digest(p.canonical(reg))
              and metadata['source_archive_sha256'] == reg['source_archive_sha256']
              and metadata['operation'] == event['inputs']['operation']
              and 0 < len(metadata['parts']) <= p.UPLOAD_SLOTS,
              'output registration/source/part count differs')
    listed = api.pages('/actions/runs/' + str(run_id) + '/artifacts', field='artifacts')
    parts = {}; artifact_receipts = {}; total = 0
    checkpoint_digest = None
    result = None
    # Every inventory is authenticated by the exact native uploader transaction
    # before any part is selected for semantic/backup use.
    for n, descriptor in enumerate(metadata['parts']):
        p.closed(descriptor, ('ordinal', 'name', 'members'))
        p.require(descriptor['ordinal'] == n and descriptor['name'] ==
                  artifact_name(metadata['session_id'], metadata['unit_ordinal'], n), 'part ordinal/name differs')
        artifacts = [a for a in listed if a.get('name') == descriptor['name']]
        p.require(len(artifacts) == 1 and artifacts[0].get('run_id', run_id) == run_id,
                  'part producer missing/ambiguous')
        artifact = artifacts[0]
        archive = p.artifact_bytes(api, artifact)
        try:
            p.require(p.file_sha(archive) == transport_hash(lines, artifact), 'native upload/API/local hash differs')
            directory = Path(api.scratch).parent/'authenticated-parts'/str(artifact['id'])
            p.unpack(archive,directory,descriptor['members'],storage_guard=api.guard_storage,verify_existing=directory.exists())
            parts[n] = directory
            total += archive.stat().st_size + sum(v['bytes'] for v in descriptor['members'])
            # Build receipt after exact uploaded result/checkpoint is read.
            artifact_receipts[n] = {'artifact': artifact, 'zip_sha256': p.file_sha(archive),
                                    'zip_bytes': archive.stat().st_size, 'members': descriptor['members']}
        finally:
            archive.unlink(missing_ok=True)
    reject_current_control_members(parts,metadata['unit_ordinal'])
    result_path = p.safe_path(metadata['result_path'])
    locations = [directory / result_path for directory in parts.values() if (directory / result_path).is_file()]
    p.require(len(locations) == 1, 'unit result member missing/duplicated')
    result = p.read(locations[0])
    if metadata['operation'] == 'probe':
        p.closed(result, ('schema_version', 'registration_sha256', 'source_archive_sha256', 'runner',
                          'measurement', 'runtime', 'session_id'))
        p.require(result['schema_version'] == 1 and result['registration_sha256'] == metadata['registration_sha256']
                  and result['source_archive_sha256'] == reg['source_archive_sha256']
                  and result['runner'] == reg['runner'] and result['runtime'] == reg['runtime']
                  and result['measurement'].get('producer')=={'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1}
                  and event['inputs']['mode'] == 'unit' and len(metadata['parts']) == p.UPLOAD_SLOTS,
                  'capability probe/source/slot identity differs')
        rust=validate_catalog_calibration(p.read(find_member(parts,'calibration/catalog-calibration-execution.json')),
             find_member(parts,'calibration/catalog-calibration.json').parent,reg,root)
        p.require(rust in result['measurement']['resource_method']['geometry'],
                  'actual Rust calibration evidence is missing from measured admission geometry')
        capability = {'registration_sha256': metadata['registration_sha256'],
                      'source_archive_sha256': reg['source_archive_sha256'], 'run': run_id,
                      'job': job['id'], 'task': job['task_id'], 'attempt': 1, 'workflow': p.WORKFLOW,
                      'event': 'workflow_dispatch', 'mode': 'unit', 'runner': reg['runner'],
                      'measurement_sha256': p.digest(p.canonical(result['measurement'])), 'authenticated': True}
        return capability, result['measurement'], metadata
    carrier,control_sha,frame_begin,_=p.parse_control_frame(lines)
    validate_control_identity(carrier,reg,metadata,run_id,job,root)
    selected=[artifact_receipts[n]['artifact'] for n in sorted(artifact_receipts)]
    p.validate_upload_order(lines,selected,frame_begin)
    if mode=='complete':
        p.require(metadata['result_path']=='complete-verification.json', 'completion verification member differs')
        result={'verified':result,'checkpoint':p.read(find_member(parts,'checkpoint.json'))}
        planned=carrier['work_unit']['record']
    else:
        p.require('work_unit' not in result and 'planned_work_unit' in result,
                  'current ZIP contains competing authoritative WorkUnit')
        planned=result.pop('planned_work_unit')
    unit=p.envelope(carrier['work_unit'],'WorkUnit',root)
    p.require({k:v for k,v in unit.items() if k!='artifact_roots'}==
              {k:v for k,v in planned.items() if k!='artifact_roots'},'planned/final unit differs')
    result['work_unit']=unit
    if mode=='unit':
        p.require(type(result['artifacts']) is list and len(result['artifacts'])==len(set(result['artifacts'])),
                  'unit output inventory is not a unique list')
        for name in result['artifacts']:find_member(parts,p.safe_path(name))
    p.require((unit['run'], unit['job'], unit['task'], unit['attempt']) ==
              (run_id, job['id'], job['task_id'], 1), 'uploaded WorkUnit task differs')
    checkpoint_digest = p.digest(p.canonical(result['checkpoint']))
    p.require(carrier['checkpoint']==p.ref(result['checkpoint'],checkpoint_digest),
              'carrier checkpoint does not resolve actual uploaded checkpoint')
    for n, row in artifact_receipts.items():
        value = {'artifact_id': row['artifact']['id'], 'session_id': unit['session_id'],
                 'unit_ordinal': unit['unit_ordinal'], 'run': run_id, 'job': job['id'], 'task': job['task_id'],
                 'attempt': 1, 'source': reg['source'], 'name': row['artifact']['name'], 'expired': False,
                 'zip_sha256': row['zip_sha256'], 'zip_bytes': row['zip_bytes'],
                 'extracted_bytes': sum(v['bytes'] for v in row['members']), 'member_count': len(row['members']),
                 'members': row['members'], 'upload_log_sha256': p.upload_slice(lines,row['artifact']['name'],row['artifact']['id'],row['artifact']['size_in_bytes'])[0],
                 'event_payload_sha256': p.digest(p.canonical(event)),
                 'expected_prior_checkpoint': unit['expected_prior_checkpoint'], 'produced_checkpoint': checkpoint_digest}
        artifact_receipts[n] = {'schema_version': 6, 'kind': 'ArtifactReceipt', 'record': value}
        p.envelope(artifact_receipts[n], 'ArtifactReceipt', root)
    expected_refs=[p.ref(artifact_receipts[n],artifact_receipts[n]['record']['artifact_id']) for n in sorted(artifact_receipts)]
    p.require(carrier['artifact_receipts']==[artifact_receipts[n] for n in sorted(artifact_receipts)]
              and unit['artifact_roots']==expected_refs,'carrier reconstructed artifact roots differ')
    _,again,_=p.producer(api,run_id,reg,mode,('success',))
    p.require((again['id'],again['task_id'],again['attempt'])==(job['id'],job['task_id'],1),
              'terminal producer identity changed after retrieval')
    live_pr(api,reg)
    return result, {'parts': parts, 'receipts': artifact_receipts, 'bytes': total,
                    'control':carrier,'control_sha256':control_sha}, metadata


def replay_control(api,control,root,work):
    p.closed(control,CONTROL_KEYS)
    p.require(type(control['schema_version']) is int and control['schema_version']==1 and type(control['unit_runs']) is list
              and type(control['attempt_runs']) is list and type(control['resume_decisions']) is list
              and len(control['unit_runs'])==len(set(control['unit_runs']))
              and len(control['attempt_runs'])==len(set(control['attempt_runs'])), 'closed attempt/control chain invalid')
    p.require(all(type(value) is int and value>0 for value in control['unit_runs']+control['attempt_runs']),
              'attempt/root run IDs must be positive integers')
    reg=registration(control['registration'],root)
    plan=p.envelope(control['resource_plan'],'ResourcePlan',root)
    p.require(len(control['attempt_runs'])<=plan['maximum_units'],'attempts exceed immutable unit allowance')
    api.maximum_session_requests=max(api.maximum_session_requests,400+(len(control['attempt_runs'])+3)*(32+3*p.UPLOAD_SLOTS))
    api.bind_storage(control['resource_plan'],reg,control['session_id']);live_pr(api,reg)
    inventory=attempt_inventory(api,reg,control,getattr(api,'current_run',None))
    p.closed(control['probe_run'],('success_run','failure_run','post_failure_run'))
    probe,measurement,_=authenticate_output(api,reg,control['probe_run']['success_run'],'unit',Path(work)/'probe',root)
    probe['finalizer_capability']=authenticate_probe_pair(api,reg,control['probe_run'])
    admitted_at=utc(inventory[0]['run']['created']) if inventory else time.time()
    state=admit(reg,control['resource_plan'],probe,measurement,root,admitted_at)
    controller_memory_limit(state['resource_plan'])
    p.require(control['session_id']==plan['session_id'],'control session UUID differs')
    decisions={}
    for decision in control['resume_decisions']:
        ordinal=decision['failed_ordinal']
        p.require(type(ordinal) is int and ordinal not in decisions,'duplicate/invalid infrastructure decision ordinal')
        decisions[ordinal]=decision
    p.require([d['failed_ordinal'] for d in control['resume_decisions']]==sorted(decisions),'unordered resume decisions')
    latest=None;history=[];successes=[]
    for ordinal,item in enumerate(inventory):
        state['next_ordinal']=ordinal
        before=item['control']
        p.require(before['unit_runs']==successes and before['attempt_runs']==control['attempt_runs'][:ordinal]
                  and before['resume_decisions']==[d for d in control['resume_decisions'] if d['failed_ordinal']<ordinal],
                  'attempt event omitted/changed its authenticated predecessor inventory or decision')
        expected_mode=dispatch_mode(state)
        p.require(item['mode']==expected_mode and state['phase'] not in ('resource_blocked','semantic_invalid','completion_rejected','complete'),
                  'attempt bypasses source phase or terminal disposition')
        if ordinal and inventory[ordinal-1]['job']['status']!='success':
            validate_resume_decision(api,decisions.get(ordinal-1),inventory[ordinal-1],state,reg)
        if item['job']['status']!='success':
            p.require(state['initial_image'] is not None,'failed initial attempt has no authenticated Init; new session required')
            if ordinal in decisions:validate_resume_decision(api,decisions[ordinal],item,state,reg)
        else:
            p.require(item['mode']=='unit','a final completion run cannot be adopted as an ordinary predecessor')
            result,transport,_=authenticate_output(api,reg,item['run']['id'],'unit',Path(work)/('unit-'+str(ordinal)),root)
            p.require(result['work_unit']['unit_ordinal']==ordinal
                      and transport['control']['previous_control_sha256']==state.get('control_sha256'),
                      'attempt ordinal or previous successful control differs')
            prior=state;usage=enforce_control_caps(state,transport,root)
            state=advance(state,result,root);state['operational_usage']=usage
            state=authenticate_verifier_progress(prior,state,result,transport,root)
            validate_backend_progress(prior,state,result,transport,root)
            state['control_sha256']=transport['control_sha256'];state['retained_bytes']+=transport['bytes']
            p.require(state['retained_bytes']<=plan['total_retained_artifact_bytes'],'retained artifact allowance exhausted')
            successes.append(item['run']['id']);history.append((result,transport))
            latest=(result,{**transport,'history':list(history)})
        state['attempts'].append({'ordinal':ordinal,'run':item['run']['id'],'job':item['job']['id'],
             'task':item['job']['task_id'],'attempt':1,'status':item['job']['status']})
    p.require(successes==control['unit_runs'] and set(decisions)<=set(range(len(inventory))),
              'successful roots/decisions do not match actual attempted inventory')
    state['next_ordinal']=len(inventory)
    state['retry_authorized']=not inventory or inventory[-1]['job']['status']=='success' or len(inventory)-1 in decisions
    return state,latest


def authenticated_roots(parts, index, root):
    """Seed only explicitly authenticated external roots; semantic pages self-link."""
    roots = []
    for directory in parts.values():
        for path in directory.rglob('*'):
            if not path.is_file() or path.is_symlink() or path.suffix != '.json':
                continue
            if path.stat().st_size > p.MAX_MEMBER or 'catalog-backup' in path.parts:
                continue
            value = p.read(path, p.MAX_MEMBER)
            if type(value) is dict and value.get('schema_version') == 6 and 'kind' in value and 'record' in value:
                kind = value['kind']; record = value['record']
                schema = p.read(Path(root) / p.SCHEMA_PATH)
                if kind not in schema['$defs']:
                    continue
                p.envelope(value, kind, root)
                if kind == 'Page': ident = record['page_id']
                elif kind == 'ManifestNode': ident = record['sha256']
                elif kind == 'Checkpoint': ident = p.digest(p.canonical(value))
                elif kind == 'InitialImage': ident = record['image_sha256']
                elif kind == 'ResourcePlan': ident = record['plan_sha256']
                else: continue
                reference = p.ref(value, ident)
                index.add(reference, p.transport_bytes(value), path.resolve())
                if kind in ('Page', 'ManifestNode'):
                    for child in record.get('dependencies', record.get('children', [])):
                        index.need(reference, child)
    for directory in parts.values():
        for sidecar in directory.rglob('*external-roots.ndjson'):
            for row in ndjson(sidecar):
                p.closed(row, ('reference', 'member_path'))
                relative = p.safe_path(row['member_path'])
                candidates = [d / relative for d in parts.values() if (d / relative).is_file()]
                p.require(len(candidates) == 1, 'external root missing/aliased')
                path = candidates[0]
                reference = row['reference']
                if path.suffix == '.json':
                    value = p.read(path, p.MAX_MEMBER)
                    payload = p.transport_bytes(value) if type(value) is dict and value.get('schema_version') == 6 else p.canonical(value)
                else:
                    p.require(path.stat().st_size <= p.MAX_MEMBER, 'external blob exceeds member bound')
                    payload = path.read_bytes()
                index.add(reference, payload, path.resolve()); index.root(reference); roots.append(reference)
    return roots


def ndjson(path):
    with Path(path).open('rb') as stream:
        while line := stream.readline(p.SMALL + 1):
            p.require(len(line) <= p.SMALL and line.endswith(b'\n'), 'unbounded/truncated NDJSON member')
            yield p.decode(line[:-1])


def member_stream(output, result):
    """Producer output selection is explicit; no hidden scratch/catalog uploads."""
    seen = set()
    for relative in result['artifacts']:
        relative = p.safe_path(relative)
        p.require(relative not in seen, 'duplicate unit artifact path')
        seen.add(relative)
        path = Path(output) / relative
        if relative.endswith('-members.ndjson'):
            for row in ndjson(path):
                p.closed(row, ('path', 'bytes', 'sha256'))
                name = p.safe_path(row['path']); target = Path(output) / name
                p.require(name not in seen and target.is_file() and not target.is_symlink()
                          and target.stat().st_size == row['bytes'] and p.file_sha(target) == row['sha256'],
                          'streamed output inventory mismatch')
                seen.add(name); yield name, target
        yield relative, path
    for name in ('unit-result.json', 'initial-image.json', 'session.json'):
        path = Path(output) / name
        if path.is_file() and name not in seen:
            seen.add(name); yield name, path


def source_commands(root, stage):
    if stage in ('semantic_verify', 'monitor_scc_verify', 'temporal_components','complete'):
        return [sys.executable, str(Path(root) / 'verification/async-driver-refinement/paged_checker.py'), 'prepare-unit', '--request']
    return [sys.executable, str(Path(root) / 'scripts/check-async-driver-refinement'), 'paged-unit', '--request']


def restore_latest(state, latest, source, request, output, reg):
    result, transport = latest
    root = transport['parts'][0] / 'catalog-backup/root.json'
    p.require(root.is_file(), 'fixed catalog backup root absent')
    generation=Path(output)/'state.restore'
    generation.mkdir(exist_ok=False)
    request['catalog_path']=str(generation/'catalog.sqlite')
    request['verifier_work_db']=str(generation/'verifier.sqlite')
    staged = generation / 'catalog.staged.sqlite'
    p.restore_catalog(p.read(root), transport['parts'], transport['receipts'], state['checkpoint'], staged, source,
                      transport_unit=result['work_unit'])
    p.write(Path(output)/'restored-catalog-root.json',p.read(root))
    index_path = Path(output) / 'dependencies.sqlite'
    index = p.DependencyIndex(index_path)
    try:
        for old_result,old_transport in transport.get('history',[(result,transport)]):
            authenticated_roots(old_transport['parts'],index,source)
            add_authenticated_members(index,old_result,old_transport,source)
    finally:
        index.close()
    # Restore authenticated private input state before any root correspondence
    # check; its safe prefix may still be preparing the current input scope.
    request['catalog_path']=str(staged)
    restore_verifier(state,latest,source,request,output,reg,index_path)
    restore = {**request, 'operation': 'validate_restore', 'catalog_path': str(staged),
               'checkpoint': state['checkpoint'], 'dependency_index': str(index_path)}
    restore['output_dir'] = str(Path(output) / 'restore-validation')
    Path(restore['output_dir']).mkdir()
    path = Path(output) / 'restore-request.json'; p.write(path, restore)
    for argv in ([sys.executable, str(Path(source) / 'scripts/check-async-driver-refinement'), 'paged-unit', '--request', str(path)],
                 [sys.executable, str(Path(source) / 'verification/async-driver-refinement/paged_checker.py'), 'validate-restore', '--request', str(path)]):
        run_quantum(path, reg, source, Path(restore['output_dir']), argv)
    private = p.read(Path(restore['output_dir']) / 'restore-validation.json')
    semantic = p.read(Path(restore['output_dir']) / 'restore-semantic.json')
    input_preparation=semantic.get('complete') is False and semantic.get('scope')=='input_preparation'
    p.atomic_install(staged, generation/'catalog.sqlite', private, semantic, state['checkpoint'],
                     backup_root=p.read(root),input_preparation=input_preparation)
    p.write(Path(output)/'restored-input-readiness.json',{'semantic_ready':not input_preparation})
    active=Path(output)/'state'
    p.require(not active.exists() and not active.is_symlink(),'refuse replace an active catalog/verifier generation')
    fd=os.open(generation,os.O_RDONLY)
    try:os.fsync(fd)
    finally:os.close(fd)
    os.replace(generation,active)
    fd=os.open(active.parent,os.O_RDONLY)
    try:os.fsync(fd)
    finally:os.close(fd)
    request['catalog_path']=str(active/'catalog.sqlite')
    request['verifier_work_db']=str(active/'verifier.sqlite')
    return index_path


def runner_body(control, root, output, mode, operation):
    """Trusted CI entry: authenticate first, isolate source, run, package, upload."""
    deadline = time.monotonic() + 1800
    root = Path(root).resolve(); output = Path(output).resolve()
    reg=registration(control['registration'],root)
    owned=validate_storage(reg['storage'],root)
    p.require(owned in output.parents,'session output escapes persistent owned storage')
    api=api_environment(owned/control['session_id']/'controller-api'/str(os.environ['GITHUB_RUN_ID']))
    api.bind_storage(control['resource_plan'],reg,control['session_id'])
    api.current_run=receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    api.maximum_session_requests=400+(len(control['attempt_runs'])+3)*(32+3*p.UPLOAD_SLOTS)
    reject_prior_ordinal_dispatch(api,reg,control,api.current_run)
    output.mkdir(parents=True, exist_ok=False)
    if operation=='execute':
        guard=owned/control['session_id']/('ordinal-'+str(len(control['attempt_runs']))+'.json')
        p.require(not guard.exists(),'executed ordinal already attempted; preserve failed state and start a new admitted session')
        with guard.open('xb') as stream:
            stream.write(p.canonical({'run':int(os.environ['GITHUB_RUN_ID']),'ordinal':len(control['attempt_runs']),
                                      'registration_sha256':p.digest(p.canonical(reg))}))
            stream.flush();os.fsync(stream.fileno())
        fd=os.open(guard.parent,os.O_RDONLY)
        try:os.fsync(fd)
        finally:os.close(fd)
    reg = registration(control['registration'], root)
    api.bind_storage(control['resource_plan'],reg,control['session_id'])
    p.require(os.environ.get('GITHUB_EVENT_NAME') == 'workflow_dispatch'
              and os.environ.get('GITHUB_RUN_ATTEMPT') == '1'
              and os.environ.get('GITHUB_SHA') == reg['source']['commit'], 'current producer environment differs')
    run_id = receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    api.current_run=run_id
    run, job, event = p.producer(api, run_id, reg, mode, ('running', 'in_progress'))
    local = p.read(os.environ['GITHUB_EVENT_PATH'])
    p.require(event == local and event['inputs']['operation'] == operation
              and p.decode(event['inputs']['control'].encode()) == control, 'current event/control differs')
    live_pr(api, reg)
    if operation=='execute':reject_prior_ordinal_dispatch(api,reg,control,run_id)
    source=materialize_session_source(root,reg,control['session_id'])
    identity = runtime_identity(source, reg)
    measurement = measured_resources(output, identity)
    measurement['storage']=reg['storage']
    measurement['producer']={'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1}
    if operation == 'probe':
        measurement=calibrate_overheads(source,output,control,measurement,deadline)
        p.require(mode == 'unit' and not control['unit_runs'], 'capability probe cannot execute a unit')
        value = {'schema_version': 1, 'registration_sha256': p.digest(p.canonical(reg)),
                 'source_archive_sha256': reg['source_archive_sha256'], 'runner': reg['runner'],
                 'measurement': measurement, 'runtime': reg['runtime'], 'session_id': control['session_id']}
        plan = p.PartPlan(output / 'upload')
        try:
            # Exercise every native slot, while performing no proof work or admission.
            for n in range(p.UPLOAD_SLOTS):
                plan.add_bytes('capability-' + str(n) + '.json', p.canonical(value), n)
            plan.add_bytes('resources.json',p.canonical(measurement),0)
            for name in ('catalog-calibration.json','catalog-calibration-execution.json','rust-catalog-calibration.log'):
                plan.add_file('calibration/'+name,Path(output)/'rust-calibration'/name,prefer=0)
            metadata = output_metadata(reg, control['session_id'], 0, plan, 'capability-0.json', 'probe')
            upload_outputs(metadata, output / 'upload')
        finally:
            plan.close()
        return
    p.require(operation == 'execute', 'unregistered operation')
    state, latest = replay_control(api, control, root, output / 'history')
    live_resources(state['resource_plan']['record'], reg, measurement)
    controller_memory_limit(state['resource_plan'])
    unit = unit_request(state, run_id, job, root)
    request = {'schema_version': 6, 'deadline_monotonic': deadline, 'operation': 'expand', 'work_unit': unit['record'],
               'resource_plan': state['resource_plan'], 'initial_image': state['initial_image'],
               'prior_checkpoint': state['checkpoint'], 'checkpoint': state['checkpoint'],
               'catalog_path': str(output / 'catalog.sqlite'), 'output_dir': str(output / 'quantum'),
               'dependency_index': str(output / 'dependencies.sqlite'),
               'source_files':reg['source_files'],'runtime':reg['runtime'],
               'driver_graph': str(output / 'driver-graph.json'),
               'verifier_work_db':str(output/'verifier.sqlite'),
               'prepare_path':str(output/'quantum/prepare-unit.json'),
               'reservation_path':str(output/'reservation.json'),
               'previous_verifier_progress_sha256':state.get('verifier_progress_sha256'),
               'previous_verifier_backup_sha256':state.get('verifier_backup_sha256'),
               'tlc': {'java': reg['runtime']['java']['path'], 'jar': reg['runtime']['tlc']['path'],
                       'java_sha256': reg['runtime']['java']['sha256'], 'jar_sha256': reg['runtime']['tlc']['sha256']}}
    quantum = Path(request['output_dir']); quantum.mkdir()
    emit_runtime_receipts(state,unit,identity,quantum)
    if latest:
        for directory in latest[1]['parts'].values():
            for name in ('driver-graph.json', state['initial_image']['record']['member_path']):
                path = directory / name
                if path.is_file():
                    target = output / name; target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(path, target)
        restore_latest(state, latest, source, request, output, reg)
    p.require(mode==dispatch_mode(state),'producer mode differs from authenticated replay routing')
    if mode == 'complete':
        p.require(state['stage'] == 'complete' and state['phase'] == 'qualified', 'completion premature')
        ready=p.read(output/'restore-validation/restore-semantic.json')
        backup=p.read(output/'restored-catalog-root.json')
        p.require(final_input_receipt(ready,state['checkpoint'],backup['catalog_sha256']),
                  'actual completion lost exact final input readiness; no preparation fallback')
        return finish_runner(api, reg, state, control, root, source, request, output, job)
    result,inventory=execute_and_package(state,reg,request,source,root,output,deadline)
    p.write(output/'finalizer-state.json',{'state':advance(state,result,root),'control':control,'unit':
            {'schema_version':6,'kind':'WorkUnit','record':result['work_unit']},
            'verified':None,'main_records':[]})
    try:
        resource = state['resource_plan']['record']
        actual = sum(r[0] for r in inventory.db.execute('SELECT bytes FROM members'))
        p.require(actual <= resource['unit_byte_allowance']
                  and actual + state['retained_bytes'] <= resource['total_retained_artifact_bytes'],
                  'actual upload bytes exceed immutable admission')
        upload_outputs(output_metadata(reg, control['session_id'], unit['record']['unit_ordinal'], inventory,
                                       'unit-result.json', 'execute'), output / 'upload')
    finally:
        inventory.close()
    p.require(time.monotonic() <= deadline, 'unit upload preflight exceeded admitted time')
    live_pr(api, reg)
    p.producer(api, run_id, reg, mode, ('running', 'in_progress'))


def live_resources(plan, reg, measurement):
    p.require(measurement.get('storage')==reg['storage'] and measurement['runner'] == reg['runner']
              and measurement['runtime_sha256'] == reg['source']['toolchain_sha256']
              and plan['total_session_disk_bytes'] <= measurement['free_disk_bytes']
              and plan['resident_rss_bytes'] <= measurement['rss_available_bytes'],
              'live owned slot no longer fits immutable resource plan')


def find_member(parts, name):
    p.safe_path(name)
    matches = [directory / name for directory in parts.values() if (directory / name).is_file()]
    p.require(len(matches) == 1, 'required immutable member absent/aliased: ' + name)
    return matches[0]


def finish_runner(api, reg, state, control, root, source, request, output, job):
    """A separate current Main verdict, never a relabelled PR run."""
    p.require(control['main_run'] is not None, 'completion needs independent current Main producer run')
    p.require(dispatch_mode(state)=='complete','final exact input certificate absent before Main aggregation')
    latest = state['units'][-1]
    # The backend unit's final root is reverified from authenticated pages and
    # receipts by the independent checker, never accepted from controller bits.
    request['operation'] = 'completion_verify'
    request['output_dir'] = str(output / 'completion-check')
    Path(request['output_dir']).mkdir()
    runtime_paths=('runtime/'+str(len(control['attempt_runs']))+'.json','resources.json',
                   'predecessors/'+str(len(control['attempt_runs']))+'.json','runtime-external-roots.ndjson')
    for name in runtime_paths:
        target=Path(request['output_dir'])/name;target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(output/'quantum'/name,target)
    path = output / 'completion-request.json'; p.write(path, request)
    argv = [sys.executable, str(Path(source) / 'verification/async-driver-refinement/paged_checker.py'),
            'complete', '--request', str(path)]
    verified=run_quantum(path,reg,source,Path(request['output_dir']),argv)
    main_records = authenticate_main(api, reg, control['main_run'], root, verified=verified)
    live = live_pr(api, reg)
    fresh = {'source': reg['source'], 'repository': reg['repository'], 'pr': reg['pr'],
             'head': live['head']['sha'], 'event_base': reg['event_base'],
             'current_base': live['base']['sha'], 'state': live['state'], 'draft': live['draft'],
             'run': int(os.environ['GITHUB_RUN_ID']), 'job': job['id'], 'task': job['task_id'], 'attempt': 1}
    out = output / 'completion'; out.mkdir()
    refs = []
    for record in main_records:
        name = 'main/' + record['condition'] + '.json'
        p.write(out / name, record)
        refs.append({'kind': 'main_receipt', 'id': name, 'sha256': p.digest(p.canonical(record))})
    p.write(out / 'fresh-head.json', fresh)
    unit = unit_request(state, int(os.environ['GITHUB_RUN_ID']), job, root)
    unit['record']['status'] = 'complete'
    result = completion(state, verified, refs,
                        {'kind': 'fresh_head_receipt', 'id': 'fresh-head.json', 'sha256': p.digest(p.canonical(fresh))},
                        p.ref(unit, unit['record']['unit_ordinal']), root)
    p.write(output / 'completion-draft.json', result)
    p.write(out / 'complete-verification.json', verified)
    p.write(out / 'checkpoint.json', state['checkpoint'])
    p.write(output / 'finalizer-state.json', {'state':state,'control':control,'unit':unit,
              'verified':verified,'main_records':main_records})
    plan = p.PartPlan(output / 'upload')
    try:
        for name in runtime_paths:plan.add_file(name,Path(request['output_dir'])/name)
        for file in sorted(out.rglob('*')):
            if file.is_file():
                plan.add_file(file.relative_to(out).as_posix(), file)
        # Completion is not a catalog work unit and cannot advance a checkpoint.
        metadata = output_metadata(reg, control['session_id'], unit['record']['unit_ordinal'], plan,
                                   'complete-verification.json', 'execute')
        upload_outputs(metadata, output / 'upload')
    finally:
        plan.close()
    live_pr(api, reg)


def authenticate_completion(api, reg, run_id, root, destination):
    result,transport,metadata=authenticate_output(api,reg,run_id,'complete',destination,root)
    carrier=transport['control'];parts=transport['parts']
    p.require(carrier['role']=='completion' and result['work_unit']['status']=='complete',
              'final producer lacks complete control')
    complete=p.envelope(carrier['completion'],'Completion',root)
    event=p.decode(api.get('/actions/runs/'+str(run_id))['event_payload'].encode())
    request_control=p.decode(event['inputs']['control'].encode())
    previous_current=getattr(api,'current_run',None)
    api.current_run=run_id
    try:state,_=replay_control(api,request_control,root,Path(destination)/'prior-chain')
    finally:api.current_run=previous_current
    p.require(carrier['previous_control_sha256']==state.get('control_sha256')
              and result['checkpoint']==state['checkpoint']
              and carrier['checkpoint']==p.ref(state['checkpoint'],p.digest(p.canonical(state['checkpoint']))),
              'completion advanced checkpoint or skipped control prefix')
    verified=result['verified']
    records=authenticate_main(api,reg,request_control['main_run'],root,verified=verified)
    refs=[]
    for row in records:
        name='main/'+row['condition']+'.json'
        p.require(p.read(find_member(parts,name))==row,'uploaded/current Main prerequisite differs')
        refs.append({'kind':'main_receipt','id':name,'sha256':p.digest(p.canonical(row))})
    fresh=p.read(find_member(parts,'fresh-head.json'))
    live=live_pr(api,reg)
    expected={'source':reg['source'],'repository':reg['repository'],'pr':reg['pr'],
        'head':live['head']['sha'],'event_base':reg['event_base'],'current_base':live['base']['sha'],
        'state':live['state'],'draft':live['draft'],'run':run_id,'job':result['work_unit']['job'],
        'task':result['work_unit']['task'],'attempt':1}
    p.require(fresh==expected,'completion archived/current live identities differ')
    freshref={'kind':'fresh_head_receipt','id':'fresh-head.json','sha256':p.digest(p.canonical(fresh))}
    actual=completion(state,verified,refs,freshref,p.ref(carrier['work_unit'],result['work_unit']['unit_ordinal']),root)
    p.require(actual==carrier['completion'] and complete['applicable_main_receipts']==refs
              and carrier['postcheck']['main_receipts']==refs
              and carrier['postcheck']['archived_fresh_head']==freshref, 'recomputed completion differs')
    member=carrier['postcheck']['semantic_verification']
    file=parts[member['part_ordinal']]/p.safe_path(member['member_path'])
    p.require(file==find_member(parts,'complete-verification.json') and file.stat().st_size==member['bytes']
              and p.file_sha(file)==member['sha256'],'independent verification member differs')
    conditions={'source':reg['source'],'event_base':reg['event_base'],
        'applicability_sha256':p.digest(p.canonical(source_scope(root,reg['event_base']))),
        'results':condition_results(root,reg,records),
        'receipt_refs':refs,'semantic_verification_sha256':member['sha256']}
    p.require(carrier['postcheck']['main_conditions_sha256']==p.digest(p.canonical(conditions)),
              'actual current Main condition preimage differs')
    p.producer(api,run_id,reg,'complete',('success',));live_pr(api,reg)
    return actual


def require_completion(root, output):
    """Ordinary PR job remains non-successful while no exact complete root exists."""
    p.require(os.environ.get('GITHUB_EVENT_NAME') == 'pull_request'
              and os.environ.get('GITHUB_RUN_ATTEMPT') == '1', 'ordinary async producer attempt/event differs')
    event = p.read(os.environ['GITHUB_EVENT_PATH'])
    commit = p.git(root, 'rev-parse', 'HEAD')
    api = api_environment(Path(output) / 'api')
    scope = {'pr': event['number'], 'repository': event['repository']['id']}
    adoption.validate_live_pr(api.get('/pulls/' + str(scope['pr'])), event, commit)
    candidates = api.pages('/actions/runs', {'workflow_id': p.WORKFLOW}, field='workflow_runs')
    for run in candidates:
        if run.get('commit_sha') != commit or run.get('event') != 'workflow_dispatch' or run.get('status')!='success':
            continue
        payload = p.decode(run.get('event_payload', '').encode())
        if payload.get('inputs', {}).get('mode') != 'complete':
            continue
        control = p.decode(payload['inputs']['control'].encode())
        reg = registration(control['registration'], root)
        p.require(reg['pr'] == scope['pr'] and reg['repository'] == scope['repository']
                  and reg['event_base'] == event['pull_request']['base']['sha'], 'completion current event scope differs')
        result = authenticate_completion(api, reg, run['id'], root, Path(output) / ('complete-' + str(run['id'])))
        p.write(Path(output) / 'completion.json', result)
        return
    raise receipts.ReceiptError('Async qualification session pending; complete registered Main verdict required')


def coordinate(control, root, output, maximum_dispatches=1):
    """Finite trusted controller; authenticate every predecessor before dispatch."""
    output = Path(output); output.mkdir(parents=True, exist_ok=True)
    api = api_environment(output / 'api')
    p.require(type(maximum_dispatches) is int and maximum_dispatches > 0, 'invalid controller dispatch bound')
    for iteration in range(maximum_dispatches):
        control,completed=reconcile_control(api,control,root)
        if completed is not None:
            result=authenticate_completion(api,control['registration'],completed,root,output/'complete')
            p.write(output/'completion.json',result);return
        p.write(output/'control.json',control)
        state, _ = replay_control(api, control, root, output / ('replay-' + str(iteration)))
        p.require(state['next_ordinal'] + maximum_dispatches - iteration <=
                  state['resource_plan']['record']['maximum_units'], 'controller exceeds immutable unit limit')
        expected = dispatch(api, state, {**control}, root)
        p.write(output / ('dispatch-' + str(iteration) + '.json'), expected)
        # No guessed next run ID. Match the actual immutable dispatch input.
        deadline = time.monotonic() + 1800
        selected = None
        while time.monotonic() < deadline:
            runs = api.pages('/actions/runs', {'workflow_id': p.WORKFLOW}, field='workflow_runs')
            matches = []
            for run in runs:
                if run.get('commit_sha') != control['registration']['source']['commit'] or run.get('event') != 'workflow_dispatch':
                    continue
                event = p.decode(run.get('event_payload', '').encode())
                inputs = event.get('inputs', {})
                if (inputs.get('registration_sha256') == expected['registration_sha256']
                    and inputs.get('mode') == expected['mode'] and inputs.get('operation') == 'execute'
                    and p.decode(inputs.get('control', '{}').encode()) == control
                    and utc(run['created']) >= utc(expected['dispatched_at']) - 1):
                    matches.append(run)
            p.require(len(matches) <= 1, 'ambiguous duplicate dispatch; no attempt splicing')
            if matches:
                selected = matches[0]
                pending=api.pages('/actions/runs/'+str(selected['id'])+'/jobs')
                name=p.COMPLETION_JOB if expected['mode']=='complete' else p.PRODUCER_JOB
                candidates=[job for job in pending if job.get('name')==name]
                p.require(len(candidates)<=1,'ambiguous pending producer job')
                if not candidates or candidates[0].get('status') in ('waiting','blocked'):
                    print('Awaiting registered producer task assignment',flush=True);time.sleep(15);continue
                _,actual_job,_=p.producer(api,selected['id'],control['registration'],expected['mode'],
                    ('running','in_progress','success','failure','cancelled','waiting','blocked'))
                terminal = actual_job['status']
                if terminal in ('success', 'failure', 'cancelled'):
                    if terminal!='success':
                        control={**control,'attempt_runs':[*control['attempt_runs'],selected['id']]}
                        p.write(output/'control.json',control)
                        raise receipts.ReceiptError('Terminal producer failure; preserved root requires diagnosed explicit infrastructure resume decision')
                    break
            print('Awaiting registered work unit', flush=True)
            time.sleep(15)
        else:
            raise receipts.ReceiptError('Async session: bounded controller wait expired; prior checkpoint preserved')
        if expected['mode'] == 'complete':
            result = authenticate_completion(api, control['registration'], selected['id'], root, output / 'complete')
            p.write(output / 'completion.json', result)
            return
        # The next loop independently authenticates parts and progress first.
        control = {**control,'unit_runs':[*control['unit_runs'],selected['id']],
                   'attempt_runs':[*control['attempt_runs'],selected['id']]}
        replay_control(api,control,root,output/('authenticated-'+str(iteration)))
        p.write(output / 'control.json', control)



def dispatch_probe(control, root, output):
    """Establish exact candidate/native-slot capability without proof admission."""
    output = Path(output); output.mkdir(parents=True,exist_ok=True)
    p.require(control['probe_run'] is None and not control['unit_runs'] and not control['attempt_runs'] and not control['resume_decisions'] and control['main_run'] is None,
              'probe cannot reuse or execute a session unit')
    reg = registration(control['registration'],root)
    p.require(control.get('probe_case') in ('success','failure','post_failure')
              and p.SHA.fullmatch(control.get('probe_nonce','')),'closed capability case/nonce required')
    p.closed(control,(*CONTROL_KEYS,'probe_case','probe_nonce'))
    api = api_environment(output/'api'); live_pr(api,reg)
    payload = p.canonical({'ref':reg['ref'],'inputs':{'mode':'unit','operation':'probe',
              'registration_sha256':p.digest(p.canonical(reg)),'control':p.canonical(control).decode()}})
    request = urllib.request.Request(api.root+'/actions/workflows/'+p.WORKFLOW+'/dispatches',
              data=payload,method='POST',headers={'Authorization':'token '+api.token,'Content-Type':'application/json'})
    authenticate_runner_route(api,reg,root)
    with api.opener.open(request,timeout=30) as response:
        p.require(response.status in (200,201,204),'candidate workflow probe dispatch rejected')
    p.write(output/'probe-dispatch.json',{'registration_sha256':p.digest(p.canonical(reg)),
            'source':reg['source'],'ref':reg['ref'],'upload_slot_count':p.UPLOAD_SLOTS,
            'capability_established':False,'dispatched_at':datetime.now(timezone.utc).isoformat()})
    # API acceptance never becomes capability. coordinate/runner admission
    # authenticates the actual successful probe/native task and all32 parts.

def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('main-aggregate')
    ordinary = sub.add_parser('require-completion')
    ordinary.add_argument('--output', type=Path, required=True)
    finalizer = sub.add_parser('finalize-uploads')
    finalizer.add_argument('--output',type=Path)
    execute = sub.add_parser('runner')
    execute.add_argument('--output', type=Path)
    probe = sub.add_parser('probe')
    probe.add_argument('--control',type=Path,required=True)
    probe.add_argument('--output',type=Path,required=True)
    controller = sub.add_parser('coordinate')
    controller.add_argument('--control', type=Path, required=True)
    controller.add_argument('--output', type=Path, required=True)
    controller.add_argument('--maximum-dispatches', type=int, default=1)
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parents[1]
    if args.command == 'main-aggregate':
        ordinary_main()
    elif args.command == 'finalize-uploads':
        finalize_uploads(args.output or event_output(root),root)
    elif args.command == 'require-completion':
        require_completion(root, args.output)
    elif args.command == 'probe':
        dispatch_probe(p.read(args.control),root,args.output)
    elif args.command == 'coordinate':
        coordinate(p.read(args.control), root, args.output, args.maximum_dispatches)
    else:
        event = p.read(os.environ['GITHUB_EVENT_PATH'])
        inputs = event['inputs']
        control = p.decode(inputs['control'].encode())
        runner(control, root, args.output or event_output(root), inputs['mode'], inputs['operation'])




PREPARE_ROLES = ('source_tables','module','config','log','receipt','local_id_map','witness',
                 'temporal_import','temporal_publication','result','evidence_index','progress',
                 'catalog_page','catalog_manifest','initial_image','source_receipt','phase_result')


def reserve_verifier(prepare, state, unit, final_checkpoint, source, output, plan, published_members=()):
    """Fix output member parts before M3 seals any result/progress preimage."""
    p.closed(prepare,('protocol','source','session_id','unit_ordinal','resource_plan_sha256',
                     'initial_image_sha256','input_checkpoint_sha256','input_manifest_root',
                     'input_catalog_commit','input_catalog_logical_digest','before_verifier_commit',
                     'before_logical_digest','before_cursor','staged_members','metadata_reservations'))
    cp=state['checkpoint']['record'];u=unit
    p.require(type(u) is dict and 'record' not in u,'late export current unit must be record')
    p.require(prepare['protocol']=='verifier_prepare_v1' and prepare['source']==u['source']
              and prepare['session_id']==u['session_id'] and prepare['unit_ordinal']==u['unit_ordinal']
              and prepare['resource_plan_sha256']==u['resource_plan_sha256']
              and prepare['initial_image_sha256']==cp['initial_image_id']
              and prepare['input_checkpoint_sha256']==p.digest(p.canonical(state['checkpoint']))
              and prepare['input_manifest_root']==cp['page_root']
              and prepare['input_catalog_commit']==cp['committed_transaction']
              and prepare['input_catalog_logical_digest']==cp['catalog_logical_digest'],
              'prepare token source/input snapshot differs')
    p.envelope(final_checkpoint,'Checkpoint',source)
    final=final_checkpoint['record']
    preparation_only=prepare['before_cursor']['operation'] in ('load_input','check_input') and final_checkpoint==state['checkpoint']
    p.require(final['source']==u['source'] and final['session_id']==u['session_id']
              and final['initial_image_id']==cp['initial_image_id']
              and (preparation_only or (final['unit_ordinal']==u['unit_ordinal']
              and final['previous_checkpoint']==prepare['input_checkpoint_sha256'])),
              'late export final checkpoint differs')
    before=state.get('verifier_progress')
    if before:
        p.require(prepare['before_verifier_commit']>=before['after_verifier_commit']
                  and prepare['before_cursor']['quantum']>=before['after_cursor']['quantum'],
                  'verifier pre-seal state moved backwards')
    else:
        p.require(type(prepare['before_verifier_commit']) is int and prepare['before_verifier_commit']>=0
                  and prepare['before_cursor']['quantum']>=0,
                  'invalid first verifier pre-seal state')
    # These are the end-of-work pre-seal values, not the before-unit origin.
    # Finalize binds them exactly to the workDB; UnitProgress separately binds
    # its before-unit state to the preceding authenticated unit's after state.
    members=[];reservations=[];names=set()
    for row in prepare['staged_members']:
        p.closed(row,('path','bytes','sha256','role'))
        name=p.safe_path(row['path']);target=Path(output)/name
        p.require(name not in names and row['role'] in PREPARE_ROLES and target.is_file()
                  and target.stat().st_size==row['bytes'] and p.file_sha(target)==row['sha256'],
                  'prepare staged member preimage differs')
        names.add(name);part=plan.add_file(name,target)
        members.append({**row,'part_ordinal':part})
    for row in prepare['metadata_reservations']:
        p.closed(row,('path','maximum_bytes','role'))
        name=p.safe_path(row['path'])
        allowed_receipt=(u['stage']=='complete' and u['mode']=='unit' and row['role']=='receipt'
                         and name=='restore-semantic.json')
        p.require(name not in names and (allowed_receipt or row['role'] in ('result','evidence_index','progress','temporal_publication')),
                  'future metadata path/role not closed')
        names.add(name);part=plan.reserve(name,row['maximum_bytes'],row['role'])
        reservations.append({**row,'part_ordinal':part})
    for row in published_members:
        p.require(row['path'] not in names,'published member aliases prepared metadata')
        names.add(row['path']);members.append(row)
    prefix=plan.preflight(pending=True)
    p.require(prefix<p.UPLOAD_SLOTS,'fixed evidence prefix leaves no complete backup slots')
    return {'protocol':'verifier_reservation_v1','prepare_sha256':p.digest(p.canonical(prepare)),
            'source':u['source'],'session_id':u['session_id'],'unit_ordinal':u['unit_ordinal'],
            'resource_plan_sha256':u['resource_plan_sha256'],'initial_image_sha256':cp['initial_image_id'],
            'input_checkpoint_sha256':prepare['input_checkpoint_sha256'],'final_checkpoint':final_checkpoint,
            'members':members,'metadata_reservations':reservations,'fixed_prefix_part_count':prefix,
            'backup_first_part':max(1,prefix),'upload_slot_count':p.UPLOAD_SLOTS,
            'maximum_backup_parts':p.UPLOAD_SLOTS-max(1,prefix)}


def directory_bytes(directory):
    total=0
    for path in Path(directory).rglob('*'):
        info=path.lstat()
        if path.is_symlink():
            total+=info.st_blocks*512
        elif path.is_file():total+=max(info.st_size,info.st_blocks*512)
        elif path.is_dir():total+=info.st_blocks*512
    return total


def calibrate_overheads(root, output, control, measurement, deadline):
    """Fresh operational calibration/allocation, explicitly not a width proof."""
    import sqlite3
    reg=control['registration'];plan=control['resource_plan']['record']
    wrapper=operational_bounds(plan['source_bounds_utf8'],reg,root,measurement_pending=True)
    widths,_=p.source_bound_receipt(wrapper['source_receipt'],root,reg['source'],reg['source_files'],
                                  plan['total_session_disk_bytes'])
    counts=wrapper['record_allowances']
    inventory=accounting_inventory(wrapper['source_receipt'])
    p.require(set(counts)==set(inventory),'calibration operational kind inventory differs')
    geometry=[]
    calibrated=Path(output)/'rust-calibration';calibrated.mkdir()
    request={'operation':'catalog_calibration','resource_plan':control['resource_plan'],'session_id':control['session_id'],
             'deadline_monotonic':deadline,'output_dir':str(calibrated)}
    request_path=Path(output)/'catalog-calibration-request.json';p.write(request_path,request)
    execution=run_quantum(request_path,reg,root,calibrated,
        [sys.executable,str(Path(root)/'scripts/check-async-driver-refinement'),'calibrate-catalog','--output-dir',str(calibrated)])
    rust=validate_catalog_calibration(execution,calibrated,reg,root)
    geometry.append(rust)
    for name in ('paged-verifier-state.sql',):
        source=Path(root)/'verification/async-driver-refinement'/name
        path=Path(output)/('calibration-'+name+'.sqlite')
        p.require(not path.exists(),'calibration DB must be fresh')
        db=sqlite3.connect(path)
        try:
            db.executescript(source.read_text());db.commit()
            table_names=[row[0] for row in db.execute("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")]
            empty_rows={table:db.execute('SELECT count(*) FROM \"'+table.replace('\"','\"\"')+'\"').fetchone()[0] for table in table_names}
            p.require(all(value==0 for value in empty_rows.values()),'calibration DDL instantiated nonempty harness/production state')
            geometry.append({'engine':'python_sqlite3','source_path':'verification/async-driver-refinement/'+name,
                             'source_sha256':p.file_sha(source),'sqlite_version':db.execute('SELECT sqlite_version()').fetchone()[0],
                             'sqlite_source_id':db.execute('SELECT sqlite_source_id()').fetchone()[0],
                             'empty_table_rows':empty_rows,'acceptance':False,
                             'page_size':db.execute('PRAGMA page_size').fetchone()[0],
                             'page_count':db.execute('PRAGMA page_count').fetchone()[0],
                             'freelist_count':db.execute('PRAGMA freelist_count').fetchone()[0],
                             'indexes':db.execute("SELECT count(*) FROM sqlite_schema WHERE type='index'").fetchone()[0],
                             'tables':db.execute("SELECT count(*) FROM sqlite_schema WHERE type='table'").fetchone()[0]})
        finally:db.close()
    total_records=sum(row['records'] for row in counts.values())
    record_bytes=sum(row['records']*row['max_encoded_bytes'] for row in counts.values())
    # Conservative allocation under this reviewed method. Empty geometry is a
    # measured input, never claimed to prove full SQLite growth. Runtime guards
    # stop before an admitted allocation is exhausted.
    index_bytes=sum(g['page_size']*(g['page_count']+total_records*max(1,g['indexes']+g['tables']))
                    for g in geometry)+2*record_bytes
    wal_bytes=2*(record_bytes+index_bytes)+sum(g['page_size'] for g in geometry)
    source_workspace_bytes=directory_bytes(root)+(Path(root).parent/('source-'+reg['source_archive_sha256']+'.tar')).stat().st_size
    actual_scratch=directory_bytes(output)+source_workspace_bytes
    retained_dir=Path(output)/'history'
    actual_retained=directory_bytes(validate_storage(reg['storage'],root))
    maximum_backups=2*(record_bytes+index_bytes)
    overheads={'index_bytes':index_bytes,'wal_bytes':wal_bytes,
               'scratch_bytes':actual_scratch+plan['unit_byte_allowance'],
               'retained_bytes':actual_retained+6*plan['total_retained_artifact_bytes']+32*p.MIB*(plan['maximum_units']+3),
               'duplicate_backup_bytes':6*maximum_backups+2*plan['unit_byte_allowance']}
    return {**measurement,'resource_overheads':overheads,
            'resource_method':{'id':'sqlite_geometry_operational_allocation_v1','source_sha256':accounting_method_digest(reg['source_files']),
               'source_bound_specification_sha256':reg['source_files'][p.SOURCE_BOUND_SPEC],
               'geometry':geometry,'record_allowances':counts,'accounting_inventory':inventory,
               'maximum_combined_backup_bytes':maximum_backups,'minimum_fixed_prefix_parts':1,
               'actual_prefix_reserved_at_late_export':True,'plan_inputs':accounting_plan_inputs(plan),
               'actual_scratch_bytes':actual_scratch,'source_workspace_bytes':source_workspace_bytes,
               'actual_retained_bytes':actual_retained,'actual_resident_bytes':resource.getrusage(resource.RUSAGE_SELF).ru_maxrss*1024,
               'is_source_valuation_theorem':False,'indivisible_fit_guaranteed':False}}



def add_authenticated_members(index, result, transport, root):
    unit=result['work_unit']
    for part,receipt_envelope in transport['receipts'].items():
        receipt=p.envelope(receipt_envelope,'ArtifactReceipt',root)
        for member in receipt['members']:
            path=transport['parts'][part]/p.safe_path(member['path'])
            p.require(path.is_file() and p.file_sha(path)==member['sha256'],'authenticated resolver member changed')
            row=(unit['unit_ordinal'],part,member['path'],member['bytes'],member['sha256'],str(path.resolve()))
            old=index.db.execute('SELECT bytes,sha256 FROM authenticated_members WHERE unit_ordinal=? AND part_ordinal=? AND member_path=?',row[:3]).fetchone()
            p.require(old is None or old==(member['bytes'],member['sha256']),'historical member tuple conflicts')
            index.db.execute('INSERT OR REPLACE INTO authenticated_members VALUES(?,?,?,?,?,?)',row)
    progress=[];backup=[]
    for directory in transport['parts'].values():
        for name,target in (('verifier-progress/unit.json',progress),('verifier-backup/root.json',backup)):
            path=directory/name
            if path.is_file():target.append(path)
    p.require(len(progress)==len(backup)<=1,'verifier progress/backup pair incomplete or duplicate')
    if progress:
        p.verifier_auxiliary(p.read(progress[0]),root);p.verifier_auxiliary(p.read(backup[0]),root)
        index.db.execute('INSERT OR REPLACE INTO verifier_chain VALUES(?,?,?,?)',
                         (unit['unit_ordinal'],str(progress[0].resolve()),str(backup[0].resolve()),
                          p.canonical({'schema_version':6,'kind':'WorkUnit','record':unit})))
    materialize_control(index,transport,root)
    index.db.commit()


def process_tree_rss(pid):
    pending=[pid];seen=set();total=0
    while pending:
        current=pending.pop()
        if current in seen:continue
        seen.add(current)
        p.require(len(seen)<=4096,'owned process-tree resource measurement exhausted')
        try:
            status=Path('/proc')/str(current)/'status'
            for line in status.read_text().splitlines():
                if line.startswith('VmRSS:'):total+=int(line.split()[1])*1024
            children=Path('/proc')/str(current)/'task'/str(current)/'children'
            pending.extend(int(item) for item in children.read_text().split())
        except FileNotFoundError:
            continue
    return total


def publication_request(request,phase,output):
    request=copy.deepcopy(request)
    request['work_unit']=phase['work_unit']
    p.require(type(request['work_unit']) is dict and 'record' not in request['work_unit'],
              'current phase WorkUnit must be record')
    request['operation']='temporal_import'
    nd=Path(output)/'temporal-import.ndjson'
    if not nd.exists():nd.write_bytes(b'')
    sidecar=Path(output)/'temporal-publication.json'
    if not sidecar.is_file():
        current=request['checkpoint']['record'];count=sum(1 for _ in ndjson(nd))
        p.write(sidecar,{'schema_version':6,'source':request['work_unit']['source'],
             'session_id':request['work_unit']['session_id'],
             'resource_plan_sha256':request['work_unit']['resource_plan_sha256'],
             'input_checkpoint_sha256':p.digest(p.canonical(request['checkpoint'])),
             'input_catalog_commit':current['committed_transaction'],
             'ndjson_sha256':p.file_sha(nd),'ndjson_bytes':nd.stat().st_size,'record_count':count,
             'final':False,'stage_complete':phase['stage_complete']})
    request['temporal_publication']=str(sidecar);request['temporal_import']=str(nd)
    return request


def transport_reservations(plan,verifier):
    names=['catalog-backup/root.json','catalog-backup/index/0-0.json']
    if verifier:names+=['verifier-backup/root.json','verifier-backup/index/0-0.json']
    for name in names:
        part=plan.reserve(name,p.SMALL,'transport')
        p.require(part==0,'fixed transport metadata does not fit part0')


def final_input_execution(unit,checkpoint,catalog_sha256,backup_sha256,progress_sha256,receipt_sha256,log,reg):
    return {'schema_version':1,'source':unit['source'],'session_id':unit['session_id'],
        'unit_ordinal':unit['unit_ordinal'],'origin':{k:unit[k] for k in ('run','job','task','attempt')},
        'operation':'validate_restore','command':['python','verification/async-driver-refinement/paged_checker.py','validate-restore','--request'],
        'checker_sha256':reg['source_files']['verification/async-driver-refinement/paged_checker.py'],
        'runtime_sha256':p.digest(p.canonical(reg['runtime'])),
        'checkpoint_sha256':p.digest(p.canonical(checkpoint)),'catalog_sha256':catalog_sha256,
        'verifier_backup_sha256':backup_sha256,'verifier_progress_sha256':progress_sha256,
        'receipt_sha256':receipt_sha256,'log_sha256':p.file_sha(log),'log_bytes':Path(log).stat().st_size,
        'acceptance':False}


def seal_final_input(request,produced,reg,source,output,request_path):
    """The future receipt authenticates the exact already sealed private bytes."""
    backup=Path(output)/'verifier-backup.sqlite';catalog=Path(request['catalog_path'])
    backup_sha=p.file_sha(backup);catalog_sha=p.file_sha(catalog)
    before_logs=set(Path(output).glob('quantum-*.log'))
    validate={**request,'operation':'validate_restore','verifier_work_db':str(backup),
              'checkpoint':produced['checkpoint']}
    p.write(request_path,validate)
    run_quantum(request_path,reg,source,output,[sys.executable,
        str(Path(source)/'verification/async-driver-refinement/paged_checker.py'),
        'validate-restore','--request',str(request_path)])
    p.require(p.file_sha(backup)==backup_sha and p.file_sha(catalog)==catalog_sha,
              'postseal final input validation mutated sealed private/catalog bytes')
    receipt=Path(output)/'restore-semantic.json'
    final_input_receipt(p.read(receipt),produced['checkpoint'],catalog_sha)
    logs=set(Path(output).glob('quantum-*.log'))-before_logs
    p.require(len(logs)==1,'postseal readonly invocation log missing/ambiguous')
    log=next(iter(logs));p.require(log.stat().st_size+len(FINAL_INPUT_LOG_PREFIX)<=p.SMALL,'postseal readonly log bound exceeded')
    # Source-owned framing preserves even an empty successful child capture.
    with (Path(output)/FINAL_INPUT_LOG).open('xb') as target,log.open('rb') as captured:
        target.write(FINAL_INPUT_LOG_PREFIX);shutil.copyfileobj(captured,target)
        target.flush();os.fsync(target.fileno())
    evidence=final_input_execution(produced['work_unit'],produced['checkpoint'],catalog_sha,backup_sha,
        p.file_sha(Path(output)/'verifier-progress/unit.json'),p.file_sha(receipt),Path(output)/FINAL_INPUT_LOG,reg)
    p.write(Path(output)/FINAL_INPUT_EXECUTION,evidence)


def authenticate_final_input(state,result,transport,progress,backup,root):
    """Scheduling evidence is source-owned same-unit execution, never a flag."""
    unit=result['work_unit'];reg=state['registration']
    p.require(unit['mode']=='unit' and result['stage_complete'] is False
              and progress['after_cursor']['operation'] in ('load_input','check_input'),
              'final preparation attempted semantic work or completion')
    receipt_path=find_member(transport['parts'],'restore-semantic.json')
    log=find_member(transport['parts'],FINAL_INPUT_LOG)
    execution=p.read(find_member(transport['parts'],FINAL_INPUT_EXECUTION))
    with log.open('rb') as captured:
        p.require(captured.read(len(FINAL_INPUT_LOG_PREFIX))==FINAL_INPUT_LOG_PREFIX,'readonly log capture framing differs')
    catalog=p.auxiliary(p.read(find_member(transport['parts'],'catalog-backup/root.json')),root)
    cp=result['checkpoint']['record']
    identities={'source':unit['source'],'session_id':unit['session_id'],'unit_ordinal':unit['unit_ordinal'],
        'checkpoint_sha256':p.digest(p.canonical(result['checkpoint'])),
        'previous_checkpoint_sha256':unit['expected_prior_checkpoint'],
        'resource_plan_sha256':unit['resource_plan_sha256'],'initial_image_sha256':cp['initial_image_id'],
        'catalog_commit':cp['committed_transaction'],'catalog_logical_digest':cp['catalog_logical_digest'],
        'semantic_manifest_root':cp['page_root'],'artifact_part_count':len(transport['parts'])}
    p.require(all(catalog[k]==value and backup[k]==value for k,value in identities.items())
              and log.stat().st_size<=p.SMALL,
              'final preparation catalog/private/current-unit/log binding differs')
    expected=final_input_execution(unit,result['checkpoint'],catalog['catalog_sha256'],backup['work_db_sha256'],
        state['verifier_progress_sha256'],p.file_sha(receipt_path),log,reg)
    p.require(p.canonical(execution)==p.canonical(expected),'final readiness source invocation/private chain differs')
    ready=final_input_receipt(p.read(receipt_path),result['checkpoint'],catalog['catalog_sha256'])
    state['final_input_checkpoint_sha256']=p.digest(p.canonical(result['checkpoint'])) if ready else None


def execute_and_package(state,reg,request,source,root,output,deadline):
    stage=state['stage'];quantum=Path(request['output_dir'])
    request['operation']='expand' if stage=='expand' else 'unit'
    path=Path(output)/'request.json';p.write(path,request)
    checked=stage in ('semantic_verify','monitor_scc_verify','temporal_components','complete')
    produced=run_quantum(path,reg,source,quantum,source_commands(source,stage)+[str(path)])
    prepare=produced if checked else None
    preparation_only=checked and prepare['before_cursor']['operation'] in ('load_input','check_input')
    if stage=='complete':
        p.require(preparation_only and request['work_unit']['mode']=='unit','final unit cannot resume semantic/TLC/backend work')
    if stage!='expand':
        phase=p.read(quantum/'phase-result.json') if checked else produced
        p.closed(phase,('work_unit','stage_complete') if checked else ('work_unit','stage_complete','receipt'))
        p.require(type(phase['stage_complete']) is bool,'phase completion flag not typed')
        if preparation_only:
            p.require(phase['stage_complete'] is False and state['checkpoint'] is not None,
                      'input preparation cannot close a stage or initialize production state')
            produced={'schema_version':6,**phase,'checkpoint':state['checkpoint'],
                      'initial_image':state['initial_image'],'session':state['session'],'artifacts':[]}
            # The restored catalog is byte-identical to its authenticated prior
            # backup. Retransport that frozen snapshot without invoking M1.
            saved=p.read(Path(output)/'restored-catalog-root.json')
            backup=Path(request['catalog_path'])
            p.require(backup.stat().st_size==saved['catalog_bytes'] and p.file_sha(backup)==saved['catalog_sha256'],
                      'preparation changed the read-only catalog snapshot')
            shutil.copyfile(backup,quantum/'catalog-backup.sqlite')
            descriptor={k:saved[k] for k in ('source','session_id','checkpoint_sha256',
                'resource_plan_sha256','initial_image_sha256','catalog_commit','catalog_logical_digest',
                'semantic_manifest_root','snapshot_boundary','format','backup_method','catalog_bytes','catalog_sha256')}
            descriptor.update(unit_ordinal=request['work_unit']['unit_ordinal'],
                              previous_checkpoint_sha256=request['work_unit']['expected_prior_checkpoint'])
            p.write(quantum/'backup-descriptor.json',descriptor)
        else:
            request=publication_request(request,phase,quantum);p.write(path,request)
            produced=run_quantum(path,reg,source,quantum,
                       [sys.executable,str(Path(source)/'scripts/check-async-driver-refinement'),'paged-unit','--request',str(path)])
    plan=p.PartPlan(Path(output)/'upload',limits={'owned_root':str(output),
          'disk_bytes':state['resource_plan']['record']['total_session_disk_bytes'],
          'unit_bytes':state['resource_plan']['record']['unit_byte_allowance'],'deadline_monotonic':deadline})
    try:
        transport_reservations(plan,checked)
        runtime_name='runtime/'+str(request['work_unit']['unit_ordinal'])+'.json'
        for name in (runtime_name,'resources.json','runtime-external-roots.ndjson',
                     'predecessors/'+str(request['work_unit']['unit_ordinal'])+'.json'):
            plan.add_file(name,quantum/name)
        published=[]
        for name,file in member_stream(quantum,produced):
            if name=='unit-result.json':continue  # written only after final export
            if checked and name in {r['path'] for r in prepare['staged_members']}:
                continue
            if name.startswith(('catalog-backup/','verifier-backup/')) or name in ('catalog-backup.sqlite','backup-descriptor.json'):
                continue
            part=plan.add_file(name,file)
            value=p.read(file,p.MAX_MEMBER) if file.suffix=='.json' else None
            if type(value) is dict and value.get('schema_version')==6 and value.get('kind') in ('Page','ManifestNode','Checkpoint'):
                role='catalog_page' if value['kind']=='Page' else 'catalog_manifest'
                published.append({'path':name,'part_ordinal':part,'bytes':file.stat().st_size,'sha256':p.file_sha(file),'role':role})
        backup_first=1
        if checked:
            # Reserve the controller result too; it is not included in any
            # self-referencing checker result/output inventory.
            if not any(r['path']=='unit-result.json' for r in prepare['metadata_reservations']):
                plan.reserve('unit-result.json',p.SMALL,'phase_result')
            if stage=='complete':
                p.require(preparation_only and request['work_unit']['mode']=='unit',
                          'final unit may only authenticate frozen inputs')
                p.require(any(r['path']=='restore-semantic.json' and r['role']=='receipt'
                              for r in prepare['metadata_reservations']), 'final preparation receipt not reserved')
                plan.reserve(FINAL_INPUT_EXECUTION,p.SMALL,'source_receipt')
                plan.reserve(FINAL_INPUT_LOG,p.SMALL,'source_receipt')
            reservation=reserve_verifier(prepare,state,request['work_unit'],produced['checkpoint'],
                                         source,quantum,plan,published)
            if stage=='complete':
                for name in (FINAL_INPUT_EXECUTION,FINAL_INPUT_LOG):
                    row=plan.db.execute('SELECT part,bytes FROM members WHERE path=?',(name,)).fetchone()
                    p.require(row is not None,'controller final-input metadata reservation disappeared')
                    reservation['metadata_reservations'].append({'path':name,'part_ordinal':row[0],
                        'maximum_bytes':row[1],'role':'source_receipt'})
            p.write(request['reservation_path'],reservation)
            published_result=produced
            request['checkpoint']=produced['checkpoint']
            request.pop('temporal_publication',None);request.pop('temporal_import',None)
            request['operation']='finalize_unit';p.write(path,request)
            finalized=run_quantum(path,reg,source,quantum,
                      [sys.executable,str(Path(source)/'verification/async-driver-refinement/paged_checker.py'),
                       'finalize-unit','--request',str(path)])
            p.closed(finalized,('schema_version','work_unit','checkpoint','artifacts','stage_complete'))
            p.require(finalized['schema_version']==6 and finalized['checkpoint']==published_result['checkpoint']
                      and finalized['stage_complete']==p.read(quantum/'phase-result.json')['stage_complete'],
                      'final checker changed authoritative publisher checkpoint or phase completion')
            produced={**published_result,**finalized,
                'initial_image':published_result['initial_image'],'session':published_result['session']}
            progress=p.verifier_auxiliary(p.read(quantum/'verifier-progress/unit.json'),root)
            p.require(progress['committed_quantum_count']>0 and progress['result_count']>0,
                      'resource_blocked: framing-only verifier unit made no useful committed progress')
            if stage=='complete':
                seal_final_input(request,produced,reg,source,quantum,path)
            for row in reservation['metadata_reservations']:
                if row['path']!='unit-result.json':plan.fulfill(row['path'],quantum/row['path'])
            backup_first=reservation['backup_first_part']
        # Always retain exact raw source graph and separate concrete Init image.
        driver=Path(request['driver_graph'])
        if driver.is_file():plan.add_file('driver-graph.json',driver)
        image=produced['initial_image']['record']
        image_name=p.safe_path(image['member_path'])
        if not plan.db.execute('SELECT 1 FROM members WHERE path=?',(image_name,)).fetchone():
            original=Path(output)/image_name
            p.require(original.is_file() and original.stat().st_size==image['image_bytes']
                      and p.file_sha(original)==image['image_sha256'],'actual original Init bytes unavailable')
            plan.add_file(image_name,original)
        if stage=='backend_qualification':
            backend=p.read(quantum/'backend-progress.json')
            receipt=backend['receipt'];name=receipt['id']
            plan.add_file(name,quantum/name)
            inventory=p.read(quantum/'backend-members.json')
            p.closed(inventory,('members',))
            for member in inventory['members']:
                p.closed(member,('path','bytes','sha256'))
                file=quantum/p.safe_path(member['path'])
                p.require(file.stat().st_size==member['bytes'] and p.file_sha(file)==member['sha256'],'backend member bytes changed')
                plan.add_file(member['path'],file)
            roots=quantum/'backend-external-roots.ndjson'
            if roots.is_file():plan.add_file(roots.name,roots)
        produced['artifacts']=[name for name, in plan.db.execute("SELECT path FROM members WHERE digest NOT LIKE 'reserved:%' ORDER BY path")
            if not name.startswith(('catalog-backup/','verifier-backup/')) and name!='unit-result.json']
        p.write(Path(output)/'unit-finalizer-result.json',produced)
        transport_result={k:v for k,v in produced.items() if k!='work_unit'}
        transport_result['planned_work_unit']=produced['work_unit']
        p.write(quantum/'unit-result.json',transport_result)
        if checked:plan.fulfill('unit-result.json',quantum/'unit-result.json')
        else:plan.add_file('unit-result.json',quantum/'unit-result.json')
        advance(state,produced,root)
        descriptor=p.read(quantum/'backup-descriptor.json')
        _,plan=p.pack_catalog(quantum/'catalog-backup.sqlite',descriptor,produced['checkpoint'],Path(output)/'upload',root,
                             plan=plan,backup_first_part=backup_first,deferred=checked,transport_unit=produced['work_unit'])
        if checked:
            p.pack_verifier(plan,quantum/'verifier-backup.sqlite',p.read(quantum/'verifier-backup-descriptor.json'),
                            produced['checkpoint'],root,backup_first_part=backup_first,transport_unit=produced['work_unit'])
        plan.preflight()
        return produced,plan
    except BaseException:
        plan.close();raise


def authenticate_verifier_progress(previous,state,result,transport,root):
    progress_paths=[d/'verifier-progress/unit.json' for d in transport['parts'].values()
                    if (d/'verifier-progress/unit.json').is_file()]
    backup_paths=[d/'verifier-backup/root.json' for d in transport['parts'].values()
                  if (d/'verifier-backup/root.json').is_file()]
    unit=result['work_unit']
    is_verifier=unit['stage'] in ('semantic_verify','monitor_scc_verify','temporal_components','complete')
    p.require(len(progress_paths)==len(backup_paths)==(1 if is_verifier else 0),
              'verifier unit missing exact progress/backup pair')
    if not is_verifier:return state
    progress=p.verifier_auxiliary(p.read(progress_paths[0]),root)
    backup=p.verifier_auxiliary(p.read(backup_paths[0]),root)
    checkpoint=p.digest(p.canonical(result['checkpoint']))
    if result['checkpoint']==previous['checkpoint']:
        p.require(result['stage_complete'] is False and progress['after_cursor']['operation'] in ('load_input','check_input'),
                  'unchanged catalog checkpoint is only valid for authenticated input preparation')
    p.require(progress['kind']=='verifier_unit_progress' and backup['kind']=='verifier_backup'
              and progress['source']==backup['source']==unit['source']
              and progress['session_id']==backup['session_id']==unit['session_id']
              and progress['unit_ordinal']==backup['unit_ordinal']==unit['unit_ordinal']
              and progress['checkpoint_sha256']==backup['checkpoint_sha256']==checkpoint
              and backup['artifact_part_count']==len(transport['parts'])
              and backup['progress_sha256']==p.file_sha(progress_paths[0])
              and progress['resource_plan_sha256']==backup['resource_plan_sha256']==unit['resource_plan_sha256']
              and progress['initial_image_sha256']==backup['initial_image_sha256']==result['initial_image']['record']['image_sha256'],
              'verifier progress/backup checkpoint/source binding differs')
    origin={k:unit[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode',
                                'run','job','task','attempt','stage')}
    p.require(progress['origin']==origin and progress['after_cursor']['position']==unit['progress']
              and progress['stage_complete']==result['stage_complete']
              and progress['previous_progress_sha256']==previous.get('verifier_progress_sha256')
              and progress['previous_backup_manifest_sha256']==previous.get('verifier_backup_sha256')
              and backup['previous_verifier_progress_sha256']==previous.get('verifier_progress_sha256')
              and progress['input_checkpoint_sha256']==p.digest(p.canonical(previous['checkpoint'])),
              'verifier origin/cursor/prior chain differs')
    before=previous.get('verifier_progress')
    if before:
        p.require(progress['before_verifier_commit']==before['after_verifier_commit']
                  and progress['before_logical_digest']==before['after_logical_digest']
                  and progress['before_cursor']==before['after_cursor']
                  and progress['first_result_sequence']==before['first_result_sequence']+before['result_count'],
                  'verifier result sequence or before state skips authenticated prefix')
        old_closure=before['source_closure_checkpoint_sha256']
        p.require(old_closure is None or progress['source_closure_checkpoint_sha256']==old_closure,
                  'independently closed source graph binding changed')
    else:
        p.require(progress['before_verifier_commit']==0 and progress['before_cursor']['quantum']==0
                  and progress['first_result_sequence']==1, 'first verifier prefix not empty/contiguous')
    p.require(progress['committed_quantum_count']>0 and progress['result_count']>0
              and progress['after_verifier_commit']>progress['before_verifier_commit']
              and progress['after_cursor']['quantum']>progress['before_cursor']['quantum']
              and backup['verifier_commit']==progress['after_verifier_commit']
              and backup['verifier_logical_digest']==progress['after_logical_digest'],
              'zero committed progress cannot advance/loop')
    state['verifier_progress']=progress
    state['verifier_progress_sha256']=p.file_sha(progress_paths[0])
    state['verifier_backup_sha256']=p.file_sha(backup_paths[0])
    if unit['stage']=='complete':
        authenticate_final_input(state,result,transport,progress,backup,root)
    return state


def validate_backend_progress(previous,state,result,transport,root):
    unit=result['work_unit']
    if unit['stage']!='backend_qualification':return
    before=previous['progress']['case_index'];after=unit['progress']['case_index']
    expected=unit['registered_cases']
    p.require(after==before+1 and before<len(expected),'backend case cursor skips/repeats')
    name='backend/'+expected[before]+'.json'
    body=p.read(find_member(transport['parts'],name))
    p.require(body['case']==expected[before] and body['source']==unit['source']
              and body['case_result']['accepted'] is True
              and type(body['case_result']['exit_code']) is int
              and body['case_result']['exit_code']==(101 if body['case']=='control-selector' else 0),'backend actual receipt case/source differs')
    completed={**previous.get('backend_cases',{}),body['case']:p.digest(p.canonical(body))}
    state['backend_cases']=completed
    if result['stage_complete']:
        p.require(after==len(expected) and set(completed)==set(expected),
                  'backend qualification missing registered actual cases')
        # Qualification remains separate from final Main acceptance. The
        # registered complete verifier will independently consume the full
        # authenticated semantic/result/backend chain before accepted:true.
        p.require(previous.get('verifier_progress',{}).get('source_closure_checkpoint_sha256') is not None,
                  'backend completion lacks independently closed source')
        state['phase']='qualified'


def worker_affected(root,base,head):
    """Read the actual frozen shell selector's literal case-pattern list."""
    text=(Path(root)/'.github/workflows/main-fast-lane.yml').read_text()
    start=text.index('      - name: Select affected worker controller proof inputs\n')
    end=text.index('      - name:',start+10)
    block=text[start:end]
    start=block.index('            case "$path" in\n')+len('            case "$path" in\n')
    end=block.index(')\n',start)
    patterns=block[start:end].replace('\\','').split('|')
    patterns=[item.strip() for item in patterns]
    import re
    p.require(patterns and all(re.fullmatch(r'[A-Za-z0-9_./*-]+',v) for v in patterns),
              'worker applicability shell grammar changed')
    paths=subprocess.check_output(['git','diff','--name-only',base+'...'+head],cwd=root,text=True,timeout=30).splitlines()
    return any(fnmatch.fnmatchcase(path,pattern) for path in paths for pattern in patterns)


AUXILIARY_ACCOUNTING_KINDS=p.AUXILIARY_ACCOUNTING_KINDS
ACCOUNTING_METHOD_FILES=('validation/async_refinement_provenance.py','validation/async_refinement_session.py')


def accounting_method_digest(files):
    return p.digest(p.canonical([[name,files[name]] for name in ACCOUNTING_METHOD_FILES]))


ACCOUNTING_METHOD={'schema_version':1,'id':'operational_encoded_caps_v1',
    'source_report':'source_global_symbolic',
    'semantic_lifetime':'session_unique_global_record',
    'auxiliary_lifetime':'session_retained_unit_member',
    'copy_inventory':[
        {'class':'canonical_authoritative_records','copies':1,'charged_to':'record_allowances'},
        {'class':'typed_payload_and_identity_copies','copies':2,'charged_to':'overhead.index_bytes'},
        {'class':'catalog_row_keys_indexes_allocators','copies':1,'charged_to':'overhead.index_bytes'},
        {'class':'verifier_record_index_algorithm_state','copies':1,'charged_to':'overhead.index_bytes'},
        {'class':'source_archive_and_extraction','copies':2,'charged_to':'overhead.scratch_bytes'},
        {'class':'semantic_pages_and_current_output','copies':1,'charged_to':'overhead.scratch_bytes'},
        {'class':'sql_wal','copies':1,'charged_to':'overhead.wal_bytes'},
        {'class':'retained_prior_live_backup_chunks_parts_restores','copies':6,'charged_to':'overhead.retained_bytes'},
        {'class':'standalone_catalog_and_verifier_backups','copies':1,'charged_to':'overhead.duplicate_backup_bytes'},
        {'class':'catalog_and_verifier_chunk_staging','copies':1,'charged_to':'overhead.duplicate_backup_bytes'},
        {'class':'packed_downloaded_artifact_files','copies':1,'charged_to':'overhead.duplicate_backup_bytes'},
        {'class':'catalog_and_verifier_restore_staging','copies':1,'charged_to':'overhead.duplicate_backup_bytes'}],
    'nonsemantic_mapping':{'logs':'overhead.retained_bytes','source_archive':'overhead.scratch_bytes',
        'backup_and_chunks':'overhead.duplicate_backup_bytes','downloads_and_restores':'overhead.duplicate_backup_bytes',
        'decoded_objects_and_cache':'resident_bytes_and_resident_rss_bytes'},
    'control_transport':{'maximum_raw_log_bytes':32*p.MIB,'maximum_control_bytes':p.SMALL,
        'base64_numerator':4,'base64_denominator':3,'maximum_chunk_characters':2048,
        'raw_log_lifetime':'session_retained','frame_and_decoded_lifetime':'resident_working_set'}}


def accounting_inventory(source_receipt):
    p.require({r['record_kind'] for r in source_receipt['records']}==set(p.SEMANTIC_SOURCE_KINDS),
              'complete semantic source-global inventory differs')
    return {r['kind']:{'class':r['role'],'lifetime':r['lifetime']}
            for r in p.accounting_mapping() if r['role']!='operational_overhead'}


def operational_bounds(encoded,reg,root,measurement_pending=False):
    value=p.decode(encoded.encode())
    p.closed(value,('schema_version','accounting_rule','source_receipt','record_allowances','overhead',
                    'accounting_method_sha256','measurement_sha256'))
    p.require(type(value['schema_version']) is int and value['schema_version']==2
              and value['accounting_rule']=='operational_encoded_caps_v1'
              and value['accounting_method_sha256']==reg['accounting_method_sha256'],
              'obsolete/unregistered operational accounting wrapper')
    p.closed(value['overhead'],('index_bytes','wal_bytes','scratch_bytes','retained_bytes','duplicate_backup_bytes'))
    p.require(type(value['record_allowances']) is dict and value['record_allowances'], 'missing operational kinds')
    for kind,row in value['record_allowances'].items():
        p.closed(row,('records','max_encoded_bytes'))
        p.require(type(row['records']) is int and row['records']>=0
                  and type(row['max_encoded_bytes']) is int and 0<row['max_encoded_bytes']<=p.MAX_MEMBER,
                  'invalid operational count/full-encoding cap')
        if kind in AUXILIARY_ACCOUNTING_KINDS:
            p.require(row['max_encoded_bytes']<=p.SMALL,'auxiliary metadata cap exceeds8MiB')
    if not measurement_pending:
        p.require(all(type(v) is int and v>=0 for v in value['overhead'].values())
                  and isinstance(value['measurement_sha256'],str) and p.SHA.fullmatch(value['measurement_sha256']),
                  'actual measured operational overhead unavailable')
    return value


def verifier_chain_digest(index_path,through):
    import sqlite3
    db=sqlite3.connect('file:'+str(Path(index_path).resolve())+'?mode=ro',uri=True)
    h=__import__('hashlib').sha256();h.update(b'[');first=True
    try:
        for ordinal,progress,backup in db.execute('SELECT unit_ordinal,progress_path,backup_root_path FROM verifier_chain WHERE unit_ordinal<=? ORDER BY unit_ordinal',(through,)):
            if not first:h.update(b',')
            first=False
            h.update(p.canonical([ordinal,p.file_sha(progress),p.file_sha(backup)]))
        h.update(b']');return h.hexdigest()
    finally:db.close()


def restore_verifier(state,latest,source,request,output,reg,index_path):
    selected=None
    for result,transport in latest[1].get('history',[]):
        roots=[d/'verifier-backup/root.json' for d in transport['parts'].values()
               if (d/'verifier-backup/root.json').is_file()]
        if roots:
            p.require(len(roots)==1,'multiple verifier roots in one authenticated unit')
            selected=(result,transport,roots[0])
    if selected is None:
        p.require(state.get('verifier_progress') is None,'authenticated verifier state disappeared')
        return
    result,transport,root_path=selected;backup=p.verifier_auxiliary(p.read(root_path),source)
    staged=Path(request['verifier_work_db']).with_name('verifier.staged.sqlite')
    p.restore_catalog(backup,transport['parts'],transport['receipts'],result['checkpoint'],staged,source,verifier=True,transport_unit=result['work_unit'])
    restore={**request,'operation':'validate_restore','verifier_work_db':str(staged),
             'verifier_checkpoint':result['checkpoint'],'verifier_backup_root':str(root_path),
             'output_dir':str(Path(output)/'verifier-restore-validation'),'dependency_index':str(index_path)}
    Path(restore['output_dir']).mkdir();path=Path(output)/'verifier-restore-request.json';p.write(path,restore)
    run_quantum(path,reg,source,Path(restore['output_dir']),
                [sys.executable,str(Path(source)/'verification/async-driver-refinement/paged_checker.py'),
                 'validate-restored-verifier','--request',str(path)])
    receipt=p.read(Path(restore['output_dir'])/'verifier-restore-validation.json')
    expected={'schema_version':1,'operation':'validate_restored_verifier','source':backup['source'],
              'session_id':backup['session_id'],'unit_ordinal':backup['unit_ordinal'],
              'checkpoint_sha256':backup['checkpoint_sha256'],'resource_plan_sha256':backup['resource_plan_sha256'],
              'initial_image_sha256':backup['initial_image_sha256'],'verifier_schema_sha256':backup['verifier_schema_sha256'],
              'verifier_commit':backup['verifier_commit'],'verifier_logical_digest':backup['verifier_logical_digest'],
              'work_db_sha256':backup['work_db_sha256'],'work_db_bytes':backup['work_db_bytes'],
              'progress_sha256':backup['progress_sha256'],
              'result_chain_root_sha256':verifier_chain_digest(index_path,backup['unit_ordinal']),
              'complete':True,'acceptance':False}
    p.require(receipt==expected and p.file_sha(staged)==expected['work_db_sha256']
              and staged.stat().st_size==expected['work_db_bytes'],'verifier staged/result-chain validation differs')
    target=Path(request['verifier_work_db'])
    p.require(target.parent==staged.parent and not target.exists(),'verifier atomic install target not fresh/owned')
    # No checker/writer remains: run_quantum waited for its process group.
    os.replace(staged,target)
    fd=os.open(target.parent,os.O_RDONLY)
    try:os.fsync(fd)
    finally:os.close(fd)




def validate_control_identity(value,reg,metadata,run_id,job,root):
    schema=p.read(Path(root)/p.CONTROL_SCHEMA)
    frozen=p.read(Path(root)/p.SCHEMA_PATH)
    def local(node):
        if isinstance(node,dict):
            if node.get('$ref','').startswith('paged-proof.schema.json#/$defs/'):
                return {'$ref':'#/$defs/'+node['$ref'].split('/')[-1]}
            return {k:local(v) for k,v in node.items()}
        if isinstance(node,list):return [local(v) for v in node]
        return node
    schema=local(schema);schema['$defs']=frozen['$defs']
    p.validate(value,{k:v for k,v in schema.items() if k not in ('$schema','$id','$defs')},schema)
    unit=p.envelope(value['work_unit'],'WorkUnit',root)
    origin={k:unit[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode','run','job','task','attempt')}
    origin['operation']='execute'
    p.require(value['registration_sha256']==p.digest(p.canonical(reg))
              and value['source_archive_sha256']==reg['source_archive_sha256']
              and value['source']==unit['source']==reg['source'] and value['origin']==origin
              and (unit['run'],unit['job'],unit['task'],unit['attempt'])==(run_id,job['id'],job['task_id'],1)
              and value['session_id']==unit['session_id']==metadata['session_id']
              and value['unit_ordinal']==unit['unit_ordinal']==metadata['unit_ordinal']
              and value['expected_prior_checkpoint']==unit['expected_prior_checkpoint']
              and len(value['artifact_receipts'])==len(metadata['parts']), 'carrier producer/plan identity differs')
    if value['role']=='unit_checkpoint':
        p.require(unit['status'] in ('checkpointed_incomplete','closed_unqualified') and value['completion'] is None,
                  'unit carrier is not a safe nonqualifying checkpoint')
    else:
        p.require(unit['status']=='complete' and p.envelope(value['completion'],'Completion',root)['accepted'] is True,
                  'completion carrier shape is not complete')
    utc(value['postcheck']['checked_at'])
    p.require(value['postcheck']['source_commit']==reg['source']['commit']
              and value['postcheck']['current_base']==reg['event_base'], 'carrier live source/base differs')


def finalize_uploads(output,root):
    """Mandatory synchronous final step; it cannot certify its own job success."""
    event=p.read(os.environ['GITHUB_EVENT_PATH']);inputs=event['inputs']
    if inputs['operation']=='probe':
        return capability_finalizer(output,root)
    draft=p.read(Path(output)/'finalizer-state.json');state=draft['state'];reg=state['registration']
    controller_memory_limit(state['resource_plan'])
    metadata=p.read(Path(output)/'upload-plan.json')
    api=api_environment(Path(output)/'finalizer-api')
    api.bind_storage(state['resource_plan'],reg,draft['control']['session_id'])
    run_id=int(os.environ['GITHUB_RUN_ID'])
    _,job,payload=p.producer(api,run_id,reg,inputs['mode'],('running','in_progress'))
    unit=draft['unit'];record=p.envelope(unit,'WorkUnit',root)
    role='completion' if inputs['mode']=='complete' else 'unit_checkpoint'
    origin={k:record[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode','run','job','task','attempt')}
    origin['operation']='execute'
    carrier={'schema_version':1,'kind':'async_session_control','role':role,
        'registration_sha256':p.digest(p.canonical(reg)),'source_archive_sha256':reg['source_archive_sha256'],
        'source':reg['source'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
        'origin':origin,'previous_control_sha256':state.get('control_sha256'),
        'expected_prior_checkpoint':record['expected_prior_checkpoint'],
        'checkpoint':p.ref(state['checkpoint'],p.digest(p.canonical(state['checkpoint']))),
        'work_unit':unit,'artifact_receipts':[],'completion':None,'postcheck':None}
    binding=p.control_binding(carrier)
    print('ASYNC-PROOF-FINALIZER-BEGIN 1 '+binding,flush=True)
    _,lines=p.producer_log(api,run_id,job,'complete' if role=='completion' else 'unit')
    listed=api.pages('/actions/runs/'+str(run_id)+'/artifacts',field='artifacts')
    selected=[]
    for part in metadata['parts']:
        matches=[a for a in listed if a.get('name')==part['name']]
        p.require(len(matches)==1,'postupload artifact absent/ambiguous')
        artifact=matches[0];selected.append(artifact)
        archive=p.artifact_bytes(api,artifact)
        try:
            slice_sha,zip_sha,_,_=p.upload_slice(lines,artifact['name'],artifact['id'],artifact['size_in_bytes'])
            p.require(p.file_sha(archive)==zip_sha,'postupload actual ZIP differs')
            p.unpack(archive,Path(output)/'finalizer-parts'/str(part['ordinal']),part['members'],storage_guard=api.guard_storage)
            receipt={'schema_version':6,'kind':'ArtifactReceipt','record':{
                'artifact_id':artifact['id'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
                'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1,'source':reg['source'],
                'name':artifact['name'],'expired':False,'zip_sha256':zip_sha,'zip_bytes':archive.stat().st_size,
                'extracted_bytes':sum(m['bytes'] for m in part['members']),'member_count':len(part['members']),
                'members':part['members'],'upload_log_sha256':slice_sha,'event_payload_sha256':p.digest(p.canonical(payload)),
                'expected_prior_checkpoint':record['expected_prior_checkpoint'],
                'produced_checkpoint':p.digest(p.canonical(state['checkpoint']))}}
            p.envelope(receipt,'ArtifactReceipt',root);carrier['artifact_receipts'].append(receipt)
        finally:archive.unlink(missing_ok=True)
    p.validate_upload_order(lines,selected,len(lines))
    record['artifact_roots']=[p.ref(v,v['record']['artifact_id']) for v in carrier['artifact_receipts']]
    live=live_pr(api,reg)
    post={'checked_at':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),'source_commit':reg['source']['commit'],
        'current_base':live['base']['sha'],'pr_state':live['state'],'draft':live['draft'],
        'archived_fresh_head':None,'main_receipts':[],'main_conditions_sha256':None,'semantic_verification':None}
    if role=='completion':
        records=authenticate_main(api,reg,draft['control']['main_run'],root,verified=draft['verified'])
        p.require(records==draft['main_records'],'Main prerequisite receipts changed after uploads')
        parts={n:Path(output)/'finalizer-parts'/str(n) for n in range(len(metadata['parts']))}
        fresh=p.read(find_member(parts,'fresh-head.json'))
        current={'source':reg['source'],'repository':reg['repository'],'pr':reg['pr'],'head':live['head']['sha'],
            'event_base':reg['event_base'],'current_base':live['base']['sha'],'state':live['state'],'draft':live['draft'],
            'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1}
        p.require(fresh==current,'archived fresh-head identities changed after uploads')
        refs=[{'kind':'main_receipt','id':'main/'+r['condition']+'.json','sha256':p.digest(p.canonical(r))} for r in records]
        freshref={'kind':'fresh_head_receipt','id':'fresh-head.json','sha256':p.digest(p.canonical(fresh))}
        carrier['completion']=completion(state,draft['verified'],refs,freshref,p.ref(unit,record['unit_ordinal']),root)
        matches=[(part['ordinal'],m) for part in metadata['parts'] for m in part['members'] if m['path']=='complete-verification.json']
        p.require(len(matches)==1,'independent complete verification member missing')
        ordinal,member=matches[0]
        conditions={'source':reg['source'],'event_base':reg['event_base'],
            'applicability_sha256':p.digest(p.canonical(source_scope(root,reg['event_base']))),
            'results':condition_results(root,reg,records),
            'receipt_refs':refs,'semantic_verification_sha256':member['sha256']}
        post.update(archived_fresh_head=freshref,main_receipts=refs,main_conditions_sha256=p.digest(p.canonical(conditions)),
            semantic_verification={'part_ordinal':ordinal,'member_path':member['path'],'bytes':member['bytes'],'sha256':member['sha256']})
    carrier['postcheck']=post
    local_parts={n:Path(output)/'finalizer-parts'/str(n) for n in range(len(metadata['parts']))}
    enforce_control_caps(state,{'parts':local_parts,'control':carrier},root)
    validate_control_identity(carrier,reg,metadata,run_id,job,root)
    raw=p.canonical(carrier)
    allowance=operational_bounds(state['resource_plan']['record']['source_bounds_utf8'],reg,root)['record_allowances']['async_session_control']
    reserved=p.read(Path(output)/'control-reservation.json')
    p.require(len(raw)<=min(allowance['max_encoded_bytes'],reserved['maximum_control_bytes']),
              'actual whole carrier exceeds pre-upload immutable reservation')
    for line in p.control_frame(carrier):print(line,flush=True)
    print('ASYNC-PROOF-FINALIZER-DONE 1 '+binding+' '+p.digest(raw),flush=True)




def validate_storage(storage,source_root):
    storage_identity(storage)
    p.require(os.geteuid()==os.getuid(),'proof storage requires matching effective/real container owner')
    root=Path(storage['root']);mount=Path(storage['mount_root']);lease=Path(storage['lease_file'])
    p.require(root.is_absolute() and root.is_dir() and root.resolve()==root
              and mount.is_absolute() and mount.resolve()==mount and mount.is_mount()
              and root!=mount and mount in root.parents and root.stat().st_dev==mount.stat().st_dev==storage['mount_device']
              and Path(source_root).resolve() not in (root,*root.parents)
              and not set(root.parts)&{'.cache','target','tmp','.git'},'persistent owned noncache mount unavailable')
    p.require(root.stat().st_uid==os.geteuid() and root.stat().st_mode & 0o022==0
              and lease.parent==root and lease.is_file() and lease.stat().st_uid==os.geteuid()
              and not lease.is_symlink() and lease.stat().st_mode & 0o022==0
              and lease.stat().st_dev==storage['mount_device'] and p.file_sha(lease)==storage['lease_sha256']
              and time.time()<utc(storage['lease_expires_at']), 'persistent owned lease absent/expired')
    return root


def event_output(root):
    event=p.read(os.environ['GITHUB_EVENT_PATH']);control=p.decode(event['inputs']['control'].encode())
    reg=registration(control['registration'],root)
    plan=p.envelope(control['resource_plan'],'ResourcePlan',root)
    p.require(control['session_id']==plan['session_id'],'event storage session UUID differs')
    owned=validate_storage(reg['storage'],root)
    return owned/control['session_id']/('run-'+str(receipts.positive(int(os.environ['GITHUB_RUN_ID']))))


def validate_workflow_protocol(root):
    text=(Path(root)/'.github/workflows/async-proof-session.yml').read_text()
    for key in ('unit','complete'):
        marker='  '+key+':'
        p.require(text.count(marker)==1,'registered producer job ambiguous')
        block=text.split(marker,1)[1].split('\n  complete:',1)[0] if key=='unit' else text.split(marker,1)[1]
        p.require(block.count('      - name: Native immutable part ')==32
                  and block.count('      - name: Finalize authenticated post-upload control')==1,
                  'fixed native slots/finalizer registration differs')
        header=block.split('    steps:',1)[0]
        route=route_contract(root)
        expected_header='    runs-on: ['+', '.join(route['labels'])+']\n'
        expected_container='    container:\n      image: ubuntu:24.04\n      volumes:\n        - '+route['container_bind']+'\n'
        container=re.search(r'(?ms)^    container:\n.*?(?=^    [^ \n]|\Z)',header)
        p.require(header.count('    runs-on:')==1 and expected_header in header
                  and header.count('    container:')==1 and container is not None
                  and container.group(0)==expected_container
                  and header.count('      volumes:')==1 and 'options:' not in header,
                  'registered dedicated route/container mount differs')
        p.require('continue-on-error' not in header and 'strategy:' not in header,
                  'producer job may tolerate failure or multiply native tasks')
        for ordinal in range(32):
            slot=block.split('      - name: Native immutable part '+str(ordinal)+'\n',1)[1].split('      - name:',1)[0]
            expected=["        if: steps.session.outputs.part_"+str(ordinal)+"_name != ''",
                '        uses: https://data.forgejo.org/forgejo/upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e # v4',
                '          name: ${{ steps.session.outputs.part_'+str(ordinal)+'_name }}',
                '          path: ${{ steps.session.outputs.part_'+str(ordinal)+'_path }}',
                '          if-no-files-found: error','          retention-days: 90']
            p.require(all(slot.count(line)==1 for line in expected) and 'continue-on-error' not in slot
                      and 'always()' not in slot,'native slot source/condition differs')
        fixture=block.split('      - name: Register source-bound capability post-action fixture',1)
        p.require(len(fixture)==2 and "        if: github.event.inputs.operation == 'probe'" in fixture[1].split('      - name:',1)[0]
                  and '        uses: ./.github/actions/async-proof-capability' in fixture[1].split('      - name:',1)[0],
                  'registered capability post-action fixture missing/changed')
        last=block.split('      - name: Finalize authenticated post-upload control',1)[1]
        expected_finalizer='''
        env:
          GITHUB_TOKEN: ${{ github.token }}
        run: >-
          python3 -m validation.async_refinement_session finalize-uploads
'''
        p.require(last.strip()==expected_finalizer.strip(),
                  'required finalizer is not the exact synchronous exit-propagating command')
    action=(Path(root)/CAPABILITY_ACTION_FILES[0]).read_text()
    p.require(action=='''name: Async proof capability post-action fixture
description: Source-bound operational probe; never proof or session credit
runs:
  using: node20
  main: main.js
  post: post.js
  post-if: always()
''','capability main/post runtime registration differs')



def condition_results(root,reg,records):
    required=main_required(source_scope(root,reg['event_base']))
    result={key:'skipped' for key in required};result['async_refinement']='success'
    for row in records:
        p.require(row['condition'] in required and required[row['condition']]
                  and row['status']=='success','unexpected Main condition receipt')
        result[row['condition']]='success'
    p.require(all(result[k]=='success' for k,v in required.items() if v),'missing applicable Main condition')
    main_conditions(result,True)
    return result


def materialize_control(index,transport,root):
    control=transport['control'];sha=transport['control_sha256']
    p.require(p.digest(p.canonical(control))==sha,'authenticated control changed before resolver install')
    base=next(iter(transport['parts'].values())).parent/'control'/sha
    values=[(control['work_unit'],control['unit_ordinal'])]
    values.extend((value,value['record']['artifact_id']) for value in control['artifact_receipts'])
    if control['completion'] is not None:values.append((control['completion'],control['session_id']))
    for value,ident in values:
        kind=value['kind'];p.require(kind in ('WorkUnit','ArtifactReceipt','Completion'),'unauthorized log resolver kind')
        p.envelope(value,kind,root)
        path=base/(kind+'-'+str(ident)+'.json');raw=p.canonical(value)
        if path.exists():p.require(path.read_bytes()==raw,'materialized control identity collision')
        else:p.write(path,value)
        reference=p.ref(value,ident)
        index.add(reference,p.transport_bytes(value),path.resolve())
        index.db.execute('INSERT OR REPLACE INTO roots VALUES(?,?,?)',
                         (kind,p.canonical(ident),reference['sha256']))



def controller_memory_limit(plan):
    limit=plan['record']['resident_rss_bytes']
    p.require(type(limit) is int and limit>0,'missing controller hard memory allowance')
    soft,hard=resource.getrlimit(resource.RLIMIT_AS)
    if hard!=resource.RLIM_INFINITY:limit=min(limit,hard)
    resource.setrlimit(resource.RLIMIT_AS,(limit,limit))



def preflight_carrier(output,metadata):
    draft=p.read(output/'finalizer-state.json');state=draft['state'];reg=state['registration']
    unit=copy.deepcopy(draft['unit']);record=unit['record'];maximum=(1<<63)-1
    receipts=[]
    for part in metadata['parts']:
        receipts.append({'schema_version':6,'kind':'ArtifactReceipt','record':{
            'artifact_id':maximum-part['ordinal'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
            'run':record['run'],'job':record['job'],'task':record['task'],'attempt':1,'source':reg['source'],
            'name':part['name'],'expired':False,'zip_sha256':'f'*64,'zip_bytes':p.MAX_ARTIFACT,
            'extracted_bytes':sum(m['bytes'] for m in part['members']),'member_count':len(part['members']),
            'members':part['members'],'upload_log_sha256':'f'*64,'event_payload_sha256':'f'*64,
            'expected_prior_checkpoint':record['expected_prior_checkpoint'],
            'produced_checkpoint':p.digest(p.canonical(state['checkpoint']))}})
    record['artifact_roots']=[p.ref(v,v['record']['artifact_id']) for v in receipts]
    role='completion' if record['mode']=='complete' else 'unit_checkpoint'
    origin={k:record[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode','run','job','task','attempt')}
    origin['operation']='execute'
    completion_value=p.read(output/'completion-draft.json') if role=='completion' else None
    post={'checked_at':'9999-12-31T23:59:59Z','source_commit':reg['source']['commit'],
        'current_base':reg['event_base'],'pr_state':'open','draft':False,
        'archived_fresh_head':None,'main_receipts':[],'main_conditions_sha256':None,'semantic_verification':None}
    if completion_value:
        post.update(archived_fresh_head=completion_value['record']['current_head_recheck'],
            main_receipts=completion_value['record']['applicable_main_receipts'],main_conditions_sha256='f'*64,
            semantic_verification={'part_ordinal':31,'member_path':'complete-verification.json','bytes':p.MAX_MEMBER,'sha256':'f'*64})
    carrier={'schema_version':1,'kind':'async_session_control','role':role,
        'registration_sha256':p.digest(p.canonical(reg)),'source_archive_sha256':reg['source_archive_sha256'],
        'source':reg['source'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
        'origin':origin,'previous_control_sha256':state.get('control_sha256'),
        'expected_prior_checkpoint':record['expected_prior_checkpoint'],
        'checkpoint':p.ref(state['checkpoint'],p.digest(p.canonical(state['checkpoint']))),
        'work_unit':unit,'artifact_receipts':receipts,'completion':completion_value,'postcheck':post}
    limit=len(p.canonical(carrier));plan=state['resource_plan']['record']
    wrapper=p.decode(plan['source_bounds_utf8'].encode())
    p.require(limit<=min(p.SMALL,wrapper['record_allowances']['async_session_control']['max_encoded_bytes']),
              'anticipated whole control cannot fit admitted carrier cap')
    frame_bytes=4*((limit+2)//3)+512+(4*((limit+2)//3)//2048+1)*96
    p.require(frame_bytes+len(p.canonical(metadata))+32*64*1024<=32*p.MIB,
              'anticipated control/planning/native sections cannot fit raw task log reservation')
    p.write(output/'control-reservation.json',{'maximum_control_bytes':limit,'maximum_frame_bytes':frame_bytes})



def probe_transactions(api,run_id,job,lines,metadata,reg):
    listed=api.pages('/actions/runs/'+str(run_id)+'/artifacts',field='artifacts');selected=[];rows=[]
    p.require(len(metadata['parts'])==32,'capability must exercise all32 native slots')
    for ordinal,part in enumerate(metadata['parts']):
        p.closed(part,('ordinal','name','members'))
        p.require(part['ordinal']==ordinal and part['name']==artifact_name(metadata['session_id'],metadata['unit_ordinal'],ordinal),
                  'capability fixed slot ordinal/name differs')
        matches=[a for a in listed if a.get('name')==part['name']]
        p.require(len(matches)==1,'capability native part absent/ambiguous')
        artifact=matches[0];selected.append(artifact)
        slice_sha,zip_sha,_,_=p.upload_slice(lines,artifact['name'],artifact['id'],artifact['size_in_bytes'])
        archive=p.artifact_bytes(api,artifact)
        try:
            p.require(p.file_sha(archive)==zip_sha,'capability downloaded ZIP differs')
            if ordinal==0:
                target=Path(api.scratch)/('probe-runtime-'+str(time.monotonic_ns()))
                p.unpack(archive,target,part['members'],storage_guard=api.guard_storage)
                payload=p.read(target/'capability-0.json')
                p.require(payload['runtime']==reg['runtime'] and payload['runner']==reg['runner']
                          and payload['registration_sha256']==p.digest(p.canonical(reg))
                          and payload['source_archive_sha256']==reg['source_archive_sha256']
                          and payload['measurement']['storage']==reg['storage']
                          and payload['measurement']['producer']=={'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1},
                          'capability actual runtime/storage producer differs')
        finally:archive.unlink(missing_ok=True)
        rows.append({'part_ordinal':part['ordinal'],'artifact_id':artifact['id'],'name':artifact['name'],
                     'bytes':artifact['size_in_bytes'],'zip_sha256':zip_sha,'upload_log_sha256':slice_sha})
    p.validate_upload_order(lines,selected,len(lines))
    return rows


def capability_finalizer(output,root):
    event=p.read(os.environ['GITHUB_EVENT_PATH']);control=p.decode(event['inputs']['control'].encode())
    p.closed(control,(*CONTROL_KEYS,'probe_case','probe_nonce'))
    reg=registration(control['registration'],root);case=control['probe_case'];nonce=control['probe_nonce']
    p.require(case in ('success','failure','post_failure') and p.SHA.fullmatch(nonce),'invalid capability request')
    api=api_environment(Path(output)/'capability-finalizer-api')
    api.bind_storage(control['resource_plan'],reg,control['session_id'])
    run_id=int(os.environ['GITHUB_RUN_ID'])
    _,job,_=p.producer(api,run_id,reg,'unit',('running','in_progress'))
    _,lines=p.producer_log(api,run_id,job,'unit');metadata=p.read(Path(output)/'upload-plan.json')
    rows=probe_transactions(api,run_id,job,lines,metadata,reg)
    value={'registration_sha256':p.digest(p.canonical(reg)),'source_archive_sha256':reg['source_archive_sha256'],
        'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1,'case':case,'nonce':nonce,'uploads':rows}
    raw=p.canonical(value);sha=p.digest(raw);encoded=base64.b64encode(raw).decode('ascii')
    chunks=[encoded[i:i+2048] for i in range(0,len(encoded),2048)]
    p.write(Path(output)/'capability-frame-binding.json',{'schema_version':1,'case':case,'nonce':nonce,
        'registration_sha256':value['registration_sha256'],'source_archive_sha256':reg['source_archive_sha256'],
        'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1,'control_sha256':sha})
    print(f'ASYNC-PROOF-CAPABILITY-BEGIN 1 {case} {nonce} {len(raw)} {len(chunks)} {sha}',flush=True)
    for ordinal,chunk in enumerate(chunks):print(f'ASYNC-PROOF-CAPABILITY-CHUNK {ordinal} {chunk}',flush=True)
    print('ASYNC-PROOF-CAPABILITY-END '+sha,flush=True)
    print('ASYNC-PROOF-CAPABILITY-DONE '+sha,flush=True)
    if case=='failure':
        print('ASYNC-PROOF-CAPABILITY-EXPECTED-FAILURE 1 '+nonce+' '+sha,flush=True)
        raise receipts.ReceiptError('intentional registered capability finalizer failure')


def capability_frame(lines):
    start=[(i,re.fullmatch(r'ASYNC-PROOF-CAPABILITY-BEGIN 1 (success|failure|post_failure) ([0-9a-f]{64}) ([1-9][0-9]*) ([1-9][0-9]*) ([0-9a-f]{64})',line))
           for i,line in enumerate(lines) if line.startswith('ASYNC-PROOF-CAPABILITY-BEGIN')]
    p.require(len(start)==1 and start[0][1] is not None,'capability frame begin missing/ambiguous')
    begin,match=start[0];case,nonce,length,count,sha=match.groups();length=int(length);count=int(count)
    p.require(length<=p.SMALL and count==(4*((length+2)//3)+2047)//2048,'capability frame bounds differ')
    p.require(begin+count+1<len(lines),'capability frame truncated')
    chunks=[]
    for ordinal in range(count):
        item=re.fullmatch(r'ASYNC-PROOF-CAPABILITY-CHUNK ([0-9]+) ([A-Za-z0-9+/]+={0,2})',lines[begin+ordinal+1])
        p.require(item is not None and item[1]==str(ordinal),'capability chunks reordered')
        p.require(len(item[2])==2048 if ordinal<count-1 else 0<len(item[2])<=2048,'capability chunk width differs')
        chunks.append(item[2])
    end=begin+count+1
    p.require(lines[end]=='ASYNC-PROOF-CAPABILITY-END '+sha,'capability frame end differs')
    protocol=[(i,line) for i,line in enumerate(lines) if line.startswith('ASYNC-PROOF-CAPABILITY-')
              and not line.startswith('ASYNC-PROOF-CAPABILITY-NODE-RUNTIME ')]
    expected=list(range(begin,end+1))
    p.require(end+1<len(lines) and lines[end+1]=='ASYNC-PROOF-CAPABILITY-DONE '+sha,'capability finalizer framing completion missing')
    expected.append(end+1)
    if case=='failure':
        p.require(end+2<len(lines) and lines[end+2]=='ASYNC-PROOF-CAPABILITY-EXPECTED-FAILURE 1 '+nonce+' '+sha,
                  'failure was not the registered post-frame intentional cause')
        expected.append(end+2)
    if case=='post_failure':
        wanted='ASYNC-PROOF-CAPABILITY-EXPECTED-POST-FAILURE 1 '+nonce+' '+sha
        post=[i for i,line in enumerate(lines) if line.startswith('ASYNC-PROOF-CAPABILITY-EXPECTED-POST-FAILURE')]
        p.require(len(post)==1 and post[0]>end+1 and lines[post[0]]==wanted,'isolated expected post-action failure missing')
        expected.append(post[0])
    p.require([i for i,_ in protocol]==expected,'duplicate/trailing capability protocol')
    encoded=''.join(chunks);raw=base64.b64decode(encoded,validate=True)
    p.require(len(raw)==length and p.digest(raw)==sha and base64.b64encode(raw).decode('ascii')==encoded,
              'capability frame digest/base64 differs')
    value=p.decode(raw);p.require(p.canonical(value)==raw and value['case']==case and value['nonce']==nonce,'capability canonical identity differs')
    return value,begin


def authenticate_probe_pair(api,reg,pair):
    p.require(len(set(pair.values()))==3,'each capability case must be independently executed')
    nonce=None;identities=set()
    for case,key,status in (('success','success_run','success'),('failure','failure_run','failure'),
                            ('post_failure','post_failure_run','failure')):
        run_id=receipts.positive(pair[key]);_,job,event=p.producer(api,run_id,reg,'unit',(status,))
        request=p.decode(event['inputs']['control'].encode())
        p.require(event['inputs']['operation']=='probe' and request['registration']==reg and request['probe_case']==case
                  and not request['unit_runs'] and not request['attempt_runs'] and not request['resume_decisions'] and request['probe_run'] is None,'capability event is not a fresh probe')
        _,lines=p.producer_log(api,run_id,job,'unit');value,begin=capability_frame(lines)
        validate_probe_node_runtime(lines,reg,request,begin)
        p.closed(value,('registration_sha256','source_archive_sha256','run','job','task','attempt','case','nonce','uploads'))
        p.require(value['registration_sha256']==p.digest(p.canonical(reg)) and value['source_archive_sha256']==reg['source_archive_sha256']
                  and (value['run'],value['job'],value['task'],value['attempt'])==(run_id,job['id'],job['task_id'],1)
                  and value['nonce']==request['probe_nonce'] and (nonce is None or nonce==value['nonce'])
                  and (run_id,job['id'],job['task_id']) not in identities,'capability origin/nonce differs')
        nonce=value['nonce'];identities.add((run_id,job['id'],job['task_id']))
        marker=[line[len(OUTPUT_MARKER):] for line in lines if line.startswith(OUTPUT_MARKER)]
        p.require(len(marker)==1,'capability native plan absent/ambiguous')
        metadata=p.decode(marker[0].encode())
        p.closed(metadata,('schema_version','registration_sha256','source_archive_sha256','operation','session_id','unit_ordinal','result_path','parts'))
        p.require(metadata['registration_sha256']==p.digest(p.canonical(reg))
                  and metadata['source_archive_sha256']==reg['source_archive_sha256']
                  and metadata['operation']=='probe' and metadata['session_id']==request['session_id']
                  and metadata['unit_ordinal']==0,'capability native source plan differs')
        rows=probe_transactions(api,run_id,job,lines[:begin],metadata,reg)
        p.require(rows==value['uploads'] and not any(line.startswith(p.CONTROL_PREFIX) for line in lines),
                  'capability frame uploads differ or impersonate an executed unit')
        _,again,_=p.producer(api,run_id,reg,'unit',(status,))
        p.require((again['id'],again['task_id'],again['attempt'])==(job['id'],job['task_id'],1),
                  'capability terminal identity changed after retrieval')
    return {'success':True,'failure_propagated':True,'post_failure_propagated':True,'same_task_slots':32,'acceptance':False}



def reject_current_control_members(parts,ordinal):
    for directory in parts.values():
        for path in directory.rglob('*.json'):
            if path.stat().st_size>p.SMALL:continue
            value=p.read(path)
            if type(value) is dict and value.get('schema_version')==6 and value.get('kind') in ('WorkUnit','ArtifactReceipt','Completion'):
                record=value.get('record')
                p.require(type(record) is dict and value['kind']!='Completion'
                          and record.get('unit_ordinal')!=ordinal,
                          'current ZIP contains competing authoritative control envelope')



def enforce_control_caps(state,transport,root):
    """Operational transport occurrences; source semantic counts remain independent."""
    wrapper=p.decode(state['resource_plan']['record']['source_bounds_utf8'].encode())
    allowances=wrapper['record_allowances'];usage=copy.deepcopy(state.get('operational_usage',{}))
    operational=set(p.CONTROL_ACCOUNTING_KINDS+p.AUXILIARY_ACCOUNTING_KINDS)
    def charge(kind,raw):
        p.require(kind in allowances and len(raw)<=allowances[kind]['max_encoded_bytes'],
                  'actual full outer control encoding exceeds operational cap: '+kind)
        usage[kind]=usage.get(kind,0)+1
        p.require(usage[kind]<=allowances[kind]['records'],'retained control count exceeds operational cap: '+kind)
    for directory in transport['parts'].values():
        for file in directory.rglob('*.json'):
            value=p.read(file,p.MAX_MEMBER)
            if type(value) is dict and value.get('kind') in operational:
                charge(value['kind'],file.read_bytes())
    carrier=transport['control']
    charge('async_session_control',p.canonical(carrier))
    for value in [carrier['work_unit'],*carrier['artifact_receipts']]:charge(value['kind'],p.canonical(value))
    if carrier['completion'] is not None:charge('Completion',p.canonical(carrier['completion']))
    return usage



def attempt_inventory(api,reg,control,current_run=None,*,require_declared=True):
    found={}
    for run in api.pages('/actions/runs',{'workflow_id':p.WORKFLOW},field='workflow_runs'):
        if run.get('id')==current_run or run.get('workflow_id')!=p.WORKFLOW or run.get('event')!='workflow_dispatch':continue
        event=p.decode(run.get('event_payload','').encode());inputs=event.get('inputs',{})
        if inputs.get('operation')!='execute':continue
        prior=p.decode(inputs.get('control','{}').encode())
        if prior.get('session_id')!=control['session_id']:continue
        p.closed(prior,CONTROL_KEYS)
        p.require(p.canonical(prior['registration'])==p.canonical(reg)
                  and p.canonical(prior['resource_plan'])==p.canonical(control['resource_plan']),
                  'same session changed selected source/registration/immutable allocation')
        p.require(type(prior['schema_version']) is int and prior['schema_version']==1
                  and type(prior['attempt_runs']) is list and type(prior['unit_runs']) is list
                  and all(type(value) is int and value>0 for value in prior['attempt_runs']+prior['unit_runs']),
                  'actual attempted control has invalid source/run identities')
        ordinal=len(prior['attempt_runs'])
        p.require(ordinal not in found,'duplicate/reused actual attempted ordinal')
        actual,job,_=p.producer(api,run['id'],reg,inputs['mode'],('success','failure','cancelled'))
        found[ordinal]={'run':actual,'job':job,'control':prior,'mode':inputs['mode']}
    p.require(sorted(found)==list(range(len(found))),'unexplained attempted-ordinal gap')
    items=[found[i] for i in range(len(found))]
    if require_declared:
        p.require([item['run']['id'] for item in items]==control['attempt_runs'],
                  'actual attempted inventory missing from controller; reconcile before dispatch')
    return items


def reject_prior_ordinal_dispatch(api,reg,control,current_run):
    return attempt_inventory(api,reg,control,current_run)


def validate_resume_decision(api,decision,failed,state,reg):
    p.require(type(decision) is dict and state['initial_image'] is not None,
              'terminal failure needs an explicit diagnosed infrastructure resume decision; no new Init')
    p.closed(decision,('schema_version','kind','registration_sha256','source','session_id','failed_run',
        'failed_job','failed_task','failed_ordinal','expected_prior_checkpoint','previous_control_sha256',
        'failed_log_sha256','reason','diagnosis','repair','decided_at','acceptance'))
    ordinal=len(failed['control']['attempt_runs'])
    p.require(type(decision['schema_version']) is int and decision['schema_version']==1 and decision['kind']=='infrastructure_resume_decision'
              and decision['acceptance'] is False and decision['source']==reg['source']
              and decision['registration_sha256']==p.digest(p.canonical(reg))
              and decision['session_id']==state['resource_plan']['record']['session_id']
              and (decision['failed_run'],decision['failed_job'],decision['failed_task'],decision['failed_ordinal'])==
                  (failed['run']['id'],failed['job']['id'],failed['job']['task_id'],ordinal)
              and failed['job']['status'] in ('failure','cancelled')
              and decision['expected_prior_checkpoint']==p.digest(p.canonical(state['checkpoint']))
              and decision['previous_control_sha256']==state.get('control_sha256')
              and decision['reason'] in ('artifact_transport_recovered','runner_recovered','owned_resource_recovered')
              and all(type(decision[k]) is str and 0<len(decision[k].strip())<=4096 for k in ('diagnosis','repair'))
              and utc(failed['run']['created'])<=utc(decision['decided_at'])<=time.time(),
              'infrastructure decision is not scoped to the exact unchanged failed predecessor')
    raw,_=p.producer_log(api,failed['run']['id'],failed['job'],failed['mode'])
    p.require(p.digest(raw)==decision['failed_log_sha256'],'diagnosed failed task log changed/unavailable')



def accounting_plan_inputs(plan):
    return {key:plan[key] for key in ('unit_byte_allowance','unit_record_allowance','maximum_units',
        'total_session_disk_bytes','total_retained_artifact_bytes','resident_bytes','resident_rss_bytes')}



def verify_backend_qualification(request):
    """Fixed source-owned complete reparse of every authenticated actual case."""
    import importlib.machinery
    root=Path(__file__).resolve().parents[1]
    module=importlib.machinery.SourceFileLoader('paged_backend_source_contract',
               str(root/'scripts/check-async-driver-refinement')).load_module()
    runtime=request['runtime'];source=request['work_unit']['source']
    p.require(p.digest(p.canonical(runtime))==source['toolchain_sha256'],'backend full runtime binding differs')
    db=sqlite3.connect(Path(request['dependency_index']).as_uri()+'?mode=ro&immutable=1',uri=True)
    results=[];cases=set()
    try:
        rows=db.execute("SELECT key,digest,path FROM objects WHERE kind='backend_receipt' ORDER BY key")
        for key,sha,path in rows:
            body=p.read(path);case=body['case']
            p.require(case not in cases and p.digest(p.canonical(body))==sha,'duplicate/backend bytes differ')
            ident=p.decode(key);p.require(ident=='backend/'+case+'.json','backend reference identity differs')
            ordinal=body['origin']['unit_ordinal']
            row=db.execute("SELECT path FROM objects WHERE kind='WorkUnit' AND key=?",(p.canonical(ordinal),)).fetchone()
            p.require(row is not None,'backend actual executed-unit control missing')
            unit=p.envelope(p.read(row[0]),'WorkUnit',root)
            p.require(unit['source']==source and unit['stage']=='backend_qualification'
                      and unit['registered_cases']==list(module.paged_backend_cases())
                      and unit['progress']['case_index']==unit['registered_cases'].index(case)+1,
                      'backend case order/source differs from actual executed unit')
            origin={k:unit[k] for k in ('unit_ordinal','run','job','task','attempt','stage')}
            def member(value):
                p.closed(value,('path','bytes','sha256'))
                matches=db.execute('SELECT bytes,sha256,local_path FROM authenticated_members WHERE unit_ordinal=? AND member_path=?',
                                   (ordinal,value['path'])).fetchall()
                p.require(len(matches)==1 and matches[0][:2]==(value['bytes'],value['sha256']),
                          'backend receipt member lacks actual authenticated origin')
                file=Path(matches[0][2])
                p.require(file.stat().st_size==value['bytes'] and p.file_sha(file)==value['sha256'],'backend retained member changed')
                return file
            log=member(body['log_member']);driver=member(body['driver_member']) if body['driver_member'] else None
            tools=body['rust_toolchain']
            p.closed(tools,('rustc','cargo','binaries'));p.closed(tools['binaries'],('rustc','cargo'))
            for name in ('rustc','cargo'):
                p.closed(tools['binaries'][name],('path','sha256'))
                p.require(tools[name]==runtime[name]['version'] and tools['binaries'][name]=={
                    'path':runtime[name]['path'],'sha256':runtime[name]['sha256']}
                    and p.file_sha(runtime[name]['path'])==runtime[name]['sha256'], 'backend actual compiler binary differs')
            categories=module.validate_paged_backend_receipt(body,log,driver,source=source,
                       source_files=request['source_files'],origin=origin)
            results.extend({'kind':kind,'id':ident,'sha256':sha} for kind in categories)
            cases.add(case)
        p.require(cases==set(module.paged_backend_cases()),'backend qualification lacks actual registered cases')
        p.require({'backend_receipt','causal_receipt','numeric_receipt','eligibility_receipt'}<=
                  {ref['kind'] for ref in results},'actual backend/numeric/causal/eligibility family missing')
        return results
    finally:db.close()



def materialize_session_source(root,reg,session_id):
    owned=validate_storage(reg['storage'],root)/session_id
    owned.mkdir(parents=True,exist_ok=True)
    archive=owned/('source-'+reg['source_archive_sha256']+'.tar')
    source=owned/('source-'+reg['source']['commit'])
    if not archive.exists():
        p.require(p.source_archive(root,archive,reg['source'],reg['source_files'])==reg['source_archive_sha256'],
                  'actual source archive differs')
    p.require(not archive.is_symlink() and p.file_sha(archive)==reg['source_archive_sha256'],
              'retained immutable source archive is corrupt')
    if not source.exists():p.extract_source(archive,source,reg['source_archive_sha256'],reg['source'],reg['source_files'])
    p.require(source.resolve()==source,'retained source extraction symlink')
    verify_source_tree(source,reg)
    return source



def verify_source_tree(source,reg):
    source=Path(source)
    actual=set()
    for file in source.rglob('*'):
        p.require(not file.is_symlink(),'symlink in source-only runtime')
        if file.is_file():actual.add(file.relative_to(source).as_posix())
    p.require(actual==set(reg['source_files']) and not (source/'.git').exists(),
              'retained source extraction contains missing/extra source or repository metadata')
    for name,sha in reg['source_files'].items():
        p.require(p.file_sha(source/p.safe_source_path(name))==sha,'retained exact archive source file changed')



def storage_identity(storage):
    p.closed(storage,('root','mount_root','mount_device','lease_file','lease_sha256','lease_expires_at'))
    p.require(type(storage['mount_device']) is int and storage['mount_device']>=0
              and type(storage['lease_sha256']) is str and p.SHA.fullmatch(storage['lease_sha256']),
              'invalid exact storage lease/device identity')
    for key in ('root','mount_root','lease_file'):
        p.require(type(storage[key]) is str and Path(storage[key]).is_absolute()
                  and '..' not in Path(storage[key]).parts,'invalid absolute registered storage path')
    p.require(time.time()<utc(storage['lease_expires_at']),'registered storage lease expired')



def validate_catalog_calibration(execution,directory,reg,root):
    p.closed(execution,('schema_version','purpose','test','argv','input_sha256','rust_toolchain',
                        'output_capture','exit_code','log_sha256','geometry_sha256','acceptance'))
    expected='store::async_driver_catalog::tests::catalog_sqlite_empty_schema_reports_actual_engine_geometry'
    p.require(execution['schema_version']==1 and execution['purpose']=='catalog_operational_calibration'
              and execution['test']==expected and execution['acceptance'] is False
              and type(execution['exit_code']) is int and execution['exit_code']==0,
              'actual empty-DDL calibration execution missing')
    import importlib.machinery
    parser=importlib.machinery.SourceFileLoader('paged_catalog_calibration_contract',
               str(Path(root)/'scripts/check-async-driver-refinement')).load_module()
    log=Path(directory)/'rust-catalog-calibration.log';raw=log.read_bytes()
    p.require(p.file_sha(log)==execution['log_sha256'] and parser.complete_rust_capture(execution,raw)
              and execution['argv']==parser.CORE+[expected,'--','--ignored','--exact','--nocapture'],
              'actual calibration fixed command/capture differs')
    source_inputs=parser.source_identity(False)
    p.require(execution['input_sha256']==source_inputs
              and all(reg['source_files'].get(path)==sha for path,sha in source_inputs.items()),
              'calibration source inputs differ')
    tools=execution['rust_toolchain']
    for name in ('rustc','cargo'):
        p.require(tools[name]==reg['runtime'][name]['version'] and tools['binaries'][name]=={
            'path':reg['runtime'][name]['path'],'sha256':reg['runtime'][name]['sha256']},'calibration compiler differs')
    value=p.read(Path(directory)/'catalog-calibration.json')
    fields=('schema_version','engine','source_path','source_sha256','auxiliary_ddl_sha256','ddl_sha256',
            'compiled_sources','sqlite_version','sqlite_source_id','page_size','page_count','freelist_count',
            'indexes','tables','empty_table_rows','acceptance')
    p.closed(value,fields)
    p.require(value['schema_version']==1 and value['engine']=='rust_rusqlite' and value['acceptance'] is False
              and value['source_path']=='verification/async-driver-refinement/paged-catalog.sql'
              and value['source_sha256']==reg['source_files'][value['source_path']]
              and p.file_sha(Path(directory)/'catalog-calibration.json')==execution['geometry_sha256'],
              'actual Rust SQLite/DDL calibration binding differs')
    compiled=('crates/plurx-core/src/store/async_driver_catalog.rs',
        'crates/plurx-core/src/store/async_driver_catalog/records.rs',
        'crates/plurx-core/src/store/async_driver_catalog/sqlite.rs',
        'crates/plurx-core/src/store/async_driver_catalog/memory.rs',
        'verification/async-driver-refinement/paged-proof.schema.json',
        'verification/async-driver-refinement/paged-catalog.sql')
    p.require(type(value['compiled_sources']) is dict
              and value['compiled_sources']=={path:reg['source_files'][path] for path in compiled},
              'calibration compiled source closure differs')
    p.require(all(type(value[k]) is int and value[k]>=0 for k in ('page_size','page_count','freelist_count','indexes','tables'))
              and value['page_size']>0 and value['tables']>0
              and type(value['empty_table_rows']) is dict and value['empty_table_rows']
              and all(type(v) is int and v==0 for v in value['empty_table_rows'].values())
              and type(value['sqlite_version']) is str and value['sqlite_version']
              and type(value['sqlite_source_id']) is str and value['sqlite_source_id']
              and all(p.SHA.fullmatch(value[k]) for k in ('auxiliary_ddl_sha256','ddl_sha256')),
              'actual Rust empty geometry/engine identity unavailable')
    implementation=(Path(root)/compiled[2]).read_text()
    auxiliary=re.findall(r'(?m)^const AUX_SCHEMA:&str=\"(.*?)\";',implementation,re.DOTALL)
    p.require(len(auxiliary)==1 and '\\' not in auxiliary[0] and '\"' not in auxiliary[0],
              'reviewed auxiliary DDL literal framing changed')
    aux=auxiliary[0].encode('utf-8');normative=(Path(root)/value['source_path']).read_bytes()
    p.require(p.digest(aux)==value['auxiliary_ddl_sha256']
              and p.digest(normative+b'\0'+aux)==value['ddl_sha256']
              and 512<=value['page_size']<=65536 and value['page_size']&(value['page_size']-1)==0
              and value['freelist_count']<=value['page_count']
              and len(value['empty_table_rows'])==value['tables'],
              'actual combined DDL/SQLite geometry differs from reviewed source')
    markers=re.findall(r'^async_paged_catalog_calibration=(.+)$',raw.decode('utf-8'),re.MULTILINE)
    p.require(len(markers)==1 and p.decode(markers[0].encode())==value,'calibration actual output marker differs')
    return value


CAPABILITY_ACTION_FILES=('.github/actions/async-proof-capability/action.yml',
    '.github/actions/async-proof-capability/main.js','.github/actions/async-proof-capability/post.js')


def validate_probe_node_runtime(lines,reg,request,frame_begin):
    prefix='ASYNC-PROOF-CAPABILITY-NODE-RUNTIME '
    records=[(i,p.decode(line[len(prefix):].encode())) for i,line in enumerate(lines) if line.startswith(prefix)]
    p.require(len(records)==1,'actual capability Node runtime observation absent/ambiguous')
    position,value=records[0]
    p.closed(value,('registration_sha256','nonce','case','runtime','acceptance'))
    starts=[i for i,line in enumerate(lines) if re.fullmatch(r'With the provided path, there will be [1-9][0-9]* files? uploaded',line)]
    p.require(starts and position<min(starts)<frame_begin and value['registration_sha256']==p.digest(p.canonical(reg))
              and value['nonce']==request['probe_nonce'] and value['case']==request['probe_case']
              and value['runtime']==reg['runtime']['action_node'] and value['acceptance'] is False,
              'actual local Node action runtime/source/case differs')



def emit_runtime_receipts(state,unit,identity,output):
    record=unit['record'];ordinal=record['unit_ordinal']
    p.require(identity['toolchain']==state['registration']['runtime'] and identity['runner']==state['registration']['runner'],
              'current actual runtime observation differs from registration')
    name='runtime/'+str(ordinal)+'.json'
    value={'schema_version':1,'source':record['source'],'session_id':record['session_id'],'unit_ordinal':ordinal,
        'origin':{k:record[k] for k in ('run','job','task','attempt')},
        'resource_plan_sha256':record['resource_plan_sha256'],'runtime':identity['toolchain'],'runner':identity['runner']}
    p.write(Path(output)/name,value);p.write(Path(output)/'resources.json',state['measurement'])
    observed={'schema_version':1,'source':record['source'],'registration_sha256':p.digest(p.canonical(state['registration'])),
        'session_id':record['session_id'],'unit_ordinal':ordinal,'observed_at':datetime.now(timezone.utc).isoformat(),
        'lease_sha256':state['registration']['storage']['lease_sha256'],'workflow_sha256':record['source']['workflow_sha256'],
        'origin':value['origin'],'attempts':state['attempts']}
    predecessor='predecessors/'+str(ordinal)+'.json';p.write(Path(output)/predecessor,observed)
    rows=[{'reference':{'kind':'runtime_receipt','id':name,'sha256':p.digest(p.canonical(value))},'member_path':name},
          {'reference':{'kind':'runtime_receipt','id':'resources.json','sha256':p.digest(p.canonical(state['measurement']))},'member_path':'resources.json'},
          {'reference':{'kind':'runtime_receipt','id':predecessor,'sha256':p.digest(p.canonical(observed))},'member_path':predecessor}]
    with (Path(output)/'runtime-external-roots.ndjson').open('xb') as out:
        for row in rows:out.write(p.canonical(row)+b'\n')
        out.flush();os.fsync(out.fileno())



def runner(control,root,output,mode,operation):
    p.closed(control,(*CONTROL_KEYS,'probe_case','probe_nonce') if operation=='probe' else CONTROL_KEYS)
    reg=registration(control['registration'],root)
    plan=p.envelope(control['resource_plan'],'ResourcePlan',root)
    p.require(control['schema_version']==1 and type(control['schema_version']) is int
              and control['session_id']==plan['session_id'] and plan['source']==reg['source'],
              'producer control session/source differs before storage')
    p.require(registration_route(reg,root)['state']=='active',
              'producer cannot execute a route pending source activation')
    owned=validate_storage(reg['storage'],root)/control['session_id']
    owned.mkdir(parents=True,exist_ok=True)
    lock=owned/'execution.lock'
    descriptor=os.open(lock,os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
    try:
        try:fcntl.flock(descriptor,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:raise receipts.ReceiptError('Owned session producer lease is already active') from None
        runner_body(control,root,output,mode,operation)
    finally:
        fcntl.flock(descriptor,fcntl.LOCK_UN);os.close(descriptor)



def reconcile_control(api,control,root):
    """Recover actual dispatch IDs; flags never stand in for authenticated roots."""
    p.closed(control,CONTROL_KEYS);reg=registration(control['registration'],root)
    api.maximum_session_requests=max(api.maximum_session_requests,400+(len(control['attempt_runs'])+3)*(32+3*p.UPLOAD_SLOTS))
    items=attempt_inventory(api,reg,control,require_declared=False)
    ids=[item['run']['id'] for item in items]
    p.require(ids[:len(control['attempt_runs'])]==control['attempt_runs'],
              'saved attempt prefix conflicts with actual source history')
    decisions={d['failed_ordinal']:d for d in control['resume_decisions']}
    for item in items:
        for decision in item['control']['resume_decisions']:
            ordinal=decision['failed_ordinal']
            p.require(ordinal not in decisions or p.canonical(decisions[ordinal])==p.canonical(decision),
                      'actual trusted infrastructure decision conflicts with saved prefix')
            decisions[ordinal]=decision
    completed=[item for item in items if item['mode']=='complete' and item['job']['status']=='success']
    if completed:
        p.require(len(completed)==1 and completed[0] is items[-1],'attempt after final successful completion')
        return control,completed[0]['run']['id']
    successes=[item['run']['id'] for item in items if item['job']['status']=='success']
    p.require(successes[:len(control['unit_runs'])]==control['unit_runs'],'saved success prefix conflicts with actual history')
    return {**control,'attempt_runs':ids,'unit_runs':successes,
            'resume_decisions':[decisions[k] for k in sorted(decisions)]},None

if __name__ == '__main__':
    try:
        main()
    except (receipts.ReceiptError, KeyError, OSError, ValueError, subprocess.SubprocessError) as error:
        reason=str(error)[:512] if isinstance(error,receipts.ReceiptError) else type(error).__name__
        print('Async session refused: '+reason,file=sys.stderr)
        raise SystemExit(1) from None

