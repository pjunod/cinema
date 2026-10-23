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
PROOF_JOB = 'fast Rust gate'
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


def transport_spec():
    return transport.TransportSpec(workflow='main-fast-lane.yml',proof_job=PROOF_JOB,
      artifact_prefix=ARTIFACT_PREFIX,trust=identical_invocation,
      max_archive_bytes=MAX_TOTAL_BYTES,label='async refinement')


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
    event = receipts.bounded_json(Path(os.environ['GITHUB_EVENT_PATH']).read_bytes())
    pr = receipts.positive(event['number'])
    verification = workspace/'target/verification'; safe_output(verification)
    verification.mkdir(parents=True,exist_ok=True)
    destination = verification/f'async-driver-work-pr{pr}'
    origin_path = verification/f'async-driver-resume-origin-{run}-{attempt}.json'
    receipts.require(not destination.exists() and not origin_path.exists(),'Async resume requires fresh working/origin paths')
    destination.mkdir()
    origin = {'schema_version':1,'created_at':datetime.now(timezone.utc).isoformat(),
      'available':False,'current_run_id':run,'current_attempt':attempt,'pr_number':pr,
      'working_dir':str(destination),'hydrated_files':0,'hydrated_bytes':0,'result_class':'fresh_no_prior_evidence','adapter':'async-driver-refinement'}
    try:
        api,scope,commit,current_run = transport.current_context()
        receipts.require(scope['pr'] == pr and current_run == run,'Canonical current PR/run differs from invocation')
        candidate = transport.find_prior(api,scope,current_run,commit,workspace,spec=transport_spec())
        origin['candidate_commit'] = commit
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
    origin_path.write_text(json.dumps(origin,sort_keys=True,indent=2)+'\n')
    output = os.environ.get('GITHUB_OUTPUT')
    receipts.require(output,'GitHub step output unavailable')
    with Path(output).open('a') as stream:
        stream.write(f'working_dir={destination}\norigin_path={origin_path}\nrestored={str(origin["available"]).lower()}\n')
    print(json.dumps({'working_dir':str(destination),'origin_path':str(origin_path),
      'restored':origin['available'],'result_class':origin['result_class']},sort_keys=True))


def snapshot(args):
    workspace,run,attempt = environment()
    source,destination = args.working_dir.resolve(),args.output_dir.resolve()
    safe_output(args.working_dir.absolute()); safe_output(args.output_dir.absolute())
    receipts.require(source.is_dir() and not destination.exists(),'Snapshot requires existing work and nonexistent output')
    receipts.require(source != destination and source not in destination.parents and destination not in source.parents,
                     'Snapshot input/output must be disjoint directories')
    if args.origin:
        origin = receipts.bounded_json(args.origin.read_bytes())
        receipts.require(origin.get('current_run_id') == run and origin.get('current_attempt') == attempt
          and origin.get('working_dir') == str(source),'Resume origin does not bind this run/working directory')
    else:
        receipts.require(os.environ.get('GITHUB_EVENT_NAME') == 'workflow_dispatch',
                         'Pull-request snapshot requires its current origin')
        origin = {'schema_version':1,'available':False,'current_run_id':run,'current_attempt':attempt,
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
    identity = {'schema_version':1,'checkout_sha':checkout_sha,'current_run_id':run,'current_attempt':attempt,
      'created_at':datetime.now(timezone.utc).isoformat(),'working_dir':str(source),
      'origin_sha256':hashlib.sha256((destination/'resume-origin.json').read_bytes()).hexdigest(),
      'files_sha256':files,'hydrated_acceptance':False}
    (destination/'snapshot-identity.json').write_text(json.dumps(identity,sort_keys=True,indent=2)+'\n')
    print(json.dumps({'snapshot':str(destination),'files':len(files),'bytes':total,'acceptance':False}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command',required=True)
    commands.add_parser('prepare')
    command = commands.add_parser('snapshot')
    command.add_argument('--working-dir',type=Path,required=True)
    command.add_argument('--output-dir',type=Path,required=True)
    command.add_argument('--origin',type=Path)
    args = parser.parse_args()
    try:
        if args.command == 'prepare': prepare()
        else: snapshot(args)
        return 0
    except (receipts.ReceiptError,OSError,KeyError,TypeError,ValueError,zipfile.BadZipFile,subprocess.SubprocessError) as error:
        print(f'Async evidence preparation failed ({type(error).__name__}): {error}',file=__import__('sys').stderr)
        return 2

if __name__ == '__main__': raise SystemExit(main())
