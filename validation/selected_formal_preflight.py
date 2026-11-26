"""Authenticate an unchanged worker-refinement smoke against its original CI receipt."""
from __future__ import annotations

import io
from pathlib import PurePosixPath
import subprocess
import sys
import zipfile

from validation import main_unit_receipts as main
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r

POLICY = 'worker-refinement-smoke-origin-v1'
PACKAGE = 'worker-refinement'
CONFIG = 'witness-both-complete.cfg'


def original_receipt(raw):
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        rows = archive.infolist()
        names = [row.filename for row in rows]
        r.require(0 < len(rows) <= 48 and len(set(names)) == len(names)
                  and sum(row.file_size for row in rows) <= 32 * 1024 * 1024,
                  'Unsafe formal evidence archive')
        receipts = [name for name in names if name.startswith(PACKAGE + '/') and name.endswith('/receipt.json')]
        r.require(len(receipts) == 1 and len(PurePosixPath(receipts[0]).parts) == 3,
                  'Missing or ambiguous worker-refinement result')
        receipt_raw = archive.read(receipts[0])
        receipt = r.bounded_json(receipt_raw)
        r.require(receipts[0] == f"{PACKAGE}/{receipt['run_id']}/receipt.json"
                  and receipt['schema_version'] == 1 and receipt['package'] == PACKAGE
                  and receipt['qualification'] == 'smoke_or_single_config' and receipt['mode'] == 'smoke'
                  and receipt['status'] == 'complete' and len(receipt['configs']) == 1
                  and not receipt['source_git']['governed_dirty_paths']
                  and not receipt['source_git']['governed_dirty_sha256'],
                  'Worker-refinement receipt lacks a complete reviewed smoke')
        row = receipt['configs'][0]
        r.require(row['config'] == CONFIG and row['complete'] is True
                  and row['actual_verdict'] == row['expected_verdict'] == 'witness'
                  and row['completion_kind'] == 'normal_reachability_witness_detected'
                  and row['exit_code'] == row['expected_exit'] == 12
                  and row['counterexample_found'] is False and row['exploration_complete'] is False
                  and type(row['trace_states']) is int and row['trace_states'] >= 2 and row['reason'] is None,
                  'Worker-refinement smoke failed or did not reach its required witness')
        log = f"{PACKAGE}/{receipt['run_id']}/{CONFIG.removesuffix('.cfg')}/tlc.log"
        r.require(row['raw_log']['path'].endswith('/target/verification/' + log) and log in names
                  and retention.digest(archive.read(log)) == row['raw_log']['sha256'],
                  'Worker-refinement raw TLC evidence differs')
        historical = r.bounded_json(archive.read(PACKAGE + '/ci-inputs.json'))
    return receipt, historical, retention.digest(receipt_raw)


def matches_current(receipt, historical, current, candidate, origin):
    r.require(receipt['source_git']['commit'] == origin
              and current['schema_version'] == historical['schema_version'] == 1
              and current['package'] == historical['package'] == PACKAGE
              and not historical['source_drift'] and not current['source_drift']
              and current['affected'] is False
              and receipt['input_sha256'] == historical['input_sha256'] == current['input_sha256']
              and receipt['mapping_sha256'] == historical['mapping_sha256'] == current['mapping_sha256']
              and receipt['runner_sha256'] == retention.digest(retention.git_bytes(candidate, 'scripts/check-formal'))
              and receipt['toolchain']['manifest_sha256'] == retention.digest(
                  retention.git_bytes(candidate, 'verification/durable-jobs/toolchain.json')),
              'Worker-refinement smoke inputs differ from its authenticated execution')


def prepare(api, value, scope, historical_journal, candidate):
    fields = {'version', 'run', 'job', 'source_commit', 'artifact', 'archive_sha256'}
    r.require(isinstance(value, dict) and set(value) == fields and value['version'] == POLICY,
              'Unknown selected formal descriptor')
    run, job_id = r.positive(value['run']), r.positive(value['job'])
    origin = r.sha(value['source_commit'])
    artifact_id = r.positive(value['artifact'])
    sha = value['archive_sha256']
    r.require(run == historical_journal['run'] and origin == historical_journal['commit']
              and isinstance(sha, str) and retention.DIGEST.fullmatch(sha),
              'Formal proof outside the original authenticated main preflight')
    job = main.preflight_job(api, scope, run)
    r.require(job['id'] == job_id and job['status'] == 'success',
              'Worker-refinement origin preflight did not succeed')
    name = f'formal-model-{run}-1'
    candidates = [row for row in api.pages(f'/actions/runs/{run}/artifacts') if row['id'] == artifact_id]
    r.require(len(candidates) == 1 and candidates[0]['run_id'] == run and candidates[0]['name'] == name
              and candidates[0]['expired'] is False, 'Unknown original worker-refinement artifact')
    log = api.bytes(f'/actions/jobs/{job_id}/logs')
    r.require(log.count(f'Artifact {name} has been successfully uploaded!'.encode()) == 1
              and log.count(f'SHA256 hash of uploaded artifact zip is {sha}'.encode()) == 1,
              'Formal artifact lacks the original creator log witness')
    raw = api.bytes(f'/actions/artifacts/{artifact_id}/zip')
    r.require(retention.digest(raw) == sha, 'Worker-refinement original artifact digest differs')
    receipt, historical, receipt_hash = original_receipt(raw)
    output = subprocess.check_output([sys.executable, '-B', 'scripts/check-formal', 'check-mapping',
                                      '--package', PACKAGE, '--affected-base', origin], timeout=60)
    current = r.bounded_json(output)
    matches_current(receipt, historical, current, candidate, origin)
    return {'origin': value, 'receipt_sha256': receipt_hash,
            'mapping_sha256': current['mapping_sha256'], 'input_sha256': current['input_sha256']}
