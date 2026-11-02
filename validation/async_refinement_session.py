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
import platform
from pathlib import Path
import resource
import re
import secrets
import hashlib
import sqlite3
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.request
import urllib.parse
import urllib.error

from validation import async_refinement_provenance as p
from validation import async_snapshot_transport as snapshots
from validation import async_physical_policy as physical
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
              'resume_decisions','main_run','session_id','transport_conformance','uncertain_dispatches')
NULL_PROGRESS = {'context': None, 'record_kind': None, 'after_key': None, 'fifo_ticket': None,
                 'component_id': None, 'family': None, 'case_index': 0}
REGISTRATION_KEYS = ('schema_version', 'source', 'repository', 'repository_name', 'pr', 'event_base',
                     'ref', 'workflow', 'event', 'modes', 'source_files', 'source_archive_sha256',
                     'accounting_method_sha256','accounting_method',
                     'storage', 'runtime', 'runner', 'route', 'expires_at', 'cases', 'main_workflow_sha256', 'upload_slot_count',
                     'transport_conformance_case')



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


PROOF_DIRECTORY = 'verification/async-driver-refinement/'
PROOF_CONTRACT_NAMES = ('PAGED-CONTRACT.md', 'paged-proof.schema.json', 'paged-catalog.sql',
    'finite-inventory.json', 'paged-outcomes.json', 'paged-catalog-transport.schema.json',
    'paged-verifier-continuation.schema.json', 'paged-verifier-state.sql',
    'source-bound-formulas.json', 'source-bound-receipt.schema.json', 'paged-control-carrier.schema.json')


def registration(value, root, now=None):
    p.closed(value, REGISTRATION_KEYS)
    now = time.time() if now is None else now
    p.require(value['schema_version'] == 6 and type(value['schema_version']) is int
              and value['workflow'] == p.WORKFLOW and value['event'] == 'workflow_dispatch'
              and value['upload_slot_count'] == p.UPLOAD_SLOTS
              and value['modes'] == ['unit', 'complete'] and now < utc(value['expires_at'])
              and value['transport_conformance_case']=='native_snapshot_three_batch_v1',
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
    proof = PROOF_DIRECTORY
    names = PROOF_CONTRACT_NAMES
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


def event_merge_base(root, event):
    """The merge base of the event's head and its base sha: the Main commit last merged into the PR.

    The workflow event payload carries `pull_request.base.sha` (the live Main tip when the event fired)
    and no merge base, so the job derives it from the checkout: every workflow that calls this checks
    out with `fetch-depth: 0`. Exactly one merge base is required; none (unrelated or shallow history)
    and several (criss-cross merges) refuse, because the registered `event_base` names one commit.
    """
    try:
        head, base = event['pull_request']['head']['sha'], event['pull_request']['base']['sha']
    except (KeyError, TypeError):
        raise receipts.ReceiptError('event lacks head/base identities') from None
    p.require(isinstance(head, str) and isinstance(base, str)
              and p.COMMIT.fullmatch(head) and p.COMMIT.fullmatch(base), 'event head/base identities malformed')
    try:
        found = subprocess.check_output(['git', 'merge-base', '--all', head, base], cwd=root, text=True,
                                        timeout=60, stderr=subprocess.DEVNULL).split()
    except (subprocess.SubprocessError, OSError):
        raise receipts.ReceiptError('event merge base unavailable; history is not complete') from None
    p.require(len(found) == 1 and p.COMMIT.fullmatch(found[0]), 'event merge base absent or ambiguous')
    return found[0]


def live_pr(api, reg):
    """The open ready PR at the registered head whose merge base with Main is `event_base`.

    `event_base` is the merge base of the registered head and Main, the Main commit last merged into
    the PR. Main may advance while the session runs (the live base sha is recorded for audit, never
    compared); merging Main into the head moves the merge base and refuses.
    """
    live = api.get('/pulls/' + str(reg['pr']))
    p.require(live.get('number') == reg['pr'] and live.get('state') == 'open'
              and live.get('draft') is False and live.get('head', {}).get('sha') == reg['source']['commit']
              and live.get('head', {}).get('repo', {}).get('id') == reg['repository']
              and live.get('base', {}).get('repo', {}).get('id') == reg['repository']
              and live.get('base', {}).get('ref') == 'main'
              and isinstance(live.get('base', {}).get('sha'), str)
              and p.COMMIT.fullmatch(live['base']['sha'])
              and isinstance(live.get('merge_base'), str) and live['merge_base']==reg['event_base'],
              'current PR is not ready/open at registered exact head and merge base')
    return live


def actual_runner(lines, job, reg):
    """The owned runner executing this task, from Forgejo's records rather than the environment.

    Forgejo's runner exports no RUNNER_NAME (see the cache actions). Forgejo writes
    the uuid of the runner that took the task into the task log preamble; the
    registration binds that uuid to the runner name (authenticate_runner_route
    checks the binding against the live inventory before every dispatch). The
    OS/architecture are the machine's own.
    """
    p.require(p.task_runner_uuid(lines, job) == reg['route']['runner_uuid'], 'unregistered runner slot')
    arch = {'x86_64': 'X64', 'amd64': 'X64', 'aarch64': 'ARM64', 'arm64': 'ARM64'}.get(platform.machine().lower())
    return {'name': reg['runner']['name'], 'os': platform.system(), 'arch': arch}


def runtime_identity(root, reg, runner):
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
    expected=physical_admission(plan,reg,measurement,bounds,inventory)
    p.require(plan['total_retained_artifact_bytes']<=plan['total_session_disk_bytes'],
              'retained artifacts exceed immutable session disk')
    return {'operational_peak_bytes':expected['peak_bytes'],'source_global_widths':widths,
            'finite_worst_case_expressions':finite,'record_allowances':allowances,
            'accounting_inventory':inventory}



def physical_method(reg, plan, policy, inventory, allowances):
    """The one closed typed_physical_caps_v1 method; probe and admission both build it here."""
    return {'id':'typed_physical_caps_v1','source_sha256':reg['accounting_method_sha256'],
            'policy_sha256':p.digest(p.canonical(policy)),'table_map_sha256':policy['table_map_sha256'],
            'accounting_inventory':inventory,'record_allowances':allowances,
            'source_bound_specification_sha256':reg['source_files'][p.SOURCE_BOUND_SPEC],
            'plan_inputs':accounting_plan_inputs(plan),'prefix_policy':'late_bound_per_batch_v1'}


def physical_admission(plan, reg, measurement, bounds, inventory):
    """Admission is exact equality with the typed physical calibration; nothing is inferred.

    The retired index/WAL/backup overheads and the whole-image-in-current-slots inequality
    have no place here: per-batch native packing is exact and late-bound.
    """
    p.require('resource_overheads' not in measurement,'retired record-times-table overheads cannot authorize v3')
    policy,allowances,calibration=bounds['physical_policy'],bounds['record_allowances'],bounds['calibration']
    p.require(type(calibration) is dict and measurement.get('resource_calibration')==calibration,
              'actual physical calibration is missing or changed')
    expected=physical.calibrate_physical_policy(policy,calibration['geometry'],calibration['baseline'],plan)
    p.require(calibration==expected and expected['peak_bytes']<=measurement['free_disk_bytes'],
              'physical calibration/available disk differs')
    p.require(measurement.get('resource_method')==physical_method(reg,plan,policy,inventory,allowances),
              'closed measured physical method differs')
    return expected


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
            'units': [], 'attempts': [], 'next_ordinal':0, 'next_dispatch_ordinal':0, 'pending_publication':None, 'session': None, 'checkpoint': None, 'initial_image': None,
            'stage': 'expand', 'progress': copy.deepcopy(NULL_PROGRESS), 'phase': 'admitted',
            'retained_bytes': 0, 'extracted_bytes': 0, 'dispatched_runs': [], 'created_at': time.time() if now is None else now}


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


def execution_origin(state, unit, operation):
    p.require(operation in ('execute','publish'),'unknown proof publication operation')
    return {key:unit[key] for key in ('repository','pr','event_base','workflow_id','event_name',
        'mode','run','job','task','attempt')} | {'operation':operation,
        'dispatch_ordinal':state['next_dispatch_ordinal']}


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


GRANT_ENV = ('PLURX_ASYNC_PHYSICAL_GRANT_FD', 'PLURX_ASYNC_PHYSICAL_GRANT_SHA256',
             'PLURX_ASYNC_PHYSICAL_LEASE_FD')


def process_start_ticks(pid):
    """Kernel start time of a live process; binds a grant to its exact parent (platform-neutral)."""
    return physical.process_start_ticks(pid)


class HeldLease:
    """The validated owned-slot lease, held open for the whole parent lifetime.

    The descriptor, not a filename, is what an allocation-capable child inherits;
    the child never reopens a lease and claims authority from a path.
    """
    def __init__(self, storage, source_root):
        root = validate_storage(storage, source_root)
        path = Path(storage['lease_file'])
        self.fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        info = os.fstat(self.fd)
        p.require(info.st_dev == storage['mount_device']
                  and os.path.samestat(info, path.lstat()) and p.file_sha(path) == storage['lease_sha256'],
                  'held lease descriptor is not the registered lease')
        self.relative_path = p.safe_path(path.relative_to(root).as_posix())
        self.device, self.inode = info.st_dev, info.st_ino

    def close(self):
        if self.fd >= 0:
            os.close(self.fd)
            self.fd = -1


CHILD_SCRATCH = 'child-scratch'
EXPORT_SCRATCH = 'manifest-closure.sqlite'
CONTINUATION_SCRATCH = 'continuation-validation.sqlite'
RESTORE_SCRATCH = 'restore-scratch.sqlite'
CATALOG_BACKUP = 'catalog-backup.sqlite'
VERIFIER_BACKUP = 'verifier-backup.sqlite'
SCRATCH_RESOURCES = ('catalog_export_scratch', 'verifier_continuation_scratch',
                     'catalog_or_verifier_restore_scratch')
# Operations whose output directory holds only small receipts (not a unit's pages and evidence).
CONTROL_OPERATIONS = ('validate_restore', 'completion_verify', 'catalog_calibration')
# The writer's named single-file claims and the runtime HOME/TMPDIR live in the sibling
# "<output>-aux" directory, so every output directory is exactly one recursive granted subtree
# with no nested claims. The Rust catalog (async_driver_catalog::AUXILIARY_SUFFIX) and the
# checker (paged_codec.AUXILIARY_SUFFIX) derive the same name;
# tests/operations/test_paged_request_fixtures.py pins all three.
AUXILIARY_SUFFIX = '-aux'
# The job output directory of one producer run: the single constructor used both to place a job's
# output (event_output) and to find the earlier jobs' outputs to discard.
JOB_OUTPUT_PREFIX = 'run-'


def auxiliary_directory(output):
    output = Path(output)
    p.require(output.name != '', 'auxiliary directory needs a named output directory')
    return output.with_name(output.name + AUXILIARY_SUFFIX)


def job_output_directory(session_directory, run_id):
    return Path(session_directory) / (JOB_OUTPUT_PREFIX + str(receipts.positive(int(run_id))))


class GrantPlan:
    """Explicit delegation for one child grant.

    A family component is delegated at most once. Pool shares are named amounts whose sum
    per pool is checked against what the parent still holds, so no capacity is ever
    delegated twice (a second delegation would deduct the pool a second time).
    """
    def __init__(self, context):
        self.context, self.entries, self.delegated, self.credits = context, [], {}, {}

    def uncommitted(self, resource, component):
        """What the parent can still give after this plan's shares (held claims are already deducted)."""
        key = resource, component
        return self.context.ledger.remaining(*key) - (self.delegated.get(key, 0) - self.credits.get(key, 0))

    def add(self, resource, component, path, amount, recursive=False, whole=False, keep_held=False):
        """Delegate `amount` (or, with `whole`, everything the family component can still give).

        When the ledger already holds exactly this claim (an activated main file, or a surviving
        journal), the entry carries that reservation_id: the issuer moves ownership to the grant
        in the one ledger without releasing it, so only the growth beyond the held charge is
        deducted from what the parent still holds.
        """
        owner = physical.FAMILY_OWNERS.get(resource, physical.POOL_OWNERS.get(resource))
        p.require(owner is not None, 'unknown physical family/pool')
        p.require(type(amount) is int, 'physical delegation share must be an integer')
        key = resource, component
        if resource in physical.FAMILY_OWNERS:
            p.require(key not in self.delegated, 'physical family component delegated twice: ' + '/'.join(key))
        entry = {'reservation_id': secrets.token_hex(32), 'owner': owner, 'resource': resource,
                 'component': component, 'bytes': amount, 'relative_path': self.context.relative(path),
                 'recursive': recursive}
        held = self.context.ledger.held_reservation(entry)
        held_bytes = 0 if held is None else held.bytes
        remaining = self.context.ledger.remaining(resource, component)
        if whole:
            amount = entry['bytes'] = remaining + held_bytes
        elif keep_held and held is not None:
            # A claim the ledger already holds is never shrunk by a later invocation on the same tree.
            amount = entry['bytes'] = max(amount, held_bytes)
        total = self.delegated.get(key, 0) + amount
        credit = self.credits.get(key, 0) + held_bytes
        if amount <= 0 or total - credit > remaining:
            raise physical.PhysicalIncomplete('physical capacity exhausted before child startup: ' + '/'.join(key))
        if held is not None:
            entry['reservation_id'] = held.reservation_id
        self.delegated[key] = total
        self.credits[key] = credit
        self.entries.append(entry)

    def family(self, resource, component, path):
        """The whole family component (only this child holds that family while it runs)."""
        self.add(resource, component, path, 1, whole=True)

    def database(self, resource, path):
        for component in ('main', 'journal', 'temp'):
            self.family(resource, component, path)

    def pool(self, resource, path, amount, recursive=False, keep_held=False):
        self.add(resource, 'bytes', path, amount, recursive, keep_held=keep_held)


class PhysicalContext:
    """Parent-owned ledger, lease and child-grant lifetime for one controller process.

    The parent deducts every delegated allocation in its single ledger, writes a
    private descriptor, launches the child with explicit pass_fds and keeps the
    lease and deductions held until the child is reaped and owned files are
    reconciled. A child never receives the full policy budget again.
    """
    def __init__(self, reg, plan, session_id, root, owned):
        self.reg, self.session_id, self.owned = reg, session_id, Path(owned).resolve()
        self.plan = p.envelope(plan, 'ResourcePlan', root)
        pending = p.decode(self.plan['source_bounds_utf8'].encode()).get('calibration') is None
        wrapper = operational_bounds(self.plan['source_bounds_utf8'], reg, root, measurement_pending=pending)
        self.policy = wrapper['physical_policy']
        self.policy_sha256 = p.digest(p.canonical(self.policy))
        self.lease = HeldLease(reg['storage'], root)
        info = self.owned.stat()
        self.ledger = physical.ReservationLedger(self.policy, self.owned)
        self.root_identity = {'path': str(self.owned), 'device': info.st_dev, 'inode': info.st_ino}
        # output directory -> read-only attachments of already held parent reservations
        self.attachments = {}
        self.grants = []
        self.survivors = []  # reservations re-tracked for objects a crashed parent left mid-transfer

    def binding(self):
        return {'source': self.reg['source'], 'session_id': self.session_id,
                'resource_plan_sha256': self.plan['plan_sha256'],
                'physical_policy_sha256': self.policy_sha256, 'owned_root': self.root_identity,
                'parent': {'pid': os.getpid(), 'start_ticks': process_start_ticks(os.getpid())}}

    def relative(self, path):
        path = Path(path).absolute()
        p.require(self.owned in path.parents, 'physical allocation path escapes the exact owned root')
        return p.safe_path(path.relative_to(self.owned).as_posix())

    def attach_restore(self, output_dir, held):
        """Read-only Attachments of the held restore_staging reservations for one validator output.

        `held` is [(PoolBudget, staged file)]: each restored file occupies its own single-file
        reservation, so each can later transfer to main alone. The validator gets no spending,
        release or ownership authority and no second main-file allocation.
        """
        attachments = []
        for budget, path in held:
            reservation, relative = budget.reservation, self.relative(path)
            p.require((relative, False) in reservation.scope() and reservation.bytes > 0
                      and not reservation.released and not reservation.delegated and reservation.transfer is None,
                      'restore attachment names no held tracked restore_staging reservation')
            attachments.append({'reservation_id': reservation.reservation_id, 'owner': reservation.owner,
                                'resource': reservation.key[0], 'component': reservation.key[1],
                                'bytes': reservation.bytes, 'relative_path': relative, 'recursive': False,
                                'access': 'read_only'})
        self.attachments[str(output_dir)] = attachments

    def output_allowance(self, request):
        """The one recursive share of an output directory: a unit writer's pages, manifests, verifier
        evidence, TLC logs and initial image are bounded by the immutable unit_byte_allowance (plus the
        controller-written captures); a control operation writes only small receipts."""
        if request.get('operation') in CONTROL_OPERATIONS:
            return physical.CONTROL_OUTPUT_BYTES
        return physical.unit_output_bytes(self.plan.get('unit_byte_allowance'))

    def child_plan(self, request, output):
        """Disjoint, explicitly sized allocations for exactly the files this child may write.

        Every family component is delegated once; a shared pool is split into named shares
        whose sum never exceeds what the parent still holds. A restore validator receives the
        read-only attachment instead of any main-file allocation.

        The output directory is ONE recursive share of source_runtime_scratch_bytes, so every
        file a child leaves there (pages, manifests, evidence, logs, the initial image) is charged.
        Nothing nested inside it is claimed: the named single-file claims and the runtime
        HOME/TMPDIR sit in the sibling auxiliary directory.
        """
        out = Path(output)
        aux = auxiliary_directory(out)
        attached = list(self.attachments.get(str(out), []))
        restoring = request.get('operation') == 'validate_restore'
        p.require(restoring == bool(attached),
                  'restore validators attach the held restore reservation and nothing else attaches')
        plan = GrantPlan(self)
        if restoring:
            # Journal/temp for ancillary writes only; the restored main file is never re-reserved.
            for component in ('journal', 'temp'):
                plan.family('catalog_or_verifier_restore_scratch', component, aux / RESTORE_SCRATCH)
        else:
            for family, path in self.writer_databases(request, aux):
                plan.database(family, path)
            plan.database('catalog_export_scratch', aux / EXPORT_SCRATCH)
            families = self.policy['families']
            # A standalone backup is at most its database's ceiling: one named share each.
            plan.pool('snapshot_spool_bytes', aux / CATALOG_BACKUP, families['catalog_main']['database_bytes'])
            plan.pool('snapshot_spool_bytes', aux / VERIFIER_BACKUP, families['verifier_main']['database_bytes'])
        plan.database('verifier_continuation_scratch', aux / CONTINUATION_SCRATCH)
        plan.pool('source_runtime_scratch_bytes', out, self.output_allowance(request), recursive=True, keep_held=True)
        # HOME and TMPDIR share one subtree allocation, so the rest of the pool is delegated exactly once.
        plan.pool('source_runtime_scratch_bytes', aux / CHILD_SCRATCH,
                  plan.uncommitted('source_runtime_scratch_bytes', 'bytes'), recursive=True)
        return plan.entries, attached

    @staticmethod
    def writer_databases(request, default_directory):
        return [('catalog_main', Path(request.get('catalog_path', default_directory / 'catalog.sqlite'))),
                ('verifier_main', Path(request.get('verifier_work_db', default_directory / 'verifier.sqlite')))]

    def issue(self, request, output):
        """Seal the child's grant. A main/journal claim the ledger already holds (an activated
        restore, an earlier writer's surviving file) is delegated by its reservation_id inside
        issue_child_grant: ownership moves atomically, with no release and no unowned interval,
        and a failed issue leaves the parent reservation exactly as it was."""
        out = Path(output)
        allocations, attachments = self.child_plan(request, out)
        # B's issuer deducts, canonicalizes, fsyncs and reopens the descriptor
        # read-only; the pre-issue descriptor reservation is charged to control_bytes.
        return self.ledger.issue_child_grant(allocations, attachments, self.binding(), self.lease)

    def finish(self, handle):
        """After the child is reaped: drop its scratch, then reconcile every surviving owned file.

        Scratch (runtime HOME/TMPDIR, export, continuation and restore scratch) has no value
        after exit and would otherwise keep its whole allocation charged and its path claimed.
        It is removed only when the child stayed within its grant, so an overage is still
        observed by reconciliation. Sealed outputs (databases, backups) and the output
        directory's own recursive share stay charged until the next job discards them.
        """
        try:
            try:
                handle.check()
            except (physical.PhysicalIncomplete, physical.PhysicalPolicyError):
                pass
            else:
                self.discard_scratch(handle)
            handle.reconcile_after_reap()
        finally:
            handle.close()

    @staticmethod
    def is_scratch(entry):
        """Runtime HOME/TMPDIR and the scratch databases; never the output directory's own share."""
        if entry['resource'] == 'source_runtime_scratch_bytes':
            return Path(entry['relative_path']).name == CHILD_SCRATCH
        return entry['resource'] in SCRATCH_RESOURCES

    def discard_scratch(self, handle):
        for entry in handle.descriptor['allocations']:
            if not self.is_scratch(entry):
                continue
            target = self.owned / entry['relative_path']
            if entry['recursive']:
                if target.is_symlink():
                    target.unlink()
                elif target.exists():
                    shutil.rmtree(target)
                continue
            for suffix in ('', *physical.DB_COMPANIONS):
                companion = Path(str(target) + suffix)
                if companion.is_symlink() or companion.exists():
                    companion.unlink()

    def attach_retained(self, output_dir, paths):
        """Read-only Attachments of reservations still held for files an earlier child left behind."""
        attachments = []
        for path in paths:
            relative = self.relative(path)
            holders = [r for r in self.ledger.reservations if not r.released and not r.delegated
                       and r.transfer is None and (relative, False) in r.scope()]
            p.require(len(holders) == 1, 'read-only validation needs exactly one held reservation: ' + relative)
            held = holders[0]
            attachments.append({'reservation_id': held.reservation_id, 'owner': held.owner, 'resource': held.key[0],
                                'component': held.key[1], 'bytes': held.bytes, 'relative_path': relative,
                                'recursive': False, 'access': 'read_only'})
        self.attachments[str(output_dir)] = attachments

    def reconcile_crash(self):
        """Settle what a crashed earlier parent left in the session's owned ledger directories.

        A `physical-transfers/` record names ONE file moving between two reservations. Its exact
        location is established with locate_transfer (ambiguity or loss refuses the job: both
        charges stay), the located object is charged to exactly the side that holds it, and only
        then is the record removed. A `physical-grants/` descriptor of a dead parent confers
        nothing: it is charged while it exists and removed; a descriptor of a live foreign parent
        refuses the job. Everything is charged before anything is removed.
        """
        ledger = self.ledger
        for record_path in sorted(self.owned.glob(physical.TRANSFER_DIRECTORY + '/*.json')):
            raw = record_path.read_bytes()
            record = physical._strict_json(raw)
            where = physical.locate_transfer(self.owned, record_path)
            # Either side of the record names its own owner/resource/component, so the located object is
            # charged to exactly the reservation the crash left it in, whatever pool that was.
            side = record['source' if where == 'source' else 'destination']
            p.require(all(type(side.get(key)) is str for key in ('owner', 'resource', 'component', 'relative_path')),
                      'physical transfer record does not name the owner it was moving from')
            owner, resource, component = side['owner'], side['resource'], side['component']
            held = ledger.reserve(owner, resource, component, record['bytes'])
            held.track(self.owned / side['relative_path'])
            self.survivors.append(held)
            control = ledger.reserve('session_controller', 'control_bytes', 'bytes', len(raw))
            control.track(record_path)
            record_path.unlink()
            fsync_directory(record_path.parent)
            control.release_after_removal()
        for descriptor_path in sorted(self.owned.glob(physical.GRANT_DIRECTORY + '/*.json')):
            raw = descriptor_path.read_bytes()
            parent = physical._strict_json(raw)['parent']
            try:
                live = physical.process_start_ticks(parent['pid']) == parent['start_ticks']
            except (physical.PhysicalPolicyError, OSError):
                live = False
            p.require(not live, 'a live foreign parent still holds a physical grant in this session')
            control = ledger.reserve('session_controller', 'control_bytes', 'bytes', len(raw))
            control.track(descriptor_path)
            descriptor_path.unlink()
            fsync_directory(descriptor_path.parent)
            control.release_after_removal()

    def prior_job_outputs(self):
        """Every job output directory of this session, found with the constructor that places them.

        The set is exactly the session-directory entries equal to job_output_directory(owned, n) for
        some run number n, so the discard can never silently stop matching the job's own output.
        """
        found = []
        for entry in sorted(self.owned.iterdir()):
            number = entry.name[len(JOB_OUTPUT_PREFIX):]
            # Run numbers are positive: `run-0` is not a name the constructor produces, so it is not an output.
            if (entry.name.startswith(JOB_OUTPUT_PREFIX) and number.isascii() and number.isdigit()
                    and int(number) > 0 and entry == job_output_directory(self.owned, int(number))):
                found.append(entry)
        return found

    def discard_prior_outputs(self, output):
        """Earlier jobs' output trees have no value: everything is re-authenticated from the API.

        Left in place they would occupy owned storage no pool ever charged. `output` must be this
        session's own job output directory (job_output_directory), and only the other job outputs are
        removed; the persistent spools, the API cache, the ledger directories and the ordinal guards
        are not job outputs and are never touched here.
        """
        output = Path(output).resolve()
        outputs = self.prior_job_outputs()
        p.require(output in outputs, 'job output is not an existing session job output directory')
        for entry in outputs:
            if entry != output and not entry.is_symlink() and entry.is_dir():
                shutil.rmtree(entry)
        self.release_removed_survivors()

    def release_removed_survivors(self):
        """A reservation re-tracked after a crash for a file that has since been removed is released.

        reconcile_crash charges every survivor it finds; one that lived inside an earlier job's output
        tree is gone once that tree is discarded, and keeping its charge would name a file that no longer
        exists. A survivor whose file still exists stays charged.
        """
        kept = []
        for held in self.survivors:
            if held.released:
                continue
            if held.surviving_files():
                kept.append(held)
            else:
                held.release_after_removal()
        self.survivors = kept

    def discard_dead_spools(self, live):
        """Remove the publication/rehearsal spools no authenticated pending chain still needs."""
        for entry in sorted(self.owned.iterdir()):
            if (entry.name.startswith(('publication-', 'conformance-')) and entry.name not in live
                    and entry.is_dir() and not entry.is_symlink()):
                shutil.rmtree(entry)

    def close(self):
        self.lease.close()


_PHYSICAL = []


class physical_session:
    """Install the one controller PhysicalContext for run_quantum in this process."""
    def __init__(self, context):
        self.context = context

    def __enter__(self):
        p.require(not _PHYSICAL, 'nested physical controller context')
        _PHYSICAL.append(self.context)
        return self.context

    def __exit__(self, *_exc):
        _PHYSICAL.clear()
        _DOWNLOADS.clear()
        _ARCHIVES.clear()
        self.context.close()


def current_physical():
    p.require(len(_PHYSICAL) == 1, 'allocation-capable child has no physical grant context')
    return _PHYSICAL[0]


def child_environment(reg, request, source, output, grant=None):
    # Never copy os.environ: API tokens, checkout credentials and CI payloads
    # stay in the trusted controller. HOME is a fresh private child directory.
    scratch = auxiliary_directory(output) / CHILD_SCRATCH  # the one granted runtime-scratch subtree
    home = scratch / 'home'
    home.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = scratch / 'temp'
    temporary.mkdir(exist_ok=True, mode=0o700)
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
    # The environment is built from scratch, so no inherited grant variable can
    # survive; each child receives only its own explicit current grant.
    p.require(not any(key.startswith('PLURX_ASYNC_PHYSICAL_') for key in env),
              'stale physical grant variable inherited')
    p.require(grant is not None, 'allocation-capable child requires an explicit physical grant')
    granted = grant.environment()
    p.require(type(granted) is dict and set(granted) == set(GRANT_ENV)
              and all(type(value) is str for value in granted.values()), 'child grant environment differs')
    env.update(granted)
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
    context = current_physical()
    # Deduct every delegated allocation before the child exists; the lease and the
    # deductions stay held through reap and owned-file reconciliation (finish).
    grant = context.issue(request, output)
    try:
        return _run_granted_child(context, grant, request, request_path, reg, source, output, command,
                                  operational, limits, deadline, limit, session_id, log_path)
    finally:
        context.finish(grant)


def _run_granted_child(context, grant, request, request_path, reg, source, output, command,
                       operational, limits, deadline, limit, session_id, log_path):
    import selectors
    import signal
    unit = request.get('work_unit')
    environment=child_environment(reg,request_path,source,output,grant)
    if operational:
        environment.pop('PLURX_ASYNC_PAGED_REQUEST',None)
        environment['PLURX_ASYNC_CATALOG_CALIBRATION']=str(Path(output)/'catalog-calibration.json')
    process = subprocess.Popen(command, cwd=source, env=environment,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               pass_fds=(grant.fd, context.lease.fd),
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
    # Completion/Main admission requires the exact current-source native transport case result.
    case_result = state.get('conformance_result')
    p.require(type(case_result) is dict and case_result.get('kind') == 'native_transport_conformance'
              and case_result.get('case_id') == snapshots.CONFORMANCE_CASE and case_result.get('complete') is True
              and case_result.get('acceptance') is False and case_result.get('source') == state['registration']['source'],
              'completion requires the exact current-source native transport conformance result')
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


def declared_uncertain(control):
    """The closed, ordered rows of the control's `uncertain_dispatches`: dispatches that were sent, never seen
    and settled as uncertain. Each consumed its ordinal and its unit of `maximum_units`."""
    rows = control['uncertain_dispatches']
    p.require(type(rows) is list, 'uncertain dispatch declaration is not a list')
    for row in rows:
        p.closed(row, ('ordinal', 'control_sha256'))
        p.require(type(row['ordinal']) is int and row['ordinal'] >= 0 and type(row['control_sha256']) is str
                  and p.SHA.fullmatch(row['control_sha256']), 'invalid uncertain dispatch declaration')
    ordinals = [row['ordinal'] for row in rows]
    p.require(ordinals == sorted(set(ordinals)), 'uncertain dispatch declarations are unordered or duplicated')
    return rows


def control_ordinal(control):
    """The ordinal a dispatch with this control takes: every run before it plus every uncertain dispatch."""
    return len(control['attempt_runs']) + len(declared_uncertain(control))


class Inventory(list):
    """The authenticated attempt items by ordinal, with the uncertain declarations that explain the other ordinals."""
    uncertain = ()


UNCERTAIN_DISPATCH_SECONDS = 1800  # how long a sent dispatch may stay invisible before it is settled as uncertain


class DispatchReservations:
    """The durable, fsynced record of every dispatch ordinal, written BEFORE the API call.

    One ordinal is one `dispatch-<n>.reserved.json` (its registration, control digest, mode and
    operation), created exclusively and fsynced with its directory before anything is sent. A
    `dispatch-<n>.sent.json` marker is created, the same way, immediately before the POST begins; it
    is removed again only when the server definitively refused the request, so a marker that survives
    means "this POST may have reached the server". A sent dispatch whose run is still not visible after
    `UNCERTAIN_DISPATCH_SECONDS` is settled as uncertain by `dispatch-<n>.uncertain.json`: the ordinal
    is consumed for good and the retry takes the next one (see `claim_dispatch`).

    The authenticated attempt inventory (the Actions runs and their controls) stays the authority on what
    ran; this record is what lets a restarted controller reconcile a crash between reservation and
    POST, or after the POST and before the run is visible, without ever reusing the ordinal for another
    dispatch or skipping it.

    The directory belongs to ONE session and registration (`session.json`, written on first use and
    compared on every later one), so a restart that points the controller at the same directory can only
    ever reconcile that session's ordinals; the controller's `--output` must therefore be stable for the
    session, and a different directory has, by construction, no reservation to recover.
    """
    STAGES = ('reserved', 'sent', 'uncertain')

    def __init__(self, directory, identity=None):
        self.directory = Path(directory)
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        p.require(self.directory.is_dir() and not self.directory.is_symlink(),
                  'dispatch reservation directory is not a private directory')
        self.identity = None
        if identity is not None:
            self.bind(identity)

    def bind(self, identity):
        p.require(type(identity) is dict and set(identity) == {'session_id', 'registration_sha256'},
                  'dispatch reservation identity is not closed')
        target = self.directory / 'session.json'
        try:
            self._create(target, {'schema_version': 1, 'kind': 'dispatch_reservation_session', **identity})
        except FileExistsError:
            pass
        held = p.read(target)
        p.require(held == {'schema_version': 1, 'kind': 'dispatch_reservation_session', **identity},
                  'the dispatch reservation directory belongs to a different session or registration')
        self.identity = identity

    def file(self, ordinal, stage):
        p.require(type(ordinal) is int and ordinal >= 0 and stage in self.STAGES,
                  'invalid dispatch reservation name')
        return self.directory / ('dispatch-%d.%s.json' % (ordinal, stage))

    def ordinals(self):
        found = []
        for entry in self.directory.glob('dispatch-*.reserved.json'):
            number = entry.name[len('dispatch-'):-len('.reserved.json')]
            p.require(number.isascii() and number.isdigit() and entry == self.file(int(number), 'reserved'),
                      'unrecognized dispatch reservation file')
            found.append(int(number))
        return sorted(found)

    def _create(self, target, value):
        """Create `target` exclusively with its complete, fsynced content (never a partial record)."""
        partial = Path(str(target) + '.partial')
        partial.unlink(missing_ok=True)  # a leftover of an earlier crash: never reachable under the final name
        raw = p.canonical(value)
        fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            written = 0
            while written < len(raw):
                written += os.write(fd, raw[written:])
            os.fsync(fd)
        finally:
            os.close(fd)
        try:
            os.link(partial, target)  # fails if the name exists: an ordinal is reserved at most once
        finally:
            partial.unlink(missing_ok=True)
        fsync_directory(self.directory)

    def read(self, ordinal):
        return p.read(self.file(ordinal, 'reserved'))

    def reserve(self, record):
        self._create(self.file(record['ordinal'], 'reserved'), record)

    def state(self, ordinal):
        """None (never reserved), 'reserved' (nothing sent), 'sent' (the POST may have reached the server)
        or 'uncertain' (sent, never seen, settled: the ordinal is consumed and will not be sent again)."""
        if not self.file(ordinal, 'reserved').exists():
            return None
        if self.file(ordinal, 'uncertain').exists():
            return 'uncertain'
        return 'sent' if self.file(ordinal, 'sent').exists() else 'reserved'

    def uncertain(self):
        """The reservations settled as uncertain, by ordinal, as the closed control rows that declare them."""
        rows = []
        for ordinal in self.ordinals():
            if self.state(ordinal) == 'uncertain':
                rows.append({'ordinal': ordinal, 'control_sha256': self.read(ordinal)['control_sha256']})
        return rows

    def mark_uncertain(self, ordinal):
        p.require(self.state(ordinal) in ('sent', 'uncertain'), 'only a sent dispatch can be settled as uncertain')
        if not self.file(ordinal, 'uncertain').exists():
            self._create(self.file(ordinal, 'uncertain'), {'ordinal': ordinal, 'kind': 'dispatch_uncertain'})

    def mark_sent(self, ordinal):
        if not self.file(ordinal, 'sent').exists():
            self._create(self.file(ordinal, 'sent'), {'ordinal': ordinal, 'kind': 'dispatch_sent'})

    def mark_refused(self, ordinal):
        """The server definitively refused the request: nothing was created, the ordinal is unsent again."""
        self.file(ordinal, 'sent').unlink(missing_ok=True)
        fsync_directory(self.directory)


def dispatch_record(ordinal, session_id, expected, control):
    """What an ordinal is reserved for. Every field is derived from replay-authenticated state."""
    return {'schema_version': 1, 'kind': 'dispatch_reservation', 'ordinal': ordinal, 'session_id': session_id,
            'registration_sha256': expected['registration_sha256'], 'mode': expected['mode'],
            'operation': expected['operation'], 'control_sha256': p.digest(p.canonical(control)),
            'reserved_at': datetime.now(timezone.utc).isoformat()}


def settle_dispatch(reservations, dispatched):
    """Reconcile the reserved ordinals with the authenticated attempt inventory.

    `dispatched` lists the digest of the control each consumed ordinal ran (or, for an ordinal settled as
    uncertain, was reserved) with, by ordinal. Every reserved ordinal inside it must have been reserved
    for exactly the control that ran, and for this session; no reservation may lie beyond the next
    ordinal (an ordinal is never skipped). An ordinal without a reservation passes: a dispatch can come
    from a controller whose directory this process never saw, and the inventory itself refuses duplicates.
    The reservation at the next ordinal, if any, is the one dispatch whose outcome the inventory cannot
    show yet: it is returned for `claim_dispatch`.
    """
    ordinals = reservations.ordinals()
    p.require(all(ordinal <= len(dispatched) for ordinal in ordinals),
              'a reserved dispatch ordinal lies beyond the authenticated attempt inventory; ordinals are never skipped')
    for ordinal in ordinals:
        if ordinal < len(dispatched):
            held = reservations.read(ordinal)
            p.require(held['control_sha256'] == dispatched[ordinal],
                      'a dispatch ran with a control other than the one its ordinal was reserved for')
            p.require(reservations.identity is None or held['session_id'] == reservations.identity['session_id'],
                      'a dispatch reservation names a different session')
    return len(dispatched) if len(dispatched) in ordinals else None


def claim_dispatch(reservations, record, runs, dispatched, now=None):
    """Reserve the next ordinal durably, or recover the reservation a crashed controller left there.

    Returns ('post', record) when the request must be sent, ('adopt', run) when the dispatch already
    created its run and ('uncertain', record) when the dispatch is settled as uncertain. A recovered
    reservation is never replaced: it keeps its ordinal and its payload.

      * never sent: the crash was before the POST; the very same request is sent for the same ordinal;
      * its run is visible (any status): the crash was after the POST; the run is adopted, nothing is sent;
      * sent, no run visible, and `UNCERTAIN_DISPATCH_SECONDS` not yet elapsed since it was reserved: the
        run may only be late; the controller refuses and waits, the ordinal stays consumed;
      * sent, no run visible after that time: the outcome is uncertain. It is settled durably as
        uncertain, consumes its unit of `maximum_units` for good, and the controller folds it into the
        control's `uncertain_dispatches`; the retry is a new dispatch with the next ordinal. Nothing is
        reused (the ordinal is never sent again) and nothing is silently skipped (the settled ordinal is
        declared in the control and reconciled against the authenticated attempt inventory, which refuses
        the session if that run ever appears).
    """
    unsettled = settle_dispatch(reservations, dispatched)
    ordinal = record['ordinal']
    p.require(ordinal == len(dispatched), 'dispatch ordinal is not the next authenticated ordinal')
    if unsettled is None:
        reservations.reserve(record)
        return 'post', record
    held = reservations.read(ordinal)
    same = {key: value for key, value in held.items() if key != 'reserved_at'} == \
           {key: value for key, value in record.items() if key != 'reserved_at'}
    p.require(same, 'the reserved dispatch ordinal belongs to a different dispatch; it is not reused')
    found = runs(held)
    p.require(len(found) <= 1, 'ambiguous duplicate dispatch; no attempt splicing')
    if reservations.state(ordinal) == 'uncertain':
        p.require(not found, 'dispatch ordinal %d was settled as uncertain but its run appeared; no attempt splicing'
                  % ordinal)
        return 'uncertain', held
    if found:
        reservations.mark_sent(ordinal)
        return 'adopt', found[0]
    if reservations.state(ordinal) == 'reserved':
        return 'post', held
    age = (datetime.now(timezone.utc).timestamp() if now is None else now) - utc(held['reserved_at'])
    p.require(age >= UNCERTAIN_DISPATCH_SECONDS,
              'dispatch ordinal %d was sent but its run is not visible yet; the ordinal stays consumed '
              '(neither reused nor skipped) until the run appears or %d seconds pass' %
              (ordinal, UNCERTAIN_DISPATCH_SECONDS))
    reservations.mark_uncertain(ordinal)
    return 'uncertain', held


class UncertainDispatch(Exception):
    """A sent dispatch was settled as uncertain: re-derive the control and retry with the next ordinal."""


def matching_runs(api, expected, control, floor):
    """The workflow runs created by exactly this dispatch (same registration, inputs and control)."""
    matches = []
    for run in api.pages('/actions/runs', {'workflow_id': p.WORKFLOW}, field='workflow_runs'):
        if run.get('commit_sha') != control['registration']['source']['commit'] or not p.dispatched(run):
            continue
        inputs = p.decode(run.get('event_payload', '').encode()).get('inputs', {})
        if (inputs.get('registration_sha256') == expected['registration_sha256']
                and inputs.get('mode') == expected['mode'] and inputs.get('operation') == expected['operation']
                and p.decode(inputs.get('control', '{}').encode()) == control
                and utc(run['created']) >= utc(floor) - 1):
            matches.append(run)
    return matches


def plan_dispatch(api, state, control, root):
    """Everything a dispatch needs, checked, with nothing sent and nothing reserved."""
    reg = state['registration']
    registration(reg, root)
    live_pr(api, reg)
    p.require(state['phase'] not in TERMINAL_PHASES,
              'terminal/incomplete resource disposition cannot dispatch another same-plan unit')
    # Every dispatch, including a failed or unknown one, consumes the immutable allowance.
    p.require(state['next_dispatch_ordinal'] < state['resource_plan']['record']['maximum_units'],
              'dispatch ordinal budget exhausted')
    p.require(state.get('retry_authorized') is True,'unknown terminal failure cannot automatically dispatch a successor')
    kind = expected_dispatch(state)
    mode = 'complete' if kind == 'complete' else 'unit'
    operation = 'publish' if kind in ('publish', 'conformance') else 'execute'
    control = {**control, 'transport_conformance': (conformance_request(state, reg, control['session_id'])[0]
                                                    if kind == 'conformance' else None)}
    reject_prior_ordinal_dispatch(api,reg,control,None)
    inputs = {'mode': mode, 'operation': operation, 'registration_sha256': p.digest(p.canonical(reg)),
              'control': p.canonical(control).decode()}
    payload = p.canonical({'ref': reg['ref'], 'inputs': inputs})
    p.require(len(payload) <= p.SMALL, 'dispatch payload too large')
    return {'registration_sha256': inputs['registration_sha256'], 'mode': mode, 'operation': operation,
            'kind': kind, 'control': control, 'ordinal': state['next_dispatch_ordinal'], 'payload': payload,
            'reg': reg, 'expected_prior_checkpoint':
            p.digest(p.canonical(state['checkpoint'])) if state['checkpoint'] else None}


def post_dispatch(api, plan, root, reservations):
    """Send the reserved dispatch. The `sent` marker is durable before the first byte leaves."""
    path = '/actions/workflows/' + p.WORKFLOW + '/dispatches'
    request = urllib.request.Request(api.root + path, data=plan['payload'], method='POST',
              headers={'Authorization': 'token ' + api.token, 'Content-Type': 'application/json'})
    authenticate_runner_route(api, plan['reg'], root)
    reservations.mark_sent(plan['ordinal'])
    try:
        with api.opener.open(request, timeout=30) as response:
            p.require(response.status in (200, 201, 204), 'dispatch was not accepted')
    except urllib.error.HTTPError as error:
        if 400 <= error.code < 500:  # the server read and refused it: no run exists for this ordinal
            reservations.mark_refused(plan['ordinal'])
        raise


def dispatch(api, state, control, root, reservations):
    """One bounded registered successor. A failed dispatch never advances state.

    The ordinal is reserved durably before the API call and reconciled against the authenticated attempt
    inventory (`claim_dispatch`), so a restarted controller adopts, resends or refuses - never reuses or
    skips - an ordinal. Returns the dispatch description and, when the reservation's run already existed,
    that run (nothing was sent).
    """
    plan = plan_dispatch(api, state, control, root)
    record = dispatch_record(plan['ordinal'], control['session_id'], plan, plan['control'])
    action, held = claim_dispatch(
        reservations, record,
        lambda reserved: matching_runs(api, plan, plan['control'], reserved['reserved_at']),
        state['attempt_control_sha256'])
    if action == 'uncertain':
        raise UncertainDispatch(plan['ordinal'])
    adopted = held if action == 'adopt' else None
    reserved_at = reservations.read(plan['ordinal'])['reserved_at']
    if adopted is None:
        post_dispatch(api, plan, root, reservations)
    # API acceptance is not run authentication/capability; controller polls
    # separately and admits only the exact event/registration/task result.
    return {'registration_sha256': plan['registration_sha256'], 'mode': plan['mode'],
            'operation': plan['operation'], 'kind': plan['kind'], 'control': plan['control'],
            'ordinal': plan['ordinal'], 'expected_prior_checkpoint': plan['expected_prior_checkpoint'],
            'dispatched_at': reserved_at}, adopted



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
              and event_merge_base(root, event) == reg['event_base'], 'Main raw event/source differs')
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
                  and type(job.get('run_id')) is int and job['run_id'] == run_id
              and type(job.get('repo_id')) is int and job['repo_id'] == reg['repository'],
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
    p.require(native.terminal_status(job) == 'success' and type(job.get('attempt')) is int
              and job['attempt'] == 1 and type(job.get('task_id')) is int and job['task_id'] > 0
              and job.get('run_id') == run_id and job.get('repo_id') == reg['repository'],
              'current native job did not succeed with its initial task')
    adopted_name = 'native-adoption-' + str(run_id) + '-1'
    adoption_artifacts = [a for a in artifacts if a.get('name') == adopted_name]
    p.require(len(adoption_artifacts) == 1, 'native adoption provenance missing')
    directory = Path(api.scratch) / ('native-adoption-' + str(run_id))
    rows = authenticated_main_artifact(api, adoption_artifacts[0], lines, directory)
    p.require(len(rows) == 1 and rows[0]['path'] == 'receipt.json', 'native adoption member ambiguous')
    report = p.read(directory / rows[0]['path'])
    p.closed(report,('version','scope','current_commit','current_run','adoptions','compatibility','tv_cases'))
    p.require(type(report['version']) is int and report['version']==1,'native report version differs')
    p.require(report.get('scope') == scope and report.get('current_commit') == reg['source']['commit']
              and type(report.get('current_run')) is int and report.get('current_run') == run_id and type(report.get('adoptions')) is dict
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
                with archive.open('rb') as stream:
                    verified = native.current_tv_result(api, scope, reg['source']['commit'], run_id, report, stream, job)
            finally:archive.unlink(missing_ok=True)
            result['tv_navigation']={**verified,'commit':reg['source']['commit'],
                'run':run_id,'job':job['id'],'task':job['task_id'],'attempt':1,'artifact':aid,
                'archive_sha256':archive_sha}
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
    if metadata['operation'] in ('execute','publish'):preflight_carrier(Path(staging).parent,metadata)
    print(OUTPUT_MARKER + encoded.decode(), flush=True)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
        stream.write('part_count=' + str(len(metadata['parts'])) + '\n')
        for part in metadata['parts']:
            n = part['ordinal']
            stream.write(f'part_{n}_name={part["name"]}\n')
            stream.write(f'part_{n}_path={Path(staging).resolve() / "parts" / str(n)}\n')


_DOWNLOADS = {}  # extraction directory -> the PoolBudget holding its members (charged once per job)
_ARCHIVES = []   # at most one transient-ZIP budget per job: only one downloaded ZIP exists at a time


def reserve_download(directory, archive_bytes, members):
    """Charge a download to restore_staging_bytes BEFORE its first byte is written.

    Downloads and their extraction belong to restore_staging_bytes (accounting `downloads_and_restores`);
    the calibration inequality `restore_staging_bytes >= 2*total_retained_artifact_bytes + min(ARTIFACT_BYTES, total_retained_artifact_bytes) + 2*max(catalog,
    verifier database_bytes) + evidence allowance` is what makes the sum of every authenticated artifact fit.

    Two different things are charged, and neither cumulatively:

      * the extracted members, once per artifact per job, in a reservation that tracks the extraction
        subtree (they stay on disk and keep their charge until the subtree is removed). A directory that
        survived an earlier stage of this job (or an earlier job) is charged at its actual size and reserves
        nothing twice;
      * the transient ZIP, every time one is downloaded (a cached artifact is re-downloaded to be
        re-verified), in one job-wide reservation that grows to the largest ZIP held at any moment: it is
        reserved before the download writes and released when the ZIP is unlinked.

    `archive_bytes` is the size the API reported for the artifact (the download refuses any other size).
    Returns the transient charge actually made, which the caller hands back to release_download.
    """
    if not _PHYSICAL:
        return 0
    context = _PHYSICAL[0]
    if not _ARCHIVES:
        _ARCHIVES.append(PoolBudget(context, 'restore_staging_bytes'))
    key = str(Path(directory))
    if key not in _DOWNLOADS:
        present = Path(directory).exists()
        budget = PoolBudget(context, 'restore_staging_bytes', directory_bytes(directory) if present else 0)
        budget.track_subtree(directory)
        if not present:
            budget.reserve(sum(member['bytes'] for member in members))
        _DOWNLOADS[key] = budget
    _ARCHIVES[0].reserve(archive_bytes)
    return archive_bytes


def release_download(charged):
    """The ZIP was unlinked: the transient charge `reserve_download` returned is free for the next download."""
    if charged:
        _ARCHIVES[0].release(charged)


def fetch_part(api, artifact, descriptor, directory, lines):
    """Download one authenticated ZIP, verify it against the native upload transaction and extract it.

    Returns (zip_sha256, zip_bytes). The ZIP is reserved before it is written and removed before this
    returns, whatever happens in between.
    """
    charged, archive = 0, None
    try:
        charged = reserve_download(directory, artifact['size_in_bytes'], descriptor['members'])
        archive = p.artifact_bytes(api, artifact)
        sha, size = p.file_sha(archive), archive.stat().st_size
        p.require(sha == transport_hash(lines, artifact), 'native upload/API/local hash differs')
        p.unpack(archive, directory, descriptor['members'], storage_guard=api.guard_storage,
                 verify_existing=Path(directory).exists())
        return sha, size
    finally:
        if archive is not None:
            archive.unlink(missing_ok=True)
        release_download(charged)


def download_parts(api, reg, run_id, mode, destination, root):
    """Authenticate every native ZIP of one terminal producer; no semantic reading."""
    run, job, event = p.producer(api, run_id, reg, mode, ('success',))
    if event['inputs']['operation'] in ('execute','publish'):
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
        directory = Path(api.scratch).parent/'authenticated-parts'/str(artifact['id'])
        zip_sha256, zip_bytes = fetch_part(api, artifact, descriptor, directory, lines)
        parts[n] = directory
        total += zip_bytes + sum(v['bytes'] for v in descriptor['members'])
        # Build receipt after exact uploaded result/checkpoint is read.
        artifact_receipts[n] = {'artifact': artifact, 'zip_sha256': zip_sha256,
                                'zip_bytes': zip_bytes, 'members': descriptor['members']}
    return {'run': run, 'job': job, 'event': event, 'lines': lines, 'metadata': metadata,
            'parts': parts, 'receipts': artifact_receipts, 'total': total}


def authenticate_output(api, reg, run_id, mode, destination, root):
    got = download_parts(api, reg, run_id, mode, destination, root)
    run, job, event, lines = got['run'], got['job'], got['event'], got['lines']
    metadata, parts, artifact_receipts, total = got['metadata'], got['parts'], got['receipts'], got['total']
    checkpoint_digest = None
    result = None
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
        measured=result['measurement']
        p.require(rust['geometry'] in measured['resource_calibration']['geometry']['catalog_main']
                  and measured['resource_method']['id']=='typed_physical_caps_v1'
                  and 'resource_overheads' not in measured,
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


# ---------------------------------------------------------------------------
# Checkpoint-capacity publication controller. Pending publication is transport
# state outside semantic progress; only finish_publication yields the ordinary
# result, and `advance` runs once for it.
# ---------------------------------------------------------------------------

def database_grant(context, path):
    for grant in context.grants:
        if grant.path == Path(path):
            return grant
    return None


def open_database(family, path):
    """Physically granted connection on the proof host; plain on a read-only consumer.

    One file has exactly one grant for the whole process: reopening a path (a database
    copied or reused by a later stage of the same job) reconnects through its existing grant
    instead of reserving the same file a second time.
    """
    if _PHYSICAL:
        context = _PHYSICAL[0]
        grant = database_grant(context, path)
        if grant is None:
            grant = physical.reserve_database(context.ledger, family, path,
                                              context.policy['families'][family]['database_bytes'])
            context.grants.append(grant)  # retained: files survive until actually removed
        else:
            p.require(grant.family == family, 'database path already granted to another family')
            grant.connection = None
        return grant.connect()
    return sqlite3.connect(path)


def discard_database(path):
    """Remove a finished scratch database, then release its reservation (caller closed it)."""
    target = Path(path)
    for suffix in ('', *physical.DB_COMPANIONS):
        Path(str(target) + suffix).unlink(missing_ok=True)
    if _PHYSICAL:
        context = _PHYSICAL[0]
        grant = database_grant(context, target)
        if grant is not None:
            grant.connection = None
            grant.release_after_removal()
            context.grants.remove(grant)


def open_index(path):
    """A dependency/locator index on its declared authentication_index grant."""
    return p.DependencyIndex(Path(path), open_database('authentication_index', path))


class PoolBudget:
    """Incremental sub-accounting of one pool reservation the parent holds for its own copies.

    The reservation grows by exactly what is about to be written, so delegated child
    grants still see the rest of the pool. Releasing a copy lowers the live count but
    never the held deduction: that returns only after the owned file is removed.
    """
    def __init__(self, context, pool, existing=0):
        self.reservation = context.ledger.reserve(physical.POOL_OWNERS[pool], pool, 'bytes', 0)
        self.reservation_id = self.reservation.reservation_id
        self.pool, self.used, self.high = pool, 0, 0
        self.reserve(existing)  # files that survived an earlier job remain charged

    def track(self, path):
        """Name the one physical file these bytes hold (what an attachment or a transfer refers to)."""
        self.reservation.track(path)

    def track_subtree(self, path):
        """Name the owned directory whose files these bytes hold."""
        self.reservation.track_subtree(path)

    def reserve(self, size):
        if type(size) is not int or size < 0:
            raise physical.PhysicalIncomplete('invalid physical pool charge: ' + self.pool)
        updated = self.used + size
        self.reservation.grow(max(self.high, updated))
        self.used, self.high = updated, max(self.high, updated)

    def release(self, size):
        self.used -= size
        p.require(self.used >= 0, 'physical pool released below zero')


class NullBudget:
    """Read-only controller replay on a host without the proof storage ledger."""
    def reserve(self, size):
        p.require(type(size) is int and size >= 0, 'invalid replay charge')

    def release(self, size):
        p.require(type(size) is int and size >= 0, 'invalid replay release')


def replay_budget(pool='restore_staging_bytes'):
    return PoolBudget(_PHYSICAL[0], pool) if _PHYSICAL else NullBudget()


def owned_directory(path, reg=None, root=None):
    """Private owned directory capability; lease-checked on the proof host."""
    path = Path(path)
    if reg is not None:
        return snapshots.OwnedDirectory(path, Path(reg['storage']['root']),
                                        lambda: validate_storage(reg['storage'], root))
    return snapshots.OwnedDirectory(path, path.parent, lambda: None)


class PublicationStore:
    """Owned spool of one pending publication: CAS, ledger, evidence copies.

    Spool bytes are sender-side cache only. Coverage is credited from authenticated
    native batches during replay and overwritten into this ledger before sending.
    """
    def __init__(self, reg, root, name, *, create, evidence=None, resume=False):
        context = current_physical()
        self.reg, self.root_source = reg, root
        self.root = context.owned / name
        # `resume` reopens the spool a failed earlier attempt of the same dispatch kind left behind:
        # the spool is sender cache only (coverage is credited from authenticated batches), so a
        # retry must never depend on an operator deleting a scratch directory.
        self.root.mkdir(mode=0o700, exist_ok=resume or not create)
        p.require(self.root.is_dir() and not self.root.is_symlink() and self.root.resolve() == self.root,
                  'publication spool is not a private owned directory')
        for sub in ('cas', 'evidence'):
            (self.root / sub).mkdir(mode=0o700, exist_ok=True)
        self.cas = owned_directory(self.root / 'cas', reg, root)
        self.cas_budget = PoolBudget(context, 'cas_bytes', directory_bytes(self.root / 'cas'))
        self.inventory = snapshots.ContentInventory(
            open_database('publication_pack_scratch', self.root / 'inventory.sqlite'),
            self.cas, self.cas_budget.reserve, self.cas_budget.release)
        self.ledger = snapshots.PublicationLedger(
            open_database('publication_pending_scratch', self.root / 'ledger.sqlite'))
        self.evidence = None
        if create and (evidence is None or evidence):
            self.spool_budget = PoolBudget(context, 'snapshot_spool_bytes')
            self.evidence = snapshots.EvidencePlan(
                owned_directory(self.root / 'evidence', reg, root),
                open_database('publication_pack_scratch', self.root / 'evidence.sqlite'),
                self.spool_budget.reserve, self.spool_budget.release)

    def close(self):
        if self.evidence is not None:
            self.evidence.close()
        self.cas.close()


def current_origin(state, reg, run, job, operation):
    p.require(operation in ('execute', 'publish'), 'unknown publication operation')
    return {'repository': reg['repository'], 'pr': reg['pr'], 'event_base': reg['event_base'],
            'workflow_id': p.WORKFLOW, 'event_name': 'workflow_dispatch', 'mode': 'unit',
            'operation': operation, 'run': run, 'job': job['id'], 'task': job['task_id'], 'attempt': 1,
            'dispatch_ordinal': state['next_dispatch_ordinal']}


def publication_binding(reg, unit, initial_image_sha256):
    return {'source': reg['source'], 'session_id': unit['session_id'], 'unit_ordinal': unit['unit_ordinal'],
            'resource_plan_sha256': unit['resource_plan_sha256'], 'initial_image_sha256': initial_image_sha256}


def batch_digest(batch):
    """Identity under which a transfer batch's locators are stored (`batch_receipt_sha256`)."""
    return p.digest(p.canonical(batch))


def lineage_references(index, accepted):
    """Content credited by the batches of ACCEPTED predecessor publications only.

    `accepted` is the replay-derived set of batch-receipt digests of publications whose activation
    was accepted. Locators of a pending publication, of a discarded one and of rehearsal batches
    are never a seed: reusing them requires the rebuilt snapshot to bind identical roots, which
    the ordinary pending chain (not this seed) establishes batch by batch.
    """
    accepted = frozenset(accepted)
    for kind_name, sha, size, batch in index.db.execute(
            "SELECT DISTINCT substr(member_path,9,instr(substr(member_path,9),'/')-1),sha256,bytes,batch_receipt_sha256 "
            "FROM native_locators WHERE member_path LIKE 'content/%'"):
        if batch in accepted:
            yield {'kind': kind_name, 'sha256': sha, 'bytes': size}


def build_publication(store, state, reg, root, index, quantum, produced, checked, origin):
    """Seal snapshot roots, evidence tree and immutable intent; no native upload yet."""
    cp = produced['checkpoint']
    binding = publication_binding(reg, produced['work_unit'], cp['record']['initial_image_id'])
    quantum = Path(quantum)
    # The sealed standalone backups sit beside the output tree, not in it (see auxiliary_directory).
    qdir = owned_directory(auxiliary_directory(quantum), reg, root)
    try:
        catalog, catalog_ref = snapshots.snapshot_root(
            store.inventory, binding, origin, cp, p.read(quantum / 'backup-descriptor.json'), qdir,
            CATALOG_BACKUP, predecessor_root=state.get('catalog_root_ref'))
        p.auxiliary(catalog, root)
        p.write(quantum / 'catalog-backup/root.json', catalog)
        store.evidence.fulfill('catalog-backup/root.json', quantum / 'catalog-backup/root.json')
        verifier_ref = None
        if checked:
            verifier, verifier_ref = snapshots.snapshot_root(
                store.inventory, binding, origin, cp, p.read(quantum / 'verifier-backup-descriptor.json'), qdir,
                VERIFIER_BACKUP, predecessor_root=state.get('verifier_root_ref'), verifier=True)
            p.verifier_auxiliary(verifier, root)
            p.write(quantum / 'verifier-backup/root.json', verifier)
            store.evidence.fulfill('verifier-backup/root.json', quantum / 'verifier-backup/root.json')
        evidence_ref = snapshots.evidence_root(store.inventory, binding, store.evidence)
    finally:
        qdir.close()
    # Unchanged content is credited only from the exact accepted authenticated lineage.
    snapshots.seed_coverage(store.ledger, lineage_references(index, state['accepted_batches']))
    intent, pending = snapshots.prepare_publication(store.inventory, store.ledger, binding, origin, produced,
                                                    catalog_ref, verifier_ref, evidence_ref)
    p.auxiliary(intent, root)
    p.auxiliary(pending, root)
    return {'binding': binding, 'intent': intent, 'pending': pending, 'catalog_ref': catalog_ref,
            'verifier_ref': verifier_ref, 'evidence_ref': evidence_ref}


def plan_transfer(store, state, reg, output, pending, execution_origin, publication_origin, binding, *,
                  purpose, offer=None, extra=()):
    """Pack one bounded native batch and the pre-upload batch draft (no receipts yet).

    `extra` rows are (kind, bytes) genuine payload (activation validation receipts)
    first stored in the CAS and queued; they are offered like any required content.
    """
    context = current_physical()
    plan_record = state['resource_plan']['record']
    budget = PoolBudget(context, 'transfer_staging_bytes')
    plan = p.PartPlan(Path(output) / 'upload', limits={'owned_root': str(output),
        'disk_bytes': plan_record['total_session_disk_bytes'], 'unit_bytes': plan_record['unit_byte_allowance'],
        'deadline_monotonic': time.monotonic() + 1800},
        database=lambda path: open_database('publication_pack_scratch', path), budget=budget)
    try:
        offered = set() if offer is None else set(offer)
        for kind, data in extra:
            reference = store.inventory.put(kind, data)
            store.ledger.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)',
                                    (reference['kind'], reference['sha256'], reference['bytes']))
            offered.add((reference['kind'], reference['sha256']))
        store.ledger.db.commit()
        transfer = snapshots.native_batch(store.inventory, store.ledger, binding, plan,
                                          only=offered if (offer is not None or extra) else None)
        refs = []
        for name, size, sha in plan.db.execute(
                "SELECT path,bytes,digest FROM members WHERE path LIKE 'content/%' ORDER BY path"):
            _, kind, identity = name.split('/')
            refs.append({'kind': kind, 'sha256': identity, 'bytes': size})
        after_sha, after_count = snapshots.coverage_after(store.ledger, refs)
        batch = {**binding, 'schema_version': 1, 'kind': 'transfer_batch_receipt',
                 'intent_sha256': pending['intent']['sha256'], 'execution_origin': execution_origin,
                 'publication_origin': publication_origin, 'batch_ordinal': pending['next_batch_ordinal'],
                 'purpose': purpose, 'previous_batch': pending['previous_batch'],
                 'transfer_manifest': transfer, 'coverage_before_sha256': pending['coverage_sha256'],
                 'coverage_after_sha256': after_sha, 'coverage_count': after_count,
                 'artifact_receipts': [], 'retained_artifact_bytes': 0, 'acceptance': False,
                 'proof_checkpoint_advanced': False}
        return batch, plan, refs
    except BaseException:
        plan.close()
        raise


def write_publication_draft(output, role, state, control, unit, pending_before, batch, refs, extra=None):
    """Pre-upload facts the post-upload finalizer completes with native receipts."""
    kept = {key: state[key] for key in ('registration', 'resource_plan', 'checkpoint', 'control_sha256',
                                        'next_dispatch_ordinal', 'operational_usage') if key in state}
    p.write(Path(output) / 'finalizer-state.json', {'state': kept, 'control': control, 'unit': unit,
        'role': role, 'pending_before': pending_before, 'batch': batch, 'payload': refs,
        'verified': None, 'main_records': [], **(extra or {})})


EMPTY_SHA = hashlib.sha256(b'').hexdigest()


def native_receipts(reg, got, unit, produced):
    """ArtifactReceipts of one publisher's actual ZIPs.

    For transfer-context receipts `produced_checkpoint` is the candidate checkpoint
    named by the publication intent (not yet accepted); expected_prior_checkpoint is
    the unit's actual accepted predecessor. The publisher's own run/job/task is the
    receipt origin, never the original execution's.
    """
    result = {}
    for n, row in got['receipts'].items():
        artifact = row['artifact']
        value = {'artifact_id': artifact['id'], 'session_id': unit['session_id'],
                 'unit_ordinal': unit['unit_ordinal'], 'run': got['run']['id'], 'job': got['job']['id'],
                 'task': got['job']['task_id'], 'attempt': 1, 'source': reg['source'],
                 'name': artifact['name'], 'expired': False, 'zip_sha256': row['zip_sha256'],
                 'zip_bytes': row['zip_bytes'], 'extracted_bytes': sum(v['bytes'] for v in row['members']),
                 'member_count': len(row['members']), 'members': row['members'],
                 'upload_log_sha256': p.upload_slice(got['lines'], artifact['name'], artifact['id'],
                                                     artifact['size_in_bytes'])[0],
                 'event_payload_sha256': p.digest(p.canonical(got['event'])),
                 'expected_prior_checkpoint': unit['expected_prior_checkpoint'], 'produced_checkpoint': produced}
        result[n] = {'schema_version': 6, 'kind': 'ArtifactReceipt', 'record': value}
    return result


def intent_content(reference, parts, resolver):
    """The frozen intent: from this batch's own ZIP when it carries it, else lineage."""
    name = 'content/publication_intent/' + reference['sha256']
    for directory in parts.values():
        path = Path(directory) / name
        if path.is_file() and not path.is_symlink():
            data = path.read_bytes()
            p.require(snapshots.content_ref('publication_intent', data) == reference, 'intent bytes differ')
            return data
    return resolver(reference)


class ReplayLedger:
    """Receiver-side authenticated coverage and closure for one pending publication."""
    def __init__(self, path):
        self.path = Path(path)
        self.db = open_database('publication_pending_scratch', self.path)
        self.ledger = snapshots.PublicationLedger(self.db)

    def close(self):
        self.db.close()

    def discard(self):
        """The publication is finished: its receiver ledger has no further value."""
        self.close()
        discard_database(self.path)


def authenticate_publication(api, reg, run_id, root, work, state, index, *, case_sha256=None):
    """One terminal execute/publish producer: native chain, carrier, credited rows."""
    got = download_parts(api, reg, run_id, 'unit', work, root)
    metadata, parts, lines = got['metadata'], got['parts'], got['lines']
    p.require(metadata['operation'] in ('execute', 'publish')
              and got['event']['inputs']['operation'] == metadata['operation'], 'publication operation differs')
    carrier, control_sha, begin, _ = p.parse_control_frame(lines)
    validate_control_identity(carrier, reg, metadata, run_id, got['job'], root)
    selected = [got['receipts'][n]['artifact'] for n in sorted(got['receipts'])]
    p.validate_upload_order(lines, selected, begin)
    resolver = snapshots.AuthenticatedResolver(index.db, Path(api.scratch).parent / 'authenticated-parts')
    pending_ref = (carrier['pending_publication']['intent'] if carrier['pending_publication'] is not None
                   else state['pending_publication']['pending']['intent'])
    intent = p.decode(intent_content(pending_ref, parts, resolver))
    unit = p.envelope(carrier['work_unit'], 'WorkUnit', root)
    receipts = native_receipts(reg, got, unit, intent['candidate_checkpoint']['sha256'])
    p.require(carrier['artifact_receipts'] == [receipts[n] for n in sorted(receipts)],
              'carrier receipts differ from reconstructed native receipts')
    rows = p.authenticate_transfer_context(api, reg, carrier, metadata, parts, receipts, root,
                                           state['next_dispatch_ordinal'], case_sha256=case_sha256)
    return {'carrier': carrier, 'control_sha256': control_sha, 'rows': rows, 'intent': intent,
            'unit': unit, 'receipts': receipts, 'got': got, 'resolver': resolver}


def credit_rows(index, ledger, resolver, unit_ordinal, pending_before, batch, rows):
    """Commit exact closure-relative coverage first; only a batch that advances gains locators.

    The batch's own authenticated bytes resolve its structured members while the closure is
    recomputed (a parent and its children may travel together); anything else resolves through
    the lineage. A refused batch leaves neither coverage nor locators behind.
    """
    local = {(reference['kind'], reference['sha256']): path for reference, path, _locator in rows}

    def resolve(reference):
        path = local.get((reference['kind'], reference['sha256']))
        if path is None:
            return resolver(reference)
        p.require(0 < reference['bytes'] <= 8 * snapshots.CHUNK_BYTES, 'batch content length is not admitted')
        data = Path(path).read_bytes()
        p.require(snapshots.content_ref(reference['kind'], data) == reference, 'batch content differs from its ContentRef')
        return data
    after = snapshots.resume_publication(ledger, pending_before, batch, [row[0] for row in rows], resolve)
    for _reference, _path, locator in rows:
        index.add_locator(unit_ordinal, locator)
    snapshots.expand_with_resolver(ledger, resolver)
    return after


def begin_pending(state, reg, root, work, authenticated, index, execution_run_item):
    """Create receiver state from an authenticated original-execution carrier."""
    carrier, batch = authenticated['carrier'], authenticated['carrier']['transfer_batch']
    p.require(carrier['role'] == 'publication_batch' and batch['batch_ordinal'] == 0
              and batch['previous_batch'] is None and batch['purpose'] == 'transfer'
              and batch['execution_origin'] == batch['publication_origin']
              and batch['execution_origin']['operation'] == 'execute', 'first publication batch differs')
    ledger_path = Path(work) / ('pending-' + str(batch['execution_origin']['dispatch_ordinal']) + '.sqlite')
    replay = ReplayLedger(ledger_path)
    try:
        seed = list(lineage_references(index, state['accepted_batches']))
        snapshots.seed_coverage(replay.ledger, seed)
        sha, count = replay.ledger.coverage()
        intent_ref = carrier['pending_publication']['intent']
        before = {'schema_version': 1, 'kind': 'pending_publication', 'intent': intent_ref,
                  'previous_batch': None, 'coverage_sha256': sha, 'coverage_count': count, 'next_batch_ordinal': 0}
        replay.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)',
                          (intent_ref['kind'], intent_ref['sha256'], intent_ref['bytes']))
        replay.db.commit()
        after = credit_rows(index, replay.ledger, authenticated['resolver'], authenticated['unit']['unit_ordinal'],
                            before, batch, authenticated['rows'])
        p.require(after == carrier['pending_publication'], 'carrier pending publication differs from replay')
    finally:
        replay.close()
    original = [authenticated['receipts'][n] for n in sorted(authenticated['receipts'])]
    check_original_roots(carrier['work_unit'], original)
    return {'pending': after, 'ledger_path': str(ledger_path), 'unit': carrier['work_unit'],
            'execution_origin': batch['execution_origin'],
            'original_receipts': original,
            'bytes': batch['retained_artifact_bytes'], 'carriers': [authenticated['control_sha256']],
            'intent': intent_ref, 'batch_receipts': [batch_digest(batch)]}


def check_original_roots(carrier_unit, original_receipts):
    expected = [p.ref(r, r['record']['artifact_id']) for r in original_receipts]
    p.require(carrier_unit['record']['artifact_roots'] == expected,
              'carrier WorkUnit roots are not the exact direct original-execution receipts')


def reconstruct_evidence(intent, resolver, binding, destination, reg, root, budget):
    """Rebuild every logical evidence member into one local resolver directory."""
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=False, mode=0o700)
    for entry in snapshots.iter_keyed_manifest(intent['evidence_root'], resolver, binding,
                                               'evidence_index', 'evidence_manifest_node'):
        member = entry['member']
        name = p.safe_path(member['member_path'])
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if entry['chunks'] is None:
            p.require(member['bytes'] == 0 and member['sha256'] == EMPTY_SHA, 'zero-length evidence identity differs')
            fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            os.close(fd)
            continue
        directory = owned_directory(target.parent, reg if _PHYSICAL else None, root)
        try:
            snapshots.restore_tree(entry['chunks'], (member['bytes'] + snapshots.CHUNK_BYTES - 1) // snapshots.CHUNK_BYTES,
                member['bytes'], member['sha256'], snapshots.scope_digest(binding, 'evidence/' + name),
                resolver, directory, target.name, budget.reserve, budget.release)
        finally:
            directory.close()
    return destination


def original_control(pend):
    """Resolver view of the direct original execution: its WorkUnit and exact receipts only."""
    unit = pend['unit']['record']
    return {'session_id': unit['session_id'], 'unit_ordinal': unit['unit_ordinal'], 'work_unit': pend['unit'],
            'artifact_receipts': pend['original_receipts'], 'completion': None}


def accepted_transport(intent, resolver, binding, evidence_dir, pend, retained):
    """Dict consumed by the historical-member machinery (single logical evidence directory)."""
    control = original_control(pend)
    return {'parts': {0: evidence_dir}, 'receipts': {}, 'bytes': retained, 'control': control,
            'control_sha256': p.digest(p.canonical(control)), 'evidence_directory': evidence_dir,
            'logical_members': lambda: (entry['member'] for entry in snapshots.iter_keyed_manifest(
                intent['evidence_root'], resolver, binding, 'evidence_index', 'evidence_manifest_node'))}


def expected_validations(checkpoint, catalog_root, input_preparation):
    """Exact restored-catalog correspondence receipts the validators must have emitted."""
    cp = checkpoint['record']
    shared = {'catalog_sha256': catalog_root['catalog_sha256'], 'checkpoint_sha256': p.digest(p.canonical(checkpoint)),
              'source': cp['source'], 'session_id': cp['session_id'], 'catalog_commit': cp['committed_transaction'],
              'catalog_logical_digest': cp['catalog_logical_digest'], 'counts': cp['counts'], 'contexts': cp['contexts']}
    private = {'schema_version': 6, 'operation': 'validate_restore', **shared,
               'catalog_bytes': catalog_root['catalog_bytes'], 'semantic_manifest_root': cp['page_root'],
               'initial_image_sha256': cp['initial_image_id'], 'resource_plan_sha256': cp['resource_plan_sha256']}
    semantic = {'schema_version': 6, **shared, 'manifest_root': cp['page_root'], 'complete': True,
                'acceptance': False, 'scope': FINAL_INPUT_SCOPE}
    if input_preparation:
        semantic.update(complete=False, scope='input_preparation')
    return private, semantic


VALIDATORS = ('catalog_private', 'catalog_semantic', 'verifier')


def validation_receipt(validator, receipt, root):
    """The one transported record for a validator's raw output (closed `ValidationReceipt`).

    Every producer builds the record here and every consumer reads it through `unwrap_validation`, so
    the bytes the session authenticates are exactly the bytes the pinned transport schema declares.
    """
    p.require(validator in VALIDATORS and type(receipt) is dict, 'validation receipt names no validator')
    return p.auxiliary({'schema_version': 1, 'kind': 'validation_receipt', 'validator': validator,
                        'receipt': receipt}, root)


def unwrap_validation(data, validator, root):
    """Decode one authenticated validation receipt record and return the validator's raw output."""
    record = p.auxiliary(data, root)
    p.require(record['validator'] == validator, 'validation receipt belongs to a different validator')
    return record['receipt']


def check_activation_validations(result, intent, validations, resolver, ledger, index_path, root):
    """Recompute every activation validation receipt from authenticated roots."""
    catalog_root = p.decode(resolver(intent['catalog_root']))
    private, semantic = expected_validations(result['checkpoint'], catalog_root, False)
    _, partial = expected_validations(result['checkpoint'], catalog_root, True)
    for key in ('catalog_private', 'catalog_semantic', 'verifier', 'final_input'):
        reference = validations[key]
        if reference is None:
            continue
        p.require(reference['kind'] == 'validation_receipt' and ledger.db.execute(
            'SELECT 1 FROM coverage WHERE kind=? AND sha256=? AND bytes=?',
            (reference['kind'], reference['sha256'], reference['bytes'])).fetchone() is not None,
            'validation receipt is not authenticated activation payload')
    got_private = unwrap_validation(p.decode(resolver(validations['catalog_private'])), 'catalog_private', root)
    got_semantic = unwrap_validation(p.decode(resolver(validations['catalog_semantic'])), 'catalog_semantic', root)
    p.require(got_private == private and got_semantic in (semantic, partial),
              'activation catalog validation differs from authenticated root')
    if validations['final_input'] is not None:
        p.require(validations['final_input'] == validations['catalog_semantic'] and got_semantic == semantic,
                  'final-input validation is not the exact complete correspondence')
    verifier_root = intent['verifier_root']
    p.require((verifier_root is None) == (validations['verifier'] is None), 'verifier validation presence differs')
    if verifier_root is not None:
        backup = p.decode(resolver(verifier_root))
        receipt = unwrap_validation(p.decode(resolver(validations['verifier'])), 'verifier', root)
        expected = {'schema_version': 1, 'operation': 'validate_restored_verifier', 'source': backup['source'],
            'session_id': backup['session_id'], 'unit_ordinal': backup['unit_ordinal'],
            'checkpoint_sha256': backup['checkpoint_sha256'], 'resource_plan_sha256': backup['resource_plan_sha256'],
            'initial_image_sha256': backup['initial_image_sha256'],
            'verifier_schema_sha256': backup['verifier_schema_sha256'], 'verifier_commit': backup['verifier_commit'],
            'verifier_logical_digest': backup['verifier_logical_digest'], 'work_db_sha256': backup['work_db_sha256'],
            'work_db_bytes': backup['work_db_bytes'], 'progress_sha256': backup['progress_sha256'],
            'result_chain_root_sha256': verifier_chain_digest(index_path, backup['unit_ordinal']),
            'complete': True, 'acceptance': False}
        p.require(receipt == expected, 'activation verifier/result-chain validation differs')


def check_root_origin(record, origin):
    """A snapshot root was sealed by exactly the execution its publication names."""
    p.require(record['execution_origin'] == origin, 'snapshot root execution origin differs from its publication')


def restore_generation(output):
    """The directory holding one restore's staged files (the generation activate_generation moves)."""
    return Path(output) / 'state.restore'


def catalog_restore_request(request, staged, verifier_staged, checkpoint, index_path, output):
    """The closed request the catalog restore validators read (Rust QuantumRequest.validate_restore and
    the checker): `request` is restore_request_base, which lacks the per-restore paths added here.

    The catalog validator reads the restored verifier's input scopes and result credits, so
    `verifier_work_db` is the STAGED verifier file the same restore produced. Nothing creates a file at any
    other name: the verifier is moved into the active generation only after both validations passed. A
    checkpoint with no verifier snapshot yet has no staged file; the request then names the name the
    generation would give it (see E1-NEEDS-FROM-E2: the checker has no "no verifier yet" form).

    tests/operations/test_paged_request_fixtures.py pins this builder to the checked-in fixture that the
    Rust catalog parses with QuantumRequest::read_shape.
    """
    verifier = restore_generation(output) / 'verifier.sqlite' if verifier_staged is None else verifier_staged
    return {**request, 'operation': 'validate_restore', 'catalog_path': str(staged),
            'verifier_work_db': str(verifier),
            'checkpoint': checkpoint, 'dependency_index': str(index_path),
            'output_dir': str(Path(output) / 'restore-validation')}


def verifier_restore_request(request, verifier_staged, checkpoint, index_path, output):
    """The closed request of the restored-verifier validator (the checker only; never Rust)."""
    return {**request, 'operation': 'validate_restore', 'verifier_work_db': str(verifier_staged),
            'verifier_checkpoint': checkpoint, 'verifier_backup_root': str(Path(output) / 'restored-verifier-root.json'),
            'output_dir': str(Path(output) / 'verifier-restore-validation'), 'dependency_index': str(index_path)}


def restore_snapshots(state, source, request, output, reg, root, index_path, resolver, catalog_ref,
                      verifier_ref, checkpoint, origin):
    """Reconstruct both complete files from authenticated content and validate them.

    Used by the accepted-checkpoint restore at job start and by activation. Each restored file
    occupies its own held restore_staging reservation; read-only validators attach it and reserve
    only their own temp/journal/scratch. Nothing becomes writer-visible here: activate_generation
    moves the validated files. Returns the validation receipts (raw dicts) for activation evidence.
    """
    context = current_physical()
    output = Path(output)
    generation = restore_generation(output)
    generation.mkdir(exist_ok=False, mode=0o700)
    directory = owned_directory(generation, reg, root)

    def stage(root_record, name):
        budget = PoolBudget(context, 'restore_staging_bytes')
        budget.track(generation / name)  # owned before the first byte is written
        path = snapshots.restore_image(root_record, resolver, directory, name, budget.reserve, budget.release)
        return budget, path
    try:
        catalog_root = p.auxiliary(p.decode(resolver(catalog_ref)), root)
        check_root_origin(catalog_root, origin)
        catalog_budget, staged = stage(catalog_root, 'catalog.staged.sqlite')
        p.write(output / 'restored-catalog-root.json', catalog_root)
        verifier_root = verifier_budget = verifier_staged = None
        if verifier_ref is not None:
            verifier_root = p.verifier_auxiliary(p.decode(resolver(verifier_ref)), root)
            check_root_origin(verifier_root, origin)
            verifier_budget, verifier_staged = stage(verifier_root, 'verifier.staged.sqlite')
    finally:
        directory.close()
    request['catalog_path'] = str(staged)
    restore = catalog_restore_request(request, staged, verifier_staged, checkpoint, index_path, output)
    Path(restore['output_dir']).mkdir(mode=0o700)
    # The catalog validator opens both restored files read-only: each is attached by its own held reservation.
    held = [(catalog_budget, staged)] + ([] if verifier_staged is None else [(verifier_budget, verifier_staged)])
    context.attach_restore(restore['output_dir'], held)
    path = output / 'restore-request.json'
    p.write(path, restore)
    for argv in ([sys.executable, str(Path(source) / 'scripts/check-async-driver-refinement'), 'paged-unit', '--request', str(path)],
                 [sys.executable, str(Path(source) / 'verification/async-driver-refinement/paged_checker.py'), 'validate-restore', '--request', str(path)]):
        run_quantum(path, reg, source, Path(restore['output_dir']), argv)
    private = p.read(Path(restore['output_dir']) / 'restore-validation.json')
    semantic = p.read(Path(restore['output_dir']) / 'restore-semantic.json')
    input_preparation = semantic.get('complete') is False and semantic.get('scope') == 'input_preparation'
    verifier_receipt = None
    if verifier_staged is not None:
        vrequest = verifier_restore_request(request, verifier_staged, checkpoint, index_path, output)
        p.write(output / 'restored-verifier-root.json', verifier_root)
        Path(vrequest['output_dir']).mkdir(mode=0o700)
        context.attach_restore(vrequest['output_dir'], [(verifier_budget, verifier_staged)])
        vpath = output / 'verifier-restore-request.json'
        p.write(vpath, vrequest)
        run_quantum(vpath, reg, source, Path(vrequest['output_dir']),
                    [sys.executable, str(Path(source) / 'verification/async-driver-refinement/paged_checker.py'),
                     'validate-restored-verifier', '--request', str(vpath)])
        verifier_receipt = p.read(Path(vrequest['output_dir']) / 'verifier-restore-validation.json')
    # Both complete validations passed; only now may either file be moved by activate_generation.
    p.check_restored_catalog(staged, private, semantic, checkpoint, backup_root=catalog_root,
                             input_preparation=input_preparation)
    staged_files = [{'budget': catalog_budget, 'path': staged, 'family': 'catalog_main', 'owner': 'catalog',
                     'name': 'catalog.sqlite'}]
    if verifier_staged is not None:
        p.require(p.file_sha(verifier_staged) == verifier_root['work_db_sha256']
                  and verifier_staged.stat().st_size == verifier_root['work_db_bytes'],
                  'verifier staged bytes differ from authenticated root')
        staged_files.append({'budget': verifier_budget, 'path': verifier_staged, 'family': 'verifier_main',
                             'owner': 'verifier', 'name': 'verifier.sqlite'})
    p.write(output / 'restored-input-readiness.json', {'semantic_ready': not input_preparation})
    return {'catalog_private': private, 'catalog_semantic': semantic, 'verifier': verifier_receipt,
            'input_preparation': input_preparation, 'generation': generation, 'staged': staged_files}


def fsync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def activate_generation(restored, output, request):
    """Move the validated staged files into the active generation, transferring each reservation.

    For each file, in the RESULT-GRANT ABI order: reserve the destination main capacity while the
    staged source stays charged (prepare_transfer), rename, fsync the directories, then commit
    exactly one ownership. Failing to secure destination capacity, or any rename failure, leaves
    the staged generation unactivated and the prior root authoritative: renames are undone, the
    destination claims aborted and the empty active directory removed.
    """
    ledger = current_physical().ledger
    active = Path(output) / 'state'
    p.require(not active.exists() and not active.is_symlink(), 'refuse replace an active catalog/verifier generation')
    active.mkdir(mode=0o700)
    prepared, moved = [], []
    try:
        for entry in restored['staged']:
            prepared.append((entry, ledger.prepare_transfer(entry['budget'].reservation_id, entry['owner'],
                                                            entry['family'], 'main', active / entry['name'])))
        for entry, handle in prepared:
            os.replace(entry['path'], active / entry['name'])
            moved.append(entry)
        for directory in (active, restored['generation']):
            fsync_directory(directory)
    except BaseException:
        for entry in reversed(moved):
            os.replace(active / entry['name'], entry['path'])
        if moved:
            fsync_directory(restored['generation'])
        for _entry, handle in reversed(prepared):
            ledger.abort_transfer(handle)
        active.rmdir()
        raise
    for entry, handle in prepared:
        # The identity the filesystem reports for the file now at its active name, never the prepared one.
        observed = os.lstat(active / entry['name'])
        ledger.commit_transfer_after_rename_fsync(handle, (observed.st_dev, observed.st_ino))
    restored['generation'].rmdir()
    fsync_directory(active.parent)
    request['catalog_path'] = str(active / 'catalog.sqlite')
    request['verifier_work_db'] = str(active / 'verifier.sqlite')


def restore_latest(state, latest, source, request, output, reg, root):
    """Restore the last accepted checkpoint's complete snapshots from the authenticated lineage."""
    result, _transport = latest
    index_path = Path(output) / 'dependencies.sqlite'
    shutil.copyfile(state['index_path'], index_path)
    index = open_index(index_path)
    try:
        resolver = snapshots.AuthenticatedResolver(index.db, Path(state['parts_root']))
        restored = restore_snapshots(state, source, request, output, reg, root, index_path, resolver,
                                     state['catalog_root_ref'], state.get('verifier_root_ref'),
                                     state['checkpoint'], state['execution_origin'])
        verifier_digest = restored['verifier']
        if state.get('verifier_root_ref') is None:
            p.require(state.get('verifier_progress') is None, 'authenticated verifier state disappeared')
        else:
            p.require(verifier_digest is not None, 'verifier validation absent')
    finally:
        index.close()
    activate_generation(restored, output, request)
    return index_path


def seed_closure_queue(ledger, resolver, intent_ref):
    """Queue the complete closure of an immutable intent from authenticated lineage bytes.

    Reads only: nothing is credited. The queue is the set of identities a transfer batch may
    legitimately cover; coverage still comes solely from authenticated batches.
    """
    stack, seen = [intent_ref], set()
    while stack:
        reference = stack.pop()
        key = (reference['kind'], reference['sha256'])
        if key in seen:
            continue
        seen.add(key)
        ledger.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)',
                          (reference['kind'], reference['sha256'], reference['bytes']))
        if reference['kind'] in snapshots.STRUCTURED_KINDS:
            stack.extend(snapshots.child_refs(reference['kind'], resolver(reference)))
    ledger.db.commit()


def fresh_conformance_pending(anchor, ledger_path, lineage_resolver):
    """Empty rehearsal receiver over the anchor's immutable intent; zero historical credit.

    The rehearsal forces its first two batches to carry sole snapshot chunks whose parents are
    not (and, with zero credit, cannot be) covered. The receiver therefore starts from the anchor's
    complete closure as its required queue, read from the accepted lineage; a transfer batch is
    still credited only for queued identities.
    """
    replay = ReplayLedger(ledger_path)
    try:
        sha, count = replay.ledger.coverage()
        intent_ref = anchor['intent']
        seed_closure_queue(replay.ledger, lineage_resolver, intent_ref)
    finally:
        replay.close()
    return {'pending': {'schema_version': 1, 'kind': 'pending_publication', 'intent': intent_ref,
                        'previous_batch': None, 'coverage_sha256': sha, 'coverage_count': count,
                        'next_batch_ordinal': 0},
            'ledger_path': str(ledger_path), 'unit': anchor['unit'], 'execution_origin': anchor['execution_origin'],
            'original_receipts': anchor['original_receipts'], 'bytes': 0, 'carriers': [], 'intent': intent_ref,
            'batches': []}


def seed_sender_queue(store, resolver, intent_ref):
    """Sender-side closure queue from authenticated lineage bytes (reads only, no credit)."""
    seed_closure_queue(store.ledger, resolver, intent_ref)


def stage_sender_content(store, resolver, offer):
    """Copy exactly the offered (or every uncovered) genuine bytes into the sender CAS."""
    rows = store.ledger.db.execute(
        'SELECT q.kind,q.sha256,q.bytes FROM queue q LEFT JOIN coverage c ON c.kind=q.kind AND c.sha256=q.sha256 '
        'WHERE c.sha256 IS NULL ORDER BY q.kind,q.sha256').fetchall()
    for kind, sha, size in rows:
        if offer is not None and (kind, sha) not in offer:
            continue
        reference = {'kind': kind, 'sha256': sha, 'bytes': size}
        data = resolver(reference)
        put = store.inventory.put(kind, data)
        p.require(put == reference, 'sender content differs from its queued identity')


def run_publish(api, reg, state, latest, control, root, source, output, job, run_id, identity, measurement, deadline):
    """A bounded publish dispatch: one transfer batch or the separate activation.

    It executes no proof operation. The receiver-authenticated coverage replaces any
    sender-side cache claim before content is offered, so lost or stale spool bytes
    can only fail the batch, never credit it.
    """
    conformance = control['transport_conformance']
    first_rehearsal = conformance is not None and state['conformance_pending'] is None
    if conformance is None:
        pend = state['pending_publication']
        p.require(pend is not None, 'no pending publication to publish')
        spool = 'publication-' + str(pend['execution_origin']['dispatch_ordinal'])
    else:
        case = snapshots.conformance_case_sha256(p.digest(p.canonical(reg)), control['session_id'],
                                                 conformance['anchor_intent_sha256'])
        p.require(state['anchor'] is not None and state['anchor']['sha256'] == conformance['anchor_intent_sha256'],
                  'rehearsal anchor is not the first authenticated snapshot pair')
        spool = 'conformance-' + case[:32]
        pend = state['conformance_pending']
    # The replay's own indexes are job-local and nothing reads them again after this dispatch, so the
    # dispatch works on them directly: a second working copy would hold a further authentication_index
    # database for the whole job (the map declares the true simultaneous count).
    index_path = Path(state['index_path'])
    index = open_index(index_path)
    case_index = None
    store = None
    try:
        parts_root = Path(state['parts_root'])
        resolver = snapshots.AuthenticatedResolver(index.db, parts_root)  # sender reads lineage bytes
        restore_resolver = resolver
        if first_rehearsal:
            pend = fresh_conformance_pending(state['anchor'], Path(output) / ('conformance-' + case[:32] + '.sqlite'),
                                             resolver)
        if conformance is not None and not first_rehearsal:
            case_index = open_index(state['conformance_index_path'])
            restore_resolver = snapshots.AuthenticatedResolver(case_index.db, Path(state['conformance_parts_root']))
        store = PublicationStore(reg, root, spool, create=first_rehearsal, evidence=False, resume=first_rehearsal)
        receiver = ReplayLedger(pend['ledger_path'])
        try:
            store.ledger.db.execute('DELETE FROM coverage')
            for row in receiver.db.execute('SELECT kind,sha256,bytes FROM coverage'):
                store.ledger.db.execute('INSERT INTO coverage VALUES(?,?,?)', row)
            store.ledger.db.commit()
            intent = p.decode(resolver(pend['pending']['intent']))
            binding = {key: intent[key] for key in ('source', 'session_id', 'unit_ordinal',
                                                    'resource_plan_sha256', 'initial_image_sha256')}
            origin = current_origin(state, reg, run_id, job, 'publish')
            complete = snapshots.closure_complete(receiver.ledger)
            extra = None
            offer = None
            if not complete:
                if conformance is not None:
                    if first_rehearsal:
                        seed_sender_queue(store, resolver, pend['pending']['intent'])
                    chunks = snapshots.conformance_chunks(p.decode(resolver(intent['catalog_root'])),
                        p.decode(resolver(intent['verifier_root'])), resolver)
                    offer = snapshots.conformance_offer(pend['pending']['next_batch_ordinal'], chunks)
                    stage_sender_content(store, resolver, offer)
                batch, plan, refs = plan_transfer(store, state, reg, output, pend['pending'],
                    pend['execution_origin'], origin, binding, purpose='transfer', offer=offer)
                payload = refs
            else:
                batch, plan, payload, extra = prepare_activation(store, state, reg, root, source, output, pend, intent,
                                                                 binding, origin, index, index_path, restore_resolver,
                                                                 conformance)
        finally:
            receiver.close()
        role = ('transport_conformance' if conformance is not None else
                'unit_checkpoint' if extra is not None else 'publication_batch')
        write_publication_draft(output, role, state, control, pend['unit'], pend['pending'], batch, payload, {
            'activation': extra if conformance is None else None,
            'conformance': None if conformance is None else {
                'case_id': snapshots.CONFORMANCE_CASE, 'case_sha256': case,
                'anchor_intent_sha256': conformance['anchor_intent_sha256'], 'result': extra},
            'candidate_checkpoint_sha256': intent['candidate_checkpoint']['sha256'],
            'candidate_checkpoint': intent['candidate_checkpoint']})
        try:
            # Refused before any byte is offered: the receiver refuses the same cumulative extracted total.
            require_upload_fits_retention(state, upload_member_bytes(plan))
            upload_outputs(output_metadata(reg, control['session_id'], binding['unit_ordinal'], plan,
                                           'transfer/node-0.json', 'publish'), Path(output) / 'upload')
        finally:
            plan.close()
    finally:
        index.close()
        if case_index is not None:
            case_index.close()
        if store is not None:
            store.close()
    p.require(time.monotonic() <= deadline, 'publication dispatch exceeded admitted time')
    live_pr(api, reg)
    p.producer(api, run_id, reg, 'unit', ('running', 'in_progress'))


def restore_request_base(state, reg, work_unit, execution_origin, initial_image, checkpoint, index_path,
                         driver_graph, output):
    """The closed private request shape every restore validation shares with the unit runner.

    `work_unit` is the frozen WorkUnit record with empty `artifact_roots` (the direct original
    receipts belong to the accepted result and carrier, never to a private request) and
    `execution_origin` is the unit's original execute origin. restore_snapshots adds the staged
    catalog/verifier paths and the per-validator output directory.
    """
    return {'schema_version': 6, 'operation': 'validate_restore', 'work_unit': work_unit,
            'resource_plan': state['resource_plan'], 'initial_image': initial_image,
            'prior_checkpoint': state['checkpoint'], 'checkpoint': checkpoint,
            'source_files': reg['source_files'], 'runtime': reg['runtime'],
            'deadline_monotonic': time.monotonic() + 1800, 'dependency_index': str(index_path),
            'driver_graph': str(driver_graph), 'execution_origin': execution_origin,
            'prepare_path': str(Path(output) / 'restore-prepare.json'),
            'reservation_path': str(Path(output) / 'restore-reservation.json'),
            'previous_verifier_progress_sha256': state.get('verifier_progress_sha256'),
            'previous_verifier_backup_sha256': state.get('verifier_backup_sha256'),
            'tlc': {'java': reg['runtime']['java']['path'], 'jar': reg['runtime']['tlc']['path'],
                    'java_sha256': reg['runtime']['java']['sha256'], 'jar_sha256': reg['runtime']['tlc']['sha256']}}


def prepare_activation(store, state, reg, root, source, output, pend, intent, binding, origin, index, index_path,
                       resolver, conformance):
    """Separate activation dispatch: restore, validate, then upload bounded evidence.

    Activation restores from the authenticated bytes of the publication's own native
    uploads (a rehearsal: this case's own uploads only), never from sender spool.
    """
    planned = p.decode(resolver(intent['execution_result']))
    accepted = snapshots.accepted_result(planned, pend['execution_origin'], pend['original_receipts'], p.ref)
    budget = replay_budget()
    evidence_dir = reconstruct_evidence(intent, resolver, binding, Path(output) / 'evidence', reg, root, budget)
    transport = accepted_transport(intent, resolver, binding, evidence_dir, pend, 0)
    authenticated_roots(transport['parts'], index, root)
    add_authenticated_members(index, accepted, transport, root)
    activation_output = Path(output) / 'activation'
    activation_output.mkdir(mode=0o700, exist_ok=False)
    request = restore_request_base(state, reg, planned['planned_work_unit'], pend['execution_origin'],
                                   accepted['initial_image'], accepted['checkpoint'], index_path,
                                   Path(evidence_dir) / 'driver-graph.json', activation_output)
    restored = restore_snapshots(state, source, request, activation_output, reg, root, index_path,
                                 resolver, intent['catalog_root'], intent['verifier_root'],
                                 accepted['checkpoint'], pend['execution_origin'])
    records = {key: p.canonical(validation_receipt(key, restored[key], root))
               for key in VALIDATORS if restored[key] is not None}
    extra = [('validation_receipt', records[key]) for key in VALIDATORS if key in records]
    refs = {key: snapshots.content_ref('validation_receipt', records[key]) if key in records else None
            for key in VALIDATORS}
    refs['final_input'] = refs['catalog_semantic'] if accepted['work_unit']['stage'] == 'complete' else None
    batch, plan, payload = plan_transfer(store, state, reg, output, pend['pending'], pend['execution_origin'],
                                         origin, binding, purpose='activate', extra=extra)
    if conformance is not None:
        catalog = p.decode(resolver(intent['catalog_root']))
        verifier = p.decode(resolver(intent['verifier_root']))
        case = snapshots.conformance_case_sha256(p.digest(p.canonical(reg)), binding['session_id'],
                                                 conformance['anchor_intent_sha256'])
        result = {'schema_version': 1, 'kind': 'native_transport_conformance',
                  'case_id': snapshots.CONFORMANCE_CASE, 'case_sha256': case,
                  'registration_sha256': p.digest(p.canonical(reg)), 'source': binding['source'],
                  'session_id': binding['session_id'], 'resource_plan_sha256': binding['resource_plan_sha256'],
                  'initial_image_sha256': binding['initial_image_sha256'],
                  'anchor_intent_sha256': conformance['anchor_intent_sha256'],
                  'execution_origin': pend['execution_origin'], 'restoration_origin': origin,
                  'catalog_root': intent['catalog_root'], 'verifier_root': intent['verifier_root'],
                  'final_batch': pend['pending']['previous_batch'],
                  'nonempty_transfer_batches': pend['pending']['next_batch_ordinal'],
                  'catalog_sha256': catalog['catalog_sha256'], 'verifier_sha256': verifier['work_db_sha256'],
                  'validations': refs, 'complete': True, 'acceptance': False, 'proof_checkpoint_advanced': False}
        return batch, plan, payload, result
    activation = {**binding, 'schema_version': 1, 'kind': 'publication_activation',
                  'intent_sha256': pend['pending']['intent']['sha256'],
                  'execution_origin': pend['execution_origin'], 'activation_origin': origin,
                  'final_batch': pend['pending']['previous_batch'],
                  'previous_checkpoint': intent['previous_checkpoint'],
                  'candidate_checkpoint': intent['candidate_checkpoint'], 'catalog_root': intent['catalog_root'],
                  'verifier_root': intent['verifier_root'], 'execution_result': intent['execution_result'],
                  'validations': refs, 'stage': accepted['work_unit']['stage'],
                  'stage_complete': accepted['stage_complete'], 'activated': True, 'acceptance': False}
    return batch, plan, payload, activation


TERMINAL_PHASES = ('resource_blocked', 'semantic_invalid', 'completion_rejected', 'complete')


def item_kind(item):
    if item['operation'] == 'publish':
        return 'conformance' if item['conformance'] is not None else 'publish'
    return 'complete' if item['mode'] == 'complete' else 'execute'


def expected_dispatch(state):
    """The one legal next dispatch kind, derived only from replay-authenticated state."""
    if state['pending_publication'] is not None:
        return 'publish'
    if state['conformance_pending'] is not None or (state['anchor'] is not None and state['conformance_result'] is None):
        return 'conformance'
    return 'complete' if dispatch_mode(state) == 'complete' else 'execute'


def conformance_request(state, reg, session_id):
    anchor = state['anchor']
    case = snapshots.conformance_case_sha256(p.digest(p.canonical(reg)), session_id, anchor['sha256'])
    return {'case_id': snapshots.CONFORMANCE_CASE, 'anchor_intent_sha256': anchor['sha256']}, case


def batch_extracted_bytes(batch):
    """Extracted member bytes of the artifacts one authenticated transfer batch retains."""
    return sum(receipt['record']['extracted_bytes'] for receipt in batch['artifact_receipts'])


def require_extracted_within(extracted, plan):
    """`total_retained_artifact_bytes` bounds the extracted bytes of the whole retained history, not only its ZIPs.

    The restore pool is sized (`restore_staging_requirement`) for a history of at most that many extracted
    bytes; a highly compressed history would otherwise extract past the pool and halt the session at a download.
    """
    p.require(extracted <= plan['total_retained_artifact_bytes'],
              'extracted artifact bytes exceed the retained artifact allowance the restore pool was sized for')


def upload_member_bytes(inventory):
    """The extracted bytes of every member one upload plan or inventory will upload."""
    return sum(row[0] for row in inventory.db.execute('SELECT bytes FROM members'))


def require_upload_fits_retention(state, upload_bytes):
    """Sender side of `require_extracted_within`: refuse an upload that would push the history past the allowance."""
    require_extracted_within(state['extracted_bytes'] + upload_bytes, state['resource_plan']['record'])


def chain_carrier(state, auth, root):
    """Linear control chain over every successful publisher carrier."""
    carrier = auth['carrier']
    p.require(carrier['previous_control_sha256'] == state.get('control_sha256'),
              'carrier skips or reorders the authenticated control chain')
    state['control_sha256'] = auth['control_sha256']
    state['retained_bytes'] += carrier['transfer_batch']['retained_artifact_bytes']
    state['extracted_bytes'] += batch_extracted_bytes(carrier['transfer_batch'])
    require_extracted_within(state['extracted_bytes'], state['resource_plan']['record'])
    state['operational_usage'] = enforce_control_caps(state, {'parts': {}, 'control': carrier}, root)


def replay_batch(state, auth, index, pend, *, activation):
    """Credit one authenticated batch into its pending publication and return the new pending."""
    carrier, batch = auth['carrier'], auth['carrier']['transfer_batch']
    p.require(carrier['work_unit'] == pend['unit'] and batch['execution_origin'] == pend['execution_origin']
              and batch['publication_origin'] == carrier['origin']
              and (batch['purpose'] == 'activate') == activation, 'batch is not the next link of its pending publication')
    receiver = ReplayLedger(pend['ledger_path'])
    try:
        after = credit_rows(index, receiver.ledger, auth['resolver'], carrier['unit_ordinal'], pend['pending'], batch,
                            auth['rows'])
        if not activation:
            p.require(after == carrier['pending_publication'], 'carrier pending publication differs from replay')
    finally:
        receiver.close()
    return after


def accept_activation(state, reg, root, work, auth, index):
    """Activation: closure + restored validations, then one accepted result and one advance."""
    carrier, activation = auth['carrier'], auth['carrier']['activation']
    pend = state['pending_publication']
    p.require(carrier['previous_control_sha256'] == state.get('control_sha256'),
              'activation carrier skips or reorders the authenticated control chain')
    p.require(carrier['role'] == 'unit_checkpoint' and carrier['pending_publication'] is None
              and activation['activation_origin'] == carrier['origin'] == carrier['transfer_batch']['publication_origin']
              and carrier['transport_conformance'] is None, 'activation carrier shape differs')
    unit = auth['unit']
    intent, resolver = auth['intent'], auth['resolver']
    binding = {key: intent[key] for key in ('source', 'session_id', 'unit_ordinal', 'resource_plan_sha256',
                                           'initial_image_sha256')}
    after = replay_batch(state, auth, index, pend, activation=True)
    check_root_origin(p.decode(resolver(intent['catalog_root'])), intent['execution_origin'])
    if intent['verifier_root'] is not None:
        check_root_origin(p.decode(resolver(intent['verifier_root'])), intent['execution_origin'])
    receiver = ReplayLedger(pend['ledger_path'])
    try:
        p.require(snapshots.closure_complete(receiver.ledger), 'activation before publication closure is complete')
        planned = p.decode(resolver(intent['execution_result']))
        provisional = snapshots.accepted_result(planned, pend['execution_origin'], pend['original_receipts'], p.ref)
        budget = replay_budget()
        evidence_dir = reconstruct_evidence(intent, resolver, binding, Path(work) / ('evidence-' + str(intent['unit_ordinal'])),
                                            reg, root, budget)
        transport = accepted_transport(intent, resolver, binding, evidence_dir, pend, pend['bytes'])
        authenticated_roots(transport['parts'], index, root)
        add_authenticated_members(index, provisional, transport, root)

        def validate_prefix(result, validations):
            check_activation_validations(result, intent, validations, resolver, receiver.ledger, state['index_path'], root)
        result = snapshots.finish_publication(receiver.ledger, intent, pend['pending'], activation, planned,
                                              pend['original_receipts'], p.ref, validate_prefix)
    finally:
        receiver.close()
    p.require(p.digest(p.canonical(result['checkpoint'])) == intent['candidate_checkpoint']['sha256'],
              'accepted checkpoint is not the frozen candidate')
    # The publication is accepted: nothing reads its receiver ledger again, so the retained set stays the live set.
    receiver.discard()
    return result, transport, after


def replay_activation(state, reg, root, work, auth, index, plan):
    result, transport, _ = accept_activation(state, reg, root, work, auth, index)
    prior = state
    usage = enforce_control_caps(state, {'parts': transport['parts'], 'control': auth['carrier']}, root)
    new = advance(state, result, root)  # exactly once per accepted publication
    new['operational_usage'] = usage
    new = authenticate_verifier_progress(prior, new, result, transport, root)
    validate_backend_progress(prior, new, result, transport, root)
    intent = auth['intent']
    held = prior['pending_publication']
    new.update(control_sha256=auth['control_sha256'], pending_publication=None,
               execution_origin=held['execution_origin'],
               accepted_batches=[*prior['accepted_batches'], *held['batch_receipts'],
                                 batch_digest(auth['carrier']['transfer_batch'])],
               catalog_root_ref=intent['catalog_root'], verifier_root_ref=intent['verifier_root'],
               retained_bytes=prior['retained_bytes'] + auth['carrier']['transfer_batch']['retained_artifact_bytes'],
               extracted_bytes=prior['extracted_bytes'] + batch_extracted_bytes(auth['carrier']['transfer_batch']))
    p.require(new['retained_bytes'] <= plan['total_retained_artifact_bytes'], 'retained artifact allowance exhausted')
    require_extracted_within(new['extracted_bytes'], plan)
    if new['anchor'] is None and intent['verifier_root'] is not None:
        # First normally authenticated catalog+verifier pair; never manufactured.
        new['anchor'] = {'intent': held['pending']['intent'], 'sha256': held['pending']['intent']['sha256'],
                         'unit': held['unit'], 'execution_origin': held['execution_origin'],
                         'original_receipts': held['original_receipts']}
    return new, result, transport


def replay_conformance(state, reg, root, work, auth, index, case):
    """One rehearsal dispatch: same namespace, separate index, zero historical credit."""
    carrier = auth['carrier']
    conformance = carrier['transport_conformance']
    anchor = state['anchor']
    p.require(anchor is not None and conformance['case_sha256'] == case
              and conformance['anchor_intent_sha256'] == anchor['sha256']
              and carrier['work_unit'] == anchor['unit']
              and carrier['checkpoint'] == p.ref(state['checkpoint'], p.digest(p.canonical(state['checkpoint'])))
              and carrier['activation'] is None and carrier['completion'] is None,
              'conformance carrier is not the fixed case over the unchanged accepted checkpoint')
    conf_index = open_index(state['conformance_index_path'])
    try:
        pend = state['conformance_pending']
        if pend is None:
            pend = fresh_conformance_pending(anchor, Path(work) / ('conformance-' + case[:32] + '.sqlite'),
                                         snapshots.AuthenticatedResolver(index.db, Path(state['parts_root'])))
        resolver = snapshots.AuthenticatedResolver(conf_index.db, Path(state['conformance_parts_root']))
        auth = {**auth, 'resolver': resolver}
        final = carrier['transport_conformance']['result'] is not None
        batch = carrier['transfer_batch']
        p.require(batch['execution_origin'] == anchor['execution_origin'] and batch['batch_ordinal'] == pend['pending']['next_batch_ordinal']
                  and (batch['purpose'] == 'activate') == final, 'conformance batch chain differs')
        receiver = ReplayLedger(pend['ledger_path'])
        finished = False
        try:
            after = credit_rows(conf_index, receiver.ledger, resolver, carrier['unit_ordinal'], pend['pending'], batch,
                                auth['rows'])
            payload = [(row[0]['kind'], row[0]['sha256']) for row in auth['rows']]
            p.require(carrier['pending_publication'] == after, 'conformance pending publication differs from replay')
            if not final:
                p.require(payload, 'conformance transfer batch carries no new payload')
                pend = {**pend, 'pending': after, 'batches': [*pend['batches'], payload],
                        'bytes': pend['bytes'] + batch['retained_artifact_bytes'],
                        'carriers': [*pend['carriers'], auth['control_sha256']]}
                state['conformance_pending'] = pend
            else:
                check_conformance_final(state, reg, root, carrier, pend, payload, receiver.ledger, resolver, conf_index, case)
                state['conformance_result'] = conformance['result']
                state['conformance_pending'] = None
                finished = True
        finally:
            receiver.close()
        if finished:
            receiver.discard()  # the case is complete: its rehearsal ledger has no further reader
    finally:
        conf_index.close()
    chain_carrier(state, auth, root)


def check_conformance_final(state, reg, root, carrier, pend, payload, ledger, resolver, conf_index, case):
    """Independently recompute shape, closure and restored validations; a result flag is not authority."""
    result = carrier['transport_conformance']['result']
    intent = p.decode(resolver(pend['intent']))
    batches = [*pend['batches']]
    p.require(snapshots.closure_complete(ledger), 'rehearsal closure incomplete at restore')
    catalog = p.decode(resolver(intent['catalog_root']))
    verifier = p.decode(resolver(intent['verifier_root']))
    chunks = snapshots.conformance_chunks(catalog, verifier, resolver)
    snapshots.check_conformance_payloads(chunks, batches)
    expected_checkpoint = p.digest(p.canonical(state['checkpoint']))
    p.require(intent['candidate_checkpoint']['sha256'] == expected_checkpoint, 'rehearsal anchor is not the unchanged checkpoint')
    check_activation_validations({'checkpoint': state['checkpoint']}, intent, result['validations'], resolver, ledger,
                                 state['index_path'], root)
    p.require(result['kind'] == 'native_transport_conformance' and result['case_sha256'] == case
              and result['case_id'] == snapshots.CONFORMANCE_CASE and result['complete'] is True
              and result['acceptance'] is False and result['proof_checkpoint_advanced'] is False
              and result['registration_sha256'] == p.digest(p.canonical(reg))
              and result['source'] == reg['source'] and result['anchor_intent_sha256'] == intent_sha(pend)
              and result['execution_origin'] == pend['execution_origin']
              and result['restoration_origin'] == carrier['origin']
              and result['catalog_root'] == intent['catalog_root'] and result['verifier_root'] == intent['verifier_root']
              and result['final_batch'] == pend['pending']['previous_batch']
              and result['nonempty_transfer_batches'] == len(batches)
              and result['catalog_sha256'] == catalog['catalog_sha256']
              and result['verifier_sha256'] == verifier['work_db_sha256']
              and result['resource_plan_sha256'] == intent['resource_plan_sha256']
              and result['initial_image_sha256'] == intent['initial_image_sha256'], 'conformance result differs from recomputation')


def intent_sha(pend):
    return pend['intent']['sha256']


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
    p.require(control_ordinal(control)<=plan['maximum_units'],'attempts exceed immutable dispatch allowance')
    api.maximum_session_requests=max(api.maximum_session_requests,400+(len(control['attempt_runs'])+3)*(32+3*p.UPLOAD_SLOTS))
    api.bind_storage(control['resource_plan'],reg,control['session_id']);live_pr(api,reg)
    inventory=attempt_inventory(api,reg,control,getattr(api,'current_run',None))
    p.closed(control['probe_run'],('success_run','failure_run','post_failure_run'))
    work=Path(work);work.mkdir(parents=True,exist_ok=True)
    probe,measurement,_=authenticate_output(api,reg,control['probe_run']['success_run'],'unit',work/'probe',root)
    probe['finalizer_capability']=authenticate_probe_pair(api,reg,control['probe_run'])
    admitted_at=utc(inventory[0]['run']['created']) if inventory else time.time()
    state=admit(reg,control['resource_plan'],probe,measurement,root,admitted_at)
    controller_memory_limit(state['resource_plan'])
    p.require(control['session_id']==plan['session_id'],'control session UUID differs')
    parts_root=str(Path(api.scratch).parent/'authenticated-parts')
    state.update(pending_publication=None,accepted_batches=[],execution_origin=None,conformance_pending=None,conformance_result=None,anchor=None,
                 index_path=str(work/'dependencies.sqlite'),conformance_index_path=str(work/'conformance.sqlite'),
                 parts_root=parts_root,conformance_parts_root=parts_root,catalog_root_ref=None,verifier_root_ref=None,
                 next_dispatch_ordinal=0,control_sha256=None,retained_bytes=state.get('retained_bytes',0),
                 extracted_bytes=state.get('extracted_bytes',0))
    decisions={}
    for decision in control['resume_decisions']:
        ordinal=decision['failed_ordinal']
        p.require(type(ordinal) is int and ordinal not in decisions,'duplicate/invalid infrastructure decision ordinal')
        decisions[ordinal]=decision
    p.require([d['failed_ordinal'] for d in control['resume_decisions']]==sorted(decisions),'unordered resume decisions')
    latest=None;history=[];successes=[]
    index=open_index(state['index_path'])
    try:
        for position,item in enumerate(inventory):
            ordinal=item['ordinal']  # runs and settled-uncertain dispatches share one ordinal sequence
            state['next_dispatch_ordinal']=ordinal
            state['next_ordinal']=len(state['units'])
            before=item['control']
            p.require(before['unit_runs']==successes and before['attempt_runs']==control['attempt_runs'][:position]
                      and before['resume_decisions']==[d for d in control['resume_decisions'] if d['failed_ordinal']<ordinal],
                      'attempt event omitted/changed its authenticated predecessor inventory or decision')
            kind=item_kind(item)
            p.require(state['phase'] not in TERMINAL_PHASES and kind==expected_dispatch(state)
                      and item['mode']==('complete' if kind=='complete' else 'unit'),
                      'dispatch bypasses source phase, publication chain or terminal disposition')
            if kind=='conformance':
                request,case=conformance_request(state,reg,control['session_id'])
                p.require(before['transport_conformance']==request,'conformance request differs from the fixed anchor case')
            else:
                case=None
                p.require(before['transport_conformance'] is None,'ordinary dispatch carries a conformance request')
            previous=inventory[position-1] if position else None
            if previous is not None and previous['job']['status']!='success':
                validate_resume_decision(api,decisions.get(previous['ordinal']),previous,state,reg)
            if item['job']['status']!='success':
                p.require(state['initial_image'] is not None,'failed initial attempt has no authenticated Init; new session required')
                if ordinal in decisions:validate_resume_decision(api,decisions[ordinal],item,state,reg)
            else:
                p.require(kind!='complete','a final completion run cannot be adopted as an ordinary predecessor')
                unit_work=work/('dispatch-'+str(ordinal))
                if kind=='conformance':
                    auth=authenticate_publication(api,reg,item['run']['id'],root,unit_work,state,index,case_sha256=case)
                    replay_conformance(state,reg,root,unit_work,auth,index,case)
                elif kind=='execute':
                    auth=authenticate_publication(api,reg,item['run']['id'],root,unit_work,state,index)
                    p.require(auth['carrier']['role']=='publication_batch','execution does not seal a publication')
                    pend=begin_pending(state,reg,root,unit_work,auth,index,item)
                    state['pending_publication']=pend
                    chain_carrier(state,auth,root)
                else:
                    auth=authenticate_publication(api,reg,item['run']['id'],root,unit_work,state,index)
                    pend=state['pending_publication']
                    if auth['carrier']['role']=='unit_checkpoint':
                        prior=state
                        state,result,transport=replay_activation(state,reg,root,unit_work,auth,index,plan)
                        history.append((result,transport))
                        latest=(result,{**transport,'history':list(history)})
                    else:
                        p.require(auth['carrier']['role']=='publication_batch','publish carrier role differs')
                        after=replay_batch(state,auth,index,pend,activation=False)
                        state['pending_publication']={**pend,'pending':after,'carriers':[*pend['carriers'],auth['control_sha256']],
                                                      'batch_receipts':[*pend['batch_receipts'],batch_digest(auth['carrier']['transfer_batch'])]}
                        chain_carrier(state,auth,root)
                successes.append(item['run']['id'])
            state['attempts'].append({'ordinal':ordinal,'run':item['run']['id'],'job':item['job']['id'],
                 'task':item['job']['task_id'],'attempt':1,'status':item['job']['status']})
    finally:
        index.close()
    p.require(successes==control['unit_runs'] and set(decisions)<={item['ordinal'] for item in inventory},
              'successful roots/decisions do not match actual attempted inventory')
    consumed=len(inventory)+len(inventory.uncertain)  # every run and every settled-uncertain dispatch consumed a unit
    state['next_dispatch_ordinal']=consumed
    # The control each consumed ordinal actually ran (or, if uncertain, was reserved) with, by ordinal: what a
    # durable dispatch reservation is reconciled against (settle_dispatch).
    by_ordinal={item['ordinal']:p.digest(p.canonical(item['control'])) for item in inventory}
    by_ordinal.update({row['ordinal']:row['control_sha256'] for row in inventory.uncertain})
    state['attempt_control_sha256']=[by_ordinal[i] for i in range(consumed)]
    state['next_ordinal']=len(state['units'])
    last=inventory[-1] if inventory else None
    state['retry_authorized']=(last is None or last['job']['status']=='success' or last['ordinal'] in decisions
                               or (bool(inventory.uncertain) and inventory.uncertain[-1]['ordinal']>last['ordinal']))
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


def live_spools(state, reg, control):
    """Spool directories an authenticated pending chain can still use; every other spool is dead."""
    live = set()
    if state['pending_publication'] is not None:
        live.add('publication-' + str(state['pending_publication']['execution_origin']['dispatch_ordinal']))
    if state['anchor'] is not None and state['conformance_result'] is None:
        live.add('conformance-' + snapshots.conformance_case_sha256(
            p.digest(p.canonical(reg)), control['session_id'], state['anchor']['sha256'])[:32])
    return live


def runner_body(control, root, output, mode, operation):
    """Trusted CI entry: the one physical context spans authenticate, run, package and upload."""
    root = Path(root).resolve(); output = Path(output).resolve()
    reg=registration(control['registration'],root)
    owned=validate_storage(reg['storage'],root)
    p.require(owned in output.parents,'session output escapes persistent owned storage')
    context=PhysicalContext(reg,control['resource_plan'],control['session_id'],root,owned/control['session_id'])
    with physical_session(context):
        context.reconcile_crash()
        return _runner_body(control, root, output, mode, operation, reg, owned)


def unit_execute_request(state, unit, reg, output, deadline, operation):
    """The closed private request of one unit-executing quantum (Rust QuantumRequest, checker request).

    tests/operations/test_paged_request_fixtures.py pins this builder to the checked-in fixture that the
    Rust catalog parses with QuantumRequest::read_shape.
    """
    output = Path(output)
    return {'schema_version': 6, 'deadline_monotonic': deadline, 'operation': 'expand', 'work_unit': unit['record'],
            'resource_plan': state['resource_plan'], 'initial_image': state['initial_image'],
            'prior_checkpoint': state['checkpoint'], 'checkpoint': state['checkpoint'],
            'catalog_path': str(output / 'catalog.sqlite'), 'output_dir': str(output / 'quantum'),
            'dependency_index': str(output / 'dependencies.sqlite'),
            'execution_origin': execution_origin(state, unit['record'], operation),
            'source_files': reg['source_files'], 'runtime': reg['runtime'],
            'driver_graph': str(output / 'driver-graph.json'),
            'verifier_work_db': str(output / 'verifier.sqlite'),
            'prepare_path': str(output / 'quantum/prepare-unit.json'),
            'reservation_path': str(output / 'reservation.json'),
            'previous_verifier_progress_sha256': state.get('verifier_progress_sha256'),
            'previous_verifier_backup_sha256': state.get('verifier_backup_sha256'),
            'tlc': {'java': reg['runtime']['java']['path'], 'jar': reg['runtime']['tlc']['path'],
                    'java_sha256': reg['runtime']['java']['sha256'], 'jar_sha256': reg['runtime']['tlc']['sha256']}}


def _runner_body(control, root, output, mode, operation, reg, owned):
    deadline = time.monotonic() + 1800
    api=api_environment(owned/control['session_id']/'controller-api'/str(os.environ['GITHUB_RUN_ID']))
    api.bind_storage(control['resource_plan'],reg,control['session_id'])
    api.current_run=receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    api.maximum_session_requests=400+(len(control['attempt_runs'])+3)*(32+3*p.UPLOAD_SLOTS)
    reject_prior_ordinal_dispatch(api,reg,control,api.current_run)
    output.mkdir(parents=True, exist_ok=False)
    current_physical().discard_prior_outputs(output)
    if operation in ('execute','publish'):
        guard=owned/control['session_id']/('ordinal-'+str(control_ordinal(control))+'.json')
        p.require(not guard.exists(),'executed ordinal already attempted; preserve failed state and start a new admitted session')
        with guard.open('xb') as stream:
            stream.write(p.canonical({'run':int(os.environ['GITHUB_RUN_ID']),'ordinal':control_ordinal(control),
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
    if operation in ('execute','publish'):reject_prior_ordinal_dispatch(api,reg,control,run_id)
    source=materialize_session_source(root,reg,control['session_id'])
    _, own_lines = p.producer_log(api, run_id, job, mode)
    identity = runtime_identity(source, reg, actual_runner(own_lines, job, reg))
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
    p.require(operation in ('execute', 'publish'), 'unregistered operation')
    state, latest = replay_control(api, control, root, output / 'history')
    current_physical().discard_dead_spools(live_spools(state, reg, control))
    live_resources(state['resource_plan']['record'], reg, measurement)
    controller_memory_limit(state['resource_plan'])
    expected = expected_dispatch(state)
    p.require(state['phase'] not in TERMINAL_PHASES and (
        (operation == 'publish') == (expected in ('publish', 'conformance'))
        and (control['transport_conformance'] is not None) == (expected == 'conformance')
        and (operation == 'execute') == (expected in ('execute', 'complete'))),
        'producer operation differs from authenticated publication-chain routing')
    if operation == 'publish':
        return run_publish(api, reg, state, latest, control, root, source, output, job, run_id, identity,
                           measurement, deadline)
    unit = unit_request(state, run_id, job, root)
    request = unit_execute_request(state, unit, reg, output, deadline, operation)
    quantum = Path(request['output_dir']); quantum.mkdir()
    emit_runtime_receipts(state,unit,identity,quantum)
    if latest:
        for directory in latest[1]['parts'].values():
            for name in ('driver-graph.json', state['initial_image']['record']['member_path']):
                path = directory / name
                if path.is_file():
                    target = output / name; target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(path, target)
        restore_latest(state, latest, source, request, output, reg, root)
    p.require(mode==dispatch_mode(state),'producer mode differs from authenticated replay routing')
    if mode == 'complete':
        p.require(state['stage'] == 'complete' and state['phase'] == 'qualified', 'completion premature')
        ready=p.read(output/'restore-validation/restore-semantic.json')
        backup=p.read(output/'restored-catalog-root.json')
        p.require(final_input_receipt(ready,state['checkpoint'],backup['catalog_sha256']),
                  'actual completion lost exact final input readiness; no preparation fallback')
        return finish_runner(api, reg, state, control, root, source, request, output, job)
    result,inventory,store,sealed=execute_and_package(state,reg,request,source,root,output,deadline)
    try:
        write_publication_draft(output,'publication_batch',state,control,
            {'schema_version':6,'kind':'WorkUnit','record':result['work_unit']},sealed['pending'],sealed['batch'],
            sealed['refs'],{'activation':None,'conformance':None,
                            'candidate_checkpoint_sha256':sealed['candidate_checkpoint_sha256'],
                            'candidate_checkpoint':None})
        resource = state['resource_plan']['record']
        actual = sum(r[0] for r in inventory.db.execute('SELECT bytes FROM members'))
        p.require(actual <= resource['unit_byte_allowance']
                  and actual + state['retained_bytes'] <= resource['total_retained_artifact_bytes'],
                  'actual upload bytes exceed immutable admission')
        require_upload_fits_retention(state, actual)
        upload_outputs(output_metadata(reg, control['session_id'], unit['record']['unit_ordinal'], inventory,
                                       'transfer/node-0.json', 'execute'), output / 'upload')
    finally:
        inventory.close()
        store.close()
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
    runtime_paths=('runtime/'+str(state['next_ordinal'])+'.json','resources.json',
                   'predecessors/'+str(state['next_ordinal'])+'.json','runtime-external-roots.ndjson')
    for name in runtime_paths:
        target=Path(request['output_dir'])/name;target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(output/'quantum'/name,target)
    path = output / 'completion-request.json'; p.write(path, request)
    argv = [sys.executable, str(Path(source) / 'verification/async-driver-refinement/paged_checker.py'),
            'complete', '--request', str(path)]
    verified=run_quantum(path,reg,source,Path(request['output_dir']),argv)
    main_records = authenticate_main(api, reg, control['main_run'], root, verified=verified)
    live = live_pr(api, reg)
    # The fresh-head receipt is compared with later live reads, so it holds only identities that cannot
    # legitimately move: the live Main tip is recorded once, in the carrier postcheck, for audit.
    fresh = {'source': reg['source'], 'repository': reg['repository'], 'pr': reg['pr'],
             'head': live['head']['sha'], 'event_base': reg['event_base'],
             'merge_base': live['merge_base'], 'state': live['state'], 'draft': live['draft'],
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
        'head':live['head']['sha'],'event_base':reg['event_base'],
        'merge_base':live['merge_base'],'state':live['state'],'draft':live['draft'],'run':run_id,'job':result['work_unit']['job'],
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
        if run.get('commit_sha') != commit or not p.dispatched(run) or run.get('status')!='success':
            continue
        payload = p.decode(run.get('event_payload', '').encode())
        if payload.get('inputs', {}).get('mode') != 'complete':
            continue
        control = p.decode(payload['inputs']['control'].encode())
        raw = control.get('registration') if type(control) is dict else None
        if type(raw) is dict and raw.get('event_base') != event_merge_base(root, event):
            # A completed session for this head against another merge base (Main merged a sibling branch,
            # the operator re-registered) is not this session. Skipping grants nothing; the loop still
            # refuses unless a matching complete root is found, and it is judged before the registration
            # is validated so a stale one (for example an expired one) cannot mask the current one.
            continue
        reg = registration(control['registration'], root)
        p.require(reg['pr'] == scope['pr'] and reg['repository'] == scope['repository']
                  and reg['event_base'] == event_merge_base(root, event), 'completion current event scope differs')
        result = authenticate_completion(api, reg, run['id'], root, Path(output) / ('complete-' + str(run['id'])))
        p.write(Path(output) / 'completion.json', result)
        return
    raise receipts.ReceiptError('Async qualification session pending; complete registered Main verdict required')


def fold_uncertain(control, reservations):
    """The control with every dispatch the reservations settled as uncertain declared in it.

    The rows are only ever added here, never removed, and the attempt inventory reconciles them: a row
    that collides with a run, leaves an ordinal gap or conflicts with a later run's control refuses.
    """
    known = {row['ordinal']: row for row in declared_uncertain(control)}
    for row in reservations.uncertain():
        p.require(known.setdefault(row['ordinal'], row) == row, 'conflicting uncertain dispatch declarations')
    return {**control, 'uncertain_dispatches': [known[ordinal] for ordinal in sorted(known)]}


def coordinate(control, root, output, maximum_dispatches=1):
    """Finite trusted controller; authenticate every predecessor before dispatch."""
    output = Path(output); output.mkdir(parents=True, exist_ok=True)
    api = api_environment(output / 'api')
    p.require(type(maximum_dispatches) is int and maximum_dispatches > 0, 'invalid controller dispatch bound')
    reservations = DispatchReservations(output / 'dispatch-reservations', {
        'session_id': control['session_id'], 'registration_sha256': p.digest(p.canonical(control['registration']))})
    for iteration in range(maximum_dispatches):
        while True:
            control = fold_uncertain(control, reservations)
            control,completed=reconcile_control(api,control,root)
            if completed is not None:
                result=authenticate_completion(api,control['registration'],completed,root,output/'complete')
                p.write(output/'completion.json',result);return
            p.write(output/'control.json',control)
            state, _ = replay_control(api, control, root, output / ('replay-' + str(iteration)))
            p.require(control_ordinal(control) == state['next_dispatch_ordinal'],
                      'the control takes a different dispatch ordinal than the authenticated inventory')
            p.require(state['next_dispatch_ordinal'] + maximum_dispatches - iteration <=
                      state['resource_plan']['record']['maximum_units'], 'controller exceeds immutable dispatch limit')
            try:
                expected, _adopted = dispatch(api, state, {**control}, root, reservations)
            except UncertainDispatch:
                # The sent dispatch never became visible: it consumed its ordinal and unit. Fold it into the
                # control and dispatch again with the next ordinal (bounded by the immutable maximum_units).
                continue
            break
        control = expected['control']
        p.write(output / ('dispatch-' + str(iteration) + '.json'), expected)
        # No guessed next run ID. Match the actual immutable dispatch input.
        deadline = time.monotonic() + 1800
        selected = None
        while time.monotonic() < deadline:
            matches = matching_runs(api, expected, control, expected['dispatched_at'])
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
    p.require(control['probe_run'] is None and not control['unit_runs'] and not control['attempt_runs'] and not control['resume_decisions'] and not control['uncertain_dispatches'] and control['main_run'] is None,
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
    landing = sub.add_parser('landing-check')
    landing.add_argument('--control', type=Path, required=True)
    landing.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    # Every owned spool, database, grant descriptor and restored image is private.
    os.umask(0o077)
    root = Path(__file__).resolve().parents[1]
    if args.command == 'main-aggregate':
        ordinary_main()
    elif args.command == 'finalize-uploads':
        finalize_uploads(args.output or event_output(root),root)
    elif args.command == 'require-completion':
        require_completion(root, args.output)
    elif args.command == 'landing-check':
        receipt = landing_check(p.read(args.control)['registration'],
                                api_environment(args.output.parent / 'landing-api'), root)
        p.write(args.output, receipt)
        print(p.canonical(receipt).decode())
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
        names.add(name)
        plan.add_file(name,target)
        members.append(row)
    for row in prepare['metadata_reservations']:
        p.closed(row,('path','maximum_bytes','role'))
        name=p.safe_path(row['path'])
        allowed_receipt=(u['stage']=='complete' and u['mode']=='unit' and row['role']=='receipt'
                         and name=='restore-semantic.json')
        p.require(name not in names and (allowed_receipt or row['role'] in ('result','evidence_index','progress','temporal_publication')),
                  'future metadata path/role not closed')
        names.add(name)
        if name!='unit-result.json':  # the PlannedResult is intent content, not an evidence member
            plan.reserve(name,row['maximum_bytes'],row['role'])
        reservations.append(row)
    for row in published_members:
        p.require(row['path'] not in names,'published member aliases prepared metadata')
        names.add(row['path']);members.append(row)
    plan.preflight(pending=True)
    return {'protocol':'verifier_reservation_v1','prepare_sha256':p.digest(p.canonical(prepare)),
            'source':u['source'],'session_id':u['session_id'],'unit_ordinal':u['unit_ordinal'],
            'resource_plan_sha256':u['resource_plan_sha256'],'initial_image_sha256':cp['initial_image_id'],
            'input_checkpoint_sha256':prepare['input_checkpoint_sha256'],'final_checkpoint':final_checkpoint,
            'members':members,'metadata_reservations':reservations}


def directory_bytes(directory, exclude=()):
    """Allocated bytes below `directory`, skipping the named subtrees/files (exact paths)."""
    excluded={Path(item).absolute() for item in exclude}
    total=0
    stack=[Path(directory)]
    while stack:
        with os.scandir(stack.pop()) as entries:
            for entry in entries:
                path=Path(entry.path)
                if path.absolute() in excluded:continue
                info=entry.stat(follow_symlinks=False)
                if entry.is_symlink():total+=info.st_blocks*512
                elif entry.is_file(follow_symlinks=False):total+=max(info.st_size,info.st_blocks*512)
                elif entry.is_dir(follow_symlinks=False):
                    total+=info.st_blocks*512
                    stack.append(path)
    return total


def observed_role_geometry(context, family, role, source_map, output):
    """Install one source-map role's exact empty DDL under a real DatabaseGrant and observe it.

    The grant reserves the family's whole database ceiling before the file exists. The scratch
    file is removed and fsynced before its reservation is released, so roles that share a family
    are measured one at a time and never inflate one another.
    """
    path=Path(output)/('calibration-'+role+'.sqlite')
    p.require(not path.exists() and not path.is_symlink(),'calibration DB must be fresh')
    grant=physical.reserve_database(context.ledger,family,path,context.policy['families'][family]['database_bytes'])
    connection=grant.connect()
    try:
        observed=physical.install_empty_role(connection,role,source_map)
        grant.retain()
    finally:
        connection.close()
        grant.connection=None
    for suffix in ('',*physical.DB_COMPANIONS):
        Path(str(path)+suffix).unlink(missing_ok=True)
    grant.release_after_removal()
    return observed


def calibration_geometry(context, source_map, output, rust):
    """Actual empty geometry of EVERY source-map database role, family by family.

    The Rust catalog role is the engine's own report (validated against the map by
    calibrate_physical_policy); every other role is installed from its pinned DDL.
    """
    geometry={}
    for family,databases in source_map['families'].items():
        geometry[family]=[rust['geometry'] if database['database_role']==rust['geometry']['database_role']
                          else observed_role_geometry(context,family,database['database_role'],source_map,output)
                          for database in databases]
    return geometry


def calibration_baseline(root, output, reg):
    """Disjoint baseline: the source workspace, and whatever else already sits in owned storage.

    The source archive and extraction are measured once into source_workspace_bytes; the job
    output belongs to the pools. Neither is counted again in the unpooled baseline.
    """
    archive=Path(root).parent/('source-'+reg['source_archive_sha256']+'.tar')
    workspace=directory_bytes(root)+archive.stat().st_size
    owned=validate_storage(reg['storage'],root)
    return {'source_workspace_bytes':workspace,
            'existing_unpooled_bytes':directory_bytes(owned,exclude=(root,archive,output))}


def calibrate_overheads(root, output, control, measurement, deadline):
    """Fresh typed physical calibration, explicitly not a width proof or a fit receipt.

    Measures actual engine geometry for every declared database role and the disjoint
    storage baseline, then lets calibrate_physical_policy compute the simultaneous
    reservation. There is no record-count x table-count term and no backup inequality.
    """
    reg=control['registration'];plan=control['resource_plan']['record']
    wrapper=operational_bounds(plan['source_bounds_utf8'],reg,root,measurement_pending=True)
    p.source_bound_receipt(wrapper['source_receipt'],root,reg['source'],reg['source_files'],
                           plan['total_session_disk_bytes'])
    counts=wrapper['record_allowances']
    inventory=accounting_inventory(wrapper['source_receipt'])
    p.require(set(counts)==set(inventory),'calibration operational kind inventory differs')
    policy=wrapper['physical_policy']
    source_map=physical.load_source_map(Path(root)/'validation/async_physical_table_map.json')
    calibrated=Path(output)/'rust-calibration';calibrated.mkdir()
    request={'operation':'catalog_calibration','resource_plan':control['resource_plan'],'session_id':control['session_id'],
             'deadline_monotonic':deadline,'output_dir':str(calibrated)}
    request_path=Path(output)/'catalog-calibration-request.json';p.write(request_path,request)
    execution=run_quantum(request_path,reg,root,calibrated,
        [sys.executable,str(Path(root)/'scripts/check-async-driver-refinement'),'calibrate-catalog','--output-dir',str(calibrated)])
    rust=validate_catalog_calibration(execution,calibrated,reg,root)
    geometry=calibration_geometry(current_physical(),source_map,output,rust)
    calibration=physical.calibrate_physical_policy(policy,geometry,calibration_baseline(root,output,reg),plan,
                                                   source_map=source_map)
    return {**measurement,'resource_calibration':calibration,
            'resource_method':physical_method(reg,plan,policy,inventory,counts)}



def add_authenticated_members(index, result, transport, root):
    unit=result['work_unit']
    for member in transport['logical_members']():
        path=transport['evidence_directory']/p.safe_path(member['member_path'])
        p.require(path.is_file() and not path.is_symlink() and path.stat().st_size==member['bytes']
                  and p.file_sha(path)==member['sha256'],'authenticated logical resolver bytes changed')
        row=(unit['unit_ordinal'],member['member_path'],member['bytes'],member['sha256'],str(path.resolve()))
        old=index.db.execute('SELECT bytes,sha256 FROM authenticated_members WHERE unit_ordinal=? AND member_path=?',row[:2]).fetchone()
        p.require(old is None or old==(member['bytes'],member['sha256']),'historical member tuple conflicts')
        index.db.execute('INSERT OR REPLACE INTO authenticated_members VALUES(?,?,?,?,?)',row)
    for kind,name in (('catalog_backup','catalog-backup/root.json'),('verifier_backup','verifier-backup/root.json')):
        path=transport['evidence_directory']/name
        if not path.exists():continue
        value=p.auxiliary(p.read(path),root)
        p.require(value['kind']==kind and value['unit_ordinal']==unit['unit_ordinal'],'authenticated snapshot root differs')
        index.db.execute('INSERT OR REPLACE INTO authenticated_snapshot_roots VALUES(?,?,?,?,?)',
                         (kind,unit['unit_ordinal'],path.stat().st_size,p.file_sha(path),str(path.resolve())))
    progress=transport['evidence_directory']/'verifier-progress/unit.json'
    backup=transport['evidence_directory']/'verifier-backup/root.json'
    p.require(progress.exists()==backup.exists(),'verifier progress/backup pair incomplete')
    if progress.exists():
        p.verifier_auxiliary(p.read(progress),root)
        value=p.auxiliary(p.read(backup),root)
        index.db.execute('INSERT OR REPLACE INTO verifier_chain VALUES(?,?,?,?,?)',
            (unit['unit_ordinal'],str(progress.resolve()),str(backup.resolve()),
             p.canonical({'schema_version':6,'kind':'WorkUnit','record':unit}),p.canonical(value['execution_origin'])))
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
    names=['catalog-backup/root.json']
    if verifier:names+=['verifier-backup/root.json']
    for name in names:
        plan.reserve(name,p.SMALL,'transport')


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
    backup=auxiliary_directory(output)/VERIFIER_BACKUP;catalog=Path(request['catalog_path'])
    backup_sha=p.file_sha(backup);catalog_sha=p.file_sha(catalog)
    before_logs=set(Path(output).glob('quantum-*.log'))
    validate={**request,'operation':'validate_restore','verifier_work_db':str(backup),
              'checkpoint':produced['checkpoint']}
    p.write(request_path,validate)
    # Both sealed files remain in the reservations their writer left held; attach, never re-reserve.
    current_physical().attach_retained(output,(catalog,backup))
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


def copy_restored_catalog(backup,quantum):
    """The preparation-only unit re-snapshots the restored catalog from a controller-made copy.

    No writer child produced it, so no grant covers it: the copy takes its own snapshot_spool_bytes
    claim (the file family the writer's standalone catalog backup belongs to) BEFORE the first byte is
    written. It stays charged while the file exists, and a later invocation on this tree delegates that
    held claim by id instead of reserving the name twice.
    """
    target=auxiliary_directory(quantum)/CATALOG_BACKUP
    target.parent.mkdir(mode=0o700,exist_ok=True)
    spool=PoolBudget(current_physical(),'snapshot_spool_bytes')
    spool.track(target)
    spool.reserve(Path(backup).stat().st_size)
    shutil.copyfile(backup,target)
    return target


def execute_and_package(state,reg,request,source,root,output,deadline):
    """Run one unit, then seal (never upload) its publication and plan batch zero.

    Logical evidence lives in the granted evidence spool, not in native parts. No
    whole-backup packing happens; only the chunk-tree publication is sealed here.
    """
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
            # snapshot; it is re-snapshotted into the same content-addressed chunks.
            saved=p.read(Path(output)/'restored-catalog-root.json')
            backup=Path(request['catalog_path'])
            p.require(backup.stat().st_size==saved['catalog_bytes'] and p.file_sha(backup)==saved['catalog_sha256'],
                      'preparation changed the read-only catalog snapshot')
            copy_restored_catalog(backup,quantum)
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
    origin=request['execution_origin']
    store=PublicationStore(reg,root,'publication-'+str(origin['dispatch_ordinal']),create=True)
    plan=store.evidence
    batch_plan=None
    try:
        transport_reservations(plan,checked)
        runtime_name='runtime/'+str(request['work_unit']['unit_ordinal'])+'.json'
        for name in (runtime_name,'resources.json','runtime-external-roots.ndjson',
                     'predecessors/'+str(request['work_unit']['unit_ordinal'])+'.json'):
            plan.add_file(name,quantum/name)
        published=[]
        for name,file in member_stream(quantum,produced):
            if name=='unit-result.json':
                continue  # the frozen PlannedResult is intent content, never an evidence member
            if checked and name in {r['path'] for r in prepare['staged_members']}:
                continue
            if name.startswith(('catalog-backup/','verifier-backup/')) or name == 'backup-descriptor.json':
                continue
            plan.add_file(name,file)
            value=p.read(file,p.MAX_MEMBER) if file.suffix=='.json' else None
            if type(value) is dict and value.get('schema_version')==6 and value.get('kind') in ('Page','ManifestNode','Checkpoint'):
                role='catalog_page' if value['kind']=='Page' else 'catalog_manifest'
                published.append({'path':name,'bytes':file.stat().st_size,'sha256':p.file_sha(file),'role':role})
        if checked:
            if stage=='complete':
                p.require(preparation_only and request['work_unit']['mode']=='unit','final unit may only authenticate frozen inputs')
                p.require(any(r['path']=='restore-semantic.json' and r['role']=='receipt'
                              for r in prepare['metadata_reservations']), 'final preparation receipt not reserved')
                plan.reserve(FINAL_INPUT_EXECUTION,p.SMALL,'source_receipt')
                plan.reserve(FINAL_INPUT_LOG,p.SMALL,'source_receipt')
            reservation=reserve_verifier(prepare,state,request['work_unit'],produced['checkpoint'],
                                         source,quantum,plan,published)
            if stage=='complete':
                for name in (FINAL_INPUT_EXECUTION,FINAL_INPUT_LOG):
                    row=plan.db.execute('SELECT bytes FROM members WHERE path=?',(name,)).fetchone()
                    p.require(row is not None,'controller final-input metadata reservation disappeared')
                    reservation['metadata_reservations'].append({'path':name,'maximum_bytes':row[0],'role':'source_receipt'})
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
            if not name.startswith(('catalog-backup/','verifier-backup/'))]
        p.write(Path(output)/'unit-finalizer-result.json',produced)
        advance(state,produced,root)  # dry run on the planned form; the single real advance follows closure
        # The transport roots (catalog-backup/root.json, verifier-backup/root.json) are reserved above and
        # fulfilled only by build_publication once the sealed intent exists; evidence_root runs the full
        # strict preflight after fulfilment. Only the pending form can be checked here.
        plan.preflight(pending=True)
        index=open_index(request['dependency_index'])
        try:
            built=build_publication(store,state,reg,root,index,quantum,produced,checked,origin)
        finally:
            index.close()
        batch,batch_plan,refs=plan_transfer(store,state,reg,output,built['pending'],origin,origin,
                                            built['binding'],purpose='transfer')
        draft={'pending':built['pending'],'batch':batch,'refs':refs,
               'candidate_checkpoint_sha256':built['intent']['candidate_checkpoint']['sha256']}
        return produced,batch_plan,store,draft
    except BaseException:
        if batch_plan is not None:batch_plan.close()
        store.close();raise


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
              and backup['schema_version']==2
              and backup['progress_sha256']==p.file_sha(progress_paths[0])
              and progress['resource_plan_sha256']==backup['resource_plan_sha256']==unit['resource_plan_sha256']
              and progress['initial_image_sha256']==backup['initial_image_sha256']==result['initial_image']['record']['image_sha256'],
              'verifier progress/backup checkpoint/source binding differs')
    origin=backup['execution_origin']
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
ACCOUNTING_METHOD_FILES=('validation/async_refinement_provenance.py','validation/async_refinement_session.py',
    'validation/async_snapshot_transport.py','validation/async_physical_policy.py',
    'validation/async_physical_table_map.json')


CATALOG_COMPILED_SOURCES = ('crates/plurx-core/src/store/async_driver_catalog.rs',
    'crates/plurx-core/src/store/async_driver_catalog/records.rs',
    'crates/plurx-core/src/store/async_driver_catalog/sqlite.rs',
    'crates/plurx-core/src/store/async_driver_catalog/memory.rs',
    'crates/plurx-core/src/store/async_driver_catalog/grant.rs',
    'verification/async-driver-refinement/paged-proof.schema.json',
    'verification/async-driver-refinement/paged-catalog.sql')
# The TLC models and configurations `paged_checker` executes, bound beside the checker's own source closure.
PROOF_MODEL_FILES = tuple(PROOF_DIRECTORY + name for name in (
    'AsyncDriverRefinement.tla', 'PagedOneStep.tla', 'PagedComponent.tla', 'PropertyMonitors.tla',
    'BoundaryControls.tla', 'paged-one-step.cfg', 'paged-component.cfg'))
# Registration and Main-condition inputs outside the proof directory: the stage registry and runner route,
# the Main workflow whose conditions the completion aggregates, and the selector that decides which Main
# jobs were required (`source_scope` runs `validation.ci_scope`, which loads `runner` and `points.toml`).
REGISTRATION_BOUND_FILES = (PROOF_DIRECTORY + 'store-tests.json', '.github/workflows/main-fast-lane.yml',
    ROUTE_FILE, 'validation/ci_scope.py', 'validation/runner.py', 'validation/points.toml')


def governed_inputs(root):
    """The (exact files, prefixes) `scripts/check-async-driver-refinement` treats as proof inputs: its
    `proof_inputs()` (`FILES`, the resume invocation paths, and the declared prefixes plus the vendored crates
    the proof compiles), read from the script in `root`, the same module `affected` uses. Not the wider build
    scope `source_identity` hashes (the whole daemon scope and all of `vendor/`)."""
    import importlib.machinery
    try:
        module = importlib.machinery.SourceFileLoader(
            'paged_landing_governance', str(Path(root) / 'scripts/check-async-driver-refinement')).load_module()
        files, prefixes = module.proof_inputs(Path(root))
        files, prefixes = tuple(files), tuple(prefixes)
    except Exception:
        raise receipts.ReceiptError('Async session: governed proof inputs are unreadable') from None
    p.require(files and prefixes and all(type(v) is str and v for v in files + prefixes)
              and all(v.endswith('/') for v in prefixes), 'governed proof inputs are malformed')
    return files, prefixes


def proof_closure(root, commit):
    """Every repository path the registered proof is bound to, as exact files and path prefixes.

    Derived, never hand-listed: the union of (a) what `scripts/check-async-driver-refinement` treats as
    proof inputs (`governed_inputs`: its `FILES`, resume invocation paths, declared prefixes and the vendored
    crates the proof compiles; deliberately not the broader build scope `source_identity` hashes),
    (b) everything `registration` and the qualification bind: the proof-contract inputs, the TLC
    models and configurations, the compiled catalog sources, the stage registry, both workflows, the
    runner route, the capability action, the scope selector, the accounting-method files and table map,
    and (c) the source-bound formula source inputs, record schemas and rule paths and the
    finite-inventory bindings read at the registered `commit`. A path is in the closure when it equals
    a file or lies under a prefix (`in_closure`), so a file added under a governed prefix is covered.
    """
    def blob(path):
        try:
            raw = subprocess.check_output(['git', 'show', commit + ':' + path], cwd=root, timeout=30,
                                          stderr=subprocess.DEVNULL)
            return json.loads(raw)
        except (subprocess.SubprocessError, OSError, ValueError):
            raise receipts.ReceiptError('Async session: proof closure list unreadable at ' + path) from None
    governed_files, prefixes = governed_inputs(root)
    closure = set(governed_files)
    closure.update(PROOF_DIRECTORY + name for name in PROOF_CONTRACT_NAMES)
    closure.update(PROOF_MODEL_FILES)
    closure.update(CATALOG_COMPILED_SOURCES)
    closure.update(REGISTRATION_BOUND_FILES)
    closure.update(CAPABILITY_ACTION_FILES)
    closure.add('.github/workflows/' + p.WORKFLOW)
    closure.add(ACCOUNTING_METHOD['table_map'])
    closure.update(ACCOUNTING_METHOD_FILES)
    formulas = blob(PROOF_DIRECTORY + 'source-bound-formulas.json')
    inventory = blob(PROOF_DIRECTORY + 'finite-inventory.json')
    p.require(type(formulas) is dict and type(inventory) is dict and type(inventory.get('source_bindings')) is dict
              and formulas.get('source_inputs') and inventory['source_bindings'], 'proof closure lists absent')
    for rows, key in ((formulas['source_inputs'], 'path'), (formulas.get('record_schemas', []), 'path'),
                      (formulas.get('primitive_bindings', []), 'source_path'),
                      (formulas.get('init_class_rules', []), 'source_path')):
        closure.update(row[key] for row in rows)
    for binding in inventory['source_bindings'].values():
        closure.add(binding['path'] if type(binding) is dict else binding)
    return {'files': sorted(p.safe_source_path(path) for path in closure), 'prefixes': sorted(prefixes)}


def in_closure(closure, path):
    """Whether `path` is a closure file or lies under a closure prefix."""
    return path in closure['files'] or any(path.startswith(prefix) for prefix in closure['prefixes'])


def main_tip_from_api(api, root):
    """The current tip of Main, read from the trusted Forgejo API and verified with git.

    The tip is never caller-chosen: it is the `main` branch commit the same trusted `api_environment`
    reports, and it must be a commit object present in the checkout.
    """
    branch = api.get('/branches/main')
    tip = branch.get('commit', {}).get('id') if type(branch) is dict and type(branch.get('commit')) is dict else None
    p.require(type(branch) is dict and branch.get('name') == 'main' and isinstance(tip, str)
              and p.COMMIT.fullmatch(tip), 'Forgejo did not report a well-formed Main tip')
    try:
        found = subprocess.check_output(['git', 'rev-parse', '--verify', '--quiet', tip + '^{commit}'], cwd=root,
                                        text=True, timeout=30, stderr=subprocess.DEVNULL).strip()
    except (subprocess.SubprocessError, OSError):
        found = None
    p.require(found == tip, 'Main tip is not a commit in this checkout; fetch Main first')
    return tip


def landing_check(reg, api, root):
    """License landing qualified evidence onto a Main that has moved since the session registered.

    Main cannot be frozen for a multi-hour session, so the session qualifies against `event_base`, the
    merge base of the registered head and Main. The check reads the PR (open, ready, at the registered
    head, merge base `event_base`) and the current Main tip from the trusted API, never from the caller,
    and verifies with git that the checkout is the registered head and the tip descends from
    `event_base`. It refuses if any file changed on Main between `event_base` and the tip is in the
    registered proof closure (`proof_closure`: exact files and prefixes), otherwise returns a receipt
    binding the PR head and the Main tip it checked. The evidence is valid on that tip only when Main
    changed nothing in the registered proof closure; the merge must then be made onto exactly that tip.
    """
    p.closed(reg, REGISTRATION_KEYS)
    source = reg['source']
    commit = source.get('commit') if type(source) is dict else None
    p.require(all(isinstance(v, str) and p.COMMIT.fullmatch(v) for v in (reg['event_base'], commit)),
              'landing check needs full commit identities')
    live_pr(api, reg)
    main_tip = main_tip_from_api(api, root)
    try:
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True, timeout=30,
                                       stderr=subprocess.DEVNULL).strip()
        ancestor = subprocess.run(['git', 'merge-base', '--is-ancestor', reg['event_base'], main_tip], cwd=root,
                                  timeout=60, stderr=subprocess.DEVNULL).returncode
        changed = subprocess.check_output(['git', 'diff', '--name-only', '-z', '--no-renames',
                                           reg['event_base'], main_tip], cwd=root, timeout=60,
                                          stderr=subprocess.DEVNULL).split(b'\0')
    except (subprocess.SubprocessError, OSError):
        raise receipts.ReceiptError('Async session: Main history from event_base to the tip is unavailable') from None
    p.require(head == commit, 'landing check must run from a checkout of the registered head')
    p.require(ancestor == 0, 'Main tip does not descend from the registered event_base')
    changed = sorted({name.decode('utf-8', 'surrogateescape') for name in changed if name})
    closure = proof_closure(root, commit)
    touched = [name for name in changed if in_closure(closure, name)]
    p.require(not touched, 'Main changed the registered proof closure since event_base: ' + ', '.join(touched[:8])
              + (' ...' if len(touched) > 8 else ''))
    return {'schema_version': 2, 'kind': 'landing_compatibility', 'repository': reg['repository'],
            'pr': reg['pr'], 'source': source, 'head_commit_id': commit, 'event_base': reg['event_base'],
            'main_tip': main_tip, 'main_changed_files': len(changed),
            'main_changed_sha256': p.digest(p.canonical(changed)),
            'closure_files': len(closure['files']), 'closure_prefixes': len(closure['prefixes']),
            'closure_sha256': p.digest(p.canonical(closure)), 'compatible': True}


def accounting_method_digest(files):
    return p.digest(p.canonical([[name,files[name]] for name in ACCOUNTING_METHOD_FILES]))


ACCOUNTING_METHOD={'schema_version':3,'id':'typed_physical_caps_v1',
    'source_report':'source_global_symbolic',
    'semantic_lifetime':'session_unique_global_record',
    'auxiliary_lifetime':'session_retained_unit_member',
    'physical_policy_location':'ResourcePlan.source_bounds_utf8',
    'table_map':'validation/async_physical_table_map.json',
    'prefix_policy':'late_bound_per_batch_v1',
    'content_identity':'sha256_original_bytes',
    'snapshot_chunk_bytes':1048576,
    'cache_presence_grants_authority':False,
    'retained_download_multiplier':6,
    'dispatch_log_reserve_bytes':32*p.MIB,
    'capability_dispatch_allowance':3}


def accounting_inventory(source_receipt):
    p.require({r['record_kind'] for r in source_receipt['records']}==set(p.SEMANTIC_SOURCE_KINDS),
              'complete semantic source-global inventory differs')
    return {r['kind']:{'class':r['role'],'lifetime':r['lifetime']}
            for r in p.accounting_mapping() if r['role']!='operational_overhead'}


def operational_bounds(encoded,reg,root,measurement_pending=False):
    value=p.decode(encoded.encode())
    p.closed(value,('schema_version','accounting_rule','source_receipt','record_allowances','physical_policy',
                    'calibration','accounting_method_sha256','measurement_sha256'))
    p.require(type(value['schema_version']) is int and value['schema_version']==3
              and value['accounting_rule']=='typed_physical_caps_v1'
              and value['accounting_method_sha256']==reg['accounting_method_sha256'],
              'obsolete/unregistered physical accounting wrapper')
    source_map=physical.load_source_map(Path(root)/'validation/async_physical_table_map.json')
    physical.validate_physical_policy(value['physical_policy'],source_map)
    p.require(type(value['record_allowances']) is dict and value['record_allowances'],'missing operational kinds')
    for kind,row in value['record_allowances'].items():
        p.closed(row,('records','max_encoded_bytes'))
        p.require(type(row['records']) is int and row['records']>=0
                  and type(row['max_encoded_bytes']) is int and 0<row['max_encoded_bytes']<=p.MAX_MEMBER,
                  'invalid operational count/full-encoding cap')
        if kind in AUXILIARY_ACCOUNTING_KINDS:
            p.require(row['max_encoded_bytes']<=p.SMALL,'auxiliary metadata cap exceeds8MiB')
    if not measurement_pending:
        p.require(type(value['calibration']) is dict and type(value['measurement_sha256']) is str
                  and p.SHA.fullmatch(value['measurement_sha256']), 'actual physical calibration unavailable')
    else:
        p.require(value['calibration'] is None and value['measurement_sha256'] is None,
                  'draft calibration must be explicitly absent')
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


def validate_control_identity(value,reg,metadata,run_id,job,root):
    schema=p.read(Path(root)/p.CONTROL_SCHEMA)
    frozen=p.read(Path(root)/p.SCHEMA_PATH)
    transport_schema=p.read(Path(root)/'verification/async-driver-refinement/paged-catalog-transport.schema.json')
    def local(node):
        if isinstance(node,dict):
            reference=node.get('$ref','')
            if reference.startswith('paged-proof.schema.json#/$defs/'):
                return {'$ref':'#/$defs/'+reference.split('/')[-1]}
            if reference.startswith('paged-catalog-transport.schema.json#/$defs/'):
                return {'$ref':'#/$defs/'+reference.split('/')[-1]}
            return {k:local(v) for k,v in node.items()}
        if isinstance(node,list):return [local(v) for v in node]
        return node
    definitions={**{k:local(v) for k,v in frozen['$defs'].items()},**{k:local(v) for k,v in transport_schema['$defs'].items()}}
    schema=local(schema);schema['$defs']=definitions
    p.validate(value,{k:v for k,v in schema.items() if k not in ('$schema','$id','$defs')},schema)
    unit=p.envelope(value['work_unit'],'WorkUnit',root)
    role=value['role'];origin=value['origin']
    p.require(all(origin[k]==reg[k] for k in ('repository','pr','event_base'))
              and origin['workflow_id']==p.WORKFLOW and origin['event_name']=='workflow_dispatch'
              and (origin['run'],origin['job'],origin['task'],origin['attempt'])==(run_id,job['id'],job['task_id'],1)
              and value['registration_sha256']==p.digest(p.canonical(reg))
              and value['source_archive_sha256']==reg['source_archive_sha256']
              and value['source']==unit['source']==reg['source']
              and value['session_id']==unit['session_id']==metadata['session_id']
              and value['unit_ordinal']==unit['unit_ordinal']==metadata['unit_ordinal']
              and value['expected_prior_checkpoint']==unit['expected_prior_checkpoint']
              and len(value['artifact_receipts'])==len(metadata['parts']), 'carrier producer/plan identity differs')
    if role=='completion':
        p.require(origin['operation']=='execute' and origin['mode']=='complete'
                  and (unit['run'],unit['job'],unit['task'],unit['attempt'])==(run_id,job['id'],job['task_id'],1)
                  and unit['status']=='complete' and p.envelope(value['completion'],'Completion',root)['accepted'] is True
                  and value['transfer_batch'] is None and value['activation'] is None,
                  'completion carrier shape is not complete')
    else:
        batch=value['transfer_batch'];execution=batch['execution_origin']
        p.require(origin['mode']=='unit' and batch['publication_origin']==origin
                  and (unit['run'],unit['job'],unit['task'],unit['attempt'])==
                      (execution['run'],execution['job'],execution['task'],1)
                  and unit['status'] in ('checkpointed_incomplete','closed_unqualified')
                  and value['completion'] is None and metadata['operation']==origin['operation'],
                  'publication carrier does not name the original execution WorkUnit and actual publisher')
        # The original execution is always `execute`; any later publisher is `publish`.
        p.require(origin['operation']==('execute' if batch['batch_ordinal']==0 and origin==execution else 'publish')
                  and execution['operation']=='execute', 'publication operation/origin ordinal differs')
        if role=='unit_checkpoint':
            p.require(batch['purpose']=='activate' and value['activation']['execution_origin']==execution,
                      'activation carrier shape differs')
        elif role=='transport_conformance':
            # The rehearsal has transfer batches, then ONE restore/validation batch of purpose `activate`
            # that carries the case result. Rehearsal never activates a checkpoint.
            case=value['transport_conformance']
            p.require(case is not None and value['activation'] is None
                      and batch['purpose']==('activate' if case['result'] is not None else 'transfer'),
                      'rehearsal carrier shape differs')
        else:
            p.require(batch['purpose']=='transfer' and value['activation'] is None, 'transfer carrier shape differs')
    utc(value['postcheck']['checked_at'])
    p.require(value['postcheck']['source_commit']==reg['source']['commit']
              and value['postcheck']['merge_base']==reg['event_base'], 'carrier live source/merge base differs')


def finalize_uploads(output,root):
    """Mandatory synchronous final step; it cannot certify its own job success."""
    event=p.read(os.environ['GITHUB_EVENT_PATH']);inputs=event['inputs']
    if inputs['operation']=='probe':
        return capability_finalizer(output,root)
    draft=p.read(Path(output)/'finalizer-state.json');state=draft['state'];reg=state['registration']
    if draft.get('role') in ('publication_batch','unit_checkpoint','transport_conformance'):
        return finalize_publication(output,root,draft,inputs)
    controller_memory_limit(state['resource_plan'])
    metadata=p.read(Path(output)/'upload-plan.json')
    api=api_environment(Path(output)/'finalizer-api')
    api.bind_storage(state['resource_plan'],reg,draft['control']['session_id'])
    run_id=int(os.environ['GITHUB_RUN_ID'])
    _,job,payload=p.producer(api,run_id,reg,inputs['mode'],('running','in_progress'))
    unit=draft['unit'];record=p.envelope(unit,'WorkUnit',root)
    role='completion'
    origin={k:record[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode','run','job','task','attempt')}
    origin['operation']='execute';origin['dispatch_ordinal']=state['next_dispatch_ordinal']
    carrier={'schema_version':2,'kind':'async_session_control','role':role,
        'registration_sha256':p.digest(p.canonical(reg)),'source_archive_sha256':reg['source_archive_sha256'],
        'source':reg['source'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
        'origin':origin,'previous_control_sha256':state.get('control_sha256'),
        'expected_prior_checkpoint':record['expected_prior_checkpoint'],
        'checkpoint':p.ref(state['checkpoint'],p.digest(p.canonical(state['checkpoint']))),
        'work_unit':unit,'artifact_receipts':[],'completion':None,'postcheck':None,'pending_publication':None,
        'transfer_batch':None,'activation':None,'transport_conformance':None}
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
        'current_base':live['base']['sha'],'merge_base':live['merge_base'],
        'pr_state':live['state'],'draft':live['draft'],
        'archived_fresh_head':None,'main_receipts':[],'main_conditions_sha256':None,'semantic_verification':None}
    if role=='completion':
        records=authenticate_main(api,reg,draft['control']['main_run'],root,verified=draft['verified'])
        p.require(records==draft['main_records'],'Main prerequisite receipts changed after uploads')
        parts={n:Path(output)/'finalizer-parts'/str(n) for n in range(len(metadata['parts']))}
        fresh=p.read(find_member(parts,'fresh-head.json'))
        current={'source':reg['source'],'repository':reg['repository'],'pr':reg['pr'],'head':live['head']['sha'],
            'event_base':reg['event_base'],'merge_base':live['merge_base'],
            'state':live['state'],'draft':live['draft'],
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




def finalize_publication(output,root,draft,inputs):
    """Post-upload publisher finalizer for one transfer/activate/rehearsal batch.

    Native receipts exist only after upload, so the batch receipt, the pending
    publication after it and the carrier are completed here without any hash cycle.
    A published byte is never credited by this step; replay recomputes coverage.
    """
    state=draft['state'];reg=state['registration'];role=draft['role']
    controller_memory_limit(state['resource_plan'])
    metadata=p.read(Path(output)/'upload-plan.json')
    api=api_environment(Path(output)/'finalizer-api')
    api.bind_storage(state['resource_plan'],reg,draft['control']['session_id'])
    run_id=int(os.environ['GITHUB_RUN_ID'])
    _,job,payload=p.producer(api,run_id,reg,'unit',('running','in_progress'))
    unit=draft['unit'];record=p.envelope(unit,'WorkUnit',root)
    operation=inputs['operation']
    origin=current_origin(state,reg,run_id,job,operation)
    batch=draft['batch']
    candidate=draft['candidate_checkpoint_sha256']
    accepted=state['checkpoint']
    checkpoint=(draft['candidate_checkpoint'] if role=='unit_checkpoint' else
                None if accepted is None else p.ref(accepted,p.digest(p.canonical(accepted))))
    carrier={'schema_version':2,'kind':'async_session_control','role':role,
        'registration_sha256':p.digest(p.canonical(reg)),'source_archive_sha256':reg['source_archive_sha256'],
        'source':reg['source'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
        'origin':origin,'previous_control_sha256':state.get('control_sha256'),
        'expected_prior_checkpoint':record['expected_prior_checkpoint'],'checkpoint':checkpoint,
        'work_unit':unit,'artifact_receipts':[],'completion':None,'postcheck':None,'pending_publication':None,
        'transfer_batch':None,'activation':None,'transport_conformance':None}
    binding=p.control_binding(carrier)
    print('ASYNC-PROOF-FINALIZER-BEGIN 1 '+binding,flush=True)
    _,lines=p.producer_log(api,run_id,job,'unit')
    listed=api.pages('/actions/runs/'+str(run_id)+'/artifacts',field='artifacts')
    selected=[];found=[]
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
                'expected_prior_checkpoint':record['expected_prior_checkpoint'],'produced_checkpoint':candidate}}
            p.envelope(receipt,'ArtifactReceipt',root);found.append(receipt)
        finally:archive.unlink(missing_ok=True)
    p.validate_upload_order(lines,selected,len(lines))
    carrier['artifact_receipts']=found
    if batch['batch_ordinal']==0 and operation=='execute':
        # Only the direct original execution's receipts become WorkUnit authority.
        record['artifact_roots']=[p.ref(v,v['record']['artifact_id']) for v in found]
    batch={**batch,'artifact_receipts':found,'retained_artifact_bytes':sum(r['record']['zip_bytes'] for r in found)}
    carrier['transfer_batch']=batch
    pending_after={**draft['pending_before'],
        'previous_batch':snapshots.content_ref('transfer_batch_receipt',p.canonical(batch)),
        'coverage_sha256':batch['coverage_after_sha256'],'coverage_count':batch['coverage_count'],
        'next_batch_ordinal':draft['pending_before']['next_batch_ordinal']+1}
    if role=='unit_checkpoint':
        carrier['activation']=draft['activation']
    else:
        carrier['pending_publication']=pending_after
    if role=='transport_conformance':
        carrier['transport_conformance']=draft['conformance']
    live=live_pr(api,reg)
    carrier['postcheck']={'checked_at':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),
        'source_commit':reg['source']['commit'],'current_base':live['base']['sha'],'merge_base':live['merge_base'],
        'pr_state':live['state'],'draft':live['draft'],'archived_fresh_head':None,'main_receipts':[],'main_conditions_sha256':None,
        'semantic_verification':None}
    local_parts={n:Path(output)/'finalizer-parts'/str(n) for n in range(len(metadata['parts']))}
    enforce_control_caps(state,{'parts':{},'control':carrier},root)
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
    return job_output_directory(owned/control['session_id'], os.environ['GITHUB_RUN_ID'])


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
        # The finalizer is the last step that can decide the job's result. The only step allowed after
        # it is the fleet-wide ownership repair every root-container job carries (test_contracts); it runs
        # under always() but cannot mask a failed finalizer, because a job fails when any step fails and
        # no step here may continue-on-error.
        expected_restore = """
      - name: Restore persistent runner workspace ownership
        if: always()
        run: |
          owner="$(stat -c '%u:%g' "$GITHUB_WORKSPACE")"
          chown -R "$owner" "$GITHUB_WORKSPACE"
"""
        p.require(last.strip()==(expected_finalizer.rstrip('\n')+expected_restore).strip(),
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



def preflight_publication(output,metadata,draft):
    """Worst-case whole carrier of one publisher batch, reserved before any upload."""
    state=draft['state'];reg=state['registration'];role=draft['role']
    unit=copy.deepcopy(draft['unit']);record=unit['record'];maximum=(1<<63)-1
    receipts=[]
    for part in metadata['parts']:
        receipts.append({'schema_version':6,'kind':'ArtifactReceipt','record':{
            'artifact_id':maximum-part['ordinal'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
            'run':maximum,'job':maximum,'task':maximum,'attempt':1,'source':reg['source'],
            'name':part['name'],'expired':False,'zip_sha256':'f'*64,'zip_bytes':p.MAX_ARTIFACT,
            'extracted_bytes':sum(m['bytes'] for m in part['members']),'member_count':len(part['members']),
            'members':part['members'],'upload_log_sha256':'f'*64,'event_payload_sha256':'f'*64,
            'expected_prior_checkpoint':record['expected_prior_checkpoint'],
            'produced_checkpoint':draft['candidate_checkpoint_sha256']}})
    record['artifact_roots']=[p.ref(v,v['record']['artifact_id']) for v in receipts]
    worst_ref={'kind':'transfer_batch_receipt','sha256':'f'*64,'bytes':maximum}
    batch={**draft['batch'],'artifact_receipts':receipts,'retained_artifact_bytes':maximum,
           'coverage_after_sha256':'f'*64,'coverage_count':maximum}
    origin={'repository':reg['repository'],'pr':reg['pr'],'event_base':reg['event_base'],'workflow_id':p.WORKFLOW,
            'event_name':'workflow_dispatch','mode':'unit','operation':'publish','run':maximum,'job':maximum,
            'task':maximum,'attempt':1,'dispatch_ordinal':state['next_dispatch_ordinal']}
    batch['publication_origin']=origin
    pending={**draft['pending_before'],'previous_batch':worst_ref,'coverage_sha256':'f'*64,'coverage_count':maximum,
             'next_batch_ordinal':maximum}
    accepted=state['checkpoint']
    checkpoint=(draft['candidate_checkpoint'] if role=='unit_checkpoint' else
                None if accepted is None else p.ref(accepted,p.digest(p.canonical(accepted))))
    post={'checked_at':'9999-12-31T23:59:59Z','source_commit':reg['source']['commit'],
        'current_base':reg['event_base'],'merge_base':reg['event_base'],'pr_state':'open','draft':False,
        'archived_fresh_head':None,'main_receipts':[],'main_conditions_sha256':None,'semantic_verification':None}
    carrier={'schema_version':2,'kind':'async_session_control','role':role,
        'registration_sha256':p.digest(p.canonical(reg)),'source_archive_sha256':reg['source_archive_sha256'],
        'source':reg['source'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
        'origin':origin,'previous_control_sha256':state.get('control_sha256') or 'f'*64,
        'expected_prior_checkpoint':record['expected_prior_checkpoint'],'checkpoint':checkpoint,
        'work_unit':unit,'artifact_receipts':receipts,'completion':None,'postcheck':post,
        'pending_publication':None if role=='unit_checkpoint' else pending,'transfer_batch':batch,
        'activation':draft['activation'] if role=='unit_checkpoint' else None,
        'transport_conformance':draft['conformance'] if role=='transport_conformance' else None}
    limit=len(p.canonical(carrier));plan=state['resource_plan']['record']
    wrapper=p.decode(plan['source_bounds_utf8'].encode())
    p.require(limit<=min(p.SMALL,wrapper['record_allowances']['async_session_control']['max_encoded_bytes']),
              'anticipated whole control cannot fit admitted carrier cap')
    frame_bytes=4*((limit+2)//3)+512+(4*((limit+2)//3)//2048+1)*96
    p.require(frame_bytes+len(p.canonical(metadata))+32*64*1024<=32*p.MIB,
              'anticipated control/planning/native sections cannot fit raw task log reservation')
    p.write(output/'control-reservation.json',{'maximum_control_bytes':limit,'maximum_frame_bytes':frame_bytes})


def preflight_carrier(output,metadata):
    draft=p.read(output/'finalizer-state.json');state=draft['state'];reg=state['registration']
    if draft.get('role') in ('publication_batch','unit_checkpoint','transport_conformance'):
        return preflight_publication(output,metadata,draft)
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
    role='completion'
    origin={k:record[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode','run','job','task','attempt')}
    origin['operation']='execute';origin['dispatch_ordinal']=state['next_dispatch_ordinal']
    completion_value=p.read(output/'completion-draft.json') if role=='completion' else None
    post={'checked_at':'9999-12-31T23:59:59Z','source_commit':reg['source']['commit'],
        'current_base':reg['event_base'],'merge_base':reg['event_base'],'pr_state':'open','draft':False,
        'archived_fresh_head':None,'main_receipts':[],'main_conditions_sha256':None,'semantic_verification':None}
    if completion_value:
        post.update(archived_fresh_head=completion_value['record']['current_head_recheck'],
            main_receipts=completion_value['record']['applicable_main_receipts'],main_conditions_sha256='f'*64,
            semantic_verification={'part_ordinal':31,'member_path':'complete-verification.json','bytes':p.MAX_MEMBER,'sha256':'f'*64})
    carrier={'schema_version':2,'kind':'async_session_control','role':role,
        'registration_sha256':p.digest(p.canonical(reg)),'source_archive_sha256':reg['source_archive_sha256'],
        'source':reg['source'],'session_id':record['session_id'],'unit_ordinal':record['unit_ordinal'],
        'origin':origin,'previous_control_sha256':state.get('control_sha256'),
        'expected_prior_checkpoint':record['expected_prior_checkpoint'],
        'checkpoint':p.ref(state['checkpoint'],p.digest(p.canonical(state['checkpoint']))),
        'work_unit':unit,'artifact_receipts':receipts,'completion':completion_value,'postcheck':post,
        'pending_publication':None,'transfer_batch':None,'activation':None,'transport_conformance':None}
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
                  and not request['unit_runs'] and not request['attempt_runs'] and not request['resume_decisions'] and not request['uncertain_dispatches'] and request['probe_run'] is None,'capability event is not a fresh probe')
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
    """Every authenticated attempt, by ordinal, reconciled with the uncertain dispatches the controls declare.

    A dispatch's ordinal is the number of runs before it plus the number of uncertain dispatches before it.
    An ordinal without a run must be declared uncertain (by the current control or by the control of a
    later run, which prove the controller settled it before dispatching again); an ordinal that is both a
    run and declared uncertain is a late run that no control accounted for and refuses the session.
    """
    found={}
    declared={row['ordinal']:row for row in declared_uncertain(control)}
    for run in api.pages('/actions/runs',{'workflow_id':p.WORKFLOW},field='workflow_runs'):
        if run.get('id')==current_run or run.get('workflow_id')!=p.WORKFLOW or not p.dispatched(run):continue
        event=p.decode(run.get('event_payload','').encode());inputs=event.get('inputs',{})
        if inputs.get('operation') not in ('execute','publish'):continue
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
        ordinal=control_ordinal(prior)
        p.require(ordinal not in found,'duplicate/reused actual attempted ordinal')
        for row in declared_uncertain(prior):
            p.require(declared.setdefault(row['ordinal'],row)==row,'conflicting uncertain dispatch declarations')
        actual,job,_=p.producer(api,run['id'],reg,inputs['mode'],('success','failure','cancelled'))
        found[ordinal]={'run':actual,'job':job,'control':prior,'mode':inputs['mode'],'ordinal':ordinal,
                        'operation':inputs['operation'],'conformance':prior['transport_conformance']}
    late=sorted(set(declared)&set(found))
    p.require(not late,'dispatch ordinal %d was settled as uncertain but its run appeared; no attempt splicing'%(late[0] if late else -1))
    p.require(sorted({*found,*declared})==list(range(len(found)+len(declared))),'unexplained attempted-ordinal gap')
    for ordinal,item in found.items():
        p.require(declared_uncertain(item['control'])==[row for _,row in sorted(declared.items()) if row['ordinal']<ordinal],
                  'a dispatch control omitted or changed an uncertain dispatch before it')
    items=Inventory(found[i] for i in sorted(found))
    items.uncertain=tuple(row for _,row in sorted(declared.items()))
    if require_declared:
        p.require([item['run']['id'] for item in items]==control['attempt_runs'],
                  'actual attempted inventory missing from controller; reconcile before dispatch')
        p.require(list(items.uncertain)==declared_uncertain(control),
                  'an uncertain dispatch declared by an attempt is missing from the controller; reconcile before dispatch')
    return items


def reject_prior_ordinal_dispatch(api,reg,control,current_run):
    return attempt_inventory(api,reg,control,current_run)


def validate_resume_decision(api,decision,failed,state,reg):
    p.require(type(decision) is dict and state['initial_image'] is not None,
              'terminal failure needs an explicit diagnosed infrastructure resume decision; no new Init')
    p.closed(decision,('schema_version','kind','registration_sha256','source','session_id','failed_run',
        'failed_job','failed_task','failed_ordinal','expected_prior_checkpoint','previous_control_sha256',
        'failed_log_sha256','reason','diagnosis','repair','decided_at','acceptance'))
    ordinal=control_ordinal(failed['control'])
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
    wrapper=p.decode(plan['source_bounds_utf8'].encode())
    return {key:plan[key] for key in ('unit_byte_allowance','unit_record_allowance','maximum_units',
        'total_session_disk_bytes','total_retained_artifact_bytes','resident_bytes','resident_rss_bytes')} | {
        'physical_policy_sha256':p.digest(p.canonical(wrapper['physical_policy'])),
        'record_allowances_sha256':p.digest(p.canonical(wrapper['record_allowances'])),
        'source_receipt_sha256':p.digest(p.canonical(wrapper['source_receipt'])),
        'unit_predecessor_allowance':plan['unit_predecessor_allowance']}



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
            'compiled_sources','geometry','sqlite_version','sqlite_source_id','page_size','page_count',
            'freelist_count','indexes','tables','empty_table_rows','acceptance')
    p.closed(value,fields)
    geometry=value['geometry']
    p.closed(geometry,('database_role','sqlite_source_id','schema_sha256','page_size','page_count','tables','indexes'))
    p.require(value['schema_version']==1 and value['engine']=='rust_rusqlite' and value['acceptance'] is False
              and value['source_path']=='verification/async-driver-refinement/paged-catalog.sql'
              and value['source_sha256']==reg['source_files'][value['source_path']]
              and p.file_sha(Path(directory)/'catalog-calibration.json')==execution['geometry_sha256'],
              'actual Rust SQLite/DDL calibration binding differs')
    compiled=CATALOG_COMPILED_SOURCES
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
    # The Rust engine's own typed geometry is the catalog_main family input; it must describe
    # exactly the database whose scalar figures were just validated, never another one.
    p.require(geometry['database_role']=='rust_catalog'
              and geometry['sqlite_source_id']==value['sqlite_source_id']
              and p.SHA.fullmatch(geometry['schema_sha256']) is not None
              and geometry['page_size']==value['page_size'] and geometry['page_count']==value['page_count']
              and all(type(geometry[k]) is list and all(type(name) is str and name for name in geometry[k])
                      and len(geometry[k])==len(set(geometry[k])) for k in ('tables','indexes'))
              and len(geometry['tables'])==value['tables'] and len(geometry['indexes'])==value['indexes']
              and set(geometry['tables'])==set(value['empty_table_rows']),
              'actual Rust typed geometry differs from the validated calibration')
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
    return {**control,'attempt_runs':ids,'unit_runs':successes,'uncertain_dispatches':list(items.uncertain),
            'resume_decisions':[decisions[k] for k in sorted(decisions)]},None

if __name__ == '__main__':
    try:
        main()
    except (receipts.ReceiptError, KeyError, OSError, ValueError, subprocess.SubprocessError) as error:
        reason=str(error)[:512] if isinstance(error,receipts.ReceiptError) else type(error).__name__
        print('Async session refused: '+reason,file=sys.stderr)
        raise SystemExit(1) from None

