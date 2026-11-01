"""Strict transport for schema-6 qualification sessions.

This module authenticates producer bytes, not graph semantics. Ordinary PR
receipt policy is deliberately not used as a session continuation exception.
"""
from __future__ import annotations

import copy
import base64
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import sqlite3
import stat
import subprocess
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request
import zipfile

from validation import python_unit_receipts as receipts
from validation import worker_control_resume as transport
from validation import main_unit_receipts

MIB = 1024 * 1024
MAX_ARTIFACT = 512 * MIB
MAX_MEMBER = 256 * MIB
MAX_MEMBERS = 300
SMALL = 8 * MIB
WORKFLOW = 'async-proof-session.yml'
PRODUCER_JOB = 'async proof session producer'
COMPLETION_JOB = 'Main promotion gate'
SCHEMA_PATH = 'verification/async-driver-refinement/paged-proof.schema.json'
SHA = re.compile(r'[0-9a-f]{64}\Z')
COMMIT = re.compile(r'[0-9a-f]{40}\Z')
PATH = re.compile(r'[A-Za-z0-9_-][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_-][A-Za-z0-9_.-]*)*\Z')
SELF_FIELDS = {'Page': 'page_id', 'ManifestNode': 'sha256', 'ResourcePlan': 'plan_sha256'}


def require(condition, message):
    receipts.require(condition, 'Async session: ' + message)


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True,
                      separators=(',', ':'), allow_nan=False).encode('utf-8')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_sha(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(MIB), b''):
            h.update(block)
    return h.hexdigest()


def decode(data, limit=SMALL):
    require(len(data) <= limit, 'JSON byte allowance exceeded')
    def pairs(rows):
        value = {}
        for key, item in rows:
            require(key not in value, 'duplicate JSON key')
            value[key] = item
        return value
    try:
        value = json.loads(data.decode('utf-8'), object_pairs_hook=pairs,
                           parse_constant=lambda _: (_ for _ in ()).throw(ValueError('nonfinite')))
        canonical(value)
        return value
    except (ValueError, UnicodeError, RecursionError) as error:
        raise receipts.IntegrityError('Async session: invalid JSON') from error


def read(path, limit=SMALL):
    path = Path(path)
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= limit,
            'missing/oversized/symlink JSON member')
    return decode(path.read_bytes(), limit)


def write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    require(not path.is_symlink(), 'symlink output')
    # Replace only private mutable controller files, never immutable artifacts.
    temporary = path.with_name(path.name + '.new')
    with temporary.open('xb') as stream:
        stream.write(canonical(value))
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    directory=os.open(path.parent,os.O_RDONLY)
    try:os.fsync(directory)
    finally:os.close(directory)


def safe_path(value):
    require(isinstance(value, str) and PATH.fullmatch(value) is not None
            and all(p not in ('.', '..', '.git') for p in PurePosixPath(value).parts),
            'unsafe or credential-bearing member path')
    return value



def safe_source_path(value):
    require(isinstance(value, str) and value and not value.startswith('/')
            and all(p not in ('', '.', '..', '.git', '.aws') for p in value.split('/'))
            and not any(ord(c) < 32 or c == '\\' for c in value),
            'unsafe source archive path')
    return value

def closed(value, keys):
    require(type(value) is dict and set(value) == set(keys), 'unknown/missing fields')
    return value


def validate(value, definition, schema):
    """Validate the frozen schema vocabulary without an optional dependency.

    Unknown schema keywords fail closed so a future contract cannot silently
    acquire constraints this decoder ignores.
    """
    allowed = {'$ref', '$comment', 'description', 'title', 'type', 'enum', 'const',
               'properties', 'required', 'additionalProperties', 'items', 'maxItems',
               'minItems', 'uniqueItems', 'minimum', 'maximum', 'minLength', 'maxLength',
               'pattern', 'anyOf', 'oneOf', 'allOf', 'if', 'then', 'else', 'not'}
    require(set(definition) <= allowed, 'unsupported schema constraint')
    if '$ref' in definition:
        ref = definition['$ref']
        require(ref.startswith('#/$defs/'), 'external schema reference')
        validate(value, schema['$defs'][ref.split('/')[-1]], schema)
    for keyword, exact in (('anyOf', False), ('oneOf', True)):
        if keyword in definition:
            matches = 0
            for branch in definition[keyword]:
                try:
                    validate(value, branch, schema)
                    matches += 1
                except receipts.ReceiptError:
                    pass
            require(matches == 1 if exact else matches > 0, 'schema union mismatch')
    for branch in definition.get('allOf', []):
        validate(value, branch, schema)
    if 'not' in definition:
        try:
            validate(value, definition['not'], schema)
        except receipts.ReceiptError:
            pass
        else:
            require(False, 'forbidden schema value')
    if 'if' in definition:
        try:
            validate(value, definition['if'], schema)
            branch = 'then'
        except receipts.ReceiptError:
            branch = 'else'
        if branch in definition:
            validate(value, definition[branch], schema)
    if 'type' in definition:
        types = {'object': dict, 'array': list, 'string': str, 'integer': int,
                 'boolean': bool, 'null': type(None)}
        require(definition['type'] in types and type(value) is types[definition['type']],
                'schema type mismatch')
    if 'const' in definition:
        require(type(value) is type(definition['const']) and value == definition['const'],
                'schema constant mismatch')
    if 'enum' in definition:
        require(any(type(value) is type(v) and value == v for v in definition['enum']),
                'schema enum mismatch')
    if isinstance(value, dict):
        require(set(definition.get('required', [])) <= set(value), 'required schema field missing')
        props = definition.get('properties', {})
        if definition.get('additionalProperties') is False:
            require(set(value) <= set(props), 'extra schema field')
        if isinstance(definition.get('additionalProperties'),dict):
            for name in set(value)-set(props):
                validate(value[name],definition['additionalProperties'],schema)
        for name in set(value) & set(props):
            validate(value[name], props[name], schema)
    if isinstance(value, list):
        require(len(value) >= definition.get('minItems', 0)
                and len(value) <= definition.get('maxItems', len(value)), 'schema array bound')
        if definition.get('uniqueItems'):
            require(len({canonical(v) for v in value}) == len(value), 'duplicate array item')
        for item in value:
            if 'items' in definition:
                validate(item, definition['items'], schema)
    if isinstance(value, str):
        require(len(value) >= definition.get('minLength', 0)
                and len(value) <= definition.get('maxLength', len(value)), 'schema string bound')
        if 'pattern' in definition:
            require(re.search(definition['pattern'], value) is not None, 'schema string pattern')
    if type(value) is int:
        require(value >= definition.get('minimum', value)
                and value <= definition.get('maximum', value), 'schema integer range')


def envelope(value, kind, root):
    closed(value, ('schema_version', 'kind', 'record'))
    require(type(value['schema_version']) is int and value['schema_version'] == 6
            and value['kind'] == kind, 'wrong envelope')
    schema = read(Path(root) / SCHEMA_PATH)
    validate(value['record'], schema['$defs'][kind], schema)
    return value['record']


def transport_bytes(value):
    value = copy.deepcopy(value)
    field = SELF_FIELDS.get(value['kind'])
    if field:
        value['record'].pop(field, None)
    return canonical(value)


def ref(value, identity):
    return {'kind': value['kind'], 'id': identity, 'sha256': digest(transport_bytes(value))}


def git(root, *arguments):
    return subprocess.check_output(['git', *arguments], cwd=root, timeout=30).strip().decode()


def source_archive(root, destination, source, members):
    """Archive committed source only; credentials and .git never cross boundary."""
    require(git(root, 'rev-parse', 'HEAD') == source['commit']
            and git(root, 'rev-parse', 'HEAD^{tree}') == source['tree']
            and not git(root, 'status', '--porcelain', '--untracked-files=no'), 'source tree differs')
    require(digest(canonical(sorted(members.items()))) == source['source_closure_sha256'],
            'source closure digest differs')
    for name, expected in members.items():
        safe_source_path(name)
        require(SHA.fullmatch(expected) and file_sha(Path(root) / name) == expected,
                'source closure file differs')
    destination = Path(destination)
    require(not destination.exists(), 'archive output already exists')
    with destination.open('xb') as stream:
        subprocess.run(['git', 'archive', '--format=tar', source['commit']], cwd=root,
                       stdout=stream, check=True, timeout=60)
    return file_sha(destination)


def extract_source(archive, destination, expected_sha, source, members):
    require(file_sha(archive) == expected_sha, 'source archive digest differs')
    destination = Path(destination)
    require(not destination.exists(), 'source extraction must be fresh')
    destination.mkdir(parents=True)
    names = set()
    # git archive metadata is not a credential. Symlinks/hardlinks are rejected.
    with tarfile.open(archive, mode='r:') as tar:
        for member in tar:
            name = member.name.rstrip('/')
            safe_source_path(name)
            require(name not in names and (member.isfile() or member.isdir()), 'archive alias/link')
            names.add(name)
            path = destination / name
            if member.isdir():
                path.mkdir(parents=True, exist_ok=True)
                continue
            require(member.size <= MAX_MEMBER, 'source member too large')
            path.parent.mkdir(parents=True, exist_ok=True)
            with tar.extractfile(member) as incoming, path.open('xb') as out:
                while block := incoming.read(MIB):
                    out.write(block)
            os.chmod(path, 0o755 if member.mode & 0o111 else 0o644)
    require(digest(canonical(sorted(members.items()))) == source['source_closure_sha256'],
            'archive closure identity differs')
    for name, expected in members.items():
        require(file_sha(destination / safe_source_path(name)) == expected, 'archive source file differs')
    return destination


def producer(api, run_id, registration, mode, status=('running', 'in_progress', 'success')):
    run = api.get('/actions/runs/' + str(receipts.positive(run_id)))
    require(run.get('id')==run_id and type(run.get('id')) is int
            and run.get('workflow_id') == WORKFLOW and run.get('event') == 'workflow_dispatch'
            and run.get('commit_sha') == registration['source']['commit']
            and run.get('repository', {}).get('id') == registration['repository'],
            'unregistered producer run/source')
    event = decode(run.get('event_payload', '').encode())
    require(event.get('repository', {}).get('id') == registration['repository']
            and event.get('inputs', {}).get('mode') == mode
            and event.get('inputs', {}).get('registration_sha256') == digest(canonical(registration)),
            'producer event registration/mode differs')
    jobs = [j for j in api.pages('/actions/runs/' + str(run_id) + '/jobs')
            if j.get('name') == (COMPLETION_JOB if mode == 'complete' else PRODUCER_JOB)]
    require(len(jobs) == 1, 'producer job ambiguous')
    job = jobs[0]
    require(job.get('attempt') == 1 and type(job.get('attempt')) is int
            and job.get('status') in status and job.get('run_id') == run_id
            and job.get('repo_id') == registration['repository'], 'producer attempt/status differs')
    for key in ('id', 'task_id'):
        receipts.positive(job.get(key))
    return run, job, event


def producer_log(api, run_id, job, job_key):
    path='/actions/jobs/'+str(job['id'])+'/logs?attempt=1'
    request=urllib.request.Request(api.root+path,headers={'Authorization':'token '+api.token})
    api.reserve_request()
    maximum=32*MIB
    target=Path(api.scratch)/('task-'+str(job['task_id'])+'-log-'+str(time.monotonic_ns())+'.txt')
    with api.opener.open(request,timeout=30) as incoming,target.open('xb') as out:
        count=0
        require(hasattr(incoming,'read1'),'bounded live task-log reader unavailable')
        while block:=incoming.read1(64*1024):
            count+=len(block);require(count<=maximum,'task raw log allowance exceeded')
            if hasattr(api,'guard_storage'):api.guard_storage(len(block))
            out.write(block)
    raw=target.read_bytes()
    require(all(len(line)<=2*SMALL for line in raw.splitlines()),'task raw line allowance exceeded')
    try:
        lines=[re.sub(r'^\d{4}-\d\d-\d\dT[^ ]+ ', '', line) for line in raw.decode('utf-8').splitlines()]
    except UnicodeError:
        raise receipts.IntegrityError('Async session: task log is not strict UTF-8') from None
    expected = (r'Runner [0-9a-f-]+ \(version:[^)]*\) received task '
                + str(job['task_id']) + ' of job ' + re.escape(job_key)
                + r', triggered by event: workflow_dispatch')
    require(lines and re.fullmatch(expected, lines[0]), 'producer task preamble differs')
    return raw, lines


def artifact_bytes(api, artifact, maximum=MAX_ARTIFACT):
    require(artifact.get('expired') is False and type(artifact.get('size_in_bytes')) is int
            and 0 < artifact['size_in_bytes'] <= maximum, 'expired or oversized artifact')
    # API.bytes intentionally has a small JSON bound. Stream large archives
    # through the same authenticated, no-redirect origin instead.
    expires = artifact.get('expires_at')
    if expires is not None:
        try:
            expiry = datetime.fromisoformat(expires.replace('Z','+00:00'))
            require(expiry.tzinfo is not None and expiry > datetime.now(timezone.utc), 'artifact expiry reached')
        except (ValueError,TypeError,AttributeError):
            raise receipts.IntegrityError('Async session: malformed artifact expiry') from None
    ident = receipts.positive(artifact.get('id'))
    path = '/actions/artifacts/' + str(ident) + '/zip'
    request = urllib.request.Request(api.root + path,
        headers={'Authorization': 'token ' + api.token, 'Accept': 'application/zip'})
    if hasattr(api,'reserve_request'):
        api.reserve_request()
    else:
        api.requests += 1
        require(api.requests <= 200, 'API request allowance exhausted')
    fd, filename = tempfile.mkstemp(prefix='artifact-', suffix='.zip', dir=api.scratch)
    os.close(fd)
    target = Path(filename)
    try:
        count = 0
        with api.opener.open(request, timeout=30) as incoming, target.open('wb') as out:
            require(hasattr(incoming,'read1'),'bounded artifact reader unavailable')
            while block := incoming.read1(MIB):
                count += len(block)
                require(count <= maximum, 'artifact download exceeded limit')
                if hasattr(api,'guard_storage'):api.guard_storage(len(block))
                out.write(block)
        require(count == artifact['size_in_bytes'], 'artifact API size differs')
        return target
    except BaseException:
        target.unlink(missing_ok=True)
        raise


def unpack(archive, destination, manifest, *, storage_guard=None, verify_existing=False):
    """Extract only an exact authenticated member inventory, with bounded IO."""
    destination = Path(destination)
    require(destination.is_dir() and not destination.is_symlink() if verify_existing else not destination.exists(),
            'artifact extraction/cache directory state differs')
    expected = {}
    for row in manifest:
        closed(row, ('path', 'bytes', 'sha256'))
        name = safe_path(row['path'])
        require(name not in expected and type(row['bytes']) is int
                and 0 <= row['bytes'] <= MAX_MEMBER and SHA.fullmatch(row['sha256']),
                'invalid member inventory')
        expected[name] = row
    if storage_guard is not None and not verify_existing:
        storage_guard(sum(row['bytes']+4096*(len(PurePosixPath(row['path']).parts)+1) for row in expected.values()))
    require(len(expected) <= MAX_MEMBERS and sum(r['bytes'] for r in expected.values()) <= MAX_ARTIFACT,
            'artifact extraction allowance exceeded')
    if verify_existing:
        present=set()
        for path in destination.rglob('*'):
            require(not path.is_symlink(),'cached artifact symlink')
            if path.is_file():present.add(path.relative_to(destination).as_posix())
        require(present==set(expected),'cached artifact inventory differs')
    else:destination.mkdir(parents=True)
    seen = set()
    with zipfile.ZipFile(archive) as z:
        require(len(z.infolist()) <= MAX_MEMBERS, 'ZIP member count exceeded')
        for member in z.infolist():
            name = safe_path(member.filename)
            mode = member.external_attr >> 16
            require(not member.is_dir() and not member.flag_bits & 1
                    and stat.S_IFMT(mode) in (0, stat.S_IFREG)
                    and name not in seen and name in expected, 'ZIP hidden/aliased/link member')
            row = expected[name]
            require(member.file_size == row['bytes'], 'ZIP declared member size differs')
            seen.add(name)
            target = destination / name
            if not verify_existing:target.parent.mkdir(parents=True, exist_ok=True)
            with z.open(member) as incoming, target.open('rb' if verify_existing else 'xb') as out:
                h = hashlib.sha256()
                size = 0
                while block := incoming.read(MIB):
                    size += len(block)
                    require(size <= row['bytes'], 'ZIP member exceeded bound')
                    h.update(block)
                    if storage_guard is not None:storage_guard(0 if verify_existing else len(block))
                    if verify_existing:require(out.read(len(block))==block,'cached bytes differ from actual current ZIP')
                    else:out.write(block)
                if verify_existing:require(not out.read(1),'cached artifact trailing bytes')
            require(size == row['bytes'] and h.hexdigest() == row['sha256'], 'ZIP member digest differs')
    require(seen == set(expected), 'artifact member missing')
    return destination


class DependencyIndex:
    """Disk-backed exact key/content index; hash collisions do not imply equality."""
    def __init__(self, path, database=None):
        # `database` is an already granted connection (a physical DatabaseGrant on the proof
        # host); without one the index opens its own plain connection on a read-only consumer.
        self.db = sqlite3.connect(path) if database is None else database
        self.db.execute('CREATE TABLE IF NOT EXISTS objects(kind TEXT,key BLOB,digest TEXT,payload BLOB,path TEXT,PRIMARY KEY(kind,key))')
        self.db.execute('CREATE TABLE IF NOT EXISTS authenticated_members(unit_ordinal INTEGER,member_path TEXT,bytes INTEGER,sha256 TEXT,local_path TEXT,PRIMARY KEY(unit_ordinal,member_path))')
        self.db.execute('CREATE TABLE IF NOT EXISTS authenticated_snapshot_roots(kind TEXT,unit_ordinal INTEGER,bytes INTEGER,sha256 TEXT,local_path TEXT,PRIMARY KEY(kind,unit_ordinal))')
        self.db.execute('CREATE TABLE IF NOT EXISTS native_locators(unit_ordinal INTEGER,member_path TEXT,batch_receipt_sha256 TEXT,publication_origin BLOB,artifact_id INTEGER,part_ordinal INTEGER,bytes INTEGER,sha256 TEXT,PRIMARY KEY(unit_ordinal,member_path,batch_receipt_sha256,artifact_id,part_ordinal))')
        self.db.execute('CREATE TABLE IF NOT EXISTS verifier_chain(unit_ordinal INTEGER PRIMARY KEY,progress_path TEXT,backup_root_path TEXT,work_unit_json BLOB,execution_origin_json BLOB)')
        self.db.execute('CREATE TABLE IF NOT EXISTS roots(kind TEXT,key BLOB,digest TEXT,PRIMARY KEY(kind,key))')
        self.db.execute('CREATE TABLE IF NOT EXISTS edges(owner_kind TEXT,owner_key BLOB,kind TEXT,key BLOB,digest TEXT,PRIMARY KEY(owner_kind,owner_key,kind,key))')

    def close(self):
        self.db.close()

    def add(self, reference, payload, path):
        closed(reference, ('kind', 'id', 'sha256'))
        key = canonical(reference['id'])
        row = self.db.execute('SELECT digest,payload FROM objects WHERE kind=? AND key=?',
                              (reference['kind'], key)).fetchone()
        require(digest(payload) == reference['sha256'], 'reference content mismatch')
        require(row is None or row == (reference['sha256'], payload), 'conflicting global identity/collision')
        self.db.execute('INSERT OR IGNORE INTO objects VALUES(?,?,?,?,?)',
                        (reference['kind'], key, reference['sha256'], payload, str(path)))
        self.db.commit()

    def add_locator(self, unit_ordinal, locator):
        """Record an authenticated native locator; callers pass only reconstructed rows."""
        closed(locator, ('batch_receipt_sha256', 'publication_origin', 'artifact_id', 'part_ordinal',
                         'member_path', 'bytes', 'sha256'))
        row = (unit_ordinal, locator['member_path'], locator['batch_receipt_sha256'],
               canonical(locator['publication_origin']), locator['artifact_id'], locator['part_ordinal'],
               locator['bytes'], locator['sha256'])
        old = self.db.execute('SELECT bytes,sha256 FROM native_locators WHERE unit_ordinal=? AND member_path=? '
                              'AND batch_receipt_sha256=? AND artifact_id=? AND part_ordinal=?',
                              (row[0], row[1], row[2], row[4], row[5])).fetchone()
        require(old is None or old == (row[6], row[7]), 'conflicting authenticated native locator')
        self.db.execute('INSERT OR IGNORE INTO native_locators VALUES(?,?,?,?,?,?,?,?)', row)
        self.db.commit()

    def root(self, reference):
        self.resolve(reference)
        self.db.execute('INSERT OR IGNORE INTO roots VALUES(?,?,?)',
                        (reference['kind'], canonical(reference['id']), reference['sha256']))
        self.db.commit()

    def need(self, owner, target):
        self.db.execute('INSERT OR IGNORE INTO edges VALUES(?,?,?,?,?)',
                        (owner['kind'], canonical(owner['id']), target['kind'],
                         canonical(target['id']), target['sha256']))
        self.db.commit()

    def resolve(self, reference):
        row = self.db.execute('SELECT digest,payload,path FROM objects WHERE kind=? AND key=?',
                              (reference['kind'], canonical(reference['id']))).fetchone()
        require(row is not None and row[0] == reference['sha256'], 'missing/conflicting dependency')
        return row[1], Path(row[2])

    def closure(self, roots):
        # Recursive CTE lives on disk, including visited-set deduplication.
        self.db.execute('DROP TABLE IF EXISTS reached')
        self.db.execute('CREATE TEMP TABLE reached(kind TEXT,key BLOB,PRIMARY KEY(kind,key))')
        for root in roots:
            self.resolve(root)
            self.db.execute('INSERT OR IGNORE INTO reached VALUES(?,?)', (root['kind'], canonical(root['id'])))
        while True:
            missing = self.db.execute('SELECT 1 FROM edges e JOIN reached r ON e.owner_kind=r.kind AND e.owner_key=r.key LEFT JOIN objects o ON o.kind=e.kind AND o.key=e.key WHERE o.digest IS NULL OR o.digest!=e.digest LIMIT 1').fetchone()
            require(missing is None, 'transitive dependency missing or forged')
            cursor = self.db.execute('INSERT OR IGNORE INTO reached SELECT e.kind,e.key FROM edges e JOIN reached r ON e.owner_kind=r.kind AND e.owner_key=r.key')
            if cursor.rowcount == 0:
                break
        self.db.commit()
        return self.db.execute('SELECT count(*) FROM reached').fetchone()[0]


def authenticate_artifact(api, artifact, unit, registration, checkpoint, manifest, destination):
    run, job, event = producer(api, unit['run'], registration, unit['mode'], ('success',))
    require((job['id'], job['task_id'], job['attempt']) == (unit['job'], unit['task'], unit['attempt']),
            'artifact task/run/attempt differs')
    require(artifact.get('workflow_run', {}).get('id', artifact.get('run_id')) == run['id'],
            'artifact producer run differs')
    raw, lines = producer_log(api, run['id'], job,
                              'complete' if unit['mode'] == 'complete' else 'unit')
    archive = artifact_bytes(api, artifact)
    try:
        actual = file_sha(archive)
        uploaded = transport.upload_transaction_sha256(lines, artifact['name'], artifact['id'],
                                                        artifact['size_in_bytes'])
        require(actual == uploaded, 'uploader/API/local artifact digest differs')
        directory = unpack(archive, destination, manifest)
        value = {'artifact_id': artifact['id'], 'session_id': unit['session_id'],
                 'unit_ordinal': unit['unit_ordinal'], 'run': run['id'], 'job': job['id'],
                 'task': job['task_id'], 'attempt': 1, 'source': unit['source'], 'name': artifact['name'],
                 'expired': False, 'zip_sha256': actual, 'zip_bytes': archive.stat().st_size,
                 'extracted_bytes': sum(r['bytes'] for r in manifest), 'member_count': len(manifest),
                 'members': manifest, 'upload_log_sha256': upload_slice(lines, artifact['name'], artifact['id'], artifact['size_in_bytes'])[0],
                 'event_payload_sha256': digest(canonical(event)),
                 'expected_prior_checkpoint': unit['expected_prior_checkpoint'],
                 'produced_checkpoint': checkpoint}
        return {'schema_version': 6, 'kind': 'ArtifactReceipt', 'record': value}, directory
    finally:
        archive.unlink(missing_ok=True)

TRANSPORT_SCHEMA = 'verification/async-driver-refinement/paged-catalog-transport.schema.json'
UPLOAD_SLOTS = 32


def auxiliary(value, root):
    schema = read(Path(root) / TRANSPORT_SCHEMA)
    # Resolve only the one explicitly permitted adjacent frozen schema.
    schema = copy.deepcopy(schema)
    frozen = read(Path(root) / SCHEMA_PATH)
    def resolve(node):
        if isinstance(node, dict):
            reference = node.get('$ref', '')
            if reference.startswith('paged-proof.schema.json#/$defs/'):
                return resolve(copy.deepcopy(frozen['$defs'][reference.split('/')[-1]]))
            return {k: resolve(v) for k, v in node.items()}
        if isinstance(node, list):
            return [resolve(v) for v in node]
        return node
    # Frozen nested local refs belong to the frozen schema, not the adjunct.
    schema['$defs'].update(frozen['$defs'])
    schema = resolve(schema)
    kind = value.get('kind')
    kinds = {'catalog_backup':'CatalogBackup', 'verifier_backup':'VerifierBackup',
             'snapshot_manifest_node':'SnapshotManifestNode', 'evidence_manifest_node':'EvidenceManifestNode',
             'transfer_manifest_node':'TransferManifestNode', 'publication_intent':'PublicationIntent',
             'pending_publication':'PendingPublication', 'transfer_batch_receipt':'TransferBatchReceipt',
             'publication_activation':'ActivationResult','native_transport_conformance':'ConformanceResult',
             'validation_receipt':'ValidationReceipt'}
    require(kind in kinds, 'unknown transport auxiliary envelope')
    validate(value, schema['$defs'][kinds[kind]], schema)
    return value


VERIFIER_SCHEMA = 'verification/async-driver-refinement/paged-verifier-continuation.schema.json'


def verifier_auxiliary(value, root):
    schema = read(Path(root) / VERIFIER_SCHEMA)
    schema = copy.deepcopy(schema)
    catalog = read(Path(root) / TRANSPORT_SCHEMA)
    frozen = read(Path(root) / SCHEMA_PATH)
    schema['$defs'].update(catalog['$defs'])
    schema['$defs'].update(frozen['$defs'])
    def local(node):
        if isinstance(node, dict):
            reference = node.get('$ref','')
            if reference.startswith(('paged-proof.schema.json#/$defs/',
                                     'paged-catalog-transport.schema.json#/$defs/')):
                return {'$ref':'#/$defs/'+reference.split('/')[-1]}
            return {k:local(v) for k,v in node.items()}
        if isinstance(node,list): return [local(v) for v in node]
        return node
    schema = local(schema)
    kinds = {'verifier_backup':'VerifierBackup','verifier_chunk_index':'VerifierChunkIndex',
             'verifier_unit_progress':'VerifierUnitProgress','verifier_check_result':'VerifierCheckResult',
             'verifier_evidence_index':'VerifierEvidenceIndex'}
    require(value.get('kind') in kinds, 'unknown verifier auxiliary kind')
    validate(value,schema['$defs'][kinds[value['kind']]],schema)
    return value


class PartPlan:
    """Disk inventory: no global backup/member descriptor list."""
    def __init__(self, staging, slots=UPLOAD_SLOTS, limits=None, *, database=None, budget=None):
        self.root = Path(staging)
        require(not self.root.exists(), 'multipart staging must be fresh')
        self.root.mkdir(parents=True)
        self.slots = slots
        self.limits = limits
        # A controller passes a physically granted connection factory and the transfer
        # staging pool; every staged copy is reserved before it is created.
        self.budget = budget
        self.db = (sqlite3.connect if database is None else database)(self.root / 'inventory.sqlite')
        self.db.execute('CREATE TABLE members(path TEXT PRIMARY KEY,part INTEGER,bytes INTEGER,digest TEXT)')
        self.db.execute('CREATE TABLE indexes(level INTEGER,first_chunk INTEGER,path TEXT,part INTEGER,digest TEXT,chunk_count INTEGER,offset INTEGER,bytes INTEGER,PRIMARY KEY(level,first_chunk))')
        self.db.commit()

    def guard(self, additional=0):
        if self.limits is None:return
        base=Path(self.limits['owned_root'])
        used=sum(path.stat().st_size for path in base.rglob('*') if path.is_file() and not path.is_symlink())
        require(used+additional<=self.limits['disk_bytes'],'actual session disk allocation exhausted')
        require(time.monotonic()<=self.limits['deadline_monotonic'],'unit packing deadline exhausted')
        staged=self.db.execute('SELECT coalesce(sum(bytes),0) FROM members').fetchone()[0]
        require(staged+additional<=self.limits['unit_bytes'],'unit staged byte allocation exhausted')

    def directory(self, ordinal):
        require(type(ordinal) is int and 0 <= ordinal < self.slots, 'native upload slots exhausted')
        directory = self.root / 'parts' / str(ordinal)
        directory.mkdir(parents=True, exist_ok=True)
        return directory

    def allocate(self, size, prefer=0):
        require(0 <= size <= MAX_MEMBER, 'part member size exceeded')
        self.guard(size)
        if self.budget is not None:
            self.budget.reserve(size)
        for ordinal in range(prefer, self.slots):
            count, total = self.db.execute('SELECT count(*),coalesce(sum(bytes),0) FROM members WHERE part=?', (ordinal,)).fetchone()
            # Reserve metadata and conservative deflate expansion/ZIP headers.
            if count < MAX_MEMBERS - 1 and total + size <= MAX_ARTIFACT - 16 * MIB:
                return ordinal
        require(False, 'actual packaging exceeds source-pinned native slots')

    def reserve(self,name,maximum_bytes,role):
        safe_path(name)
        require(type(maximum_bytes) is int and 0<maximum_bytes<=SMALL,'metadata reservation bound')
        part=self.allocate(maximum_bytes)
        self.db.execute('INSERT INTO members VALUES(?,?,?,?)',(name,part,maximum_bytes,'reserved:'+role))
        self.db.commit();return part

    def fulfill(self,name,source):
        row=self.db.execute('SELECT part,bytes,digest FROM members WHERE path=?',(name,)).fetchone()
        require(row is not None and row[2].startswith('reserved:'),'member is not reserved')
        source=Path(source);require(source.is_file() and not source.is_symlink()
                                   and 0<source.stat().st_size<=row[1],'sealed metadata exceeds reservation')
        target=self.directory(row[0])/safe_path(name);target.parent.mkdir(parents=True,exist_ok=True)
        require(not target.exists(),'reserved target already materialized')
        with source.open('rb') as incoming,target.open('xb') as out:
            for block in iter(lambda:incoming.read(MIB),b''):out.write(block)
        self.db.execute('UPDATE members SET bytes=?,digest=? WHERE path=?',
                        (target.stat().st_size,file_sha(target),name));self.db.commit()
        return row[0]

    def add_bytes(self, name, data, part=None):
        require(len(data) <= SMALL, 'transport metadata exceeds8MiB')
        self.guard(len(data))
        reserved=self.db.execute('SELECT part,bytes,digest FROM members WHERE path=?',(name,)).fetchone()
        if reserved is not None:
            require(reserved[2].startswith('reserved') and len(data)<=reserved[1], 'metadata reservation changed/exceeded')
            require(part is None or part==reserved[0], 'reserved metadata part changed')
            ordinal=reserved[0]
        else:
            ordinal = self.allocate(len(data)) if part is None else part
        target = self.directory(ordinal) / safe_path(name)
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open('xb') as stream:
            stream.write(data)
        if reserved is None:
            self.register(name, len(data), digest(data), ordinal)
        else:
            self.db.execute('UPDATE members SET bytes=?,digest=? WHERE path=?',(len(data),digest(data),name));self.db.commit()
        return ordinal

    def register(self, name, size, sha, part):
        safe_path(name)
        require(SHA.fullmatch(sha), 'bad member hash')
        self.db.execute('INSERT INTO members VALUES(?,?,?,?)', (name, part, size, sha))
        self.db.commit()

    def add_file(self, name, source, prefer=0):
        source = Path(source)
        require(source.is_file() and not source.is_symlink(), 'source member not regular')
        size = source.stat().st_size
        existing=self.db.execute('SELECT part,bytes,digest FROM members WHERE path=?',(name,)).fetchone()
        if existing is not None:
            if existing[2].startswith('reserved:'):return self.fulfill(name,source)
            require(existing[1]==size and existing[2]==file_sha(source),'conflicting packed member identity')
            with source.open('rb') as left,(self.directory(existing[0])/name).open('rb') as right:
                while True:
                    a=left.read(MIB);b=right.read(MIB)
                    require(a==b,'packed member digest collision/full-byte mismatch')
                    if not a:break
            return existing[0]
        ordinal = self.allocate(size, prefer)
        target = self.directory(ordinal) / safe_path(name)
        target.parent.mkdir(parents=True, exist_ok=True)
        with source.open('rb') as incoming, target.open('xb') as out:
            h = hashlib.sha256()
            written = 0
            for block in iter(lambda: incoming.read(MIB), b''):
                out.write(block); h.update(block); written += len(block)
        require(written == size, 'source member changed while packaging')
        self.register(name, size, h.hexdigest(), ordinal)
        return ordinal

    def manifest(self, ordinal):
        return [{'path': name, 'bytes': size, 'sha256': sha} for name, size, sha in
                self.db.execute('SELECT path,bytes,digest FROM members WHERE part=? ORDER BY path', (ordinal,))]

    def preflight(self, pending=False):
        if not pending:
            require(self.db.execute("SELECT 1 FROM members WHERE digest LIKE 'reserved%' LIMIT 1").fetchone() is None,
                    'unfulfilled metadata reservation')
        parts = self.db.execute('SELECT DISTINCT part FROM members ORDER BY part').fetchall()
        require(parts and [r[0] for r in parts] == list(range(len(parts))) and len(parts) <= self.slots,
                'multipart ordinal gap/slot overrun')
        for (ordinal,) in parts:
            rows = self.manifest(ordinal)  # at most300 bounded members
            require(len(rows) <= MAX_MEMBERS and sum(r['bytes'] for r in rows) <= MAX_ARTIFACT,
                    'part extracted/member bound exceeded')
            # Native action compression interface is deliberately not assumed.
            # Bound incompressible deflate overhead plus headers conservatively.
            worst = sum(r['bytes'] + (r['bytes'] // 16383 + 1) * 5
                        + len(r['path'].encode()) * 2 + 1024 for r in rows) + 65536
            require(worst <= MAX_ARTIFACT, 'part ZIP overhead bound exceeded')
        return len(parts)

    def close(self):
        self.db.close()


def check_restored_catalog(staged, private_validation, semantic_validation, checkpoint, *, backup_root=None, input_preparation=False):
    """Both independent validators must bind exact staged bytes/checkpoint (no filesystem change)."""
    cp = checkpoint['record']
    shared = {'catalog_sha256': file_sha(staged), 'checkpoint_sha256': digest(canonical(checkpoint)),
              'source': cp['source'], 'session_id': cp['session_id'],
              'catalog_commit': cp['committed_transaction'],
              'catalog_logical_digest': cp['catalog_logical_digest'],
              'counts': cp['counts'], 'contexts': cp['contexts']}
    private = {'schema_version': 6, 'operation': 'validate_restore', **shared,
               'catalog_bytes': Path(staged).stat().st_size, 'semantic_manifest_root': cp['page_root'],
               'initial_image_sha256': cp['initial_image_id'], 'resource_plan_sha256': cp['resource_plan_sha256']}
    semantic = {'schema_version': 6, **shared, 'manifest_root': cp['page_root'],
                'complete': True, 'acceptance': False,
                'scope': 'restored catalog/page identity correspondence; production replay and full semantic qualification remain separate'}
    if input_preparation:
        semantic.update(complete=False,scope='input_preparation')
    require(private_validation == private and semantic_validation == semantic,
            'private catalog or independent semantic restore validation differs')
    require(type(backup_root) is dict and backup_root.get('kind')=='catalog_backup'
            and backup_root.get('catalog_sha256')==shared['catalog_sha256']
            and backup_root.get('catalog_bytes')==Path(staged).stat().st_size
            and backup_root.get('checkpoint_sha256')==shared['checkpoint_sha256']
            and backup_root.get('source')==cp['source'] and backup_root.get('session_id')==cp['session_id'],
            'post-validator actual catalog bytes differ from authenticated backup root')


def atomic_install(staged, target, private_validation, semantic_validation, checkpoint, *, backup_root=None, input_preparation=False):
    """Validate, then atomically rename the staged catalog beside itself."""
    check_restored_catalog(staged, private_validation, semantic_validation, checkpoint,
                           backup_root=backup_root, input_preparation=input_preparation)
    target = Path(target)
    require(Path(staged).parent == target.parent and not target.is_symlink(), 'atomic restore crosses owned directory')
    for suffix in ('-wal', '-shm', '-journal'):
        require(not Path(str(target) + suffix).exists(), 'active catalog writer/journal')
    lease = target.with_name(target.name + '.restore-lock')
    with lease.open('xb'):
        os.replace(staged, target)
        directory = os.open(target.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    lease.unlink()


SOURCE_BOUND_SCHEMA = 'verification/async-driver-refinement/source-bound-receipt.schema.json'
SOURCE_BOUND_SPEC = 'verification/async-driver-refinement/source-bound-formulas.json'


def source_bound_receipt(value, root, source, registered_files, cap):
    """Independently check the reviewed construction and evaluate its closed DAG.

    A capped arithmetic result is a proven greater-than marker, never an exact
    printed cardinality. The full symbolic formulas remain in the receipt.
    """
    schema = copy.deepcopy(read(Path(root)/SOURCE_BOUND_SCHEMA))
    frozen = read(Path(root)/SCHEMA_PATH)
    schema['$defs'].update(frozen['$defs'])
    def local(node):
        if isinstance(node,dict):
            reference=node.get('$ref','')
            if reference.startswith('paged-proof.schema.json#/$defs/'):
                return {'$ref':'#/$defs/'+reference.split('/')[-1]}
            return {k:local(v) for k,v in node.items()}
        if isinstance(node,list):return [local(v) for v in node]
        return node
    schema=local(schema);validate(value,schema['$defs']['SourceBoundReceipt'],schema)
    spec_path=Path(root)/SOURCE_BOUND_SPEC
    specification=read(spec_path)
    closed(specification,('schema_version','kind','algorithm_version','complete','finite_inventory_sha256',
                         'record_schemas','init_class','init_class_rules','primitive_bindings','formulas',
                         'records','limits','source_inputs','report_role','inventory_mapping'))
    require(specification['schema_version']==1 and specification['kind']=='source_bound_specification'
            and specification['algorithm_version']==1 and specification['complete'] is True
            and specification['report_role']==value['report_role']=='source_global_symbolic',
            'source-bound per-field derivation is incomplete')
    require(value['source']==source and value['specification_sha256']==file_sha(spec_path)
            ==registered_files.get(SOURCE_BOUND_SPEC)
            and value['finite_inventory_sha256']==specification['finite_inventory_sha256']
            ==source['finite_inventory_sha256'], 'source-bound receipt/specification source differs')
    for key in ('record_schemas','init_class','primitive_bindings','formulas','records'):
        require(value[key]==specification[key], 'receipt changes registered source derivation')
    for inventory in (specification['record_schemas'],specification['source_inputs']):
        names=[]
        for row in inventory:
            closed(row,('path','sha256'));safe_source_path(row['path'])
            require(row['sha256']==registered_files.get(row['path'])==file_sha(Path(root)/row['path']),
                    'source/schema operand bytes differ')
            names.append(row['path'])
        require(names==sorted(set(names)), 'source/schema inventory not sorted unique')
    require({SCHEMA_PATH,TRANSPORT_SCHEMA,VERIFIER_SCHEMA,SOURCE_BOUND_SCHEMA,CONTROL_SCHEMA}
            <= {r['path'] for r in specification['record_schemas']}, 'record schema closure missing')
    rules=specification['init_class_rules']
    require(type(rules) is list and rules and all(type(r) is dict and isinstance(r.get('id'),str) for r in rules),
            'Init-class reviewed rule bodies unavailable')
    for row in rules:
        closed(row,('id','rule_id','source_path','source_sha256','anchor','operands'))
        path=Path(root)/safe_source_path(row['source_path'])
        require(row['source_sha256']==registered_files.get(row['source_path'])==file_sha(path)
                and row['anchor'] in path.read_text() and type(row['operands']) is list
                and all(isinstance(v,str) and re.fullmatch(r'(?:0|[1-9][0-9]*)',v) for v in row['operands']),
                'Init-class source rule/operand differs')
    require([r['id'] for r in rules]==sorted({r['id'] for r in rules})
            and value['init_class']['predicates']==[r['id'] for r in rules]
            and value['init_class']['specification_sha256']==digest(canonical(rules)),
            'Init-class predicate preimage differs')
    limits=specification['limits']
    closed(limits,('maximum_nodes','maximum_depth','maximum_arguments','maximum_integer_digits','maximum_metadata_bytes'))
    require(all(type(v) is int and v>0 for v in limits.values())
            and limits['maximum_metadata_bytes']<=SMALL and limits['maximum_integer_digits']<=4096
            and len(canonical(value))<=limits['maximum_metadata_bytes'], 'source-bound metadata limits exceeded')
    primitive={}
    for row in value['primitive_bindings']:
        require(row['id'] not in primitive and len(row['value'])<=limits['maximum_integer_digits'],
                'duplicate/oversized primitive')
        path=Path(root)/safe_source_path(row['source_path'])
        require(row['source_sha256']==registered_files.get(row['source_path'])==file_sha(path)
                and row['anchor'] in path.read_text(), 'primitive source anchor unavailable')
        primitive[row['id']]=int(row['value'])
    require([r['id'] for r in value['primitive_bindings']]==sorted(primitive), 'primitive inventory not sorted')
    nodes=value['formulas'];require(len(nodes)<=limits['maximum_nodes'],'source formula node allowance exceeded')
    values={};depths={}
    def capped(number):return (number,True) if number<=cap else (cap+1,False)
    def multiply(args):
        if any(v==0 and exact for v,exact in args):return (0,True)
        if any(not exact for _,exact in args):return (cap+1,False)
        result=1
        for number,_ in args:
            result*=number
            if result>cap:return (cap+1,False)
        return result,True
    for node in nodes:
        name=node['id'];require(name not in values,'duplicate formula ID')
        operation=node['op'];dependencies=[]
        if operation=='literal':
            require(len(node['value'])<=limits['maximum_integer_digits'],'literal digits exceeded')
            result=capped(int(node['value']))
        elif operation=='ref':
            require(node['primitive_id'] in primitive,'unknown primitive reference')
            result=capped(primitive[node['primitive_id']])
        else:
            dependencies=node['args'] if operation in ('add','multiply','max') else [node['base'],node['exponent']]
            require(dependencies and len(dependencies)<=limits['maximum_arguments']
                    and all(d in values for d in dependencies),'cyclic/forward/unknown formula operand')
            args=[values[d] for d in dependencies]
            if operation=='add':
                result=capped(sum(v for v,_ in args)) if all(e for _,e in args) else (cap+1,False)
            elif operation=='multiply':result=multiply(args)
            elif operation=='max':result=max(args,key=lambda v:v[0])
            elif operation=='power':
                (base,bexact),(exponent,eexact)=args
                if eexact and exponent==0:result=(1,True)
                elif bexact and base in (0,1):result=(base,True)
                elif not bexact or not eexact:result=(cap+1,False)
                else:
                    result=(1,True);factor=(base,True)
                    while exponent:
                        if exponent&1:result=multiply([result,factor])
                        exponent//=2
                        if exponent:factor=multiply([factor,factor])
                        if not result[1]:break
            else:require(False,'unknown formula operator')
        depth=1+max((depths[d] for d in dependencies),default=0)
        require(depth<=limits['maximum_depth'],'source formula depth exceeded')
        values[name]=result;depths[name]=depth
    require(specification['inventory_mapping']==accounting_mapping(),'source-reviewed lifetime/nonsemantic inventory differs')
    inventory=[r['record_kind'] for r in value['records']]
    require(set(inventory)==set(SEMANTIC_SOURCE_KINDS) and len(set(inventory))==len(inventory)
            and value['complete_kind_inventory_sha256']==digest(canonical(inventory)),
            'complete record-kind inventory differs')
    premises=set(primitive)|set(value['init_class']['predicates'])
    widths={};finite={}
    for row in value['records']:
        require(set(row['premise_ids'])<=premises and row['encoded_width_formula'] in values
                and row['finite_cardinality_formula'] in values,'missing record width/cardinality premise')
        width,exact=values[row['encoded_width_formula']]
        require(width>0,'source-global encoded width is not positive')
        widths[row['record_kind']]={'formula':row['encoded_width_formula'],
                                  'exact_value':str(width) if exact else None,
                                  'proven_greater_than':None if exact else str(cap)}
        number,exact=values[row['finite_cardinality_formula']]
        finite[row['record_kind']]={'formula':row['finite_cardinality_formula'],
                                   'exact_value':str(number) if exact else None,
                                   'proven_greater_than':None if exact else str(cap)}
    return widths,finite


SEMANTIC_SOURCE_KINDS=('PhysicalRows','Episode','Store','Product','Applied','OracleEvidence','Candidate','Demand',
    'ProductEdge','FairPair','Enabledness','Expansion','Counts','ContextRoot','InitialImage','MonitorNode',
    'MonitorEdge','Scc','Counterexample','Outcome','BoundaryObservation')
CONTROL_ACCOUNTING_KINDS=('Page','ManifestNode','Checkpoint','ResourcePlan','Session','WorkUnit','Completion','ArtifactReceipt')
AUXILIARY_ACCOUNTING_KINDS=('catalog_backup','verifier_backup','snapshot_manifest_node','evidence_manifest_node',
    'transfer_manifest_node','publication_intent','pending_publication','transfer_batch_receipt',
    'publication_activation','native_transport_conformance','validation_receipt',
    'verifier_unit_progress','verifier_check_result','verifier_evidence_index','source_bound_receipt','async_session_control')


def accounting_mapping():
    rows=[]
    for kind in SEMANTIC_SOURCE_KINDS:
        rows.append({'kind':kind,'role':'source_global','lifetime':'session_unique_global_record',
                     'source_record_kind':kind,'overhead_field':None})
    for kind in CONTROL_ACCOUNTING_KINDS+AUXILIARY_ACCOUNTING_KINDS:
        rows.append({'kind':kind,'role':'operational_only','lifetime':'session_retained_unit_member',
                     'source_record_kind':None,'overhead_field':None})
    # v3 physical caps: each nonsemantic class is charged to an explicit pool/family
    # of PhysicalPolicy3 (never multiplied against logical record allowances).
    for kind,field in (('logs','retained_artifact_bytes'),('source_archive','source_runtime_scratch_bytes'),
                       ('backup_and_chunks','snapshot_spool_bytes'),('downloads_and_restores','restore_staging_bytes'),
                       ('sql_indexes','family_main_bytes'),('sql_wal','family_journal_bytes')):
        rows.append({'kind':kind,'role':'operational_overhead','lifetime':'peak_workspace',
                     'source_record_kind':None,'overhead_field':field})
    rows.append({'kind':'decoded_objects_and_cache','role':'operational_overhead','lifetime':'resident_working_set',
                 'source_record_kind':None,'overhead_field':None})
    return sorted(rows,key=lambda row:row['kind'])


CONTROL_SCHEMA = 'verification/async-driver-refinement/paged-control-carrier.schema.json'
CONTROL_PREFIX = 'ASYNC-PROOF-CONTROL-'


def upload_slice(lines, name, artifact_id, size):
    """Preserve the existing native validator; hash its exact normalized slice."""
    zip_sha = transport.upload_transaction_sha256(lines, name, artifact_id, size)
    terminal = f'Artifact {name} has been successfully uploaded! Final size is {size} bytes. Artifact ID is {artifact_id}'
    end = lines.index(terminal)
    starts = [i for i in range(end) if re.fullmatch(
        r'With the provided path, there will be [1-9][0-9]* files? uploaded', lines[i])]
    start = starts[-1]
    section = lines[start:end + 1]
    require(sum(bool(re.fullmatch(r'With the provided path, there will be [1-9][0-9]* files? uploaded', x))
                for x in section) == 1, 'interleaved native upload start')
    payload = ('\n'.join(section) + '\n').encode('utf-8')
    return digest(b'plurx-async-upload-transaction-v1\0' + payload), zip_sha, start, end


def control_binding(control):
    origin = control['origin']
    return digest(canonical({k: control[k] for k in
        ('registration_sha256','source_archive_sha256','session_id','unit_ordinal','role')} | {
        k: origin[k] for k in ('run','job','task','attempt')}))


def control_frame(control):
    raw = canonical(control)
    require(0 < len(raw) <= SMALL, 'whole control exceeds carrier allowance')
    encoded = base64.b64encode(raw).decode('ascii')
    chunks = [encoded[i:i+2048] for i in range(0,len(encoded),2048)]
    sha = digest(raw)
    return [f'ASYNC-PROOF-CONTROL-BEGIN 1 {control["role"]} {len(raw)} {len(chunks)} {sha}'] + [
        f'ASYNC-PROOF-CONTROL-CHUNK {i} {chunk}' for i,chunk in enumerate(chunks)] + [
        f'ASYNC-PROOF-CONTROL-END {sha}']


def parse_control_frame(lines, maximum=SMALL):
    """Unique canonical frame inside the mandatory synchronous finalizer."""
    starts = [(i,re.fullmatch(r'ASYNC-PROOF-CONTROL-BEGIN 1 (unit_checkpoint|completion|publication_batch|transport_conformance) ([1-9][0-9]*) ([1-9][0-9]*) ([0-9a-f]{64})',line))
              for i,line in enumerate(lines) if line.startswith(CONTROL_PREFIX+'BEGIN')]
    require(len(starts)==1 and starts[0][1] is not None, 'missing/duplicate/malformed control begin')
    start,match=starts[0];role,length,count,sha=match.groups();length=int(length);count=int(count)
    require(length<=min(maximum,SMALL) and count==(4*((length+2)//3)+2047)//2048,
            'control length/chunk allowance differs')
    require(start+count+1<len(lines),'truncated control')
    chunks=[]
    for ordinal in range(count):
        item=re.fullmatch(r'ASYNC-PROOF-CONTROL-CHUNK ([0-9]+) ([A-Za-z0-9+/]+={0,2})',lines[start+1+ordinal])
        require(item is not None and item[1]==str(ordinal), 'control chunk order differs')
        chunk=item[2]
        require(len(chunk)==2048 if ordinal<count-1 else 0<len(chunk)<=2048,'control chunk width differs')
        chunks.append(chunk)
    end=start+count+1
    require(lines[end]=='ASYNC-PROOF-CONTROL-END '+sha and
            [i for i,line in enumerate(lines) if line.startswith(CONTROL_PREFIX)]==list(range(start,end+1)),
            'duplicate/trailing/nested control protocol')
    encoded=''.join(chunks)
    try: raw=base64.b64decode(encoded,validate=True)
    except ValueError: raise receipts.IntegrityError('Async session: invalid control base64') from None
    require(len(raw)==length and digest(raw)==sha and base64.b64encode(raw).decode('ascii')==encoded,
            'control length/hash/base64 differs')
    value=decode(raw,maximum)
    require(canonical(value)==raw and value.get('role')==role and value.get('schema_version')==2,
            'noncanonical or obsolete control')
    binding=control_binding(value)
    begin=f'ASYNC-PROOF-FINALIZER-BEGIN 1 {binding}'
    done=f'ASYNC-PROOF-FINALIZER-DONE 1 {binding} {sha}'
    markers=[(i,line) for i,line in enumerate(lines) if line.startswith('ASYNC-PROOF-FINALIZER-')]
    require(len(markers)==2 and markers[0][1]==begin and markers[1][1]==done
            and markers[0][0]<start<end<markers[1][0], 'finalizer framing/order differs')
    return value,sha,markers[0][0],markers[1][0]


def validate_upload_order(lines, parts, finalizer_begin):
    previous=-1
    for part in parts:
        _,_,start,end=upload_slice(lines,part['name'],part['id'],part['size_in_bytes'])
        require(previous<start<=end<finalizer_begin,'overlapping/reordered/post-finalizer native uploads')
        previous=end
    starts=[i for i,line in enumerate(lines) if re.fullmatch(
        r'With the provided path, there will be [1-9][0-9]* files? uploaded',line)]
    require(len(starts)==len(parts) and all(i<finalizer_begin for i in starts),
            'unplanned native upload transaction')


def authenticate_transfer_context(api, registration, carrier, metadata, parts, artifact_receipts,
                                  root, expected_dispatch_ordinal, *, case_sha256=None):
    """Authenticate publisher locators without relaxing ordinary receipt origins.

    The caller has already revalidated each exact native API/upload/ZIP/member
    transaction. This separate context preserves the original WorkUnit producer.
    Returned content rows gain authority only through this batch's frozen lineage.
    """
    from validation import async_snapshot_transport as snapshots
    # Ordinary and rehearsal batches are deliberately not interchangeable: an
    # ordinary role refuses any conformance object, and a rehearsal role requires
    # the exact fixed case namespace. A receipt is never relabeled across them.
    conformance=carrier['transport_conformance']
    if case_sha256 is None:
        require(carrier['role'] in ('publication_batch','unit_checkpoint') and conformance is None,
                'ordinary batch authentication refuses a conformance producer event')
    else:
        require(carrier['role']=='transport_conformance' and conformance is not None
                and conformance['case_id']=='native_snapshot_three_batch_v1'
                and conformance['case_sha256']==case_sha256,
                'rehearsal batch is not the exact fixed case')
    batch=auxiliary(carrier['transfer_batch'],root)
    require(batch['retained_artifact_bytes']==sum(r['record']['zip_bytes'] for r in artifact_receipts.values()),
            'retained bytes do not equal actual unique archived bytes')
    publication=batch['publication_origin']
    execution=batch['execution_origin']
    require(publication==carrier['origin'] and publication['dispatch_ordinal']==expected_dispatch_ordinal
            and publication['mode']=='unit' and publication['attempt']==1,
            'publication origin/dispatch differs')
    _,job,event=producer(api,publication['run'],registration,'unit',('success',))
    require((publication['job'],publication['task'],publication['attempt'])==(job['id'],job['task_id'],1),
            'publication task differs')
    inputs=event['inputs']
    require(inputs['operation']==publication['operation'] and publication['operation'] in ('execute','publish'),
            'publication event operation differs')
    # Original execution success is indispensable even when every byte was later
    # copied successfully. A failed original cannot be repaired by publication.
    _,original,original_event=producer(api,execution['run'],registration,'unit',('success',))
    require(execution['operation']=='execute' and execution['mode']=='unit'
            and original_event['inputs']['operation']=='execute'
            and (execution['job'],execution['task'],execution['attempt'])==
                (original['id'],original['task_id'],1), 'original execution failed or foreign')
    unit=envelope(carrier['work_unit'],'WorkUnit',root)
    require((unit['run'],unit['job'],unit['task'],unit['attempt'])==
            (execution['run'],execution['job'],execution['task'],1),
            'publisher relabeled original WorkUnit')
    require(batch['artifact_receipts']==[artifact_receipts[n] for n in sorted(artifact_receipts)]
            and carrier['artifact_receipts']==batch['artifact_receipts'], 'current batch artifact set differs')
    for receipt in batch['artifact_receipts']:
        record=envelope(receipt,'ArtifactReceipt',root)
        require((record['run'],record['job'],record['task'],record['attempt'])==
                (publication['run'],publication['job'],publication['task'],1)
                and record['unit_ordinal']==unit['unit_ordinal'] and record['source']==registration['source'],
                'transfer receipt is not actual publisher')
    require(batch['source']==unit['source']==registration['source']
            and batch['session_id']==unit['session_id']==metadata['session_id']
            and batch['unit_ordinal']==unit['unit_ordinal']==metadata['unit_ordinal']
            and batch['resource_plan_sha256']==unit['resource_plan_sha256'], 'transfer unit binding differs')
    native={member['path']:(part,member) for part,receipt in artifact_receipts.items()
            for member in receipt['record']['members']}
    require(len(native)==sum(len(receipt['record']['members']) for receipt in artifact_receipts.values()),
            'current native member paths alias')
    used_nodes=set()
    def resolve(reference):
        require(reference['kind']=='transfer_manifest_node','transfer tree child kind differs')
        candidates=[(name,part,m) for name,(part,m) in native.items()
                    if name.startswith('transfer/node-') and m['sha256']==reference['sha256']
                    and m['bytes']==reference['bytes']]
        require(len(candidates)==1,'transfer node absent/ambiguous')
        name,part,_=candidates[0]
        path=parts[part]/safe_path(name)
        data=path.read_bytes()
        require(snapshots.content_ref('transfer_manifest_node',data)==reference,'transfer node changed')
        auxiliary(decode(data),root)
        used_nodes.add(name)
        return data
    leaves=snapshots.iter_keyed_manifest(batch['transfer_manifest'],resolve,batch,
                                         'transfer_index','transfer_manifest_node')
    selected=set()
    rows=[]  # at most32*300 native members; no session-global payload list
    for leaf in leaves:
        name=safe_path(leaf['member_path']);reference=leaf['content']
        require(name not in selected and name in native,'transfer member duplicate/missing')
        require(name=='content/'+reference['kind']+'/'+reference['sha256'],
                'transfer member name is not its content identity')
        part,member=native[name]
        require(part==leaf['part_ordinal'] and member['bytes']==reference['bytes']
                and member['sha256']==reference['sha256'],'transfer locator/member differs')
        path=parts[part]/name
        require(path.is_file() and not path.is_symlink() and file_sha(path)==reference['sha256'],
                'authenticated transfer bytes changed')
        selected.add(name)
        rows.append((reference,path,{'batch_receipt_sha256':digest(canonical(batch)),
            'publication_origin':publication,'artifact_id':artifact_receipts[part]['record']['artifact_id'],
            'part_ordinal':part,'member_path':name,'bytes':member['bytes'],'sha256':member['sha256']}))
    require(set(native)==selected|used_nodes,'native ZIP has hidden or unreferenced payload')
    _,again,_=producer(api,publication['run'],registration,'unit',('success',))
    require((again['id'],again['task_id'])==(job['id'],job['task_id']), 'publication terminal identity changed')
    return rows
