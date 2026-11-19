"""Authenticated main-lane Python continuation.

prepare adopts the successes recorded by this PR's earlier preflight runs, so
they are not executed again, and run executes everything else. A prior run's
journal is adopted only when its run, job, start marker, journal and commit
all authenticate; anything else is simply not adopted and its tests run
again. History never refuses the lane: refusals are reserved for the
current run's own integrity (re-run attempt, already-published journal,
checkout/source mismatch, credential origin).
"""
from __future__ import annotations

import argparse
import ast
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import unittest
import urllib.parse

from validation import main_preflight_adoption as adoption
from validation import python_unit_receipts as receipts

WORKFLOW = 'main-fast-lane.yml'
JOB = adoption.JOB
PATH = Path('.main-python-unit-receipts/receipt.json')
PRODUCER = 'validation/main_unit_receipts.py'
REVIEWED_INPUTS_PROTOCOL = 1
require = receipts.require


def identity(api, pr, commit):
    repository = receipts.positive(api.get('')['id'])
    pull = api.get(f'/pulls/{receipts.positive(pr)}')
    require(pull['state'] == 'open' and pull['base']['ref'] == 'main'
            and pull['base']['repo']['id'] == repository
            and pull['head']['repo']['id'] == repository
            and pull['head']['sha'] == commit,
            'Main receipt must match an open same-repository PR head')
    return {'repository': repository, 'pr': pr, 'branch': pull['head']['ref'],
            'base': 'main', 'workflow': WORKFLOW}


def key(scope):
    return f"main-python-units-v1-r{scope['repository']}-pr{scope['pr']}"


def authenticate_run(scope, prior):
    require(prior['workflow_id'] == WORKFLOW and prior['prettyref'] == f"#{scope['pr']}"
            and prior['event'] == 'pull_request', 'Historical workflow/PR/event mismatch')
    event = prior['event_payload']
    if isinstance(event, str):
        event = receipts.bounded_json(event.encode())
    pull = event['pull_request']
    require(event['repository']['id'] == scope['repository'] and event['number'] == scope['pr']
            and pull['number'] == scope['pr'] and pull['head']['ref'] == scope['branch']
            and pull['head']['sha'] == prior['commit_sha'] and pull['base']['ref'] == 'main'
            and pull['head']['repo']['id'] == scope['repository']
            and pull['base']['repo']['id'] == scope['repository'],
            'Historical event repository/PR/head/base mismatch')


def emit_snapshot(journal, phase):
    raw = json.dumps(journal, sort_keys=True, separators=(',', ':')).encode()
    require(len(raw) <= receipts.MAX_BYTES, 'Log journal exceeds bound')
    encoded = base64.b64encode(raw).decode()
    chunks = [encoded[i:i + 2048] for i in range(0, len(encoded), 2048)]
    print(f'MAIN-UNIT-JOURNAL {phase} {len(chunks)} {hashlib.sha256(raw).hexdigest()}', flush=True)
    for index, chunk in enumerate(chunks):
        print(f'MAIN-UNIT-CHUNK {phase} {index} {chunk}', flush=True)
    print(f'MAIN-UNIT-END {phase}', flush=True)


def log_lines(raw):
    require(len(raw) <= receipts.MAX_BYTES, 'Historical log exceeds bound')
    return [re.sub(r'^\d{4}-\d\d-\d\dT[^ ]+ ', '', line) for line in raw.decode().splitlines()]


def read_snapshots(lines):
    snapshots = {}
    index = 0
    while index < len(lines):
        line = lines[index]
        if not line.startswith('MAIN-UNIT-'):
            index += 1
            continue
        marker = re.fullmatch(r'MAIN-UNIT-JOURNAL (start|final) ([1-9][0-9]*) ([0-9a-f]{64})', line)
        require(marker is not None, 'Unknown log journal framing')
        phase, count, digest = marker[1], int(marker[2]), marker[3]
        require((phase == 'start' and not snapshots)
                or (phase == 'final' and set(snapshots) == {'start'}),
                'Reordered log journal phases')
        require(phase not in snapshots and count <= (receipts.MAX_BYTES * 2 // 2048) + 1,
                'Duplicate/oversized log journal')
        chunks = []
        for position in range(count):
            index += 1
            require(index < len(lines), 'Truncated log journal')
            part = re.fullmatch(r'MAIN-UNIT-CHUNK ' + phase + r' ' + str(position) + r' ([A-Za-z0-9+/=]{1,2048})', lines[index])
            require(part is not None, 'Missing/reordered log journal chunk')
            chunks.append(part[1])
        index += 1
        require(index < len(lines) and lines[index] == f'MAIN-UNIT-END {phase}', 'Missing log journal end')
        try:
            raw = base64.b64decode(''.join(chunks), validate=True)
        except ValueError:
            raise receipts.ReceiptError('Invalid log journal encoding') from None
        require(hashlib.sha256(raw).hexdigest() == digest, 'Log journal digest mismatch')
        snapshots[phase] = receipts.bounded_json(raw)
        index += 1
    return snapshots


def preflight_job(api, scope, rid):
    """The run's one original preflight job, terminal and actually started."""
    matches = [job for job in api.pages(f'/actions/runs/{rid}/jobs') if job['name'] == JOB]
    require(len(matches) == 1, 'no unique preflight job')
    job = matches[0]
    require(job['repo_id'] == scope['repository'] and job['run_id'] == rid,
            'job repository/run mismatch')
    # A re-run job shares the run's artifact names, so its journal cannot be
    # attributed to one execution.
    require(type(job['attempt']) is int and job['attempt'] == 1,
            'job was re-run; ambiguous artifact identity')
    require(job['status'] in ('success', 'failure', 'cancelled'),
            f"preflight job is not terminal ({job['status']})")
    require(not (type(job.get('task_id')) is int and job['task_id'] == 0),
            'preflight job was never assigned to a runner')
    return job


def reviewed_input_bindings(journal):
    return journal.get('reviewed_inputs', {})


def validate_journal(journal, scope, run, commit, completed=True):
    passes = receipts.validate_journal(journal, scope, run, commit, completed=completed)
    bindings = reviewed_input_bindings(journal)
    require(isinstance(bindings, dict) and bindings.keys() <= passes.keys()
            and all(isinstance(value, str) and re.fullmatch(r'[0-9a-f]{64}', value)
                    for value in bindings.values()), 'Invalid reviewed input bindings')
    return passes


def inherits(start, final):
    start_inputs, final_inputs = reviewed_input_bindings(start), reviewed_input_bindings(final)
    return all(final['passes'].get(test) == source and final_inputs.get(test) == start_inputs.get(test)
               for test, source in start['passes'].items())


class ReviewedInputs:
    """Exact per-ID input policy at each committed source."""

    def __init__(self, commit):
        self.commit = commit
        self.families = {}
        self.origin_protocols = {}

    def origin_protocol(self, commit):
        commit = receipts.sha(commit)
        if commit not in self.origin_protocols:
            source = ast.parse(adoption.git('show', f'{commit}:{PRODUCER}'))
            names = [node for node in ast.walk(source) if isinstance(node, ast.Name)
                     and node.id == 'REVIEWED_INPUTS_PROTOCOL']
            declarations = [node for node in names if isinstance(node.ctx, ast.Store)]
            if names:
                assignments = [node for node in source.body if isinstance(node, ast.Assign)
                               and node.targets == declarations]
                require(len(declarations) == len(assignments) == 1
                        and isinstance(assignments[0].value, ast.Constant)
                        and type(assignments[0].value.value) is int
                        and assignments[0].value.value == REVIEWED_INPUTS_PROTOCOL,
                        'Unknown or ambiguous reviewed-input producer protocol')
            self.origin_protocols[commit] = REVIEWED_INPUTS_PROTOCOL if names else None
        return self.origin_protocols[commit]

    def witness(self, commit, test):
        commit = receipts.sha(commit)
        if commit not in self.families:
            document = receipts.bounded_json(adoption.git('show', f'{commit}:{adoption.MANIFEST}').encode())
            require(document['version'] == 1 and isinstance(document['families'], list),
                    'Unknown reviewed input manifest')
            self.families[commit] = document['families']
        return adoption.witness(test, self.families[commit], optional=True)

    def current_digest(self, test):
        row = self.witness(self.commit, test)
        return adoption.input_digest(self.commit, row) if row is not None else None

    def applicable(self, test, source, recorded):
        try:
            origin = source['commit']
            row = self.witness(self.commit, test)
            original = self.witness(origin, test)
            protocol = self.origin_protocol(origin)
            if row is None:
                return original is None and recorded is None
            if original is None or original['id_prefix'] != row['id_prefix']:
                return False
            if (recorded is None) != (protocol is None):
                return False
            origin_digest = adoption.input_digest(origin, original)
            return ((recorded is None or recorded == origin_digest)
                    and origin_digest == adoption.input_digest(self.commit, row))
        except receipts.IntegrityError:
            raise
        except (*adoption.UNADOPTABLE, SyntaxError) as error:
            print(f'Reviewed inputs unavailable for {test}: {adoption.reason(error)}; it executes')
            return False


def listed_run(listed, rid):
    priors = listed.get(rid, [])
    require(len(priors) == 1, "run is not exactly one of this PR's main-lane runs")
    return priors[0]


def journal_from_artifacts(api, scope, prior, finals):
    """A published start marker and final journal of one authenticated run."""
    authenticate_run(scope, prior)
    rid, commit = receipts.positive(prior['id']), receipts.sha(prior['commit_sha'])
    require(len(finals) == 1, 'several final journals for one run')
    artifact = finals[0]
    require(artifact['name'] == key(scope) and not artifact['expired']
            and artifact['size_in_bytes'] <= receipts.MAX_BYTES,
            'final journal expired, misnamed or oversized')
    preflight_job(api, scope, rid)
    require(adoption.commit_available(commit), f'commit {commit} is not available in this checkout')
    markers = api.pages('/actions/artifacts', {'name': key(scope) + f'-start-{rid}'})
    require(len(markers) == 1 and markers[0]['run_id'] == rid and not markers[0]['expired'],
            'missing/ambiguous attempt-start marker')
    start = receipts.artifact_json(api.bytes(f"/actions/artifacts/{receipts.positive(markers[0]['id'])}/zip"))
    validate_journal(start, scope, rid, commit, completed=False)
    require(start['complete'] is False, 'start marker is not an initial journal')
    journal = receipts.artifact_json(api.bytes(f"/actions/artifacts/{receipts.positive(artifact['id'])}/zip"))
    # An interrupted unit runner can leave the final journal incomplete. Its
    # positive atomic success records remain evidence; absence from the map
    # never establishes that a method did not run.
    validate_journal(journal, scope, rid, commit, completed=False)
    require(isinstance(journal.get('complete'), bool), 'invalid completion marker')
    require(inherits(start, journal),
            'final journal discarded or changed inherited evidence')
    return journal


def journal_from_log(api, scope, prior):
    """The completed framed journal a run printed when its upload never happened.

    The run step prints its start and final journals into the job log. A job
    deadline or failed upload after that leaves the log as the only copy of
    the same positive records; anything short of a complete pair is nothing.
    """
    authenticate_run(scope, prior)
    rid, commit = receipts.positive(prior['id']), receipts.sha(prior['commit_sha'])
    job = preflight_job(api, scope, rid)
    require(adoption.commit_available(commit), f'commit {commit} is not available in this checkout')
    return recover_log(api, scope, prior, job)


def recover_log(api, scope, prior, job):
    rid, commit = receipts.positive(prior['id']), receipts.sha(prior['commit_sha'])
    require(job['status'] in ('success', 'failure', 'cancelled'),
            'log journal needs a terminal preflight')
    lines = log_lines(api.bytes(f"/actions/jobs/{receipts.positive(job['id'])}/logs"))
    require(sum(commit in line for line in lines) >= 2
            and any('triggered by event: pull_request' in line for line in lines),
            'log lacks exact-source checkout evidence')
    snapshots = read_snapshots(lines)
    require(set(snapshots) == {'start', 'final'}, 'no complete start/final journal in the log')
    start, final = snapshots['start'], snapshots['final']
    validate_journal(start, scope, rid, commit, completed=False)
    validate_journal(final, scope, rid, commit)
    require(start['complete'] is False and inherits(start, final), 'log journal inheritance mismatch')
    return final


def restore(api, scope, current_run, applicability, reviewed):
    """Adopt only authenticated prior successes; a history problem never refuses.

    Each prior journal either authenticates exactly as before or is not
    adopted, with one line saying why. A success inherited from an earlier
    run is kept only when that run's own journal was adopted and recorded it.
    """
    try:
        artifacts = api.pages('/actions/artifacts', {'name': key(scope)})
    except receipts.IntegrityError:
        raise
    except adoption.UNADOPTABLE as error:
        # GITHUB_RUN_ATTEMPT (checked in main) still guards against a re-run.
        print(f'Prior journals unavailable — {adoption.reason(error)}; nothing adopted, every test runs')
        applicability.finish({})
        return {}, {}
    if any(artifact.get('run_id') == current_run for artifact in artifacts):
        raise receipts.IntegrityError('Attempt already executed')
    try:
        runs = api.pages('/actions/runs', {'workflow_id': WORKFLOW,
                                         'ref': f"refs/pull/{scope['pr']}/head"}, 'workflow_runs')
    except receipts.IntegrityError:
        raise
    except adoption.UNADOPTABLE as error:
        # Journals cannot be bound to their runs without the listing.
        print(f'Prior runs unavailable — {adoption.reason(error)}; nothing adopted, every test runs')
        applicability.finish({})
        return {}, {}
    listed, finals = {}, {}
    for prior in runs:
        listed.setdefault(prior.get('id'), []).append(prior)
    for artifact in artifacts:
        finals.setdefault(artifact.get('run_id'), []).append(artifact)
    newest = lambda rid: rid if type(rid) is int else -1
    attempts = [(rid, lambda rid=rid: journal_from_artifacts(api, scope, listed_run(listed, rid), finals[rid]))
                for rid in sorted(finals, key=newest, reverse=True)]
    attempts += [(rid, lambda rid=rid: journal_from_log(api, scope, listed_run(listed, rid)))
                 for rid in sorted(listed, key=newest, reverse=True)
                 if rid != current_run and rid not in finals]
    journals = []
    for rid, authenticate in attempts:
        try:
            journals.append((rid, authenticate()))
        except receipts.IntegrityError:
            raise
        except adoption.UNADOPTABLE as error:
            print(f'run {rid}: journal not adopted — {adoption.reason(error)}; its tests run again')
    trusted = {(test, rid, journal['commit']): reviewed_input_bindings(journal).get(test)
               for rid, journal in journals
               for test, value in journal['passes'].items()
               if value == {'run': rid, 'commit': journal['commit']}}
    passes, bindings = {}, {}
    for rid, journal in journals:
        dropped = 0
        recorded = reviewed_input_bindings(journal)
        for test, value in journal['passes'].items():
            origin = (test, value.get('run'), value.get('commit'))
            if origin not in trusted or trusted[origin] != recorded.get(test):
                dropped += 1
                continue
            if (test not in passes and applicability(test, value)
                    and reviewed.applicable(test, value, recorded.get(test))):
                passes[test] = value
                if test in recorded:
                    bindings[test] = recorded[test]
        if dropped:
            print(f'run {rid}: {dropped} inherited successes not adopted — '
                  'their originating journal was not adopted or binding differed; they run again')
    applicability.finish(passes)
    return passes, bindings


class MainResult(receipts.RecordingResult):
    """Record ordinary unittest outcomes while caching only real successes."""

    def __init__(self, *args, reviewed, **kwargs):
        super().__init__(*args, **kwargs)
        self.reviewed = reviewed
        self.observed = set()

    def addSuccess(self, test):
        key = self.suite + ':' + test.id()
        digest = self.reviewed.current_digest(key)
        unittest.TextTestResult.addSuccess(self, test)
        self.observed.add(test.id())
        self.journal['passes'][key] = {'commit': self.journal['commit'], 'run': self.journal['run']}
        if digest is not None:
            self.journal.setdefault('reviewed_inputs', {})[key] = digest
        receipts.atomic_json(self.path, self.journal)

    def addFailure(self, test, error):
        self.observed.add(test.id())
        super().addFailure(test, error)

    def addError(self, test, error):
        self.observed.add(test.id())
        super().addError(test, error)

    def addSubTest(self, test, subtest, error):
        if error is not None:
            self.observed.add(test.id())
        super().addSubTest(test, subtest, error)

    def addExpectedFailure(self, test, error):
        self.observed.add(test.id())
        super().addExpectedFailure(test, error)

    def addUnexpectedSuccess(self, test):
        self.observed.add(test.id())
        super().addUnexpectedSuccess(test)

    def addSkip(self, test, reason):
        unittest.TextTestResult.addSkip(self, test, reason)
        if test.id() in self.known_ids:
            self.observed.add(test.id())
        else:
            fixture = re.fullmatch(r'setUpClass \(([A-Za-z0-9_.]+)\)', test.id())
            require(fixture is not None, 'Unknown skipped fixture outcome')
            self.observed.update(test_id for test_id in self.known_ids
                                 if test_id.rsplit('.', 1)[0] == fixture[1])
        self.journal.setdefault('skips', {})[self.suite + ':' + test.id()] = str(reason)[:4096]
        receipts.atomic_json(self.path, self.journal)


def execute(journal, path, suites=None, *, reviewed=None):
    """Keep main unittest semantics: a legitimate skip is not a cached pass."""
    require(journal.get('applicability_commit') == journal['commit'], 'Missing current-source prepare')
    reviewed = reviewed if reviewed is not None else ReviewedInputs(journal['commit'])
    if suites is None:
        # Only rechecked passes may skip a test. A pass whose recheck cannot
        # complete (say, a transient git failure reading its old source) is
        # withdrawn and executes; it never refuses the run. Withdrawing it
        # also means this run's final journal will not be adopted later,
        # because it no longer carries every record its start marker did.
        applicability = receipts.SourceApplicability(journal['commit'])
        applicable = {}
        for test, value in journal['passes'].items():
            try:
                if (applicability(test, value)
                        and reviewed.applicable(test, value, reviewed_input_bindings(journal).get(test))):
                    applicable[test] = value
            except receipts.IntegrityError:
                raise
            except adoption.UNADOPTABLE as error:
                print(f'Recheck incomplete for {test} — {adoption.reason(error)}; it executes')
        applicability.finish(applicable)
        withdrawn = sorted(set(journal['passes']) - set(applicable))
        if withdrawn:
            print(f'{len(withdrawn)} prepared passes could not be rechecked; they execute: ' + ', '.join(withdrawn))
            journal['passes'] = applicable
            journal['reviewed_inputs'] = {test: digest for test, digest in reviewed_input_bindings(journal).items()
                                          if test in applicable}
            receipts.atomic_json(path, journal)
    require(not journal['fixture_errors'], 'Unresolved fixture errors')
    inventories = {name: list(suites[name]) if suites is not None else receipts.discover(name)
                   for name in receipts.SUITES}
    failed = False
    for name, tests in inventories.items():
        ids = {test.id() for test in tests}
        require(tests and len(ids) == len(tests), 'Empty/duplicate current discovery')
        pending = [test for test in tests if name + ':' + test.id() not in journal['passes']]
        print(f'{name}: discovered={len(tests)}, historical-passes={len(tests)-len(pending)}, pending={len(pending)}')
        if pending:
            result = unittest.TextTestRunner(verbosity=2, resultclass=lambda *a, **kw:
                MainResult(*a, **kw, journal=journal, path=path, suite=name,
                                         known_ids=ids, reviewed=reviewed)).run(unittest.TestSuite(pending))
            require({test.id() for test in pending} <= result.observed, 'Unknown pending unit outcome')
            failed |= not result.wasSuccessful() or bool(journal['fixture_errors'])
    journal['complete'] = True
    receipts.atomic_json(path, journal)
    return int(failed)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('prepare', 'run'))
    parser.add_argument('--suite-dir', action='append', default=[])
    args = parser.parse_args(argv)
    require(args.suite_dir == list(receipts.SUITES.values()) if args.command == 'run'
            else not args.suite_dir, 'Declare exactly the two executed suite directories')
    require(not PATH.parent.is_symlink(), 'Receipt directory is a symlink')
    origin = urllib.parse.urlsplit(os.environ['GITHUB_API_URL'])
    server = urllib.parse.urlsplit(os.environ['GITHUB_SERVER_URL'])
    require((origin.scheme, origin.netloc) == (server.scheme, server.netloc), 'Refuse credential forwarding')
    api = receipts.API(os.environ['GITHUB_API_URL'], os.environ['GITHUB_REPOSITORY'], os.environ.get('GITHUB_TOKEN'))
    commit = receipts.sha(os.environ['GITHUB_SHA'])
    require(subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == commit,
            'Checkout is not event source')
    require(os.environ.get('GITHUB_RUN_ATTEMPT', '1') == '1', 'Dispatch fresh runs; never rerun jobs')
    run = receipts.positive(int(os.environ['GITHUB_RUN_ID']))
    require(not adoption.git('diff', '--name-only', 'HEAD'), 'Current tracked source differs from dispatched Git source')
    scope = identity(api, receipts.positive(int(os.environ['PR_NUMBER'])), commit)
    reviewed = ReviewedInputs(commit)
    if args.command == 'prepare':
        adoption.prepare(output_key='adoption_receipt_key')
        passes, bindings = restore(api, scope, run, receipts.SourceApplicability(commit), reviewed)
        journal = {'version': receipts.VERSION, 'scope': scope, 'run': run, 'commit': commit,
                   'complete': False, 'fixture_errors': [], 'applicability_commit': commit,
                   'passes': passes, 'reviewed_inputs': bindings}
        receipts.atomic_json(PATH, journal)
        emit_snapshot(journal, 'start')
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'receipt_key={key(scope)}\n')
        return 0
    journal = receipts.bounded_json(PATH.read_bytes())
    validate_journal(journal, scope, run, commit, completed=False)
    require(journal['complete'] is False, 'Attempt already complete')
    try:
        return execute(journal, PATH, reviewed=reviewed)
    finally:
        emit_snapshot(receipts.bounded_json(PATH.read_bytes()), 'final')


if __name__ == '__main__':
    try:
        sys.exit(main())
    except receipts.ReceiptError as error:
        print(f'Main Python receipt refused: {error}', file=sys.stderr)
        sys.exit(1)
