"""Schema6 transport: closed records, exact bytes, authenticated disk indexes.

This module never treats a checkpoint or a parsed page as proof acceptance.
The scratch index is independently built; producer catalog SQL is not executed.
"""
from __future__ import annotations

import hashlib
import io
import math
import json
import os
from pathlib import Path
import re
import resource
import sqlite3
import sys
import time

PACKAGE = Path(__file__).resolve().parent
PAGE_BYTES = 256 * 1024 * 1024
ARTIFACT_BYTES = 512 * 1024 * 1024
NUMERIC = frozenset(('PhysicalRows', 'Episode', 'Store', 'Product', 'OracleEvidence',
                     'Candidate', 'Demand', 'ProductEdge', 'Expansion', 'MonitorNode',
                     'MonitorEdge', 'Scc', 'BoundaryObservation'))
BLOBS = frozenset(('initial_image_blob', 'schema_inventory', 'source_file', 'source_tables',
                   'tlc_receipt', 'runtime_receipt', 'numeric_receipt', 'backend_receipt',
                   'causal_receipt', 'eligibility_receipt', 'fresh_head_receipt', 'main_receipt',
                   'monitor_definition', 'fairness_receipt'))
COUNT_KINDS = dict(physical_rows='PhysicalRows', episodes='Episode', stores='Store',
                   oracle_edges='OracleEvidence', products='Product', candidates='Candidate',
                   demands='Demand', product_edges='ProductEdge', fair_pairs='FairPair',
                   enabledness='Enabledness', expansions='Expansion', monitor_nodes='MonitorNode',
                   monitor_edges='MonitorEdge', components='Scc', boundaries='BoundaryObservation')


class Invalid(ValueError):
    """Invalid proof evidence; never an incomplete resource disposition."""


class Incomplete(RuntimeError):
    """Preserve scratch/checkpoint; the admitted resource quantum did not suffice."""


def require(ok, message):
    if not ok:
        raise Invalid(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':'), allow_nan=False)


def encoded(value):
    return canonical(value).encode('utf-8')


def sha_bytes(value):
    return hashlib.sha256(value).hexdigest()


def strict_json(value):
    def unique(pairs):
        out = {}
        for key, item in pairs:
            require(key not in out, 'duplicate JSON key: ' + key)
            out[key] = item
        return out
    def bad_constant(value):
        raise Invalid('nonfinite JSON number: ' + value)
    def finite_float(value):
        number=float(value)
        require(math.isfinite(number),'nonfinite JSON number: '+value)
        return number
    try:
        return json.loads(value, object_pairs_hook=unique, parse_constant=bad_constant,parse_float=finite_float)
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise Invalid('invalid JSON') from error


def inner(value):
    """Parse an opaque exact preimage without rewriting its byte identity."""
    require(isinstance(value, str), 'canonical_utf8 is not UTF-8 text')
    value.encode('utf-8', errors='strict')
    return strict_json(value)


def envelope(kind, record):
    return {'schema_version': 6, 'kind': kind, 'record': record}


def transport_bytes(value, *, measure=False):
    result = {'schema_version': value['schema_version'], 'kind': value['kind'],
              'record': dict(value['record'])}
    self_field = {'Page': 'page_id', 'ManifestNode': 'sha256', 'ResourcePlan': 'plan_sha256'}.get(value['kind'])
    if self_field:
        result['record'].pop(self_field, None)
    if measure and value['kind'] in ('Page', 'ManifestNode'):
        result['record'].pop('utf8_bytes', None)
    return encoded(result)


def digest(value):
    return sha_bytes(transport_bytes(value))


def record_key(value):
    kind, r = value['kind'], value['record']
    if kind in NUMERIC:
        return r['id']
    if kind == 'FairPair':
        return canonical([r['context'], r['from_product'], r['to_product']])
    if kind == 'Enabledness':
        return canonical([r['context'], r['product_id']])
    if kind == 'Counts':
        return 'global'
    if kind == 'ContextRoot':
        return r['context']
    if kind == 'Applied':
        return digest(value)
    if kind in ('Session', 'Completion'):
        return r['session_id']
    if kind == 'WorkUnit':
        return r['unit_ordinal']
    if kind in ('Counterexample', 'Outcome'):
        return r['family']
    if kind == 'ArtifactReceipt':
        return r['artifact_id']
    field = {'InitialImage': 'image_sha256', 'Page': 'page_id', 'ManifestNode': 'sha256',
             'ResourcePlan': 'plan_sha256'}.get(kind)
    if field:
        return r[field]
    if kind == 'Checkpoint':
        return digest(value)
    raise Invalid('unsupported record key: ' + kind)


def ref(value):
    return {'kind': value['kind'], 'id': record_key(value), 'sha256': digest(value)}


def safe_path(root, member):
    require(isinstance(member, str) and re.fullmatch(
        r'[A-Za-z0-9_-][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_-][A-Za-z0-9_.-]*)*', member), 'unsafe member path')
    root = Path(root).resolve(strict=True)
    path = root
    for part in member.split('/'):
        require(part not in ('.', '..'), 'member path traversal')
        path = path / part
        require(not path.is_symlink(), 'symlink member')
    require(path.is_file() and path.resolve().is_relative_to(root), 'missing/outside member')
    return path


class BoundedList(list):
    """A complete indivisible list with prospective encoded/resident charges."""
    def __init__(self,budget,kind,values=()):
        super().__init__()
        self.budget,self.kind=budget,kind
        self.byte_count,self.charged=2,0
        budget.reserve_working(64)
        self.charged=64
        try:
            for value in values:self.append(value)
        except BaseException:
            self.clear()
            raise

    def append(self,value):
        data=encoded(value)
        allocation=self.budget.plan['resident_bytes'] if self.kind is None else self.budget.allowances[self.kind]['max_encoded_bytes']
        width=len(data)+(1 if self else 0)
        if self.byte_count+width>min(allocation,self.budget.plan['page_bytes']):
            raise Incomplete('indivisible complete/generated array exceeds its operational processing allocation')
        charge=32*len(data)+512
        self.budget.reserve_working(charge)
        self.charged+=charge
        self.byte_count+=width
        super().append(value)

    def extend(self,values):
        for value in values:self.append(value)

    def __del__(self):
        if hasattr(self,'budget'):
            self.budget.release_working(self.charged)


def decoded_reservation(path, budget):
    """Bounded lexical pass before a Python JSON object/string is allocated.

    The conservative charge includes encoded buffers, boxed scalars, container
    slots/hash tables, Unicode expansion and parser slack. The measured process
    RSS guard remains separate; no encoded-byte cap masquerades as an RSS proof.
    """
    size=Path(path).stat().st_size
    if size>min(PAGE_BYTES,budget.member_limit()):
        raise Incomplete('indivisible JSON member exceeds encoded/resident admission')
    with Path(path).open('rb') as stream:
        return decoded_stream_reservation(stream,size,budget)


def decoded_stream_reservation(stream,size,budget):
    tokens=containers=depth=0
    quoted=escaped=False
    while chunk:=stream.read(65536):
        for byte in chunk:
            if quoted:
                if escaped:
                    escaped=False
                elif byte==92:
                    escaped=True
                elif byte==34:
                    quoted=False
                continue
            if byte==34:
                quoted=True
                tokens+=1
            elif byte in (91,123):
                containers+=1
                depth+=1
                if depth>128:
                    raise Incomplete('JSON nesting exceeds reviewed bounded decoder stack')
            elif byte in (93,125):
                depth-=1
            elif byte in (44,58):
                tokens+=1
        # <=32x input bytes plus 512 bytes for each token/container safely
        # overcharges the actual fixed Python decoder representation.
        estimate=32*size+512*(tokens+containers)+65536
        if estimate>budget.plan['resident_bytes']:
            raise Incomplete('decoded working-set reservation exceeds admitted resident bytes')
        budget.check()
    require(not quoted and depth==0,'unterminated JSON lexical frame')
    return 32*size+512*(tokens+containers)+65536


def bounded_json(data,budget):
    raw=data.encode('utf-8') if type(data) is str else data
    require(type(raw) is bytes,'bounded decoder needs exact UTF-8 bytes')
    if len(raw)>budget.member_limit():raise Incomplete('indivisible private/semantic JSON exceeds processing allocation')
    estimate=decoded_stream_reservation(io.BytesIO(raw),len(raw),budget)
    budget.reserve_working(estimate)
    try:return strict_json(raw)
    finally:budget.release_working(estimate)


class Schema:
    """The closed draft2020 subset actually used by the pinned normative schema.

    Unknown assertion keywords fail at construction rather than being ignored.
    This avoids an optional Python-package version silently changing admission.
    """
    ASSERTIONS = {'$schema', '$id', '$defs', '$ref', '$comment', 'title', 'description',
                  'type', 'properties', 'required', 'additionalProperties', 'const', 'enum',
                  'items', 'minItems', 'maxItems', 'uniqueItems', 'minLength', 'maxLength',
                  'minimum', 'maximum', 'pattern', 'oneOf', 'anyOf', 'allOf', 'not', 'if', 'then', 'else'}

    def __init__(self, path=PACKAGE / 'paged-proof.schema.json'):
        self.path = Path(path).resolve()
        self.document = strict_json(self.path.read_bytes())
        self.documents = {self.path.name: self.document}
        self._audit(self.document)

    def _audit(self, rule):
        if not isinstance(rule, dict):
            return
        require(not (set(rule) - self.ASSERTIONS), 'unsupported schema assertion')
        for key in ('properties', '$defs'):
            for child in rule.get(key, {}).values():
                self._audit(child)
        for key in ('items', 'not', 'if', 'then', 'else', 'additionalProperties'):
            if key in rule:
                self._audit(rule[key])
        for key in ('oneOf', 'anyOf', 'allOf'):
            for child in rule.get(key, []):
                self._audit(child)

    def check(self, value, name=None):
        self._check(value, self.document['$defs'][name] if name else self.document, '$')
        return value

    def _matches(self, value, rule, where, document=None):
        try:
            self._check(value, rule, where, document)
            return True
        except Invalid:
            return False

    def _check(self, value, rule, where, document=None):
        document = self.document if document is None else document
        if '$ref' in rule:
            target = rule['$ref']
            filename, marker, fragment = target.partition('#')
            require(marker and (not filename or re.fullmatch(r'[A-Za-z0-9_-]+\.schema\.json', filename)), 'unsafe external schema ref')
            selected = document
            if filename:
                if filename not in self.documents:
                    external = self.path.parent / filename
                    require(external.is_file() and not external.is_symlink(), 'missing pinned auxiliary schema')
                    self.documents[filename] = strict_json(external.read_bytes())
                    self._audit(self.documents[filename])
                selected = self.documents[filename]
            require(fragment.startswith('/$defs/'), 'unsupported schema fragment')
            self._check(value, selected['$defs'][fragment[7:]], where, selected)
        types = {'object': lambda: type(value) is dict, 'array': lambda: type(value) is list,
                 'integer': lambda: type(value) is int, 'string': lambda: type(value) is str,
                 'boolean': lambda: type(value) is bool, 'null': lambda: value is None}
        if 'type' in rule:
            require(rule['type'] in types and types[rule['type']](), 'schema type ' + where)
        if 'const' in rule:
            require(type(value) is type(rule['const']) and value == rule['const'], 'schema const ' + where)
        if 'enum' in rule:
            require(any(type(value) is type(v) and value == v for v in rule['enum']), 'schema enum ' + where)
        if isinstance(value, dict):
            props = rule.get('properties', {})
            require(set(rule.get('required', [])) <= set(value), 'missing fields ' + where)
            require(rule.get('additionalProperties', True) or set(value) <= set(props), 'unknown fields ' + where)
            for key, item in value.items():
                if key in props:
                    self._check(item, props[key], where + '.' + key, document)
                elif type(rule.get('additionalProperties')) is dict:
                    self._check(item,rule['additionalProperties'],where+'.'+key,document)
        if isinstance(value, list):
            require(len(value) >= rule.get('minItems', 0) and len(value) <= rule.get('maxItems', len(value)), 'array bound ' + where)
            if rule.get('uniqueItems'):
                require(len({canonical(v) for v in value}) == len(value), 'duplicate array item ' + where)
            if 'items' in rule:
                for item in value:
                    self._check(item, rule['items'], where + '[]', document)
        if type(value) is int:
            require(value >= rule.get('minimum', value) and value <= rule.get('maximum', value), 'integer bound ' + where)
        if isinstance(value, str):
            require(len(value) >= rule.get('minLength', 0) and len(value) <= rule.get('maxLength', len(value)), 'text bound ' + where)
            require('pattern' not in rule or re.search(rule['pattern'], value) is not None, 'pattern ' + where)
        for key, predicate in (('oneOf', lambda n: n == 1), ('anyOf', lambda n: n >= 1)):
            if key in rule:
                require(predicate(sum(self._matches(value, r, where, document) for r in rule[key])), key + ' ' + where)
        for child in rule.get('allOf', []):
            self._check(value, child, where, document)
        if 'not' in rule:
            require(not self._matches(value, rule['not'], where, document), 'schema not ' + where)
        if 'if' in rule:
            branch = 'then' if self._matches(value, rule['if'], where, document) else 'else'
            if branch in rule:
                self._check(value, rule[branch], where, document)


def process_resident_bytes():
    statm=Path('/proc/self/statm')
    return (int(statm.read_text().split()[1])*os.sysconf('SC_PAGE_SIZE') if statm.is_file() else
            resource.getrusage(resource.RUSAGE_SELF).ru_maxrss*(1 if sys.platform=='darwin' else 1024))


class Budget:
    """Separate logical admission from measured resident/disk/work ceilings."""
    def __init__(self, plan, directory, *, seconds=None):
        Schema().check(plan, 'ResourcePlan')
        require(plan['admission'] == 'admitted', 'resource plan unadmitted')
        require(plan['plan_sha256'] == digest(envelope('ResourcePlan', plan)), 'resource plan digest')
        self.plan, self.directory = plan, Path(directory).resolve()
        self.accounting = strict_json(plan['source_bounds_utf8'])
        require(type(self.accounting) is dict and set(self.accounting)=={'schema_version','accounting_rule','source_receipt','record_allowances','overhead','accounting_method_sha256','measurement_sha256'} and self.accounting['schema_version']==2 and self.accounting['accounting_rule']=='operational_encoded_caps_v1','unregistered operational accounting meaning')
        self.allowances=self.accounting['record_allowances']
        for kind,allocation in self.allowances.items():
            require(type(allocation) is dict and set(allocation)=={'records','max_encoded_bytes'} and type(allocation['records']) is int and allocation['records']>=0 and type(allocation['max_encoded_bytes']) is int and allocation['max_encoded_bytes']>0,'invalid operational record allocation')
        require(seconds is None or type(seconds) in (int,float) and math.isfinite(seconds),'invalid remaining unit time')
        if seconds is not None and seconds<=0:raise Incomplete('shared unit deadline exhausted before decoder admission')
        self.deadline = time.monotonic() + min(plan['unit_seconds'],plan['unit_seconds'] if seconds is None else seconds)
        self.records = self.bytes = self.predecessors = 0
        self.working_bytes=0
        self.resident_base=process_resident_bytes()

    def check(self, *, records=0, bytes_=0, predecessors=0):
        self.records += records
        self.bytes += bytes_
        self.predecessors += predecessors
        for actual, key in ((self.records, 'unit_record_allowance'), (self.bytes, 'unit_byte_allowance'),
                            (self.predecessors, 'unit_predecessor_allowance')):
            if actual > self.plan[key]:
                raise Incomplete('admitted work allowance exhausted: ' + key)
        if time.monotonic() >= self.deadline:
            raise Incomplete('safe work quantum deadline')
        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (1 if sys.platform == 'darwin' else 1024)
        if rss > self.plan['resident_rss_bytes']:
            raise Incomplete('resident RSS allowance exhausted')
        # Index, WAL, tables, extracted members and witnesses all live here.
        total = 0
        for directory, _, files in os.walk(self.directory, followlinks=False):
            for name in files:
                path = Path(directory) / name
                require(not path.is_symlink(), 'symlink in owned proof storage')
                total += path.stat().st_size
        if total > self.plan['total_session_disk_bytes']:
            raise Incomplete('total session disk allowance exhausted')

    def reserve_working(self,amount):
        require(type(amount) is int and amount>=0,'invalid working reservation')
        if self.working_bytes+amount>self.plan['resident_bytes']:
            raise Incomplete('prospective decoded/generated working-set allocation exhausted')
        current=process_resident_bytes()
        if (current+amount>self.plan['resident_rss_bytes'] or
                max(0,current-self.resident_base)+amount>self.plan['resident_bytes']):
            raise Incomplete('prospective measured resident allocation exhausted')
        self.check()
        self.working_bytes+=amount

    def release_working(self,amount):
        self.working_bytes=max(0,self.working_bytes-amount)

    def member_limit(self):
        # Decoding one indivisible member has a conservative charged resident
        # bound, in addition to the separately measured RSS ceiling.
        return min(self.plan['page_bytes'], self.plan['resident_bytes'] // 12)


def links(value):
    """All semantic record references, independently of advertised dependencies."""
    k, r = value['kind'], value['record']
    pairs = {
        'Store': [('PhysicalRows', 'physical_rows_id'), ('Episode', 'episode_id'),
                  ('InitialImage', 'initial_image_id'), ('BoundaryObservation', 'omitted_observation_id'), ('OracleEvidence', 'replay_parent_edge')],
        'Product': [('Store', 'store_id'), ('ProductEdge', 'predecessor_edge')],
        'Candidate': [('Product', 'predecessor')],
        'Demand': [('Candidate', 'candidate'), ('Store', 'from_store'), ('OracleEvidence', 'outcome_id')],
        'ProductEdge': [('Product', 'from_product'), ('Product', 'to_product'), ('Candidate', 'candidate'),
                        ('OracleEvidence', 'primary_edge'), ('OracleEvidence', 'resolution_edge'),
                        ('OracleEvidence', 'read_edge'), ('OracleEvidence', 'cleanup_edge')],
        'FairPair': [('Product', 'from_product'), ('Product', 'to_product')],
        'Enabledness': [('Product', 'product_id')],
        'Expansion': [('Product', 'predecessor')],
        'ContextRoot': [('Product', 'initial_product')],
        'InitialImage': [('Store', 'initial_store')],
        'MonitorNode': [('Product', 'product')],
        'MonitorEdge': [('MonitorNode', 'from_monitor'), ('MonitorNode', 'to_monitor'), ('ProductEdge', 'product_edge')],
        'Scc': [('MonitorNode', 'monitor_root')],
        'Counterexample': [('Product', 'original_initial'), ('Scc', 'monitor_component'),('Product','stutter_at')],
        'BoundaryObservation': [('PhysicalRows', 'physical_rows_id'), ('Episode', 'episode_id'), ('InitialImage', 'initial_image_id')],
    }
    for target, field in pairs.get(k, []):
        if r[field] is not None:
            yield target, r[field], None
    arrays = {'Demand': [('Demand', 'depends_on')], 'Expansion': [('Candidate', 'candidates'),
        ('Demand', 'demands'), ('OracleEvidence', 'oracle_evidence'), ('ProductEdge', 'product_edges'),
        ('Product', 'discovered_products'), ('Store', 'capacity_only_stores')],
        'Scc': [('MonitorNode', 'members'), ('MonitorEdge', 'forward_tree'), ('MonitorEdge', 'reverse_tree')],
        'Counterexample': [('MonitorEdge', 'prefix'), ('MonitorEdge', 'cycle')]}
    for target, field in arrays.get(k, []):
        for identity in r[field]:
            yield target, identity, None
    if k in ('Applied', 'OracleEvidence'):
        a = r['applied'] if k == 'OracleEvidence' else r
        yield 'Store', a['from_store'], None
        yield 'Store', a['to_store'], None
    if k == 'InitialImage':
        yield 'initial_image_blob', r['member_path'], r['image_sha256']
    if k == 'Page':
        for field, target in (('previous_page', 'Page'), ('previous_checkpoint', 'Checkpoint')):
            if r[field] is not None:
                yield target, r[field], None
    if k == 'Checkpoint':
        yield 'ManifestNode', r['page_root'], None
        if r['previous_checkpoint'] is not None:
            yield 'Checkpoint', r['previous_checkpoint'], None
    # Explicit Ref values nested at any depth are also mandatory dependencies.
    def refs(v):
        if type(v) is dict:
            if set(v) == {'kind', 'id', 'sha256'}:
                yield v['kind'], v['id'], v['sha256']
            else:
                for key, item in v.items():
                    if key != 'records':
                        yield from refs(item)
        elif type(v) is list:
            for item in v:
                yield from refs(item)
    yield from refs(r)


class PrivateConnection(sqlite3.Connection):
    protected = False
    meaningful_change=False
    FRAMING={'check_results','inventory_entries','execution_commits','verifier_meta','unit_work','authenticated_units',
        'export_members','export_evidence','export_result_members','records','links','page_members','page_deps','manifest_children',
        'loaded_members','blobs','authoritative','external_roots','selected','semantic_links','cache_bindings','meta'}

    def _track(self,sql,cursor):
        if self.protected and cursor.rowcount>0:
            m=re.match(r'\s*(?:INSERT(?: OR \w+)? INTO|UPDATE|DELETE FROM)\s+([A-Za-z_][A-Za-z0-9_]*)',sql,re.I)
            if m and m.group(1) not in self.FRAMING:self.meaningful_change=True
        return cursor

    def execute(self,sql,parameters=()):
        return self._track(sql,super().execute(sql,parameters))

    def executemany(self,sql,parameters):
        return self._track(sql,super().executemany(sql,parameters))

    def commit(self):
        if not self.protected:
            super().commit()

    def __exit__(self, kind, error, traceback):
        if self.protected:
            return False
        return super().__exit__(kind, error, traceback)

    def executescript(self, script):
        require(not self.protected, 'DDL/implicit commit inside private execution quantum')
        return super().executescript(script)


class RecordIndex:
    """Full-key, collision-checked independently populated bounded SQLite index."""
    def __init__(self, path, *, source, session_id, budget):
        self.path, self.source, self.session_id, self.budget = Path(path), source, session_id, budget
        self.schema = Schema()
        self.schema.check(source, 'SourceIdentity')
        self.schema.check(session_id, 'Uuid')
        self.db = sqlite3.connect(self.path, factory=PrivateConnection)
        self.db.executescript((PACKAGE / 'paged-verifier-state.sql').read_text())
        self.db.execute('PRAGMA foreign_keys=ON')
        self.db.execute('PRAGMA temp_store=FILE')
        self.db.execute('PRAGMA cache_size=-2048')
        self.db.executescript('''
        CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS records(kind TEXT,key TEXT,number INTEGER,text_key TEXT,digest TEXT NOT NULL,payload BLOB NOT NULL,PRIMARY KEY(kind,key));
        CREATE UNIQUE INDEX IF NOT EXISTS record_number ON records(kind,number) WHERE number IS NOT NULL;
        CREATE INDEX IF NOT EXISTS record_text ON records(kind,text_key);
        CREATE TABLE IF NOT EXISTS blobs(kind TEXT,key TEXT,digest TEXT,path TEXT,bytes INTEGER,PRIMARY KEY(kind,key));
        CREATE TABLE IF NOT EXISTS links(owner_kind TEXT,owner_key TEXT,target_kind TEXT,target_key TEXT,digest TEXT,PRIMARY KEY(owner_kind,owner_key,target_kind,target_key));
        CREATE TABLE IF NOT EXISTS page_members(page TEXT,kind TEXT,key TEXT,PRIMARY KEY(page,kind,key));
        CREATE TABLE IF NOT EXISTS page_deps(page TEXT,kind TEXT,key TEXT,digest TEXT,PRIMARY KEY(page,kind,key));
        CREATE TABLE IF NOT EXISTS manifest_children(parent TEXT,child_kind TEXT,child_key TEXT,PRIMARY KEY(parent,child_kind,child_key));
        CREATE TABLE IF NOT EXISTS loaded_members(path TEXT PRIMARY KEY,digest TEXT,reference BLOB);
        CREATE TABLE IF NOT EXISTS authoritative(kind TEXT,key TEXT,PRIMARY KEY(kind,key));
        CREATE TABLE IF NOT EXISTS external_roots(kind TEXT,key TEXT,digest TEXT,PRIMARY KEY(kind,key));
        CREATE TABLE IF NOT EXISTS selected(kind TEXT,key TEXT,PRIMARY KEY(kind,key));
        CREATE TABLE IF NOT EXISTS semantic_links(kind TEXT,key TEXT,relation TEXT,target INTEGER,PRIMARY KEY(kind,key,relation,target));
        CREATE INDEX IF NOT EXISTS semantic_target ON semantic_links(relation,target,kind,key);
        ''')
        identity = encoded([source, session_id, budget.plan['plan_sha256']])
        old = self.db.execute("SELECT value FROM meta WHERE key='identity'").fetchone()
        require(old is None or old[0] == identity, 'scratch index identity drift')
        self.db.execute("INSERT OR IGNORE INTO meta VALUES('identity',?)", (identity,))
        self.db.commit()

    def close(self):
        self.db.close()

    def _identity(self, key):
        return canonical(key)

    def add_blob(self, kind, key, path, expected_digest):
        require(kind in BLOBS, 'not an external blob kind')
        path = Path(path)
        require(path.is_file() and not path.is_symlink(), 'unsafe external blob')
        h, count = hashlib.sha256(), 0
        with path.open('rb') as stream:
            while chunk := stream.read(65536):
                h.update(chunk)
                count += len(chunk)
        require(h.hexdigest() == expected_digest, 'external member digest mismatch')
        prior = self.db.execute('SELECT digest,bytes,path FROM blobs WHERE kind=? AND key=?', (kind, canonical(key))).fetchone()
        require(prior is None or prior[:2] == (expected_digest, count), 'conflicting external member')
        if prior is not None and Path(prior[2]).resolve()!=path.resolve():
            prior_path=Path(prior[2])
            require(prior_path.is_file() and not prior_path.is_symlink(),'prior external member disappeared')
            with prior_path.open('rb') as left,path.open('rb') as right:
                while True:
                    a,b=left.read(65536),right.read(65536)
                    require(a==b,'external member full-byte collision')
                    if not a:break
                    self.budget.check()
        self.db.execute('INSERT OR IGNORE INTO blobs VALUES(?,?,?,?,?)', (kind, canonical(key), expected_digest, str(path.resolve()), count))
        self.db.commit()

    def add(self, value):
        self.schema.check(value)
        kind, r = value['kind'], value['record']
        if 'source' in r:
            require(r['source'] == self.source, 'mixed source record')
        if 'session_id' in r:
            require(r['session_id'] == self.session_id, 'mixed session record')
        key = record_key(value)
        require(key is not None, 'unaddressable record')
        data = encoded(value)
        if len(data) > self.budget.member_limit():
            raise Incomplete('indivisible record exceeds resident admission')
        prior = self.db.execute('SELECT payload FROM records WHERE kind=? AND key=?', (kind, canonical(key))).fetchone()
        if prior is not None:
            require(prior[0] == data, 'conflicting duplicate record key')
            return key
        allocation=self.budget.allowances.get(kind)
        require(allocation is not None,'record kind absent from admitted accounting inventory')
        if len(data)>allocation['max_encoded_bytes'] or self.count(kind)>=allocation['records']:
            raise Incomplete('operational full outer record/count allocation exhausted: '+kind)
        self.budget.check(records=1, bytes_=len(data))
        if kind in ('PhysicalRows','Episode','Store','Product'):
            try:
                self.db.execute('INSERT INTO canonical_values VALUES(?,?,?)',(kind,r['canonical_utf8'].encode(),canonical(key)))
            except sqlite3.IntegrityError as error:
                raise Invalid('duplicate exact canonical identity under different global IDs') from error
            require(sha_bytes(r['canonical_utf8'].encode()) == r['sha256'], 'inner canonical digest')
            inner(r['canonical_utf8'])
            if kind in ('PhysicalRows', 'Episode'):
                require(len(r['canonical_utf8'].encode()) == r['utf8_bytes'], 'inner UTF-8 byte count')
        if kind=='ResourcePlan':
            require(r['plan_sha256']==digest(value),'resource plan content address mismatch')
        if kind in ('Page', 'ManifestNode'):
            field = 'page_id' if kind == 'Page' else 'sha256'
            require(r[field] == digest(value), 'content address mismatch')
            require(r['utf8_bytes'] == len(transport_bytes(value, measure=True)), 'transport byte measure')
        if kind == 'Outcome':
            require(r['actual'] in ('pass', 'named_semantic_counterexample', 'finite_witness'), 'premature singleton outcome')
        if kind in ('Counts', 'ContextRoot'):
            require(r['frontier'] == 0, 'premature final singleton')
        prior = self.db.execute('SELECT payload FROM records WHERE kind=? AND key=?', (kind, canonical(key))).fetchone()
        require(prior is None or prior[0] == data, 'conflicting duplicate record key')
        self.db.execute('INSERT OR IGNORE INTO records VALUES(?,?,?,?,?,?)',
                        (kind, canonical(key), key if type(key) is int else None,
                         key if type(key) is str else None, digest(value), data))
        for target, identity, expected in links(value):
            old = self.db.execute('SELECT digest FROM links WHERE owner_kind=? AND owner_key=? AND target_kind=? AND target_key=?',
                                  (kind, canonical(key), target, canonical(identity))).fetchone()
            require(old is None or old[0] in (None, expected) or expected is None, 'conflicting dependency digests')
            self.db.execute('INSERT OR REPLACE INTO links VALUES(?,?,?,?,?)',
                            (kind, canonical(key), target, canonical(identity), expected or (old[0] if old else None)))
        for relation, field in (('predecessor', 'predecessor'), ('from_product', 'from_product'),
                                ('to_product', 'to_product'), ('candidate', 'candidate'),
                                ('from_store', 'from_store'), ('product', 'product'),
                                ('from_monitor', 'from_monitor'), ('to_monitor', 'to_monitor')):
            if field in r:
                self.db.execute('INSERT OR IGNORE INTO semantic_links VALUES(?,?,?,?)', (kind, canonical(key), relation, r[field]))
        return key

    def propose(self,value,operation):
        execution=getattr(self,'execution',None)
        def insert():
            key=record_key(value)
            data=encoded(value)
            old=self.db.execute('SELECT payload FROM proposed_records WHERE kind=? AND key=?',(value['kind'],canonical(key))).fetchone()
            require(old is None or old[0]==data,'changed immutable temporal proposal')
            self.add(value)
            self.db.execute('INSERT OR IGNORE INTO proposed_records VALUES(?,?,?)',(value['kind'],canonical(key),data))
        if execution is None or execution.active:
            insert()
        else:
            with execution.quantum(operation):insert()
        return record_key(value)

    def rebuild_input_cache(self):
        require(not getattr(self,'execution',None) or not self.execution.active,'cache rebuild inside semantic quantum')
        for table in ('loaded_members','blobs','records','links','page_members','page_deps','manifest_children',
                      'authoritative','external_roots','selected','semantic_links','cache_bindings','canonical_values','manifest_tally'):
            self.db.execute('DELETE FROM '+table)
        self.db.commit()

    def rebind_proposals(self):
        for payload, in self.db.execute('SELECT payload FROM proposed_records ORDER BY kind,key'):
            self.add(strict_json(payload))
        self.db.commit()

    def get_envelope(self, kind, key):
        args=kind,canonical(key)
        size=self.db.execute('SELECT length(payload) FROM records WHERE kind=? AND key=?',args).fetchone()
        require(size is not None,'missing record '+kind+':'+canonical(key))
        if size[0]>self.budget.member_limit():raise Incomplete('indivisible indexed record exceeds resident allocation')
        encoded_charge=2*size[0]
        self.budget.reserve_working(encoded_charge)
        decoded_charge=0
        try:
            data=self.db.execute('SELECT payload FROM records WHERE kind=? AND key=?',args).fetchone()[0]
            estimate=decoded_stream_reservation(io.BytesIO(data),len(data),self.budget)
            self.budget.reserve_working(estimate)
            decoded_charge=estimate
            value=strict_json(data)
            execution=getattr(self,'execution',None)
            if execution is not None and execution.active:execution.record_input(value)
            return value
        finally:
            self.budget.release_working(encoded_charge+decoded_charge)

    def get(self, kind, key):
        return self.get_envelope(kind,key)['record']

    def find(self, kind, relation=None, target=None):
        if relation is None:
            sql,args='SELECT key FROM records WHERE kind=? ORDER BY number,text_key',(kind,)
        else:
            sql=('SELECT r.key FROM records r JOIN semantic_links s ON r.kind=s.kind AND r.key=s.key '
                 'WHERE r.kind=? AND s.relation=? AND s.target=? ORDER BY r.number,r.text_key')
            args=kind,relation,target
        for key, in self.db.execute(sql,args):yield self.get(kind,strict_json(key))

    def find_after(self,kind,after):
        require(kind in NUMERIC and type(after) is int and after>=0,'typed numeric operation cursor required')
        for key, in self.db.execute('SELECT key FROM records WHERE kind=? AND number>? ORDER BY number',(kind,after)):
            yield self.get(kind,strict_json(key))

    def find_unchecked(self,kind,table,offset=0):
        require(table in ('v_stores','v_oracle','v_products','verified_sccs'),'unregistered private coverage table')
        require(type(offset) is int and offset in (0,1),'invalid fixed source/global ID bijection')
        sql='SELECT r.key FROM records r LEFT JOIN '+table+' c ON c.id=r.number-? WHERE r.kind=? AND c.id IS NULL ORDER BY r.number'
        for key, in self.db.execute(sql,(offset,kind)):yield self.get(kind,strict_json(key))

    def count(self, kind):
        return self.db.execute('SELECT count(*) FROM records WHERE kind=?', (kind,)).fetchone()[0]

    def load_member(self,path):
        require(Path(path).is_file() and not Path(path).is_symlink(),'unsafe proof member before decoding')
        charge=decoded_reservation(path,self.budget)
        self.budget.reserve_working(charge)
        try:return self._load_member_reserved(path)
        finally:self.budget.release_working(charge)

    def _load_member_reserved(self, path):
        path = Path(path)
        require(path.is_file() and not path.is_symlink(), 'unsafe proof member')
        if path.stat().st_size > self.budget.member_limit():
            raise Incomplete('page/member requires more admitted resident bytes')
        data = path.read_bytes()
        file_digest = sha_bytes(data)
        old = self.db.execute('SELECT digest,reference FROM loaded_members WHERE path=?', (str(path.resolve()),)).fetchone()
        if old:
            require(old[0] == file_digest, 'mutated loaded member')
            return strict_json(old[1])
        value = strict_json(data)
        require(encoded(value) == data, 'outer member is not canonical JSON')
        self.add(value)
        prior=self.db.execute('SELECT reference FROM loaded_members WHERE reference=? LIMIT 1',(encoded(ref(value)),)).fetchone()
        if prior is not None:
            self.db.execute('INSERT INTO loaded_members VALUES(?,?,?)',(str(path.resolve()),file_digest,encoded(ref(value))))
            self.db.commit()
            return ref(value)
        self.db.execute('INSERT OR IGNORE INTO authoritative VALUES(?,?)', (value['kind'], canonical(record_key(value))))
        if value['kind'] == 'Page':
            r = value['record']
            require(len(data) <= PAGE_BYTES, 'full page byte ceiling')
            require(r['record_count'] == len(r['records']) and r['records'], 'page record count/empty page')
            keys = []
            for item in r['records']:
                require(item['kind'] == r['kind'], 'heterogeneous data page')
                key = self.add(item)
                self.db.execute('INSERT OR IGNORE INTO authoritative VALUES(?,?)', (item['kind'], canonical(key)))
                keys.append(key)
                try:
                    self.db.execute('INSERT INTO page_members VALUES(?,?,?)', (r['page_id'], item['kind'], canonical(key)))
                except sqlite3.IntegrityError as error:
                    raise Invalid('duplicate record inside page') from error
            require(all(type(k) is type(keys[0]) for k in keys) and keys == sorted(keys) and len(set(keys)) == len(keys), 'page key ordering')
            require((r['first_key'], r['last_key']) == (keys[0], keys[-1]), 'page key range')
            for dep in r['dependencies']:
                try:
                    self.db.execute('INSERT INTO page_deps VALUES(?,?,?,?)', (r['page_id'], dep['kind'], canonical(dep['id']), dep['sha256']))
                except sqlite3.IntegrityError as error:
                    raise Invalid('duplicate page dependency') from error
        if value['kind'] == 'ManifestNode':
            r = value['record']
            require(r['children'], 'empty manifest node')
            for child in r['children']:
                require(child['kind'] == ('Page' if r['level'] == 0 else 'ManifestNode'), 'manifest level/kind')
                try:
                    self.db.execute('INSERT INTO manifest_children VALUES(?,?,?)', (r['sha256'], child['kind'], canonical(child['id'])))
                except sqlite3.IntegrityError as error:
                    raise Invalid('duplicate manifest child') from error
        self.db.execute('INSERT INTO loaded_members VALUES(?,?,?)', (str(path.resolve()), file_digest, encoded(ref(value))))
        self.db.commit()
        self.budget.check()
        return ref(value)

    def verify_dependencies(self, manifest_root, *, high_water):
        """Resolve all refs and exact external inventories, with no global set."""
        for owner_kind, owner_key, target, key, expected in self.db.execute('SELECT l.* FROM links l JOIN authoritative a ON a.kind=l.owner_kind AND a.key=l.owner_key'):
            table = 'blobs' if target in BLOBS else 'records'
            row = self.db.execute('SELECT digest FROM ' + table + ' WHERE kind=? AND key=?', (target, key)).fetchone()
            require(row is not None and (expected is None or expected == row[0]), 'missing/mismatched transitive dependency')
        # Dependencies must exactly cover links leaving each page, neither hide
        # payloads nor supply a convenient subset of the needed Store outcomes.
        for page in self.find('Page'):
            require(page['transaction_high_water'] <= high_water, 'post-snapshot page')
            page_id = page['page_id']
            needed = '''SELECT DISTINCT l.target_kind,l.target_key FROM page_members m JOIN links l
                ON l.owner_kind=m.kind AND l.owner_key=m.key WHERE m.page=? AND NOT EXISTS
                (SELECT 1 FROM page_members n WHERE n.page=m.page AND n.kind=l.target_kind AND n.key=l.target_key)'''
            missing = self.db.execute('SELECT * FROM (' + needed + ') EXCEPT SELECT kind,key FROM page_deps WHERE page=?', (page_id, page_id)).fetchone()
            extra = self.db.execute('SELECT kind,key FROM page_deps WHERE page=? EXCEPT ' + needed, (page_id, page_id)).fetchone()
            require(missing is None and extra is None, 'page external dependency inventory differs')
            for kind, key, expected in self.db.execute('SELECT kind,key,digest FROM page_deps WHERE page=?', (page_id,)):
                table = 'blobs' if kind in BLOBS else 'records'
                actual = self.db.execute('SELECT digest FROM ' + table + ' WHERE kind=? AND key=?', (kind, key)).fetchone()
                require(actual is not None and actual[0] == expected, 'page dependency authentication')
        for kind, key, expected in self.db.execute('SELECT kind,key,digest FROM external_roots'):
            table = 'blobs' if kind in BLOBS else 'records'
            actual = self.db.execute('SELECT digest FROM ' + table + ' WHERE kind=? AND key=?', (kind, key)).fetchone()
            require(actual is not None and actual[0] == expected, 'external root authentication')
        self.db.execute('DELETE FROM selected')
        self.db.execute('INSERT INTO selected VALUES(?,?)', ('ManifestNode', canonical(manifest_root)))
        self.db.execute('INSERT OR IGNORE INTO selected SELECT kind,key FROM external_roots')
        changed = True
        while changed:
            before = self.db.total_changes
            self.db.execute('''INSERT OR IGNORE INTO selected SELECT m.child_kind,m.child_key FROM manifest_children m
                JOIN selected s ON s.kind='ManifestNode' AND s.key=json_quote(m.parent)''')
            self.db.execute('''INSERT OR IGNORE INTO selected SELECT m.kind,m.key FROM page_members m
                JOIN selected s ON s.kind='Page' AND s.key=json_quote(m.page)''')
            self.db.execute('''INSERT OR IGNORE INTO selected SELECT l.target_kind,l.target_key FROM links l
                JOIN selected s ON s.kind=l.owner_kind AND s.key=l.owner_key''')
            changed = self.db.total_changes != before
            self.budget.check()
        require(self.db.execute('SELECT 1 FROM authoritative r LEFT JOIN selected s ON s.kind=r.kind AND s.key=r.key WHERE s.key IS NULL LIMIT 1').fetchone() is None,
                'orphan/unreferenced hidden record')
        require(self.db.execute('SELECT 1 FROM blobs b LEFT JOIN selected s ON s.kind=b.kind AND s.key=b.key WHERE s.key IS NULL LIMIT 1').fetchone() is None,
                'unreferenced hidden external payload')
        # A manifest child can be shared only with identical bytes; cycles and
        # incorrect levels are independently rejected, not assumed from hashes.
        for node in self.find('ManifestNode'):
            for child in node['children']:
                if child['kind'] == 'ManifestNode':
                    require(self.get('ManifestNode', child['id'])['level'] + 1 == node['level'], 'manifest cycle/level')
        for node in self.find('ManifestNode'):
            require(node['counts']==self.manifest_counts(node['sha256']),'manifest exact transitive unique-record counts differ')
        self.db.commit()
        return self.counts()

    def manifest_counts(self,root):
        self.db.execute('DELETE FROM manifest_tally')
        self.db.execute('INSERT INTO manifest_tally VALUES(?,?)',('ManifestNode',canonical(root)))
        changed=True
        while changed:
            before=self.db.total_changes
            self.db.execute("INSERT OR IGNORE INTO manifest_tally SELECT m.child_kind,m.child_key FROM manifest_children m JOIN manifest_tally s ON s.kind='ManifestNode' AND s.key=json_quote(m.parent)")
            self.db.execute("INSERT OR IGNORE INTO manifest_tally SELECT p.kind,p.key FROM page_members p JOIN manifest_tally s ON s.kind='Page' AND s.key=json_quote(p.page)")
            self.db.execute('INSERT OR IGNORE INTO manifest_tally SELECT l.target_kind,l.target_key FROM links l JOIN manifest_tally s ON s.kind=l.owner_kind AND s.key=l.owner_key')
            changed=self.db.total_changes!=before
            self.budget.check()
        values={field:self.db.execute('SELECT count(*) FROM manifest_tally WHERE kind=?',(kind,)).fetchone()[0] for field,kind in COUNT_KINDS.items()}
        values['frontier']=values['products']-values['expansions']
        require(values['frontier']>=0,'manifest closure has expansions without unique predecessor ownership')
        return values

    def counts(self):
        return {**{field: self.count(kind) for field, kind in COUNT_KINDS.items()}, 'frontier': self.count('Product') - self.count('Expansion')}


OPERATIONS = {
    'materialize_store': {'materialize_store', 'materialize_oracle'},
    'materialize_oracle': {'materialize_oracle', 'materialize_product'},
    'materialize_product': {'materialize_product', 'seed_fifo'},
    'seed_fifo': {'check_expansion'},
    'check_expansion': {'check_expansion', 'source_reachability'},
    'source_reachability': {'source_reachability', 'source_finish'},
    'source_finish': {'stage_finish'},
    'produce_monitor': {'produce_monitor', 'check_monitor_node','select_view'},
    'check_monitor_node': {'check_monitor_node', 'check_monitor_edge'},
    'check_monitor_edge': {'check_monitor_edge', 'check_monitor_successors'},
    'check_monitor_successors': {'check_monitor_successors', 'monitor_reachability'},
    'monitor_reachability': {'monitor_reachability', 'select_view'},
    'select_view': {'dfs_forward', 'check_scc', 'select_view'},
    'dfs_forward': {'dfs_forward', 'dfs_reverse'},
    'dfs_reverse': {'dfs_reverse', 'condensation_rank'},
    'condensation_rank': {'condensation_rank', 'build_tree', 'check_scc'},
    'build_tree': {'build_tree', 'check_scc', 'select_view','check_monitor_node'},
    'check_scc': {'check_scc', 'select_view', 'stage_finish'},
    'one_step_tlc': {'one_step_tlc', 'component_tlc', 'boundary_tlc', 'family_finish'},
    'component_tlc': {'component_tlc', 'fair_search', 'family_finish'},
    'fair_search': {'fair_search', 'component_tlc', 'family_finish'},
    'boundary_tlc': {'boundary_tlc', 'family_finish'},
    'family_finish': {'one_step_tlc', 'component_tlc', 'boundary_tlc', 'family_finish', 'publish_temporal', 'stage_finish'},
    'publish_temporal': {'publish_temporal', 'stage_finish','family_finish','check_monitor_node','select_view'},
    'stage_finish': {'produce_monitor', 'one_step_tlc', 'publish_temporal', 'stage_finish'},
}
for _publishing_operation in ('produce_monitor','select_view','dfs_forward','dfs_reverse','condensation_rank','build_tree',
        'check_monitor_node','check_monitor_edge','check_monitor_successors','monitor_reachability','check_scc',
        'one_step_tlc','component_tlc','fair_search','boundary_tlc'):
    OPERATIONS[_publishing_operation].add('publish_temporal')
del _publishing_operation


def empty_position():
    return {'context': None, 'record_kind': None, 'after_key': None, 'fifo_ticket': None,
            'component_id': None, 'family': None, 'case_index': 0}


def empty_scope():
    return {'context': None, 'property': None, 'obligation': None, 'input_root': None, 'family': None}


class ExecutionState:
    """Private atomic execution journal. Its bytes alone confer no CI credit.

    Only a finalized, authenticated prior-unit result/snapshot chain may resume
    these rows on another invocation. Current unpublished work is never promoted
    by a pathname, checked flag, or scalar cursor.
    """
    def __init__(self, index, initial_image):
        self.index, self.db, self.budget = index, index.db, index.budget
        self.sql_path = PACKAGE / 'paged-verifier-state.sql'
        self.schema_sha256 = sha_bytes(self.sql_path.read_bytes())
        self._validate_schema()
        initial = {'stage': 'semantic_verify', 'operation': 'materialize_store',
                   'scope': empty_scope(), 'position': empty_position(), 'quantum': 0}
        row = self.db.execute('SELECT source_json,session,plan,initial_image FROM verifier_meta WHERE singleton=1').fetchone()
        identity = encoded(index.source), index.session_id, index.budget.plan['plan_sha256'], initial_image
        if row is None:
            self.db.execute('INSERT INTO verifier_meta VALUES(1,1,?,?,?,?,0,0,?,NULL)', (*identity, encoded(initial)))
            self.db.commit()
        else:
            require(row == identity, 'private verifier source/session/plan/Init mismatch')
        self.active = False
        self.resource_failure=None
        self.db.set_progress_handler(self._resource_monitor,10000)

    def _resource_monitor(self):
        try:
            self.budget.check()
            return 0
        except Incomplete as error:
            self.resource_failure=error
            return 1

    def prospective_commit(self):
        self.budget.check()
        size=self.db.execute('PRAGMA page_size').fetchone()[0]
        pages=self.db.execute('PRAGMA page_count').fetchone()[0]
        # Private SQLite uses bounded cache + FILE temporaries and DELETE
        # journaling. Reserve a full main image and two rollback copies before
        # commit; this is operational capacity, never a source-width theorem.
        live=0
        for directory,_,names in os.walk(self.budget.directory):
            for name in names:
                live+=(Path(directory)/name).stat().st_size
        if live+3*size*pages>self.budget.plan['total_session_disk_bytes']:
            raise Incomplete('prospective private DB/journal commit reservation exhausted')

    def _validate_schema(self):
        expected = sqlite3.connect(':memory:')
        try:
            expected.executescript(self.sql_path.read_text())
            wanted = list(expected.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name,tbl_name,sql"))
            actual = list(self.db.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name,tbl_name,sql"))
            require(actual == wanted, 'private verifier installed schema differs from pinned SQL')
        finally:
            expected.close()

    def cursor(self):
        return strict_json(self.db.execute('SELECT cursor FROM verifier_meta WHERE singleton=1').fetchone()[0])

    def sequence(self):
        return self.db.execute('SELECT private_commit,last_result FROM verifier_meta WHERE singleton=1').fetchone()

    def logical_digest(self):
        """Stream every full durable row in explicit source-reviewed PK order."""
        sql = self.sql_path.read_text()
        line = next(x for x in sql.splitlines() if x.startswith('-- logical-order: '))
        order = strict_json(line.split(': ', 1)[1])
        h = hashlib.sha256(b'plurx-independent-verifier-state-v1\0')
        h.update(bytes.fromhex(self.schema_sha256))
        for table, keys in order.items():
            require(re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', table), 'bad pinned table name')
            cols = [r[1] for r in self.db.execute('PRAGMA table_info("' + table + '")')]
            require(cols, 'missing private durable table')
            select = 'SELECT ' + ','.join('"' + c + '"' for c in cols) + ' FROM "' + table + '" ORDER BY ' + ','.join('"' + c + '"' for c in keys)
            for row in self.db.execute(select):
                # BLOBs retain their exact bytes; hex is a lossless typed
                # canonical representation, never parsed/re-normalized JSON.
                def cell(v):
                    return {'blob_hex': v.hex()} if type(v) is bytes else v
                values = [cell(v) for v in row]
                pk = [cell(row[cols.index(k)]) for k in keys]
                data = encoded([table, pk, values])
                h.update(len(data).to_bytes(8, 'big'))
                h.update(data)
        return h.hexdigest()

    def deterministic_initial(self):
        temporary=sqlite3.connect(':memory:')
        try:
            temporary.executescript(self.sql_path.read_text())
            row=self.db.execute('SELECT source_json,session,plan,initial_image FROM verifier_meta WHERE singleton=1').fetchone()
            require(row is not None,'missing verifier identity')
            initial={'stage':'semantic_verify','operation':'materialize_store','scope':empty_scope(),'position':empty_position(),'quantum':0}
            temporary.execute("INSERT INTO meta VALUES('identity',?)",(encoded([strict_json(row[0]),row[1],row[2]]),))
            temporary.execute('INSERT INTO verifier_meta VALUES(1,1,?,?,?,?,0,0,?,NULL)',(*row,encoded(initial)))
            empty=ExecutionState.__new__(ExecutionState)
            empty.db,empty.sql_path,empty.schema_sha256=temporary,self.sql_path,self.schema_sha256
            return initial,empty.logical_digest()
        finally:temporary.close()

    def quantum(self, operation, *, stage=None, scope=None, position=None, inputs=(),
                claim='algorithm_checkpoint', coverage=None, framing=False):
        return ExecutionQuantum(self, operation, stage, scope, position, inputs, claim, coverage,framing)

    def record_input(self, value):
        require(self.active, 'input capture outside quantum')
        sequence = self.db.execute('SELECT max(sequence) FROM check_results').fetchone()[0]
        authority=self.db.execute('SELECT 1 FROM authoritative WHERE kind=? AND key=?',(value['kind'],canonical(record_key(value)))).fetchone()
        if authority is None:
            claim=self.db.execute('SELECT claim FROM check_results WHERE sequence=?',(sequence,)).fetchone()[0]
            require(claim=='algorithm_checkpoint','semantic/SCC/TLC credit requires actual authenticated input-manifest record')
            require(self.db.execute('SELECT 1 FROM proposed_records WHERE kind=? AND key=?',(value['kind'],canonical(record_key(value)))).fetchone() is not None,'unattributed private derived input')
            return
        entry = encoded({'entry_kind': 'record', 'record_kind': value['kind'], 'key': record_key(value), 'sha256': digest(value)})
        if self.db.execute("SELECT 1 FROM inventory_entries WHERE result_sequence=? AND purpose='inputs' AND entry=?", (sequence, entry)).fetchone() is None:
            count, size = self.db.execute("SELECT count(*),coalesce(sum(length(entry)),0) FROM inventory_entries WHERE result_sequence=? AND purpose='inputs'", (sequence,)).fetchone()
            if size + len(entry) > self.budget.member_limit():
                raise Incomplete('indivisible consumed-input inventory exceeds resident bound')
            self.db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)', (sequence, 'inputs', count, entry))

    def backup(self, destination):
        """One standalone consistent SQLite backup at a committed boundary."""
        require(not self.active and not self.db.in_transaction, 'backup during active verifier quantum')
        before = self.sequence(), self.logical_digest()
        target = Path(destination)
        require(target.is_absolute() and not target.is_symlink() and not target.exists(), 'unsafe verifier backup destination')
        out = sqlite3.connect(target)
        try:
            self.db.backup(out, pages=128, progress=lambda status, remaining, total: self.budget.check())
            out.commit()
            require(out.execute('PRAGMA integrity_check').fetchone() == ('ok',), 'verifier backup integrity')
        finally:
            out.close()
        require(before == (self.sequence(), self.logical_digest()), 'verifier mutated during backup')
        h = hashlib.sha256()
        with target.open('rb') as stream:
            while chunk := stream.read(1024 * 1024):
                h.update(chunk)
                self.budget.check(bytes_=len(chunk))
        return {'path': str(target), 'work_db_bytes': target.stat().st_size, 'work_db_sha256': h.hexdigest(),
                'verifier_schema_sha256': self.schema_sha256, 'verifier_algorithm_version': 1,
                'verifier_commit': before[0][0], 'verifier_logical_digest': before[1]}


class ExecutionQuantum:
    def __init__(self, state, operation, stage, scope, position, inputs, claim, coverage,framing=False):
        self.state, self.operation = state, operation
        self.stage, self.scope, self.position = stage, scope, position
        self.inputs, self.claim, self.coverage = inputs, claim, coverage
        self.framing=framing

    def __enter__(self):
        s = self.state
        require(not s.active and not s.db.in_transaction, 'nested/uncommitted private verifier quantum')
        s.budget.check()
        self.before = s.cursor()
        require(self.operation in OPERATIONS and (self.operation in OPERATIONS[self.before['operation']] or
                self.framing and self.operation==self.before['operation'] or
                self.before['quantum'] == 0 and self.operation == self.before['operation']), 'illegal explicit verifier operation transition')
        self.after = {'stage': self.stage or self.before['stage'], 'operation': self.operation,
                      'scope': self.scope or self.before['scope'], 'position': self.position or self.before['position'],
                      'quantum': self.before['quantum'] + 1}
        require(self.after['stage'] in ('semantic_verify', 'monitor_scc_verify', 'temporal_components'), 'invalid verifier stage')
        if self.framing:
            require(self.operation==self.before['operation'] and self.claim=='algorithm_checkpoint' and not self.inputs and
                    all(self.after[k]==self.before[k] for k in ('stage','operation','scope','position')),'framing cannot change proof progress')
        s.db.execute('BEGIN IMMEDIATE')
        s.active = True
        s.db.protected = True
        s.db.meaningful_change=False
        try:
            self.sequence = s.sequence()[1] + 1
            coverage = self.coverage or {'record_kind': None, 'lower_exclusive': None, 'upper_inclusive': None,
                                         'count': 0, 'scope': self.after['scope']}
            # The exact ordered input entries, not scalar endpoints, confer coverage.
            h = hashlib.sha256(b'plurx-verifier-input-inventory-v1\0')
            entries = []
            for ordinal, item in enumerate(self.inputs):
                data = encoded(item)
                h.update(len(data).to_bytes(8, 'big'))
                h.update(data)
                entries.append(data)
                if sum(map(len, entries)) > s.budget.member_limit():
                    raise Incomplete('indivisible quantum input inventory exceeds resident admission')
            root = h.hexdigest()
            s.db.execute('INSERT INTO check_results VALUES(?,?,?,?,?,?,?,?,NULL,NULL)',
                         (self.sequence, self.claim, encoded(coverage), root, encoded(self.before), encoded(self.after), self.after['quantum'], 1))
            s.db.executemany('INSERT INTO inventory_entries VALUES(?,?,?,?)',
                             ((self.sequence, 'inputs', i, data) for i, data in enumerate(entries)))
            return self
        except BaseException:
            s.db.rollback()
            s.active = False
            s.db.protected = False
            raise

    def credit(self, table, key):
        self.state.db.execute('INSERT INTO credit_links VALUES(?,?,?)', (table, encoded(key), self.sequence))

    def __exit__(self, kind, error, traceback):
        s = self.state
        try:
            if kind is not None:
                s.db.rollback()
                if s.resource_failure is not None:
                    resource_error=s.resource_failure
                    s.resource_failure=None
                    raise resource_error
                return False
            require(not self.framing or not s.db.meaningful_change,
                    'framing cannot mutate semantic or algorithm work')
            commit = s.sequence()[0] + 1
            h = hashlib.sha256(b'plurx-verifier-input-inventory-v1\0')
            for (entry,) in s.db.execute("SELECT entry FROM inventory_entries WHERE result_sequence=? AND purpose='inputs' ORDER BY ordinal", (self.sequence,)):
                h.update(len(entry).to_bytes(8, 'big'))
                h.update(entry)
            root = h.hexdigest()
            s.db.execute('UPDATE check_results SET input_root=? WHERE sequence=?', (root, self.sequence))
            s.db.execute('INSERT INTO execution_commits VALUES(?,?,?,?,?,?,1,?)',
                         (commit,self.operation,encoded(self.before),encoded(self.after),root,self.sequence,int(s.db.meaningful_change)))
            s.db.execute('UPDATE verifier_meta SET private_commit=?,last_result=?,cursor=? WHERE singleton=1',
                         (commit, self.sequence, encoded(self.after)))
            s.prospective_commit()
            sqlite3.Connection.commit(s.db)
        except BaseException:
            s.db.rollback()
            raise
        finally:
            s.active = False
            s.db.protected = False
        s.budget.check()
        return False


class ContinuationResolver:
    """Authenticated-member location resolver; no path or claimed bit authority."""
    def __init__(self, dependency_index, directory, *, source, session_id, budget):
        path = Path(dependency_index)
        require(path.is_absolute() and path.is_file() and not path.is_symlink(), 'continuation resolver path')
        self.auth = sqlite3.connect(path.as_uri() + '?mode=ro&immutable=1', uri=True)
        self.db = sqlite3.connect(Path(directory) / 'continuation-validation.sqlite')
        self.db.execute('PRAGMA temp_store=FILE')
        self.db.execute('PRAGMA cache_size=-2048')
        self.db.executescript('''CREATE TABLE IF NOT EXISTS evidence_work(root TEXT,unit INTEGER,part INTEGER,path TEXT,digest TEXT,level INTEGER,first_ordinal INTEGER,item_count INTEGER,PRIMARY KEY(root,unit,part,path));
            CREATE TABLE IF NOT EXISTS evidence_entries(root TEXT,ordinal INTEGER,entry BLOB,PRIMARY KEY(root,ordinal));
            CREATE TABLE IF NOT EXISTS chain_results(sequence INTEGER PRIMARY KEY,unit INTEGER,payload BLOB,digest TEXT);
            CREATE TABLE IF NOT EXISTS chain_progress(unit INTEGER PRIMARY KEY,payload BLOB,sha256 TEXT,backup_sha256 TEXT);''')
        self.source, self.session, self.budget = source, session_id, budget
        self.schema = Schema(PACKAGE / 'paged-verifier-continuation.schema.json')

    def close(self):
        self.db.close()
        self.auth.close()

    def member_path(self, member):
        require(type(member) is dict and set(member) == {'unit_ordinal','part_ordinal','member_path','bytes','sha256'}, 'closed continuation MemberRef')
        row = self.auth.execute('SELECT bytes,sha256,local_path FROM authenticated_members WHERE unit_ordinal=? AND part_ordinal=? AND member_path=?',
            (member['unit_ordinal'],member['part_ordinal'],member['member_path'])).fetchone()
        require(row is not None and (row[0],row[1]) == (member['bytes'],member['sha256']), 'unauthenticated/wrong continuation member')
        path = Path(row[2])
        require(path.is_absolute() and path.is_file() and not path.is_symlink(), 'unsafe authenticated member location')
        h,count = hashlib.sha256(),0
        with path.open('rb') as stream:
            while chunk:=stream.read(1024*1024):
                h.update(chunk)
                count+=len(chunk)
                self.budget.check()
        require(count==member['bytes'] and h.hexdigest()==member['sha256'], 'continuation retained bytes mismatch')
        return path

    def metadata(self, member, kind=None):
        require(member['bytes']<=8*1024*1024, 'continuation metadata member exceeds 8 MiB')
        raw=self.member_path(member).read_bytes()
        value=strict_json(raw)
        require(encoded(value)==raw, 'noncanonical continuation metadata')
        self.schema.check(value)
        require(value['source']==self.source and value['session_id']==self.session, 'mixed continuation source/session')
        if kind is not None:
            require(value['kind']==kind,'wrong continuation metadata kind')
        return value

    def evidence(self, root, purpose, *, common, count=None):
        if root is None:
            require(count in (None,0), 'missing nonempty evidence inventory')
            return iter(())
        require(type(root) is dict and set(root)=={'member','level','first_ordinal','item_count'}, 'closed evidence root')
        token=sha_bytes(encoded([purpose,root]))
        self.db.execute('DELETE FROM evidence_work WHERE root=?',(token,))
        self.db.execute('DELETE FROM evidence_entries WHERE root=?',(token,))
        self.db.execute('CREATE TABLE IF NOT EXISTS evidence_pending(root TEXT,ordinal INTEGER,payload BLOB,PRIMARY KEY(root,ordinal))')
        self.db.execute('DELETE FROM evidence_pending WHERE root=?',(token,))
        pending=[root]  # At most one root; descendants are immediately spilled.
        while pending:
            node=pending.pop()
            m=node['member']
            try:
                self.db.execute('INSERT INTO evidence_work VALUES(?,?,?,?,?,?,?,?)',
                    (token,m['unit_ordinal'],m['part_ordinal'],m['member_path'],m['sha256'],node['level'],node['first_ordinal'],node['item_count']))
            except sqlite3.IntegrityError as error:
                raise Invalid('duplicate/cyclic evidence index member') from error
            value=self.metadata(m,'verifier_evidence_index')
            require(all(value[k]==v for k,v in common.items()) and value['purpose']==purpose and
                    all(value[k]==node[k] for k in ('level','first_ordinal','item_count')), 'evidence source/checkpoint/range mismatch')
            require(1<=len(value['entries'])<=256,'evidence fanout bound')
            ordinal=node['first_ordinal']
            if node['level']==0:
                for entry in value['entries']:
                    require(entry['entry_kind'] in (('result',) if purpose=='results' else ('record','member')), 'wrong evidence entry purpose')
                    self.db.execute('INSERT INTO evidence_entries VALUES(?,?,?)',(token,ordinal,encoded(entry)))
                    ordinal+=1
            else:
                for child in value['entries']:
                    require(child['level']+1==node['level'] and child['first_ordinal']==ordinal and child['item_count']>0,'evidence index gap/overlap/level')
                    ordinal+=child['item_count']
                    # Persistent queue; the resident work list never grows with
                    # the total index tree. One bounded sibling set at a time.
                    cm=child['member']
                    self.db.execute('CREATE TABLE IF NOT EXISTS evidence_pending(root TEXT,ordinal INTEGER,payload BLOB,PRIMARY KEY(root,ordinal))')
                    self.db.execute('INSERT INTO evidence_pending VALUES(?,?,?)',(token,child['first_ordinal'],encoded(child)))
            require(ordinal==node['first_ordinal']+node['item_count'],'evidence advertised item count differs')
            row=self.db.execute('SELECT ordinal,payload FROM evidence_pending WHERE root=? ORDER BY ordinal LIMIT 1',(token,)).fetchone() if node['level'] or self.db.execute("SELECT 1 FROM sqlite_master WHERE name='evidence_pending'").fetchone() else None
            if row:
                self.db.execute('DELETE FROM evidence_pending WHERE root=? AND ordinal=?',(token,row[0]))
                pending.append(strict_json(row[1]))
            self.budget.check()
        actual=self.db.execute('SELECT count(*) FROM evidence_entries WHERE root=?',(token,)).fetchone()[0]
        require(actual==root['item_count'] and (count is None or actual==count),'missing evidence leaf interval')
        def entries():
            for ordinal,payload in self.db.execute('SELECT ordinal,entry FROM evidence_entries WHERE root=? ORDER BY ordinal',(token,)):
                yield strict_json(payload)
        return entries()

    def reconcile_chain(self, state, *, latest_progress_sha256, latest_backup_sha256, tlc_reparser):
        """Reconcile source-pinned results/credits, not self-asserted checked bits."""
        self.db.execute('DELETE FROM chain_results')
        self.db.execute('DELETE FROM chain_progress')
        previous_progress=previous_backup=None
        previous_after=None
        next_sequence=1
        latest=None
        initial_cursor,initial_digest=state.deterministic_initial()
        for unit,progress_path,backup_path,work_unit_json in self.auth.execute('SELECT unit_ordinal,progress_path,backup_root_path,work_unit_json FROM verifier_chain ORDER BY unit_ordinal'):
            work_unit=strict_json(work_unit_json)
            work_unit=work_unit['record'] if work_unit.get('kind')=='WorkUnit' else work_unit
            raw=Path(progress_path).read_bytes()
            require(len(raw)<=8*1024*1024,'progress metadata bound')
            progress=strict_json(raw)
            self.schema.check(progress)
            require(encoded(progress)==raw and progress['kind']=='verifier_unit_progress' and progress['source']==self.source and progress['session_id']==self.session and progress['unit_ordinal']==unit,'verifier progress binding')
            origin={k:work_unit[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode','run','job','task','attempt','stage')}
            require(progress['origin']==origin and origin['attempt']==1,'verifier authenticated origin mismatch')
            identity=state.db.execute('SELECT algorithm,source_json,session,plan,initial_image FROM verifier_meta WHERE singleton=1').fetchone()
            require(identity==(1,encoded(self.source),self.session,progress['resource_plan_sha256'],progress['initial_image_sha256']),'restored private identity differs from authenticated result chain')
            useful=state.db.execute('SELECT coalesce(sum(useful),0) FROM execution_commits WHERE sequence>? AND sequence<=?',(progress['before_verifier_commit'],progress['after_verifier_commit'])).fetchone()[0]
            require(useful==progress['committed_quantum_count'] and useful>0,'framing-only or miscounted unit grants progress')
            require(progress['previous_progress_sha256']==previous_progress and progress['previous_backup_manifest_sha256']==previous_backup,'verifier result chain skipped/reordered')
            if previous_after is not None:
                require((progress['before_verifier_commit'],progress['before_logical_digest'],progress['before_cursor'])==previous_after,'verifier before/after state chain differs')
            else:
                require(progress['before_verifier_commit']==0 and progress['before_cursor']==initial_cursor and progress['before_logical_digest']==initial_digest,'first verifier state is not deterministic empty initialization')
            require(progress['first_result_sequence']==next_sequence,'noncontiguous verifier result sequence')
            common={k:progress[k] for k in ('source','session_id','unit_ordinal','checkpoint_sha256')}
            for entry in self.evidence(progress['result_index_root'],'results',common=common,count=progress['result_count']):
                require(entry['result_sequence']==next_sequence,'duplicate/omitted verifier check result')
                result=self.metadata(entry['member'],'verifier_check_result')
                require(result['result_sequence']==next_sequence and all(result[k]==v for k,v in common.items()),'result origin/input checkpoint mismatch')
                saved=state.db.execute('SELECT claim,coverage,input_root,before_cursor,after_cursor,first_quantum,quantum_count,record FROM check_results WHERE sequence=?',(next_sequence,)).fetchone()
                require(saved is not None and (saved[0],strict_json(saved[1]),saved[2],strict_json(saved[3]),strict_json(saved[4]),saved[5],saved[6]) ==
                    (result['claim'],result['coverage'],result['input_root'],result['before_cursor'],result['after_cursor'],result['first_quantum'],result['quantum_count']),'restored result/credit row differs from authenticated execution')
                require(saved[7] is not None and strict_json(saved[7])==result,'private canonical result bytes missing/mutated')
                # Exact ordered inventories are independently compared to the
                # private journal and authenticated input record preimages.
                for purpose,field in (('inputs','input_inventory_root'),('outputs','output_inventory_root')):
                    entries=self.evidence(result[field],purpose,common=common)
                    local=iter(state.db.execute('SELECT entry FROM inventory_entries WHERE result_sequence=? AND purpose=? ORDER BY ordinal',(next_sequence,purpose)))
                    for item in entries:
                        expected=next(local,None)
                        require(expected is not None and strict_json(expected[0])==item,'changed/skipped inventory credit')
                        if item['entry_kind']=='record':
                            value=state.index.get_envelope(item['record_kind'],item['key'])
                            require(digest(value)==item['sha256'],'checked input raw preimage changed')
                        else:
                            self.member_path(item['member'])
                    require(next(local,None) is None,'omitted private inventory credit')
                if result['claim']=='tlc_completed':
                    require(result['tlc'] is not None,'TLC credit lacks retained execution bytes')
                    tlc_reparser(result,progress,self,state)
                else:
                    require(result['tlc'] is None,'non-TLC result supplies unrelated execution credit')
                self.db.execute('INSERT INTO chain_results VALUES(?,?,?,?)',(next_sequence,unit,encoded(result),sha_bytes(encoded(result))))
                next_sequence+=1
            local_inputs=iter(state.db.execute("SELECT entry FROM inventory_entries WHERE purpose='inputs' AND result_sequence>=? AND result_sequence<? ORDER BY result_sequence,ordinal",(progress['first_result_sequence'],next_sequence)))
            input_count=state.db.execute("SELECT count(*) FROM inventory_entries WHERE purpose='inputs' AND result_sequence>=? AND result_sequence<?",(progress['first_result_sequence'],next_sequence)).fetchone()[0]
            for entry in self.evidence(progress['input_inventory_root'],'inputs',common=common,count=input_count):
                local=next(local_inputs,None)
                require(local is not None and strict_json(local[0])==entry,'unit consumed-input inventory differs from exact ordered results')
            require(next(local_inputs,None) is None,'unit omitted consumed-input inventory interval')
            previous_progress=sha_bytes(raw)
            backup_raw=Path(backup_path).read_bytes()
            backup=strict_json(backup_raw)
            self.schema.check(backup)
            require(encoded(backup)==backup_raw and backup['kind']=='verifier_backup' and backup['progress_sha256']==previous_progress and backup['checkpoint_sha256']==progress['checkpoint_sha256'],'backup/progress final checkpoint mismatch')
            previous_backup=sha_bytes(backup_raw)
            previous_after=(progress['after_verifier_commit'],progress['after_logical_digest'],progress['after_cursor'])
            latest=progress
        require(latest is not None and previous_progress==latest_progress_sha256 and previous_backup==latest_backup_sha256,'missing/latest verifier snapshot chain mismatch')
        require(previous_after==(state.sequence()[0],state.logical_digest(),state.cursor()),'private snapshot after state differs from authenticated chain')
        require(next_sequence-1==state.sequence()[1] and state.db.execute('SELECT count(*) FROM check_results').fetchone()[0]==next_sequence-1,'unattributed private check result rows')
        cursor=initial_cursor
        journal_count=0
        for sequence,operation,before_raw,after_raw,input_root,first_result,result_count,useful in state.db.execute('SELECT sequence,operation,before_cursor,after_cursor,input_digest,first_result,result_count,useful FROM execution_commits ORDER BY sequence'):
            journal_count+=1
            before,after=strict_json(before_raw),strict_json(after_raw)
            require(sequence==journal_count and first_result==sequence and result_count==1 and before==cursor and
                    after['quantum']==before['quantum']+1 and after['operation']==operation and operation in OPERATIONS,
                    'private journal has skipped/reordered cursor or result')
            saved=state.db.execute('SELECT before_cursor,after_cursor,input_root,claim FROM check_results WHERE sequence=?',(first_result,)).fetchone()
            require(saved is not None and (saved[0],saved[1],saved[2])==(before_raw,after_raw,input_root),'private journal/result binding differs')
            if useful:
                require(operation in OPERATIONS[before['operation']] or before['quantum']==0 and operation==before['operation'],
                        'private useful operation transition is illegal')
            else:
                require(saved[3] in ('algorithm_checkpoint','temporal_publication_checked') and
                        all(before[k]==after[k] for k in ('stage','operation','scope','position')),
                        'framing-only journal row changed proof progress')
            cursor=after
            self.budget.check()
        require(journal_count==state.sequence()[0] and cursor==state.cursor(),'private journal does not cover exact final cursor')
        # Every durable skip/completion row needs its owning actual result.
        for table,key,selection in (('v_stores','id','checked=1'),('v_oracle','id','1'),('v_products','id','checked=1'),
                ('checked_monitors','id','1'),('checked_monitor_edges','id','1'),('verified_sccs','id','1'),
                ('verified_views','view','1'),('rooted_products_checked','id','1')):
            for identity, in state.db.execute('SELECT '+key+' FROM '+table+' WHERE '+selection):
                credit_table={'v_products':'expanded_product','checked_monitors':'checked_monitor_node',
                    'checked_monitor_edges':'checked_monitor_edge','verified_sccs':'verified_scc',
                    'verified_views':'verified_view','rooted_products_checked':'rooted_product'}.get(table,table)
                require(state.db.execute('SELECT 1 FROM credit_links c JOIN check_results r ON r.sequence=c.result_sequence WHERE c.table_name=? AND c.row_key=?',(credit_table,encoded(identity))).fetchone() is not None,'altered checked bit without execution credit')
        require(state.db.execute('SELECT 1 FROM credit_links c LEFT JOIN check_results r ON r.sequence=c.result_sequence WHERE r.sequence IS NULL LIMIT 1').fetchone() is None,'orphan private credit')
        self.db.commit()
        return latest
