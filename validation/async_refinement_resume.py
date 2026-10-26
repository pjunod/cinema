"""Authenticated same-PR transport for bounded async refinement evidence.

Transport does not accept a case result. The current runner reparses every
restored source/tool/command/log/graph identity before reusing a success.
"""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import zipfile
import io
import subprocess

from validation import worker_control_resume as transport
from validation import python_unit_receipts as receipts
from validation import main_preflight_adoption as adoption
from validation import main_unit_receipts as main_receipts

MIB = 1024 * 1024
MAX_GRAPH_BYTES = 256 * MIB
MAX_TOTAL_BYTES = 512 * MIB
MAX_SMALL_BYTES = 8 * MIB
MAX_FILES = 300
INVOCATION_PATHS = (
    'scripts/check-async-driver-refinement',
    'verification/async-driver-refinement/checker.py',
    'verification/async-driver-refinement/product-schema.json',
    'verification/async-driver-refinement/store-tests.json',
    'rust-toolchain.toml',
    '.github/workflows/main-fast-lane.yml',
    'verification/durable-jobs/toolchain.json',
    'validation/async_refinement_resume.py',
    'validation/worker_control_resume.py',
)
ARTIFACT_PREFIX = 'async-driver-refinement'
PROOF_JOB = 'fast async driver refinement gate'
JOB_ID = 'async_refinement'
ARTIFACT_PHASES = (('final',ARTIFACT_PREFIX),('prefix',ARTIFACT_PREFIX+'-prefix'))
MODEL_NAMES = {'AsyncDriverRefinement.tla','BoundaryControls.tla','SourceTables.tla'}
CONFIG_NAMES = {'safety','cleanup','selected-success','nonvacuity','control-weak-admission',
    'control-no-claim-win','control-withheld-join','control-old-poll-policy',
    'control-drop-before-join','control-local-revocation','control-unknown-as-rollback'}
METADATA = {'resume-origin.json','snapshot-identity.json'}


def identical_invocation(prior_commit, current_commit, workspace):
    prior = transport.invocation_digests(prior_commit,workspace,paths=INVOCATION_PATHS)
    current = transport.invocation_digests(current_commit,workspace,paths=INVOCATION_PATHS)
    receipts.require(prior == current,'Prior async invocation tuple differs; no historical exceptions are allowed')
    return {'prior_commit':prior_commit,'current_commit':current_commit,
      'prior_digests':prior,'current_digests':current,'decision':'identical_invocation',
      'policy_path':None,'policy_sha256':None,'reviewed_compatibility':None}


def transport_spec(attempt=1):
    return transport.TransportSpec(workflow='main-fast-lane.yml',proof_job=PROOF_JOB,
      artifact_prefix=ARTIFACT_PREFIX,trust=identical_invocation,
      max_archive_bytes=MAX_TOTAL_BYTES,label='async refinement',artifact_phases=ARTIFACT_PHASES,
      same_run_attempt=attempt,upload_sha256=uploaded_sha256,validate_archive=validate_snapshot_archive,attempt_log=attempt_log)


def attempt_log(api,latest,attempt):
    """Documented Forgejo historical log selection; never invent old task state."""
    receipts.require(type(attempt) is int and 0 < attempt <= latest['attempt'], 'Async historical attempt differs')
    if attempt == latest['attempt']:
        receipts.require(latest.get('status') in ('success','failure','cancelled'),'Async latest attempt is not terminal')
    lines = main_receipts.log_lines(api.bytes('/actions/jobs/'+str(receipts.positive(latest['id']))+'/logs',{'attempt':attempt}))
    pattern = r'Runner [0-9a-f-]+ \(version:[^)]*\) received task ([1-9][0-9]*) of job '+JOB_ID+r', triggered by event: pull_request'
    preambles = [(i,re.fullmatch(pattern,line)) for i,line in enumerate(lines) if line.startswith('Runner ')]
    receipts.require(len(preambles) == 1 and preambles[0][0] == 0 and preambles[0][1] is not None,
                     'Async historical runner preamble missing/ambiguous')
    task = int(preambles[0][1][1])
    if attempt == latest['attempt']:
        receipts.require(task == latest['task_id'],'Async terminal task differs from API')
        return latest,lines
    job = {key:latest[key] for key in ('id','name','repo_id','run_id')}
    job.update(attempt=attempt,task_id=task,status=None,
      terminal_basis='superseded_by_authenticated_later_attempt',latest_attempt=latest['attempt'])
    return job,lines


def uploaded_sha256(lines,name,artifact_id,size,prior,job,scope):
    head = '[command]/usr/bin/git log -1 --format=%H'
    positions = [i for i,line in enumerate(lines) if line == head]
    receipts.require(len(positions) == 1 and positions[0]+1 < len(lines)
      and lines[positions[0]+1] == prior['commit_sha'], 'Async pinned checkout head differs')
    receipts.require(any(re.fullmatch(r'Runner [0-9a-f-]+ \(version:[^)]*\) received task '+str(job['task_id'])+
      r' of job '+JOB_ID+r', triggered by event: pull_request',line) for line in lines),
      'Async job log task identity differs')
    return transport.upload_transaction_sha256(lines,name,artifact_id,size)


def current_context(workspace,run,attempt):
    """Positive attempts are confined to this authenticated async proof job."""
    receipts.require(os.environ.get('GITHUB_JOB') == JOB_ID and type(attempt) is int and attempt > 0,
                     'Async context requires its positive current job attempt')
    event = receipts.bounded_json(Path(os.environ['GITHUB_EVENT_PATH']).read_bytes())
    pull,repo = event['pull_request'],event['repository']
    receipts.require(os.environ.get('GITHUB_EVENT_NAME') == 'pull_request'
      and pull['head']['repo']['id'] == pull['base']['repo']['id'] == repo['id']
      and event['number'] == pull['number'] and pull['draft'] is False
      and pull['state'] == 'open' and pull['base']['ref'] == 'main'
      and repo['full_name'] == os.environ['GITHUB_REPOSITORY'], 'Async context is not a ready same-repository main PR')
    commit = subprocess.check_output(['git','rev-parse','HEAD'],cwd=workspace,text=True,timeout=10).strip()
    receipts.require(transport.COMMIT.fullmatch(commit) and commit == pull['head']['sha'] == os.environ['GITHUB_SHA']
      and not subprocess.check_output(['git','diff','--name-only','HEAD'],cwd=workspace,timeout=10),
      'Async checkout/source mismatch')
    api_root = os.environ['GITHUB_API_URL']
    receipts.require(api_root.startswith(os.environ['GITHUB_SERVER_URL'].rstrip('/')+'/'),'Async API origin mismatch')
    api = receipts.API(api_root,os.environ['GITHUB_REPOSITORY'],os.environ.get('GITHUB_TOKEN'))
    current = api.get('/actions/runs/'+str(run));scope = main_receipts.identity(api,event['number'],commit)
    receipts.require(current.get('id') == run and current.get('commit_sha') == commit
      and adoption.bind_event(current,scope) == event,'Async current API event/head differs')
    live = api.get('/pulls/'+str(scope['pr']))
    receipts.require(live['number'] == scope['pr'] and live['head']['sha'] == commit
      and live['base']['sha'] == pull['base']['sha'] and live['base']['ref'] == 'main'
      and live['draft'] is False and live['state'] == 'open'
      and live['head']['repo']['id'] == live['base']['repo']['id'] == repo['id'],
      'Async current PR moved/closed/drafted')
    jobs = [job for job in api.pages('/actions/runs/'+str(run)+'/jobs')
      if job.get('name') == PROOF_JOB and job.get('attempt') == attempt]
    receipts.require(len(jobs) == 1 and type(jobs[0].get('attempt')) is int
      and type(jobs[0].get('run_id')) is int and jobs[0]['run_id'] == run
      and type(jobs[0].get('repo_id')) is int and jobs[0]['repo_id'] == scope['repository']
      and type(jobs[0].get('task_id')) is int and jobs[0]['task_id'] > 0
      and jobs[0].get('status') in ('running','in_progress'), 'Async current proof job/attempt differs')
    receipts.positive(jobs[0].get('id'))
    return api,scope,commit,run,jobs[0]


def recheck_current_context(workspace,run,attempt,scope,commit,job):
    """Current integrity failure is fatal, never a rejected-history fallback."""
    _,confirmed_scope,confirmed_commit,confirmed_run,confirmed_job = current_context(workspace,run,attempt)
    receipts.require(confirmed_scope == scope and confirmed_commit == commit and confirmed_run == run
      and all(confirmed_job[key] == job[key] for key in ('id','task_id','attempt')),
      'Async current proof context changed during restore')


def selected(parts, is_dir=False):
    if is_dir: return False
    if len(parts) == 1:
        return (parts[0] in {'provenance.json','driver-graph.json','product-bundle.json'}
          or re.fullmatch(r'rust-(?:[a-z0-9_-]{1,96}|control-C[49])\.log',parts[0]) is not None)
    if len(parts) == 2 and parts[0] == 'tlc':
        name = parts[1]
        return name == 'receipt.json' or re.fullmatch(r'[A-Za-z0-9_-]{1,64}\.log',name) is not None
    return False


def member_limit(parts):
    return MAX_GRAPH_BYTES if parts in (('driver-graph.json',),('product-bundle.json',),('tlc','SourceTables.tla')) else MAX_SMALL_BYTES


def extract_evidence(data, destination):
    # Validate all headers before creating any evidence. Metadata from a prior
    # run is permitted in the compact archive but never hydrated as current origin.
    receipts.require(len(data) <= MAX_TOTAL_BYTES,'Async archive exceeds download bound')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist()
        receipts.require(0 < len(members) <= MAX_FILES,'Async archive member count invalid')
        for member in members:
            parts = transport.safe_member(member.filename.rstrip('/') if member.is_dir() else member.filename)
            mode = member.external_attr >> 16
            receipts.require(stat.S_IFMT(mode) in (0,stat.S_IFREG,stat.S_IFDIR), 'Async archive contains a special file')
            if member.is_dir(): continue
            receipts.require(member.file_size <= member_limit(parts), 'Async archive member exceeds its kind limit')
    limits = transport.ExtractionSpec(select=selected,max_archive_bytes=MAX_TOTAL_BYTES,
      max_files=MAX_FILES,max_member_bytes=MAX_GRAPH_BYTES,max_total_bytes=MAX_TOTAL_BYTES,
      max_hydrated_bytes=MAX_TOTAL_BYTES,max_hydrated_files=MAX_FILES)
    return transport.extract_evidence(data,destination,spec=limits)


def validate_snapshot_archive(data,artifact,job,prior,attempt,phase):
    """Authenticate the phase envelope only; never accept a transported case."""
    receipts.require(phase in dict(ARTIFACT_PHASES) and 0 < len(data) <= MAX_TOTAL_BYTES,
                     'Async snapshot phase/archive bound differs')
    payload,files,total = {},{},0
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist();seen = set()
        receipts.require(0 < len(members) <= MAX_FILES,'Async snapshot member bound differs')
        for member in members:
            parts = transport.safe_member(member.filename.rstrip('/') if member.is_dir() else member.filename)
            receipts.require(parts not in seen,'Async snapshot duplicate member');seen.add(parts)
            mode = stat.S_IFMT(member.external_attr >> 16)
            receipts.require(((member.is_dir() and mode in (0,stat.S_IFDIR)) or (not member.is_dir() and mode in (0,stat.S_IFREG)))
              and not member.flag_bits & 1 and member.compress_type in (zipfile.ZIP_STORED,zipfile.ZIP_DEFLATED),'Async snapshot unsafe member')
            if member.is_dir():continue
            receipts.require(member.compress_size > 0 and member.file_size/member.compress_size <= 250,
                             'Async snapshot compression ratio differs')
            receipts.require(selected(parts) or len(parts)==1 and parts[0] in METADATA,'Async snapshot unknown member')
            bound = member_limit(parts)
            receipts.require(0 < member.file_size <= bound and total+member.file_size <= MAX_TOTAL_BYTES,
                             'Async snapshot expanded bound differs')
            digest = hashlib.sha256();count=0;small=[]
            with archive.open(member) as stream:
                while chunk := stream.read(min(MIB,bound+1-count)):
                    count += len(chunk);receipts.require(count <= bound,'Async snapshot expanded member exceeds bound')
                    digest.update(chunk)
                    if len(parts)==1 and parts[0] in METADATA:small.append(chunk)
            receipts.require(count == member.file_size,'Async snapshot expanded size differs')
            total += count
            if len(parts)==1 and parts[0] in METADATA:payload[parts[0]] = b''.join(small)
            else:files['/'.join(parts)] = digest.hexdigest()
    receipts.require(set(payload) == METADATA,'Async snapshot identity/origin missing')
    identity = receipts.bounded_json(payload['snapshot-identity.json']);origin = receipts.bounded_json(payload['resume-origin.json'])
    receipts.require(set(identity) == {'schema_version','phase','checkout_sha','current_run_id','current_attempt',
      'current_job_id','current_task_id','current_job_name','created_at','working_dir','origin_sha256','files_sha256','hydrated_acceptance'}
      and identity.get('schema_version') == 2 and identity.get('phase') == phase
      and identity.get('checkout_sha') == prior['commit_sha'] and type(identity.get('current_run_id')) is int
      and identity['current_run_id'] == prior['id']
      and type(identity.get('current_attempt')) is int and identity['current_attempt'] == attempt == job['attempt']
      and type(identity.get('current_job_id')) is int and identity['current_job_id'] == job['id']
      and type(identity.get('current_task_id')) is int and identity['current_task_id'] == job['task_id']
      and identity.get('current_job_name') == PROOF_JOB
      and identity.get('hydrated_acceptance') is False and identity.get('files_sha256') == files
      and identity.get('origin_sha256') == hashlib.sha256(payload['resume-origin.json']).hexdigest(),
      'Async snapshot phase/run/job/source/file identity differs')
    receipts.require(origin.get('schema_version') == 2 and type(origin.get('current_run_id')) is int
      and origin['current_run_id'] == prior['id']
      and type(origin.get('current_attempt')) is int and origin['current_attempt'] == attempt
      and type(origin.get('current_job_id')) is int and origin['current_job_id'] == job['id']
      and type(origin.get('current_task_id')) is int and origin['current_task_id'] == job['task_id']
      and origin.get('candidate_commit') == prior['commit_sha'] and origin.get('current_job_name') == PROOF_JOB
      and origin.get('working_dir') == identity['working_dir'] and isinstance(identity['working_dir'],str)
      and identity['working_dir'].startswith('/'),'Async snapshot origin differs')
    receipts.require('provenance.json' in files,'Async snapshot has no ordinary journal')
    if phase == 'prefix':
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            provenance = archive.read('provenance.json')
        receipts.require(receipts.bounded_json(provenance).get('complete') is False,
                         'Prefix checkpoint cannot claim whole completion')
    return {'snapshot_identity_sha256':hashlib.sha256(payload['snapshot-identity.json']).hexdigest(),
            'phase':phase,'case_acceptance':False}


def environment():
    workspace = Path(os.environ['GITHUB_WORKSPACE']).resolve()
    run = receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    attempt = receipts.positive(int(os.environ.get('GITHUB_RUN_ATTEMPT','1')))
    return workspace,run,attempt


def safe_output(path):
    receipts.require(not path.is_symlink() and not any(p.is_symlink() for p in path.parents),
                     'Async evidence output contains a symbolic link')


def prepare():
    workspace,run,attempt = environment()
    receipts.require(os.environ.get('GITHUB_EVENT_NAME') == 'pull_request','Authenticated restore is only for pull requests')
    api,scope,commit,current_run,job = current_context(workspace,run,attempt)
    pr = receipts.positive(scope['pr'])
    verification = workspace/'target/verification'; safe_output(verification)
    verification.mkdir(parents=True,exist_ok=True)
    destination = verification/f'async-driver-work-pr{pr}'
    origin_path = verification/f'async-driver-resume-origin-{run}-{attempt}.json'
    receipts.require(not destination.exists() and not origin_path.exists(),'Async resume requires fresh working/origin paths')
    destination.mkdir()
    origin = {'schema_version':2,'current_job_id':job['id'],'current_task_id':job['task_id'],'current_job_name':PROOF_JOB,'candidate_commit':commit,'created_at':datetime.now(timezone.utc).isoformat(),
      'available':False,'current_run_id':run,'current_attempt':attempt,'pr_number':pr,
      'working_dir':str(destination),'hydrated_files':0,'hydrated_bytes':0,'result_class':'fresh_no_prior_evidence','adapter':'async-driver-refinement'}
    try:
        receipts.require(current_run == run,'Canonical current run differs from invocation')
        candidate = transport.find_prior(api,scope,current_run,commit,workspace,spec=transport_spec(attempt))
        if candidate:
            files,size = extract_evidence(candidate.pop('data'),destination)
            origin.update(candidate)
            origin['decision'] = candidate['invocation_trust']['decision']
            if not files: origin['reason'] = 'Prior artifact contained no useful receipt evidence'
            origin.update({'available':files > 0,'hydrated_files':files,'hydrated_bytes':size,
              'result_class':'authenticated_evidence_transport' if files else 'fresh_no_reusable_files'})
        else:
            origin['reason'] = 'No compatible authenticated same-PR artifact was available'
    except (receipts.ReceiptError,OSError,KeyError,TypeError,ValueError,zipfile.BadZipFile) as error:
        shutil.rmtree(destination); destination.mkdir()
        origin.update({'available':False,'hydrated_files':0,'hydrated_bytes':0,
          'result_class':'rejected_prior_evidence','reason':f'Prior evidence not adopted ({type(error).__name__})'})
    recheck_current_context(workspace,run,attempt,scope,commit,job)
    origin_path.write_text(json.dumps(origin,sort_keys=True,indent=2)+'\n')
    output = os.environ.get('GITHUB_OUTPUT')
    receipts.require(output,'GitHub step output unavailable')
    with Path(output).open('a') as stream:
        stream.write(f'working_dir={destination}\norigin_path={origin_path}\nrestored={str(origin["available"]).lower()}\n')
    print(json.dumps({'working_dir':str(destination),'origin_path':str(origin_path),
      'restored':origin['available'],'result_class':origin['result_class']},sort_keys=True))


def snapshot(args):
    workspace,run,attempt = environment()
    phase = getattr(args,'phase','final')
    receipts.require(phase in dict(ARTIFACT_PHASES),'Unknown async snapshot phase')
    source,destination = args.working_dir.resolve(),args.output_dir.resolve()
    safe_output(args.working_dir.absolute()); safe_output(args.output_dir.absolute())
    receipts.require(source.is_dir() and not destination.exists(),'Snapshot requires existing work and nonexistent output')
    receipts.require(source != destination and source not in destination.parents and destination not in source.parents,
                     'Snapshot input/output must be disjoint directories')
    if args.origin:
        origin = receipts.bounded_json(args.origin.read_bytes())
        receipts.require(origin.get('schema_version') == 2 and origin.get('current_run_id') == run
          and origin.get('current_attempt') == attempt and origin.get('working_dir') == str(source)
          and origin.get('current_job_name') == PROOF_JOB,'Resume origin does not bind this run/job/working directory')
        receipts.positive(origin.get('current_job_id'));receipts.positive(origin.get('current_task_id'))
    else:
        receipts.require(os.environ.get('GITHUB_EVENT_NAME') == 'workflow_dispatch',
                         'Pull-request snapshot requires its current origin')
        origin = {'schema_version':2,'available':False,'current_run_id':run,'current_attempt':attempt,
          'current_job_id':None,'current_task_id':None,'current_job_name':None,
          'working_dir':str(source),'result_class':'fresh_non_pr_execution','reason':'No authenticated adoption on workflow_dispatch'}
    chosen,total = [],0
    for path in sorted(source.rglob('*')):
        relative = path.relative_to(source); parts = relative.parts
        if not selected(parts): continue
        receipts.require(not path.is_symlink() and path.is_file() and not any(p.is_symlink() for p in path.parents),
                         'Snapshot evidence contains a symbolic link or nonregular file')
        size = path.stat().st_size
        receipts.require(size <= member_limit(parts),'Snapshot evidence exceeds member limit')
        total += size; chosen.append((path,relative))
        receipts.require(total <= MAX_TOTAL_BYTES and len(chosen) <= MAX_FILES-2,'Snapshot exceeds evidence bounds')
    destination.mkdir(parents=True)
    files = {}
    for path,relative in chosen:
        target = destination/relative; target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(path,target)
        files[relative.as_posix()] = hashlib.sha256(target.read_bytes()).hexdigest()
    (destination/'resume-origin.json').write_text(json.dumps(origin,sort_keys=True,indent=2)+'\n')
    checkout_sha = subprocess.check_output(['git','rev-parse','HEAD'],cwd=workspace,text=True,timeout=10).strip()
    receipts.require(transport.COMMIT.fullmatch(checkout_sha),'Current checkout SHA unavailable')
    if args.origin:
        receipts.require(checkout_sha == origin.get('candidate_commit'),'Snapshot checkout differs from authenticated current source')
    identity = {'schema_version':2,'phase':phase,'checkout_sha':checkout_sha,'current_run_id':run,'current_attempt':attempt,
      'current_job_id':origin.get('current_job_id'),'current_task_id':origin.get('current_task_id'),'current_job_name':origin.get('current_job_name'),
      'created_at':datetime.now(timezone.utc).isoformat(),'working_dir':str(source),
      'origin_sha256':hashlib.sha256((destination/'resume-origin.json').read_bytes()).hexdigest(),
      'files_sha256':files,'hydrated_acceptance':False}
    identity_raw = (json.dumps(identity,sort_keys=True,indent=2)+'\n').encode()
    origin_bytes = (destination/'resume-origin.json').stat().st_size
    receipts.require(len(identity_raw) <= MAX_SMALL_BYTES and origin_bytes <= MAX_SMALL_BYTES
      and total+len(identity_raw)+origin_bytes <= MAX_TOTAL_BYTES,'Snapshot metadata exceeds artifact budget')
    if phase == 'prefix':
        receipts.require((source/'provenance.json').is_file()
          and receipts.bounded_json((source/'provenance.json').read_bytes()).get('complete') is False,
          'Prefix snapshot requires its ordinary incomplete journal')
    (destination/'snapshot-identity.json').write_bytes(identity_raw)
    print(json.dumps({'snapshot':str(destination),'phase':phase,'files':len(files),'bytes':total+len(identity_raw)+origin_bytes,'acceptance':False}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command',required=True)
    commands.add_parser('prepare')
    command = commands.add_parser('snapshot')
    command.add_argument('--working-dir',type=Path,required=True)
    command.add_argument('--output-dir',type=Path,required=True)
    command.add_argument('--origin',type=Path)
    command.add_argument('--phase',choices=('prefix','final'),default='final')
    args = parser.parse_args()
    try:
        if args.command == 'prepare': prepare()
        else: snapshot(args)
        return 0
    except (receipts.ReceiptError,OSError,KeyError,TypeError,ValueError,zipfile.BadZipFile,subprocess.SubprocessError) as error:
        print(f'Async evidence preparation failed ({type(error).__name__}): {error}',file=__import__('sys').stderr)
        return 2

if __name__ == '__main__': raise SystemExit(main())
