"""Independent bounded schema6 semantic verifier and work-unit entry point.

Legacy schema5 admission remains in checker.py. Schema6 never imports a Rust
transition implementation, trusts a catalog projection, or page-constrains D.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import sys
import time

import checker as legacy
from paged_codec import (BLOBS, COUNT_KINDS, BoundedList, Budget, ContinuationResolver, ExecutionState, Incomplete, Invalid, RecordIndex, bounded_json, canonical,
                         digest, encoded, envelope, inner, ref, require, sha_bytes,
                         strict_json)


class Rows:
    def __init__(self, owner, kind):
        self.owner, self.kind = owner, kind

    def __len__(self):
        return self.owner.index.count(self.kind)

    def __getitem__(self, identity):
        return self.owner.store(identity) if self.kind == 'Store' else self.owner.oracle_edge(identity)

    def __iter__(self):
        for r in self.owner.index.find(self.kind):
            yield self[r['id']]


class Oracle:
    def __init__(self, owner):
        self.owner = owner

    def __contains__(self, key):
        return self.owner.db.execute('SELECT 1 FROM v_oracle WHERE src=? AND action=?', key).fetchone() is not None

    def __getitem__(self, key):
        row = self.owner.db.execute('SELECT id FROM v_oracle WHERE src=? AND action=?', key).fetchone()
        require(row is not None, 'missing required actual production outcome')
        return row[0]


class Payloads:
    def __init__(self, owner):
        self.owner = owner

    def get(self, payload):
        # Digest is an index only. Full canonical bytes decide equality.
        row = self.owner.db.execute('SELECT id FROM v_products WHERE digest=? AND canonical=?',
                                    (sha_bytes(payload.encode()), payload)).fetchone()
        return None if row is None else row[0]


def certify_semantic_numbers(value, kind):
    """Global handles are relabeled bijectively; raw source numbers never are."""
    excluded={'Store':{'id'},'Product':{'id','sql'},'OracleEvidence':{'from','to'}}[kind]
    legacy.certify_tlc_numbers({k:v for k,v in value.items() if k not in excluded},'paged raw '+kind)


class Semantic:
    def __init__(self, index, driver, image, checkpoint=None):
        self.index, self.db, self.budget = index, index.db, index.budget
        self.driver, self.image, self.checkpoint = driver, image, checkpoint
        legacy.validate_driver(driver)
        require(image['source'] == index.source and isinstance(driver, dict), 'driver/source image binding')
        self.fixture = inner(image['raw_fixture_utf8'])
        legacy.exact_keys(self.fixture, {'jobs', 'library_id', 'omitted', 'initial_projection', 'initial_inputs'}, 'actual raw fixture')
        self.ids = legacy.fixture_jobs({'jobs': [{'job': j, 'raw_job_id': raw} for j, raw in enumerate(self.fixture['jobs'])]})
        self.store_view = {'states': Rows(self, 'Store'), 'edges': Rows(self, 'OracleEvidence')}
        self.oracle, self.payloads = Oracle(self), Payloads(self)
        self.db.executescript('''
        CREATE TABLE IF NOT EXISTS v_stores(id INTEGER PRIMARY KEY,canonical TEXT UNIQUE,payload BLOB NOT NULL,parent INTEGER,checked INTEGER DEFAULT 0);
        CREATE TABLE IF NOT EXISTS v_oracle(id INTEGER PRIMARY KEY,src INTEGER,dst INTEGER,action TEXT,payload BLOB,used INTEGER DEFAULT 0,UNIQUE(src,action));
        CREATE INDEX IF NOT EXISTS v_oracle_src ON v_oracle(src,id);
        CREATE TABLE IF NOT EXISTS v_products(id INTEGER PRIMARY KEY,context TEXT,store INTEGER,digest TEXT,canonical TEXT UNIQUE,payload BLOB NOT NULL,goal INTEGER,capacity INTEGER,checked INTEGER DEFAULT 0,reachable INTEGER DEFAULT 0,parent INTEGER);
        CREATE INDEX IF NOT EXISTS v_product_digest ON v_products(digest,canonical);
        CREATE INDEX IF NOT EXISTS v_product_context ON v_products(context,id);
        CREATE TABLE IF NOT EXISTS v_edges(id INTEGER PRIMARY KEY,src INTEGER,dst INTEGER,branch BLOB,context TEXT);
        CREATE INDEX IF NOT EXISTS v_edge_src ON v_edges(src,id);
        CREATE INDEX IF NOT EXISTS v_edge_dst ON v_edges(dst,id);
        CREATE TABLE IF NOT EXISTS v_roots(context TEXT PRIMARY KEY,initial INTEGER);
        CREATE TABLE IF NOT EXISTS v_queue(ticket INTEGER PRIMARY KEY AUTOINCREMENT,node INTEGER UNIQUE);
        CREATE TABLE IF NOT EXISTS v_owned(kind TEXT,key TEXT,owner INTEGER,PRIMARY KEY(kind,key));
        CREATE TABLE IF NOT EXISTS v_progress(phase TEXT PRIMARY KEY,after_id INTEGER NOT NULL,complete INTEGER DEFAULT 0);
        CREATE TABLE IF NOT EXISTS v_fifo(ticket INTEGER PRIMARY KEY AUTOINCREMENT,product INTEGER UNIQUE,status INTEGER DEFAULT 0);
        CREATE TABLE IF NOT EXISTS v_discovered(product INTEGER PRIMARY KEY);
        ''')
        self.columns = self.initial_image()
        verify_proof_contract(index)
        if not hasattr(index, 'execution'):
            index.execution = ExecutionState(index, image['image_sha256'])
        self.execution = index.execution

    def initial_image(self):
        image = self.image
        row = self.index.blob('initial_image_blob',image['member_path'])
        require(row is not None and row[1] == image['image_sha256'] and row[2] == image['image_bytes'], 'initial image file identity')
        db = sqlite3.connect(Path(row[0]).resolve().as_uri() + '?mode=ro&immutable=1', uri=True)
        try:
            from paged_bounds import validate_initial_class
            validate_initial_class(db,self.fixture)
            schema = self.index.get('Store', image['initial_store'])['sqlite_schema_sha256']
            inventory_ref = image['schema_inventory']
            require(inventory_ref['kind'] == 'schema_inventory', 'initial schema inventory ref kind')
            inventory_blob = self.index.blob(inventory_ref['kind'],inventory_ref['id'])[:2]
            require(inventory_blob is not None and inventory_blob[1] == inventory_ref['sha256'], 'schema inventory authentication')
            inventory = strict_json(Path(inventory_blob[0]).read_bytes())
            legacy.exact_keys(inventory, {'compiled_sqlite_migrations', 'installed_sqlite_schema_sha256', 'schema_inventory', 'table_columns'}, 'actual installed schema inventory')
            require(sha_bytes(encoded(inventory))==schema==inventory_ref['sha256'] and
                    sha_bytes(encoded(inventory['schema_inventory']))==inventory['installed_sqlite_schema_sha256'], 'actual full inventory and installed SQLite schema digests')
            # Source inventory contains the exact production sqlite_master rows;
            # compare those rows, not an advertised opaque hash alone.
            actual = [list(r) for r in db.execute("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name,tbl_name,sql")]
            advertised = inventory['schema_inventory']
            require(actual == advertised, 'actual installed schema inventory differs')
            columns, initial_rows = {}, {}
            for table in sorted(legacy.SNAPSHOT_TABLES):
                cols = [r[1] for r in db.execute('PRAGMA table_info("' + table + '")')]
                require(cols and len(cols) == len(set(cols)), 'missing installed table columns')
                columns[table] = cols
                quoted = ','.join('"' + c.replace('"', '""') + '"' for c in cols)
                initial_rows[table] = [list(row) for row in db.execute('SELECT ' + quoted + ' FROM "' + table + '" ORDER BY ' + quoted)]
            require(columns == inventory['table_columns'], 'actual installed column inventory differs')
            initial = inner(self.index.get('Store', image['initial_store'])['canonical_utf8'])
            require(initial['rows'] == initial_rows, 'actual persisted initial nine-table fixture differs')
            ep = initial['episode']
            require(ep == {'now': 1000, 'claims': [0, 0], 'claim_snapshots': [], 'tokens': [[], []],
                           'renewed_claims': [], 'leases': [], 'domain_acquired': [False] * 3, 'removed': [False] * 2},
                    'actual initial Episode differs')
            # Omitted state is checked from the actual image separately; this
            # observation never becomes a substitute for live production replay.
            omitted = inner(self.index.get('BoundaryObservation', image['omitted_observation']['id'])['omitted_utf8'])
            omitted_actual = {}
            for key, table in (('files','files'), ('demand','dv_title_jobs_demand'), ('handoffs','playback_handoffs'),
                ('handoff_tokens','playback_handoffs_tokens'), ('text_cues','subtitle_text_translation_cues'),
                ('caption_leases','shared_caption_read_leases'), ('range_continuities','subtitle_translation_range_continuities')):
                omitted_actual[key] = db.execute('SELECT count(*) FROM "' + table + '"').fetchone()[0]
                require(omitted_actual[key] == 0, 'actual initial omitted extension is not empty')
            for key, table in (('marker','dv_title_jobs_schema'), ('handoff_marker','playback_handoffs_schema'),
                ('text_marker','subtitle_text_translation_schema'), ('caption_marker','shared_caption_schema')):
                omitted_actual[key] = [list(r) for r in db.execute('SELECT singleton,version FROM "' + table + '" ORDER BY singleton,version')]
                require(omitted_actual[key] == [[1,1]], 'actual initial omitted marker differs')
            omitted_actual['sequence'] = [list(r) for r in db.execute("SELECT name,seq FROM sqlite_sequence WHERE name='dv_title_jobs_demand' ORDER BY name,seq")]
            require(omitted == self.fixture['omitted'] == omitted_actual, 'actual initial omitted observation drift')
            return columns
        finally:
            db.close()

    def table(self, raw, name):
        cols = self.columns[name]
        rows = raw['rows'][name]
        require(all(type(r) is list and len(r) == len(cols) for r in rows), 'full-column row width differs')
        return [dict(zip(cols, r)) for r in rows]

    def projection(self, identity):
        record = self.index.get('Store', identity + 1)
        raw = inner(record['canonical_utf8'])
        legacy.exact_keys(raw, {'rows', 'episode'}, 'exact Store identity')
        legacy.exact_keys(raw['rows'], legacy.SNAPSHOT_TABLES, 'full snapshot')
        ep = raw['episode']
        jobs, cutoffs = [], []
        queue = self.table(raw, 'background_jobs')
        requests = self.table(raw, 'background_library_requests')
        waiters = self.table(raw, 'background_job_waiters')
        reservations = self.table(raw, 'background_job_reservations')
        for j, raw_id in enumerate(self.ids):
            matches = [r for r in queue if r['id'] == raw_id]
            paired = [r for r in requests if r['job_id'] == raw_id]
            require(len(matches) == len(paired) == 1, 'selected raw job/request identity drift')
            r, request = matches[0], paired[0]
            waiter = [w for w in waiters if w['request_id'] == request['request_id'] and w['job_id'] == raw_id and w['request_scope'] == 'library']
            require(len(waiter) == 1, 'selected request/waiter pairing')
            owned = all(r[k] is not None for k in ('owner_node_id', 'owner_boot_id', 'claim_id', 'lease_expires_ms'))
            expiry = r['lease_expires_ms'] if owned else None
            jobs.append({'state': r['state'], 'owner': r['owner_node_id'] if owned else None,
                'fence': r['fence'], 'revision': r['revision'], 'expiry': expiry,
                'failed_attempts': r['failed_attempts'], 'not_before_ms': r['not_before_ms'],
                'active': owned and r['state'] == 'running' and expiry > ep['now'],
                'request_done': request['result_json'] is not None, 'request_state': waiter[0]['state'],
                'reservation_count': sum(x['job_id'] == raw_id for x in reservations)})
            if ep['claims'][j] == 2:
                cutoffs.append('attempt_budget_j' + str(j))
        leases = [r for r in self.table(raw, 'job_leases') if r['resource'] == 'scan:library:' + str(self.fixture['library_id'])]
        require(len(leases) <= 1, 'multiple selected domain rows')
        domain = None if not leases else {a: leases[0][b] for a, b in (('resource', 'resource'), ('owner', 'owner_node_id'), ('fence', 'fence'), ('revision', 'revision'), ('expiry', 'expires_at_ms'))}
        for owner, acquired in zip(('node-a', 'node-b', 'foreign-holder'), ep['domain_acquired']):
            if acquired:
                cutoffs.append('domain_acquire_budget_' + owner)
        if all(ep['domain_acquired']):
            cutoffs.append('domain_acquire_budget')
        cutoffs.extend('renewal_budget_' + c for c in ep['renewed_claims'])
        state = {'id': identity, 'now': ep['now'], 'jobs': jobs, 'domain': domain,
                 'captured_tokens': ep['tokens'], 'captured_leases': ep['leases'],
                 'budgets': {'claims': ep['claims'], 'renewed_claims': ep['renewed_claims'],
                             'domain_acquired': ep['domain_acquired'], 'removed': ep['removed']},
                 'cutoffs': cutoffs, 'canonical_sha256': record['sha256'], 'canonical_json': record['canonical_utf8'], 'raw': raw}
        if not any(inner(a)['op'] == 'clock_advance' for a in legacy.expected_actions(state)):
            cutoffs.append('no_intrinsic_expiry_boundary')
        return state

    def store(self, identity):
        row = self.db.execute('SELECT payload FROM v_stores WHERE id=?', (identity,)).fetchone()
        require(row is not None, 'unindexed Store identity')
        self.index.get('Store', identity + 1)
        return strict_json(row[0])

    def oracle_edge(self, identity):
        row = self.db.execute('SELECT payload FROM v_oracle WHERE id=?', (identity,)).fetchone()
        require(row is not None, 'unindexed Oracle evidence')
        self.index.get('OracleEvidence', identity + 1)
        return strict_json(row[0])

    def product(self, identity):
        row = self.db.execute('SELECT payload FROM v_products WHERE id=?', (identity,)).fetchone()
        require(row is not None, 'unindexed Product identity')
        self.index.get('Product', identity)
        return strict_json(row[0])

    def boundary(self, reference, phase, state):
        require(reference['kind'] == 'BoundaryObservation', 'boundary reference kind')
        r = self.index.get('BoundaryObservation', reference['id'])
        require(ref(envelope('BoundaryObservation', r)) == reference, 'boundary digest')
        require(r['phase'] == phase and r['source'] == self.index.source and r['initial_image_id'] == self.image['image_sha256'], 'boundary phase/source/image drift')
        require(r['canonical_utf8'] == state['canonical_json'], 'actual observed endpoint differs')
        store_record=self.index.get('Store',state['id']+1)
        require((r['physical_rows_id'],r['episode_id'])==(store_record['physical_rows_id'],store_record['episode_id']),'boundary canonical factor references differ from exact Store identity')
        self.db.execute('INSERT OR IGNORE INTO boundary_used VALUES(?)',(r['id'],))
        require(inner(r['omitted_utf8']) == self.fixture['omitted'], 'omitted extension/marker/sequence drift')
        rows = self.index.get('PhysicalRows', r['physical_rows_id'])
        episode = self.index.get('Episode', r['episode_id'])
        require(inner(rows['canonical_utf8']) == state['raw']['rows'] and inner(episode['canonical_utf8']) == state['raw']['episode'], 'boundary factoring identity mismatch')
        return r

    def _progress(self, phase, identity, *, complete=False):
        self.db.execute('INSERT OR REPLACE INTO v_progress VALUES(?,?,?)', (phase, identity, int(complete)))
        self.db.commit()

    def materialize(self):
        """Each insert commits independently; a quantum can resume every phase."""
        for r in self.index.find_unchecked('Store','v_stores',1):
            if self.db.execute('SELECT 1 FROM v_stores WHERE id=?', (r['id'] - 1,)).fetchone():
                continue
            with self.execution.quantum('materialize_store', position={**self.execution.cursor()['position'], 'record_kind': 'Store', 'after_key': r['id']}, claim='semantic_range_checked', coverage={'record_kind': 'Store', 'lower_exclusive': self.execution.cursor()['position']['after_key'] if self.execution.cursor()['position']['record_kind']=='Store' else None, 'upper_inclusive': r['id'], 'count': 1, 'scope': self.execution.cursor()['scope']}) as quantum:
                state = self.projection(r['id'] - 1)
                legacy.validate_store_state(state, product=True)
                if r['id'] == self.image['initial_store']:
                    require(self.fixture['initial_projection'] == {k: v for k, v in state.items() if k not in ('raw', 'canonical_json')}, 'actual initial projection differs')
                    legacy.require_exact_inventory(self.fixture['initial_inputs'], [inner(a) for a in legacy.expected_actions(state)], 'actual initial structural inputs')
                certify_semantic_numbers(state,'Store')
                require(r['initial_image_id'] == self.image['image_sha256'], 'Store initial-image drift')
                require(r['sqlite_schema_sha256'] == self.index.get('Store', self.image['initial_store'])['sqlite_schema_sha256'], 'Store installed schema drift')
                require((r['id'] == self.image['initial_store']) == (r['certification'] == 'actual_init_checked'), 'Store initial certification')
                require((r['replay_parent_edge'] is None) == (r['id'] == self.image['initial_store']), 'Store replay root')
                require(inner(self.index.get('PhysicalRows', r['physical_rows_id'])['canonical_utf8']) == state['raw']['rows'] and
                        inner(self.index.get('Episode', r['episode_id'])['canonical_utf8']) == state['raw']['episode'], 'exact Store factoring mismatch')
                certified_boundary = self.index.get_envelope('BoundaryObservation', r['omitted_observation_id'])
                self.boundary(ref(certified_boundary), 'initial' if r['id'] == self.image['initial_store'] else 'post_apply', state)
                if r['id']==self.image['initial_store']:
                    self.db.execute('INSERT INTO boundary_owner VALUES(?,0,?)',(certified_boundary['record']['id'],'initial'))
                attempts = self.table(state['raw'], 'background_job_attempts')
                require(len({(a['job_id'], a['fence']) for a in attempts}) == len(attempts), 'single owner per actual attempt fence')
                self.db.execute('INSERT OR IGNORE INTO factor_used VALUES(?,?)',('PhysicalRows',r['physical_rows_id']))
                self.db.execute('INSERT OR IGNORE INTO factor_used VALUES(?,?)',('Episode',r['episode_id']))
                for j, captures in enumerate(state['captured_tokens']):
                    require(all(t['job_id'] == self.ids[j] for t in captures), 'raw capture ID normalization')
                for snapshot in state['raw']['episode']['claim_snapshots']:
                    legacy.validate_snapshot(snapshot, state)
                    legacy.validate_claim_snapshot_identity(snapshot, self.ids)
                self.db.execute('INSERT INTO v_stores VALUES(?,?,?,?,1)', (r['id'] - 1, r['canonical_utf8'], encoded(state), None if r['replay_parent_edge'] is None else r['replay_parent_edge'] - 1))
                self._progress('Store', r['id'])
                quantum.credit('v_stores', r['id'] - 1)
            self.budget.check(records=1)
        for r in self.index.find_unchecked('OracleEvidence','v_oracle',1):
            if self.db.execute('SELECT 1 FROM v_oracle WHERE id=?', (r['id'] - 1,)).fetchone():
                continue
            with self.execution.quantum('materialize_oracle', position={**self.execution.cursor()['position'], 'record_kind': 'OracleEvidence', 'after_key': r['id']}, claim='semantic_range_checked', coverage={'record_kind': 'OracleEvidence', 'lower_exclusive': self.execution.cursor()['position']['after_key'] if self.execution.cursor()['position']['record_kind']=='OracleEvidence' else None, 'upper_inclusive': r['id'], 'count': 1, 'scope': self.execution.cursor()['scope']}) as quantum:
                a = r['applied']
                edge = {'from': a['from_store'] - 1, 'to': a['to_store'] - 1, 'action': inner(a['action_utf8']), 'result': a['result'],
                        'resolved_token': a['resolved_token'], **{key: None if a[key + '_utf8'] is None else inner(a[key + '_utf8']) for key in ('claim_snapshot', 'claim_request', 'job_read')}, 'error': a['error']}
                before, after = self.store(edge['from']), self.store(edge['to'])
                legacy.validate_store_edge(edge, before, after)
                certify_semantic_numbers(edge,'OracleEvidence')
                require(edge['action']['op'] in ('ordinary_settle', 'clock_advance', 'resolve_initial_claim', 'read_claim_job') or canonical(edge['action']) in legacy.expected_actions(before), 'oracle outside intrinsic D')
                bs = [self.boundary(r[field], phase, state) for field, phase, state in (
                    ('before_boundary', 'pre_apply', before), ('after_boundary', 'post_apply', after), ('rollback_boundary', 'post_rollback', before))]
                require(len({b['writer_epoch'] for b in bs}) == 1, 'actual call crossed writer epochs')
                for boundary in bs:
                    require(self.db.execute('SELECT 1 FROM boundary_owner WHERE id=?',(boundary['id'],)).fetchone() is None,'durable boundary reused across distinct actual Oracle evidence')
                    self.db.execute('INSERT INTO boundary_owner VALUES(?,?,?)',(boundary['id'],r['id'],boundary['phase']))
                witness = inner(r['production_witness_utf8'])
                legacy.exact_keys(witness, {'from_projection', 'to_projection', 'applied', 'target_inputs'}, 'actual production witness')
                legacy.require_exact_inventory(witness['target_inputs'], [inner(a) for a in legacy.expected_actions(after)], 'actual intrinsic target inputs')
                require(witness['from_projection'] == {k: v for k, v in before.items() if k not in ('raw', 'canonical_json')} and
                        witness['to_projection'] == {k: v for k, v in after.items() if k not in ('raw', 'canonical_json')}, 'actual production projection differs from full rows')
                require(witness['applied'] == edge, 'all actual Applied fields differ')
                checks = inner(r['independent_checks_utf8'])
                legacy.exact_keys(checks, {'before_boundary', 'after_boundary', 'rollback_boundary', 'target_certification', 'target_boundary'}, 'actual independent check linkage')
                require(all(checks[k] == r[k] for k in ('before_boundary', 'after_boundary', 'rollback_boundary')), 'actual independent endpoint check linkage')
                target = self.index.get('Store', a['to_store'])
                require(checks['target_certification'] == target['certification'] and checks['target_boundary'] == ref(self.index.get_envelope('BoundaryObservation', target['omitted_observation_id'])), 'actual novel-state certification linkage')
                self.db.execute('INSERT INTO v_oracle VALUES(?,?,?,?,?,0)', (r['id'] - 1, edge['from'], edge['to'], canonical(edge['action']), encoded(edge)))
                self._progress('OracleEvidence', r['id'])
                quantum.credit('v_oracle', r['id'] - 1)
            self.budget.check(records=1)
        for r in self.index.find_unchecked('Product','v_products'):
            if self.db.execute('SELECT 1 FROM v_products WHERE id=?', (r['id'],)).fetchone():
                continue
            with self.execution.quantum('materialize_product', position={**self.execution.cursor()['position'], 'record_kind': 'Product', 'after_key': r['id']}, claim='semantic_range_checked', coverage={'record_kind': 'Product', 'lower_exclusive': self.execution.cursor()['position']['after_key'] if self.execution.cursor()['position']['record_kind']=='Product' else None, 'upper_inclusive': r['id'], 'count': 1, 'scope': self.execution.cursor()['scope']}) as quantum:
                p = inner(r['canonical_utf8'])
                legacy.exact_keys(p, set(legacy.PRODUCT_VALUE_FIELDS), 'exact product payload')
                require(p['context'] == r['context'] and p['sql'] + 1 == r['store_id'], 'Product key contextual Store mismatch')
                require(p['context'] in legacy.CONTEXTS, 'unknown source context')
                state = self.store(p['sql'])
                require(len(p['w']) == len(p['available']) == 2 and all(type(v) is bool for v in p['available']), 'two workers/availability required')
                require(p['completed'] == sorted(set(p['completed'])) and set(p['completed']) <= {0, 1}, 'completed history')
                for field in ('wall_anchors', 'mono_anchors'):
                    require(p[field] == sorted(set(p[field])) and all(legacy.integer(x, 0, legacy.I64_MAX) for x in p[field]), 'raw retained anchor order')
                require(legacy.integer(p['mono_now_ms'], 0, legacy.I64_MAX), 'raw monotonic time')
                require(len(p['claim_loss_keys']) <= 8 and p['claim_loss_keys'] == sorted(p['claim_loss_keys'], key=lambda x: (x['job'], x['node'], x['claim_id'])) and len({canonical(x) for x in p['claim_loss_keys']}) == len(p['claim_loss_keys']), 'persistent claim-loss keys')
                from paged_scc import CLAIMS
                for key in p['claim_loss_keys']:
                    legacy.exact_keys(key, {'job', 'node', 'claim_id'}, 'claim-loss identity')
                    require(type(key['job']) is int and key['job'] in (0, 1) and type(key['node']) is int and key['node'] in (0, 1) and key['claim_id'] in CLAIMS[key['job']*2:key['job']*2+2], 'claim-loss universe')
                for w in p['w']:
                    legacy.validate_worker(w, state, self.driver)
                    if w['claim_recovery']['phase'] != 'off':
                        require(legacy.claim_loss_key(w) in p['claim_loss_keys'], 'ambiguous episode forgot fault key')
                require(legacy.anchor_payload(legacy.logical_copy(p), self.store_view) == p, 'missing exact current source anchor')
                require(set(p['completed']) <= {j for j, job in enumerate(state['jobs']) if job['request_done']}, 'result atomicity history')
                certify_semantic_numbers(p,'Product')
                self.db.execute('INSERT INTO v_products VALUES(?,?,?,?,?,?,?,NULL,0,0,?)',
                    (r['id'], r['context'], r['store_id'] - 1, sha_bytes(canonical(p).encode()), canonical(p), encoded({'id': r['id'], **p}),
                     int(all(job['request_done'] for job in state['jobs'])), r['predecessor_edge']))
                self._progress('Product', r['id'])
                quantum.credit('v_products', r['id'])
            self.budget.check(records=1)
        if self.db.execute("SELECT 1 FROM v_progress WHERE phase='fifo_seeded'").fetchone() is not None:
            return
        with self.execution.quantum('seed_fifo', position={**self.execution.cursor()['position'], 'record_kind': None, 'after_key': None}):
            roots = list(self.index.find('ContextRoot')) if self.index.count('ContextRoot') else self.checkpoint['contexts']
            require(len(roots) == len(legacy.CONTEXTS) and {r['context'] for r in roots} == set(legacy.CONTEXTS), 'missing/unknown context roots')
            for context in roots:
                name = context['context']
                require(name in legacy.CONTEXTS and context['frontier'] == 0, 'context frontier/identity')
                initial = {'context': name, 'sql': self.image['initial_store'] - 1, 'w': [legacy.logical_copy(legacy.IDLE), legacy.logical_copy(legacy.IDLE)],
                    'available': [True, True], 'completed': [j for j, job in enumerate(self.store(self.image['initial_store'] - 1)['jobs']) if job['request_done']],
                    'mono_now_ms': 0, 'wall_anchors': [], 'mono_anchors': [], 'claim_loss_keys': []}
                legacy.anchor_payload(initial, self.store_view)
                require(legacy.product_payload(self.product(context['initial_product'])) == initial, 'exact original Init mismatch')
                require(self.index.get('Product', context['initial_product'])['predecessor_edge'] is None, 'initial Product has a predecessor')
                self.db.execute('INSERT OR REPLACE INTO v_roots VALUES(?,?)', (name, context['initial_product']))
            if self.db.execute("SELECT 1 FROM v_progress WHERE phase='fifo_seeded'").fetchone() is None:
                for name in legacy.CONTEXTS:
                    initial = self.db.execute('SELECT initial FROM v_roots WHERE context=?', (name,)).fetchone()[0]
                    self.db.execute('INSERT INTO v_fifo(product) VALUES(?)', (initial,))
                    self.db.execute('INSERT INTO v_discovered VALUES(?)', (initial,))
                self.db.execute("INSERT INTO v_progress VALUES('fifo_seeded',0,1)")
            self.db.commit()

    def own(self, kind, key, predecessor):
        try:
            self.db.execute('INSERT INTO v_owned VALUES(?,?,?)', (kind, canonical(key), predecessor))
        except sqlite3.IntegrityError as error:
            raise Invalid('duplicate predecessor ownership: ' + kind) from error

    def verify_predecessors(self):
        for expansion in self.index.find('Expansion'):
            identity = expansion['predecessor']
            row = self.db.execute('SELECT checked FROM v_products WHERE id=?', (identity,)).fetchone()
            require(row is not None, 'expansion missing predecessor')
            if row[0]:
                continue
            with self.execution.quantum('check_expansion',scope={'context':expansion['context'],'property':None,'obligation':None,'input_root':self.index.input_snapshot['record']['page_root'],'family':None}, position={**self.execution.cursor()['position'], 'record_kind': 'Expansion', 'after_key': expansion['id'], 'fifo_ticket': expansion['fifo_ticket'], 'context': expansion['context']}, claim='semantic_range_checked', coverage={'record_kind': 'Expansion', 'lower_exclusive': self.execution.cursor()['position']['after_key'] if self.execution.cursor()['position']['record_kind']=='Expansion' else None, 'upper_inclusive': expansion['id'], 'count': 1, 'scope': {**self.execution.cursor()['scope'], 'context': expansion['context']}}) as quantum:
                p = self.product(identity)
                require(expansion['context'] == p['context'] and expansion['complete'], 'expansion context/seal')
                self.own('ExpansionPredecessor', identity, identity)
                fifo = self.db.execute('SELECT ticket,product FROM v_fifo WHERE status=0 ORDER BY ticket LIMIT 1').fetchone()
                require(fifo == (expansion['fifo_ticket'], identity), 'global FIFO predecessor order differs')
                expected_demands, used = legacy.complete_product_demands(p, self.driver, self.store_view, self.oracle)
                legacy.certify_tlc_arithmetic(p, self.store_view,local_index_counts=(0,0))
                candidates = legacy.expected_product_successors(p, self.driver, self.store_view, self.oracle,collection=lambda:BoundedList(self.budget,None))
                for _, q, _, _, _, _, _ in candidates:
                    certify_semantic_numbers(q,'Product')
                expected_edges, enabled, targets = legacy.product_pair_obligations(p, candidates, self.payloads,collection=lambda:BoundedList(self.budget,None))
                actual_demands = []
                require(len(expansion['demands']) == len(expected_demands), 'missing/extra structural demands')
                require(len(expansion['candidates']) == len(expected_demands) + len(candidates), 'missing/extra independently enumerable candidates')
                for ordinal, (did, expected_d) in enumerate(zip(expansion['demands'], expected_demands)):
                    d = self.index.get('Demand', did)
                    cid = expansion['candidates'][ordinal]
                    c = self.index.get('Candidate', cid)
                    exclusion = expected_d['exclusion']
                    capture = exclusion['reason'] if exclusion is not None and exclusion['kind'] == 'capture' else None
                    family, worker = expected_d['branch']['family'], expected_d['branch']['worker']
                    sample = None
                    if worker is not None and (family == 'RetireApply' or expected_d['role'] == 'capacity' and p['w'][worker]['retire'] in {'first', 'resolve', 'second'}):
                        sample = p['w'][worker]['sample']
                    classification = 'eligible' if exclusion is None else 'excluded_sample' if exclusion['kind'] == 'sample' else 'excluded_domain'
                    reason = None if exclusion is None else canonical(exclusion)
                    choice = {'oracle_from': expected_d['oracle_from'], 'role': expected_d['role'],
                        'input': expected_d['input'], 'capture': capture, 'sample': sample}
                    descriptor = {'kind': 'oracle_demand', 'family': family, 'worker': worker, **choice}
                    expected_c = {'id': cid, 'context': p['context'], 'predecessor': identity, 'ordinal': ordinal,
                        'family': family, 'worker': worker, 'choice_utf8': canonical(choice),
                        'descriptor_utf8': canonical(descriptor), 'classification': classification, 'reason': reason}
                    require(c == expected_c, 'structural oracle candidate/classification differs')
                    role = {'EnvironmentResolve': 'dependent_resolve', 'ClaimRecoverRead': 'dependent_read', 'ClaimRecoverCancel': 'dependent_cancel'}.get(family, expected_d['role'])
                    dependencies = []
                    if role.startswith('dependent_'):
                        for i, primary in enumerate(expected_demands):
                            trigger = primary['branch']
                            if family == 'EnvironmentResolve':
                                matches = trigger['family'] == 'Environment' and primary['input'] is not None and primary['input']['op'] == 'renew'
                            else:
                                matches = trigger == {'family': 'ClaimRecover', 'worker': worker}
                            if matches:
                                primary_raw = {k: v for k, v in primary.items() if k != 'oracle_edge'}
                                generated = (legacy.dependent_resolve_demands(p, self.driver, self.store_view, self.oracle, [primary_raw])
                                    if family == 'EnvironmentResolve' else legacy.dependent_claim_demands(p, self.store_view, self.oracle, [primary_raw]))
                                expected_raw = {k: v for k, v in expected_d.items() if k != 'oracle_edge'}
                                matches = expected_raw in generated
                            if matches and primary['oracle_edge'] is not None and self.oracle_edge(primary['oracle_edge'])['to'] == expected_d['oracle_from']:
                                dependencies.append(expansion['demands'][i])
                        require(dependencies, 'dependent demand lost triggering primary')
                    expected_wire = {'id': did, 'candidate': cid, 'ordinal': 0, 'from_store': expected_d['oracle_from'] + 1,
                        'family': family, 'worker': worker, 'role': role, 'input_utf8': canonical(expected_d['input']),
                        'capture': capture, 'sample': sample, 'classification': classification, 'reason': reason,
                        'outcome_id': None if expected_d['oracle_edge'] is None else expected_d['oracle_edge'] + 1,
                        'depends_on': sorted(set(dependencies))}
                    require(d == expected_wire, 'complete apply/capacity/dependent demand differs')
                    actual_demands.append(expected_d)
                    self.own('Candidate', cid, identity)
                    self.own('Demand', did, identity)
                require(expansion['oracle_evidence'] == list(dict.fromkeys(d['oracle_edge'] + 1 for d in expected_demands if d['oracle_edge'] is not None)), 'expansion first-use actual outcome inventory')
                for oid in used:
                    self.db.execute('UPDATE v_oracle SET used=1 WHERE id=?', (oid,))
                step_ids = expansion['candidates'][len(expected_demands):]
                for ordinal, (cid, candidate) in enumerate(zip(step_ids, candidates), start=len(expected_demands)):
                    branch = candidate[0]
                    expected_c = {'id': cid, 'context': p['context'], 'predecessor': identity, 'ordinal': ordinal,
                        'family': branch['family'], 'worker': branch['worker'], 'choice_utf8': canonical(branch['choice']),
                        'descriptor_utf8': canonical({'kind': 'product_step', 'branch': branch}), 'classification': 'eligible', 'reason': None}
                    require(self.index.get('Candidate', cid) == expected_c, 'independent original Step candidate differs')
                    self.own('Candidate', cid, identity)
                actual_edges = []
                pair_labels = {}
                require(len(expansion['product_edges']) == len(step_ids), 'Step edge count')
                for ordinal, eid in enumerate(expansion['product_edges']):
                    e = self.index.get('ProductEdge', eid)
                    require(e['from_product'] == identity and e['context'] == p['context'] and e['candidate'] in expansion['candidates'], 'product edge owner/context')
                    require(e['candidate'] == step_ids[ordinal], 'Step candidate/edge ordinal')
                    c = self.index.get('Candidate', e['candidate'])
                    branch = inner(e['branch_utf8'])
                    require(branch == {'family': c['family'], 'worker': c['worker'], 'choice': inner(c['choice_utf8'])}, 'branch candidate mismatch')
                    pair = self.index.get('FairPair', canonical([p['context'], identity, e['to_product']]))
                    pair_labels[e['to_product']] = pair
                    actual_edges.append({'from': identity, 'to': e['to_product'], 'branch': branch,
                        'oracle_edge': None if e['primary_edge'] is None else e['primary_edge'] - 1,
                        'resolution_edge': None if e['resolution_edge'] is None else e['resolution_edge'] - 1,
                        'read_edge': None if e['read_edge'] is None else e['read_edge'] - 1,
                        'cleanup_edge': None if e['cleanup_edge'] is None else e['cleanup_edge'] - 1, 'fair_actions': pair['labels']})
                    self.own('ProductEdge', eid, identity)
                    self.db.execute('INSERT INTO v_edges VALUES(?,?,?,?,?)', (eid, identity, e['to_product'], encoded(branch), p['context']))
                legacy.require_exact_inventory(actual_edges, expected_edges, 'streamed original Next equality')
                expected_pair_refs = []
                for target in sorted(set(targets)):
                    pair = pair_labels[target]
                    require(pair['labels'] == sorted(set(pair['labels'])) and pair['nonstuttering'] == (canonical(legacy.product_payload(p)) != canonical(legacy.product_payload(self.product(target)))), 'nonstuttering pair equality/labels')
                    expected_pair_refs.append(ref(envelope('FairPair', pair)))
                    self.own('FairPair', canonical([p['context'], identity, target]), identity)
                require(expansion['fair_pairs']==expected_pair_refs,'expansion exact target-ordered fair pair inventory')
                actual_enabled = self.index.get('Enabledness', canonical([p['context'], identity]))
                require(actual_enabled == {'context': p['context'], 'product_id': identity, 'full_graph_labels': enabled['actions']} and expansion['enabledness'] == ref(envelope('Enabledness', actual_enabled)), 'full original nonstuttering enabledness differs')
                self.own('Enabledness', canonical([p['context'], identity]), identity)
                capacity = all(d['exclusion'] is None and d['oracle_edge'] is not None for d in expected_demands if d['role'] == 'capacity')
                selected_stores = {self.product(t)['sql'] for t in targets}
                capacity_only = {self.oracle_edge(d['oracle_edge'])['to'] for d in expected_demands if d['role'] == 'capacity' and d['oracle_edge'] is not None} - selected_stores
                require(expansion['capacity_only_stores']==sorted(x+1 for x in capacity_only),'capacity-only exact sorted Store inventory')
                discovered = []
                for ordinal, target in enumerate(targets):
                    if self.db.execute('INSERT OR IGNORE INTO v_discovered VALUES(?)', (target,)).rowcount:
                        discovered.append(target)
                        self.db.execute('INSERT INTO v_fifo(product) VALUES(?)', (target,))
                        require(self.index.get('Product', target)['predecessor_edge'] == expansion['product_edges'][ordinal], 'first discovery parent edge differs')
                require(expansion['discovered_products'] == discovered, 'exact first discovery order differs')
                self.db.execute('UPDATE v_fifo SET status=1 WHERE ticket=?', (expansion['fifo_ticket'],))
                self.db.execute('UPDATE v_products SET checked=1,capacity=? WHERE id=?', (int(capacity), identity))
                quantum.credit('expanded_product', identity)
                self.db.execute('INSERT OR REPLACE INTO v_progress VALUES(?,?,0)', ('Expansion', expansion['id']))
            self.budget.check(predecessors=1)
        require(self.db.execute('SELECT 1 FROM v_products WHERE checked=0 LIMIT 1').fetchone() is None, 'nonempty global frontier/unowned predecessor')
        require(self.db.execute('SELECT 1 FROM v_oracle WHERE used=0 LIMIT 1').fetchone() is None, 'undemanded Oracle evidence')
        for kind in ('Candidate', 'Demand', 'ProductEdge', 'FairPair', 'Enabledness'):
            require(self.index.count(kind) == self.db.execute('SELECT count(*) FROM v_owned WHERE kind=?', (kind,)).fetchone()[0], 'orphan semantic record: ' + kind)
        require(self.index.count('Expansion') == self.index.count('Product'), 'expansion partition not exact')
        self.verify_roots_and_reachability()
        with self.execution.quantum('stage_finish', position={**self.execution.cursor()['position'], 'record_kind': None, 'after_key': None}):
            self._progress('semantic_complete', 0, complete=True)
        return {'schema_version': 6, 'source': self.index.source, 'session_id': self.index.session_id,
                'counts': self.index.counts(), 'result_class': 'independent_paged_semantic_closure', 'acceptance': False,
                'numeric': 'all consumed/disabled candidates exact; source saturation unreachable',
                'frontier': 0, 'contexts': list(legacy.CONTEXTS)}

    def verify_roots_and_reachability(self):
        initialized = self.db.execute("SELECT 1 FROM operation_state WHERE operation='source_reachability'").fetchone()
        if initialized is None:
            with self.execution.quantum('source_reachability'):
                self.db.execute('DELETE FROM v_queue')
                self.db.execute('UPDATE v_products SET reachable=0')
                self.db.execute('INSERT INTO v_queue(node) SELECT initial FROM v_roots')
                self.db.execute('DELETE FROM rooted_stores')
                self.db.execute('INSERT INTO rooted_stores VALUES(?)', (self.image['initial_store'] - 1,))
                self.db.execute("INSERT INTO operation_state VALUES('source_reachability',?,?)", (encoded(self.execution.cursor()['scope']), encoded({'phase':'products'})))
        while row := self.db.execute('SELECT ticket,node FROM v_queue ORDER BY ticket LIMIT 1').fetchone():
            with self.execution.quantum('source_reachability', position={**self.execution.cursor()['position'], 'after_key': row[0]}):
                self.db.execute('DELETE FROM v_queue WHERE ticket=?', (row[0],))
                if self.db.execute('UPDATE v_products SET reachable=1 WHERE id=? AND reachable=0', (row[1],)).rowcount:
                    self.db.execute('INSERT OR IGNORE INTO v_queue(node) SELECT e.dst FROM v_edges e JOIN v_products p ON p.id=e.dst WHERE e.src=? AND p.reachable=0', (row[1],))
        if self.db.execute("SELECT 1 FROM operation_state WHERE operation='store_replay_roots'").fetchone() is None:
            with self.execution.quantum('source_reachability'):
                self.db.execute('INSERT INTO store_queue(store) VALUES(?)',(self.image['initial_store']-1,))
                self.db.execute("INSERT INTO operation_state VALUES('store_replay_roots',?,?)",(encoded(None),encoded({'after_edge':0})))
        while pending:=self.db.execute('SELECT ticket,store FROM store_queue ORDER BY ticket LIMIT 1').fetchone():
            state=strict_json(self.db.execute("SELECT state FROM operation_state WHERE operation='store_replay_roots'").fetchone()[0])
            edge=self.db.execute('SELECT e.id,s.id FROM v_stores s JOIN v_oracle e ON e.id=s.parent WHERE e.src=? AND e.dst=s.id AND e.id>? ORDER BY e.id LIMIT 1',(pending[1],state['after_edge'])).fetchone()
            with self.execution.quantum('source_reachability'):
                if edge:
                    if self.db.execute('INSERT OR IGNORE INTO rooted_stores VALUES(?)',(edge[1],)).rowcount:
                        self.db.execute('INSERT INTO store_queue(store) VALUES(?)',(edge[1],))
                    self.index.get('OracleEvidence',edge[0]+1)
                    state['after_edge']=edge[0]
                else:
                    self.db.execute('DELETE FROM store_queue WHERE ticket=?',(pending[0],))
                    state['after_edge']=0
                self.db.execute("UPDATE operation_state SET state=? WHERE operation='store_replay_roots'",(encoded(state),))
        for identity,parent,context in self.db.execute('SELECT id,parent,context FROM v_products ORDER BY id'):
            if self.db.execute('SELECT 1 FROM rooted_products_checked WHERE id=?',(identity,)).fetchone():continue
            with self.execution.quantum('source_reachability') as quantum:
                root=self.db.execute('SELECT initial FROM v_roots WHERE context=?',(context,)).fetchone()[0]
                if identity==root:
                    require(parent is None,'nonroot initial witness')
                else:
                    require(parent is not None,'missing discovery predecessor')
                    e=self.index.get('ProductEdge',parent)
                    require(e['to_product']==identity and e['context']==context,'false discovery parent edge')
                self.db.execute('INSERT INTO rooted_products_checked VALUES(?)',(identity,))
                quantum.credit('rooted_product',identity)
        require(self.db.execute('SELECT 1 FROM v_products WHERE reachable=0 LIMIT 1').fetchone() is None,'unreachable/orphan product')
        with self.execution.quantum('source_finish',scope={'context':None,'property':None,'obligation':None,'input_root':self.index.input_snapshot['record']['page_root'],'family':None},claim='semantic_range_checked') as quantum:
            self.index.get_envelope('Checkpoint',digest(self.index.input_snapshot))
            require(self.db.execute('SELECT count(*) FROM rooted_stores').fetchone()[0]==self.index.count('Store'),'unrooted Store replay chain')
            require(self.db.execute('SELECT 1 FROM v_fifo WHERE status=0 LIMIT 1').fetchone() is None,'global FIFO frontier nonzero')
            roots=self.index.find('ContextRoot') if self.index.count('ContextRoot') else self.checkpoint['contexts']
            for root in roots:
                count=self.db.execute('SELECT count(*) FROM v_products WHERE context=?',(root['context'],)).fetchone()[0]
                require(root['reachable_products']==root['expanded_products']==count and root['frontier']==0,'final context count/closure differs')
            counts=self.index.get('Counts','global') if self.index.count('Counts') else self.checkpoint['counts']
            require(counts==self.index.counts(),'global unique record counts differ')
            require(self.db.execute('SELECT count(*) FROM boundary_used').fetchone()[0]==self.index.count('BoundaryObservation'),'orphan durable boundary observation')
            require(self.index.count('BoundaryObservation')==1+3*self.index.count('OracleEvidence')==self.db.execute('SELECT count(*) FROM boundary_owner').fetchone()[0],'durable boundary inventory is not exactly Init plus three per unique Oracle evidence')
            for kind in ('PhysicalRows','Episode'):
                require(self.db.execute('SELECT count(*) FROM factor_used WHERE kind=?',(kind,)).fetchone()[0]==self.index.count(kind),'orphan canonical factor identity: '+kind)
            self.db.execute('UPDATE verifier_meta SET source_closure_checkpoint=? WHERE singleton=1',(digest(self.index.input_snapshot),))
            self.db.execute("INSERT OR REPLACE INTO operation_state VALUES('source_closure_binding',?,?)",(encoded(None),encoded(self.index.input_snapshot)))
            quantum.credit('source_closure', self.image['image_sha256'])


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.partial')
    temporary.write_bytes(encoded(value))
    temporary.replace(path)


def run_unit(request):
    """Controller-authenticated request; semantic authority stays in this lane."""
    validate_private_request(request,'unit')
    unit = request['work_unit']
    require(unit['stage'] in ('semantic_verify','monitor_scc_verify','temporal_components') or unit['stage']=='complete' and unit['mode']=='unit','wrong verifier stage/mode')
    output = Path(request['output_dir'])
    require(output.is_absolute() and not output.is_symlink(), 'owned absolute output required')
    output.mkdir(parents=True, exist_ok=True)
    remaining=request['deadline_monotonic']-time.monotonic()
    if remaining<=0:
        raise Incomplete('work-unit total deadline exhausted')
    budget = Budget(request['resource_plan']['record'], output,seconds=remaining)
    index = RecordIndex(Path(request['verifier_work_db']), source=unit['source'], session_id=unit['session_id'], budget=budget)
    index.execution=ExecutionState(index,request['initial_image']['record']['image_sha256'])
    index.request=request
    begin_private_unit(index,request)
    index.schema.check(unit, 'WorkUnit')
    index.schema.check(request['checkpoint'])
    checkpoint = request['checkpoint']['record']
    require(request['initial_image']['record']['image_sha256']==checkpoint['initial_image_id'],'input preparation Init/checkpoint identity mismatch')
    index.input_snapshot = request['checkpoint']
    index.resolver_path=Path(request['dependency_index'])
    require(checkpoint['source'] == unit['source'] and checkpoint['session_id'] == unit['session_id'] and checkpoint['resource_plan_sha256'] == budget.plan['plan_sha256'], 'work-unit snapshot binding')
    require(unit['expected_prior_checkpoint'] == (None if request['prior_checkpoint'] is None else digest(request['prior_checkpoint'])), 'prior checkpoint authentication binding')
    artifacts, complete = [], False
    stage_ready=True
    try:
        load_current_runtime(index,request)
        was_ready=index.db.execute("SELECT 1 FROM input_scopes WHERE checkpoint_sha256=? AND status='ready'",(digest(request['checkpoint']),)).fetchone() is not None
        index.prepare_input(request)
        prepare_catalog_input(index,request)
        if unit['stage']=='complete':
            raise Incomplete('final input preparation safe boundary; saved proof cursor remains suspended and nonqualifying')
        if not was_ready:
            raise Incomplete('authenticated input preparation ready; safe prefix awaits publication before semantic resume')
        saved=strict_json(index.db.execute('SELECT resume_cursor FROM input_scopes WHERE scope=?',(index.scope,)).fetchone()[0])
        if index.execution.cursor()['operation']=='check_input':
            with index.execution.quantum(saved['operation'],stage=saved['stage'],scope=saved['scope'],position=saved['position']) as quantum:
                quantum.credit('input_resume',index.scope)
        image = request['initial_image']['record']
        require(image['image_sha256'] == checkpoint['initial_image_id'], 'snapshot initial-image mismatch')
        driver_row = index.blob('source_file','driver-graph.json')
        require(driver_row is not None, 'missing authenticated driver graph')
        driver_bytes = Path(driver_row[0]).read_bytes()
        require(sha_bytes(driver_bytes) == unit['source']['driver_sha256'], 'driver artifact hash mismatch')
        semantic = Semantic(index, strict_json(driver_bytes), image, checkpoint)
        semantic.materialize()
        if unit['stage'] == 'semantic_verify':
            receipt = semantic.verify_predecessors()
            write_json(output / 'semantic-receipt.json', receipt)
            artifacts.append('semantic-receipt.json')
        else:
            require(index.db.execute("SELECT complete FROM v_progress WHERE phase='semantic_complete'").fetchone() == (1,), 'temporal stage before verified source closure')
            from paged_scc import Temporal
            temporal = Temporal(index)
            if unit['stage'] == 'monitor_scc_verify':
                if index.db.execute("SELECT complete FROM v_progress WHERE phase='monitor_build_complete'").fetchone() != (1,):
                    temporal.produce_monitors()
                if index.db.execute("SELECT complete FROM v_progress WHERE phase='scc_build_complete'").fetchone() != (1,):
                    temporal.produce_sccs()
                pending=index.db.execute("SELECT 1 FROM proposed_records p WHERE p.kind IN ('MonitorNode','MonitorEdge','Scc') AND NOT EXISTS (SELECT 1 FROM active_authoritative a WHERE a.kind=p.kind AND a.key=p.key) LIMIT 1").fetchone()
                stage_ready=pending is None
                if stage_ready:
                    temporal.verify_monitors()
                    temporal.verify_sccs()
                write_json(output / 'temporal-structure-receipt.json', {'schema_version': 6, 'source': index.source,
                    'session_id': index.session_id, 'counts': index.counts(), 'monitor_history_verified': stage_ready,
                    'all_fixed_obligation_views': stage_ready, 'all_induced_edges_and_trees': stage_ready, 'complete':stage_ready, 'acceptance': False})
                artifacts.append('temporal-structure-receipt.json')
            else:
                temporal.verify_monitors()
                temporal.verify_sccs()
                receipts = qualify_temporal(index, semantic, temporal, output, request['tlc'])
                artifacts.extend(receipts)
        complete = stage_ready
        unit['status'] = 'closed_unqualified' if complete else 'checkpointed_incomplete'
    except Incomplete as error:
        index.db.rollback()
        unit['status'] = 'checkpointed_incomplete'
        write_json(output / 'incomplete.json', {'schema_version': 6, 'source': index.source, 'session_id': index.session_id, 'reason': str(error), 'acceptance': False})
        artifacts.append('incomplete.json')
    finally:
        if hasattr(index, 'execution'):
            unit['progress'] = index.execution.cursor()['position']
        index.db.rollback()
        index.close()
    result = {'schema_version': 6, 'work_unit': unit, 'checkpoint': request['checkpoint'],
              'artifacts': artifacts, 'stage_complete': complete}
    write_json(output / 'phase-result.json', {'work_unit':unit,'stage_complete':complete})
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    command = commands.add_parser('unit')
    command.add_argument('--request', required=True, type=Path)
    restore = commands.add_parser('validate-restore')
    restore.add_argument('--request', required=True, type=Path)
    for name in ('prepare-unit','finalize-unit','complete'):
        entry=commands.add_parser(name)
        entry.add_argument('--request',required=True,type=Path)
    restored=commands.add_parser('validate-restored-verifier')
    restored.add_argument('--request',required=True,type=Path)
    args = parser.parse_args(argv)
    require(args.request.is_absolute() and args.request.is_file() and not args.request.is_symlink(), 'absolute regular request path required')
    require(args.request.stat().st_size<=8*1024*1024,'private request exceeds bounded metadata limit')
    if args.command in ('unit','prepare-unit'):
        prepare_unit(strict_json(args.request.read_bytes()))
    elif args.command=='validate-restore':
        validate_restore(strict_json(args.request.read_bytes()))
    elif args.command=='validate-restored-verifier':
        validate_restored_verifier(strict_json(args.request.read_bytes()))
    elif args.command=='finalize-unit':
        finalize_unit(strict_json(args.request.read_bytes()))
    else:
        complete_verification(strict_json(args.request.read_bytes()))
    return 0



def verify_proof_contract(index):
    package = Path(__file__).resolve().parent
    names = ('PAGED-CONTRACT.md', 'paged-proof.schema.json', 'paged-catalog.sql',
             'finite-inventory.json', 'paged-outcomes.json', 'paged-catalog-transport.schema.json',
             'paged-verifier-continuation.schema.json', 'paged-verifier-state.sql', 'source-bound-formulas.json', 'source-bound-receipt.schema.json','paged-control-carrier.schema.json')
    values = []
    for name in names:
        path = package / name
        require(path.is_file(), 'missing frozen proof-contract source: ' + name)
        values.append(['verification/async-driver-refinement/' + name, file_sha(path)])
    require(sha_bytes(encoded(sorted(values))) == index.source['proof_contract_sha256'], 'actual proof-contract bytes differ from SourceIdentity')
    finite = package / 'finite-inventory.json'
    require(file_sha(finite) == index.source['finite_inventory_sha256'], 'actual finite inventory source differs')
    # The source closure is authenticated by M4; prove that the executed checker
    # and imported semantic kernel are the exact registered source bytes too.
    for name in ('checker.py', 'paged_codec.py', 'paged_checker.py', 'paged_scc.py', 'AsyncDriverRefinement.tla',
                 'PagedOneStep.tla', 'PagedComponent.tla', 'PropertyMonitors.tla', 'paged-one-step.cfg', 'paged-component.cfg', 'BoundaryControls.tla',
                 'paged_bounds.py', 'paged-verifier-state.sql', 'source-bound-formulas.json'):
        relative = 'verification/async-driver-refinement/' + name
        row = (index.blob('source_file',relative)[1],)
        require(row is not None and row[0] == file_sha(package / name), 'actual semantic source closure mismatch: ' + name)
    from paged_bounds import derive
    from paged_codec import Schema
    expected=derive(package/'source-bound-formulas.json',index.source,
                    expected_sha256=file_sha(package/'source-bound-formulas.json'))
    require(index.budget.accounting['source_receipt']==expected,'operational plan source-global symbolic report differs from actual source derivation')
    Schema(package/'source-bound-receipt.schema.json').check(expected,'SourceBoundReceipt')
    specification=strict_json((package/'source-bound-formulas.json').read_bytes())
    kinds={r['kind'] for r in specification['inventory_mapping'] if r['role']!='operational_overhead'}
    require(set(index.budget.allowances)==kinds,'complete semantic/control/aux operational allocation inventory differs')


def bounded_source(path, chunks, budget):
    total = 0
    path = Path(path)
    temporary = path.with_suffix(path.suffix + '.partial')
    with temporary.open('wb') as out:
        for chunk in chunks:
            data = chunk.encode()
            total += len(data)
            if total > budget.plan['source_tables_bytes']:
                raise Incomplete('indivisible SourceTables/component exceeds 256 MiB')
            out.write(data)
    temporary.replace(path)
    budget.check(bytes_=total)
    return sha_bytes(path.read_bytes()) if total <= budget.member_limit() else file_sha(path)


def file_sha(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        while chunk := stream.read(65536):
            h.update(chunk)
    return h.hexdigest()


class LocalIds(dict):
    """An injective exact local naming table; raw global preimages stay external."""
    def __init__(self,values,budget):
        super().__init__()
        self.budget,self.charged=budget,0
        try:
            for value in values:
                require(type(value) is int and value>=0 and value not in self,'invalid/duplicate global ID in local bijection')
                charge=1024+32*len(encoded(value))
                budget.reserve_working(charge)
                self.charged+=charge
                legacy.certify_tlc_numbers(len(self)+1,'exact local index/count arithmetic')
                self[value]=len(self)
        except BaseException:
            budget.release_working(self.charged)
            self.charged=0
            raise
    def __del__(self):
        if hasattr(self,'budget'):self.budget.release_working(self.charged)


def local_tables(semantic, predecessor, *, boundary_edges=None):
    """Bounded exact local-ID bijections, with canonical global preimages."""
    index = semantic.index
    if boundary_edges is None:
        p = semantic.product(predecessor)
        demands, used = legacy.complete_product_demands(p, semantic.driver, semantic.store_view, semantic.oracle)
        edges = list(index.find('ProductEdge', 'from_product', predecessor))
        product_ids = sorted({predecessor, *(e['to_product'] for e in edges)})
        oracle_ids = sorted(used)
    else:
        p, demands, edges, product_ids = None, [], [], []
        oracle_ids = sorted(set(boundary_edges))
    stores = {semantic.image['initial_store'] - 1}
    for oid in oracle_ids:
        edge = semantic.oracle_edge(oid)
        stores.update((edge['from'], edge['to']))
    for identity in product_ids:
        stores.add(semantic.product(identity)['sql'])
    store_ids = sorted(stores)
    if (len(store_ids) + len(product_ids) + len(oracle_ids)) * 8192 > index.budget.member_limit():
        raise Incomplete('indivisible one-step dependency closure exceeds resident admission')
    legacy.certify_tlc_numbers((len(store_ids)+1,len(product_ids)+1,len(oracle_ids)+1),'exact invocation local index/count arithmetic')
    if p is not None:
        legacy.certify_tlc_arithmetic(p,semantic.store_view,local_index_counts=(len(store_ids),len(oracle_ids)))
    store_map = LocalIds(store_ids,index.budget)
    product_map = LocalIds(product_ids,index.budget)
    states = [semantic.store(i) for i in store_ids]
    original_edges = [semantic.oracle_edge(i) for i in oracle_ids]
    local_edges = [{**e, 'from': store_map[e['from']], 'to': store_map[e['to']]} for e in original_edges]
    products = []
    for identity in product_ids:
        value = legacy.logical_copy(semantic.product(identity))
        value['id'] = product_map[identity]
        value['sql'] = store_map[value['sql']]
        products.append(value)
    mapping = {'schema_version': 6, 'source': index.source, 'session_id': index.session_id,
               'store': [{'global': i + 1, 'local': store_map[i], 'canonical_utf8': index.get('Store', i + 1)['canonical_utf8']} for i in store_ids],
               'product': [{'global': i, 'local': product_map[i], 'canonical_utf8': index.get('Product', i)['canonical_utf8']} for i in product_ids],
               'oracle': [{'global': i + 1, 'local': n, 'applied': index.get('OracleEvidence', i + 1)['applied']} for n, i in enumerate(oracle_ids)]}
    owners = {None: -1, 'node-a': 0, 'node-b': 1, 'foreign-holder': 2}
    ranks = legacy.validate_driver(semantic.driver)
    def store_state(s):
        return {'input_domain': {k: s[k] for k in ('now', 'jobs', 'domain', 'captured_tokens', 'captured_leases', 'budgets')},
                'now': s['now'], 'jobs': [{'state': j['state'], 'active': j['active'], 'done': j['request_done'], 'owner': owners[j['owner']], 'reservations': j['reservation_count']} for j in s['jobs']],
                'tokens': [[t['claim_id'] for t in ts] for ts in s['captured_tokens']],
                'leases': [{'owner': owners[l['owner_node_id']]} for l in s['captured_leases']]}
    def chunks():
        yield '--------------------------- MODULE SourceTables ---------------------------\nEXTENDS Integers, Sequences, TLC\n'
        yield 'DriverInitial == 0\nStoreInitial == ' + str(store_map[semantic.image['initial_store'] - 1]) + '\n'
        yield 'DriverStates == ' + legacy.tla([{'pending': s['pending'], 'terminal': s['terminal'], 'work': legacy.driver_work_class(s['work']), **s['held'], 'revoked': s['revoked'], 'cancelled': s['cancelled'], 'rank': r} for s, r in zip(semantic.driver['states'], ranks)]) + '\n'
        yield 'DriverEdges == {' + ','.join(legacy.tla({'src': e['from'], 'dst': e['to'], 'reply': e['reply']}) for e in semantic.driver['edges']) + '}\n'
        yield 'StoreStates == ' + legacy.tla([store_state(s) for s in states]) + '\nStoreEdges == {'
        for n, (e, original) in enumerate(zip(local_edges, original_edges)):
            if n:
                yield ','
            yield legacy.tla({'src': e['from'], 'dst': e['to'], 'fresh': legacy.edge_fresh(original, semantic.store_view), 'monotone': legacy.edge_monotone(original, semantic.store_view),
                'op': e['action']['op'], 'result': e['result'], 'resolved': -1 if e['resolved_token'] is None else e['resolved_token'], 'input': e['action'],
                **{k: e[k] for k in ('claim_snapshot', 'claim_request', 'job_read', 'error')},
                **{k: e['action'].get(k, -1) for k in ('job', 'node', 'token', 'lease', 'disposition')}})
        from paged_scc import CLAIMS
        yield '}\nClaimIds == {' + ','.join(legacy.tla(x) for x in CLAIMS) + '}\n'
        rows = [{'id': q['id'], 'context': q['context'], 'sql': q['sql'], 'workers': q['w'], **{k: q[k] for k in ('available', 'completed', 'mono_now_ms', 'wall_anchors', 'mono_anchors', 'claim_loss_keys')}} for q in products]
        yield 'ProductStates == ' + legacy.tla(rows) + '\n'
        outgoing, enabled_rows = [], []
        for identity in product_ids:
            pair_rows = []
            if identity == predecessor:
                for target in sorted({e['to_product'] for e in edges}):
                    pair = index.get('FairPair', canonical([p['context'], predecessor, target]))
                    pair_rows.append({'dst': product_map[target], 'fair': pair['labels']})
            outgoing.append(pair_rows)
            enabled_rows.append({'state': product_map[identity], 'actions': index.get('Enabledness', canonical([p['context'], identity]))['full_graph_labels']})
        yield 'ProductOutgoing == ' + legacy.tla(outgoing) + '\nProductEnabled == ' + legacy.tla(enabled_rows) + '\n'
        yield 'ProductEdges == UNION {{[src |-> i-1,dst |-> e.dst,fair |-> e.fair] : e \\in {ProductOutgoing[i][j] : j \\in 1..Len(ProductOutgoing[i])}} : i \\in 1..Len(ProductOutgoing)}\n'
        yield 'ProductKey(p) == <<p.context,p.sql,p.workers,p.available,p.completed,p.mono_now_ms,p.wall_anchors,p.mono_anchors,p.claim_loss_keys>>\n'
        yield 'ProductIndex == '
        yield from legacy.merge_function_chunks(len(rows), lambda i: '(ProductKey(ProductStates[' + str(i + 1) + ']) :> ' + str(i) + ')')
        snapshots = {(x['raw_job_id'], x['failed_attempts']) for s in states for x in s['raw']['episode']['claim_snapshots']}
        snapshots.update((w['claim_snapshot']['raw_job_id'], w['claim_snapshot']['failed_attempts']) for q in products for w in q['w'] if w['claim_snapshot'] is not None)
        snapshots = sorted(snapshots)
        yield '\nRetryDelays == '
        yield from legacy.merge_function_chunks(len(snapshots), lambda i: '(' + legacy.tla(list(snapshots[i])) + ' :> ' + str(legacy.retry_delay_ms(*snapshots[i])) + ')')
        yield '\n=============================================================================\n'
    return chunks(), mapping, (None if predecessor is None else product_map[predecessor])


def tlc_identity(index, directory, module, config, tlc, tag):
    directory = Path(directory)
    if module == 'PagedOneStep':
        family, number = tag.rsplit('-one-step-', 1)
        scope = {'owned_predecessor': int(number)}
    elif module == 'PagedComponent':
        family, number = tag.rsplit('-component-', 1)
        scope = {'component': int(number)}
    else:
        family, scope = tag, {'boundary': tag}
    require(family in legacy.CONFIGS, 'unregistered TLC family')
    modules = [[path.name, file_sha(path)] for path in sorted(directory.glob('*.tla'))]
    required = {'PagedOneStep': {'PagedOneStep.tla', 'PagedTables.tla', 'AsyncDriverRefinement.tla', 'SourceTables.tla'},
                'PagedComponent': {'PagedComponent.tla', 'PropertyMonitors.tla', 'ComponentTables.tla'},
                'BoundaryControls': {'BoundaryControls.tla', 'SourceTables.tla'}}[module]
    require({m[0] for m in modules} == required, 'TLC imported module closure differs')
    generated = [m for m in modules if m[0] in ('SourceTables.tla', 'ComponentTables.tla', 'PagedTables.tla')]
    argv = ['java', '-Xmx4g', '-cp', 'tla2tools.jar', 'tlc2.TLC', '-workers', '1', '-metadir', 'states', '-config', Path(config).name, module + '.tla']
    require(hasattr(index, 'input_snapshot'), 'TLC lacks exact authenticated input snapshot')
    binding=index.db.execute("SELECT state FROM operation_state WHERE operation='source_closure_binding'").fetchone()
    require(binding is not None,'TLC before source closure binding')
    closure=strict_json(binding[0])
    value = {'family': family, 'scope': scope, 'source': index.source, 'input_checkpoint_sha256': digest(closure),
             'input_manifest_root': closure['record']['page_root'],
             'module': module, 'module_closure_root': sha_bytes(encoded(modules)),
             'config_sha256': file_sha(config), 'generated_inputs_root': sha_bytes(encoded(generated)),
             'local_id_map_sha256': file_sha(directory / 'id-map.json'),
             'java_sha256': tlc['java_sha256'], 'jar_sha256': tlc['jar_sha256'], 'argv_sha256': sha_bytes(encoded(argv)),
             'heap_bytes': 4294967296, 'workers': 1, 'maximum_seconds': 1800}
    return value, sha_bytes(encoded(value))


def parse_tlc_log(path, code, *, temporal_property='EventuallyDischarged'):
    import re
    failure, complete, generated, trace, back, fatal = None, False, None, False, False, False
    temporal_failure=False
    with Path(path).open(errors='strict') as log:
        while True:
            line = log.readline(1024 * 1024 + 1)
            if not line:
                break
            require(len(line) <= 1024 * 1024, 'unbounded TLC diagnostic line')
            fatal |= re.search(r'(?i)(outofmemory|java heap|parse error|semantic error|cannot find|stack overflow)', line) is not None
            complete |= 'Model checking completed. No error has been found.' in line
            m = re.search(r'([\d,]+) states generated, ([\d,]+) distinct states found, ([\d,]+) states left on queue', line)
            if m:
                generated = [int(x.replace(',', '')) for x in m.groups()]
            trace |= 'State 1:' in line
            back |= 'Back to state' in line or 'Stuttering' in line
            if 'Error: Invariant ' in line and ' is violated' in line:
                require(failure is None, 'multiple TLC first failures')
                failure = line.split('Error: Invariant ', 1)[1].split(' is violated', 1)[0]
            if 'Error: Temporal properties were violated.' in line:
                require(failure is None, 'multiple TLC first failures')
                require(type(temporal_property) is str and temporal_property,'missing fixed configured temporal property identity')
                failure = temporal_property
                temporal_failure=True
    require(not fatal and generated is not None and generated[0] > 0 and generated[1] > 0, 'missing/incompatible TLC execution inventory')
    require((code == 0 and complete and failure is None and generated[2] == 0) or
            (code == 12 and failure is not None and not temporal_failure and trace) or
            (code == 13 and temporal_failure and trace and back), 'TLC incomplete/wrong named execution verdict')
    return {'exit_code': code, 'first_failure': failure, 'complete': True, 'states': generated,
            'log_sha256': file_sha(path)}


def invoke_tlc(index, directory, module, config, tlc, *, tag):
    """Pinned fixed argv, bounded logs, exact source inputs and strict verdict."""
    import subprocess
    invocation, invocation_key = tlc_identity(index, directory, module, config, tlc, tag)
    require(set(tlc) == {'java', 'jar', 'java_sha256', 'jar_sha256'}, 'TLC runtime descriptor fields')
    for name in ('java', 'jar'):
        path = Path(tlc[name])
        require(path.is_absolute() and path.is_file() and not path.is_symlink(), 'unsafe TLC runtime path')
        require(file_sha(path) == tlc[name + '_sha256'], 'TLC runtime byte identity')
    require(hasattr(index,'current_runtime_ref'),'actual invocation lacks the fixed current runtime receipt')
    reference=index.current_runtime_ref
    runtime=index.blob(reference['kind'],reference['id'])[:2]
    require(runtime is not None and runtime[1]==reference['sha256'] and file_sha(runtime[0])==reference['sha256'],'current invocation runtime bytes differ')
    runtime_value=strict_json(Path(runtime[0]).read_bytes())
    require(runtime_value['runtime']==index.request['runtime'] and sha_bytes(encoded(runtime_value['runtime']))==index.source['toolchain_sha256'] and
        runtime_value['runtime']['java']['sha256']==tlc['java_sha256'] and runtime_value['runtime']['tlc']['sha256']==tlc['jar_sha256'],
        'actual current runtime/TLC source identity differs')
    index.budget.check()
    remaining = min(1800, index.budget.deadline - time.monotonic() - 2)
    if remaining <= 0:
        raise Incomplete('no safe TLC invocation quantum remains')
    directory = Path(directory)
    log_path = directory / (tag + '.tlc.log')
    argv = [tlc['java'], '-Xmx4g', '-cp', tlc['jar'], 'tlc2.TLC', '-workers', '1', '-metadir', str(directory / 'states'), '-config', str(config), str(directory / (module + '.tla'))]
    # Output never accumulates in RAM. Disk/RSS/time are polled while running.
    with log_path.open('wb') as log:
        process = subprocess.Popen(argv, cwd=directory, stdout=log, stderr=subprocess.STDOUT)
        try:
            while process.poll() is None:
                if time.monotonic() + 2 >= index.budget.deadline:
                    raise Incomplete('TLC work-unit deadline; component not qualified')
                index.budget.check()
                if log_path.stat().st_size > index.budget.plan['artifact_retained_bytes']:
                    raise Incomplete('TLC log artifact allowance exhausted')
                time.sleep(0.1)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    parsed = parse_tlc_log(log_path, process.returncode)
    receipt = {'schema_version': 6, 'source': index.source, 'session_id': index.session_id,
               'module': module, 'module_sha256': file_sha(directory / (module + '.tla')), 'config_sha256': file_sha(config),
               'source_tables_sha256': file_sha(directory / ('ComponentTables.tla' if module == 'PagedComponent' else 'SourceTables.tla')),
               'java_sha256': tlc['java_sha256'], 'jar_sha256': tlc['jar_sha256'], 'heap_bytes': 4294967296,
               'workers': 1, 'maximum_seconds': 1800, **invocation, 'invocation_key': invocation_key, **parsed, 'acceptance': False}
    write_json(directory / (tag + '.tlc-receipt.json'), receipt)
    return receipt


def component_tables(index, temporal, component):
    from paged_scc import fairness, OBLIGATIONS
    members = component['members']
    dependencies = set(members)
    full_edges = BoundedList(index.budget,None)
    for node in members:
        for eid, dst in index.db.execute('SELECT id,dst FROM tm_edges WHERE src=? ORDER BY id', (node,)):
            dependencies.add(dst)
            full_edges.append((node, dst))
    root = index.db.execute('SELECT node FROM tm_roots WHERE context=?', (component['context'],)).fetchone()[0]
    dependencies.add(root)
    ordered = members + sorted(dependencies - set(members))
    if (len(ordered) + len(full_edges)) * 8192 > index.budget.member_limit():
        raise Incomplete('indivisible component dependency closure exceeds resident allowance')
    local = LocalIds(ordered,index.budget)
    weak, strong = fairness(component['context'])
    histories, enabled, products, edges = {}, {}, {}, BoundedList(index.budget,None)
    for node in ordered:
        n = temporal.node(node)
        histories[local[node]] = {key: n[key] for key in ('seen_goal', 'capacity_ok', 'pending_attempts')}
        enabled[local[node]] = index.get('Enabledness', canonical([component['context'], n['product']]))['full_graph_labels']
        product = temporal.product(n['product'])
        facts = temporal.facts(n['product'])
        # Goal membership comes from independently projected durable jobs, not
        # producer monitor flags; v_products.goal was verified before monitors.
        store_row = index.db.execute('SELECT payload FROM v_stores WHERE id=?', (product['sql'],)).fetchone()
        state = strict_json(store_row[0])
        done = [j for j, job in enumerate(state['jobs']) if job['request_done']]
        products[local[node]] = {'workers': product['w'], 'capacity': facts[1], 'done_jobs': done}
        if node not in members:
            continue
        for eid, dst in index.db.execute('SELECT id,dst FROM tm_edges WHERE src=? ORDER BY id', (node,)):
            if dst not in members:
                continue
            q = temporal.node(dst)
            pair = index.get('FairPair', canonical([component['context'], n['product'], q['product']]))
            edges.append({'src': local[node], 'dst': local[dst], 'nonstuttering': pair['nonstuttering'], 'labels': pair['labels']})
    def set_record(value, field):
        fields = {k: v for k, v in value.items() if k != field}
        return legacy.tla(fields)[:-1] + ',' + field + ' |-> {' + ','.join(legacy.tla(x) for x in value[field]) + '}]'
    def chunks():
        yield '--------------------------- MODULE ComponentTables ---------------------------\nEXTENDS Integers, Sequences, TLC\n'
        yield 'SelectedProperty == ' + legacy.tla(component['property']) + '\n'
        yield 'SelectedObligation == ' + legacy.tla(component['obligation']) + '\n'
        yield 'AllObligations == {' + ','.join(legacy.tla(x) for x in OBLIGATIONS) + '}\n'
        yield 'ComponentMembers == 0..' + str(len(members) - 1) + '\nOriginalRoot == ' + str(local[root]) + '\n'
        yield 'Histories == '
        yield from legacy.merge_function_chunks(len(ordered), lambda i: '(' + str(i) + ' :> ' + set_record(histories[i], 'pending_attempts') + ')')
        yield '\nOriginalProducts == '
        yield from legacy.merge_function_chunks(len(ordered), lambda i: '(' + str(i) + ' :> ' + set_record(products[i], 'done_jobs') + ')')
        yield '\nFullMonitorEdges == {' + ','.join(legacy.tla({'src': local[a], 'dst': local[b]}) for a, b in full_edges) + '}\n'
        yield 'FullEnabled == '
        yield from legacy.merge_function_chunks(len(ordered), lambda i: '(' + str(i) + ' :> {' + ','.join(legacy.tla(x) for x in enabled[i]) + '})')
        yield '\nComponentEdges == {' + ','.join(set_record(e, 'labels') for e in edges) + '}\n'
        yield 'WeakLabels == {' + ','.join(legacy.tla(x) for x in weak) + '}\nStrongLabels == {' + ','.join(legacy.tla(x) for x in strong) + '}\n=============================================================================\n'
    mapping = {'schema_version': 6, 'source': index.source, 'session_id': index.session_id,
               'component': component['id'], 'nodes': [{'global': n, 'local': local[n], 'record': temporal.node(n)} for n in ordered]}
    return chunks(), mapping


def qualify_temporal(index, semantic, temporal, output, tlc):
    """Every original predecessor and every required component, all 11 families."""
    import shutil
    package = Path(__file__).resolve().parent
    output = Path(output)
    artifacts = []
    index.db.executescript('''CREATE TABLE IF NOT EXISTS tlc_done(tag TEXT PRIMARY KEY,invocation_key TEXT NOT NULL,input_tuple BLOB NOT NULL,receipt BLOB,relative_path TEXT);
        CREATE TABLE IF NOT EXISTS finite_failures(family TEXT PRIMARY KEY,product INTEGER,receipt BLOB);''')
    def run_once(tag, module, directory, config):
        identity, key = tlc_identity(index, directory, module, config, tlc, tag)
        old = index.db.execute('SELECT invocation_key,input_tuple,receipt,relative_path FROM tlc_done WHERE tag=?', (tag,)).fetchone()
        if old:
            require(old[0] == key and old[1] == encoded(identity), 'same TLC tag has different exact invocation key')
            retained = strict_json(old[2])
            row=index.db.execute("SELECT r.record FROM credit_links c JOIN check_results r ON r.sequence=c.result_sequence WHERE c.table_name='tlc_invocation' AND c.row_key=?",(encoded(key),)).fetchone()
            require(row is not None and row[0] is not None,'TLC reuse lacks an authenticated finalized result')
            result=strict_json(row[0])
            resolver=ContinuationResolver(index.request['dependency_index'],output,source=index.source,session_id=index.session_id,budget=index.budget)
            try:
                reparse_retained_tlc(result,None,resolver,index.execution)
                for member in (result['tlc']['receipt_member'],result['tlc']['log_member']):
                    original=resolver.member_path(member)
                    destination=output/member['member_path']
                    require(destination.resolve().is_relative_to(output.resolve()) and not destination.is_symlink(),'unsafe relocated TLC cache path')
                    destination.parent.mkdir(parents=True,exist_ok=True)
                    if original.resolve()!=destination.resolve():
                        with original.open('rb') as src,destination.open('wb') as dst:
                            while block:=src.read(65536):
                                dst.write(block)
                                index.budget.check()
                    require(file_sha(destination)==member['sha256'],'relocated TLC cache bytes differ')
                    index.db.execute('INSERT OR REPLACE INTO cache_bindings VALUES(?,?)',(str(destination),encoded(member)))
                index.db.commit()  # Excluded relocatable paths confer no proof credit.
            finally:resolver.close()
            receipt_path = output / old[3]
            require(receipt_path.is_file() and strict_json(receipt_path.read_bytes()) == retained, 'retained TLC receipt bytes changed')
            parsed = parse_tlc_log(directory / (tag + '.tlc.log'), retained['exit_code'])
            require(all(retained[k] == v for k, v in parsed.items()) and retained['invocation_key'] == key, 'retained actual TLC log/receipt disagrees')
            return retained
        operation = {'PagedOneStep': 'one_step_tlc', 'PagedComponent': 'component_tlc', 'BoundaryControls': 'boundary_tlc'}[module]
        position={**index.execution.cursor()['position'],'family':identity['family'],
            'after_key':identity['scope'].get('owned_predecessor'),'component_id':identity['scope'].get('component')}
        with index.execution.quantum(operation, stage='temporal_components', claim='tlc_completed',position=position,
                coverage={'record_kind':'Product' if module=='PagedOneStep' else 'Scc' if module=='PagedComponent' else None,
                    'lower_exclusive':None,'upper_inclusive':position['after_key'] if module=='PagedOneStep' else position['component_id'],
                    'count':1,'scope':{**index.execution.cursor()['scope'],'family':identity['family'],'input_root':key}},
                scope={**index.execution.cursor()['scope'], 'family': identity['family'], 'input_root': key}) as quantum:
            receipt = invoke_tlc(index, directory, module, config, tlc, tag=tag)
            relative = str((directory / (tag + '.tlc-receipt.json')).relative_to(output))
            index.db.execute('INSERT INTO tlc_done VALUES(?,?,?,?,?)', (tag, key, encoded(identity), encoded(receipt), relative))
            quantum.credit('tlc_invocation', key)
        return receipt
    # One-step safety/coverage checks run at every exact owned predecessor;
    # mutation invariant failures remain named expected outcome evidence.
    for family, descriptor in legacy.CONFIGS.items():
        if descriptor.get('model') == 'BoundaryControls':
            continue
        context = legacy.CONFIG_CONTEXT[family]
        for identity, in index.db.execute('SELECT id FROM v_products WHERE context=? ORDER BY id', (context,)):
            tag = family + '-one-step-' + str(identity)
            directory = output / 'temporal' / tag
            directory.mkdir(parents=True, exist_ok=True)
            chunks, mapping, owned = local_tables(semantic, identity)
            bounded_source(directory / 'SourceTables.tla', chunks, index.budget)
            write_json(directory / 'id-map.json', mapping)
            for module in ('AsyncDriverRefinement', 'PagedOneStep'):
                shutil.copyfile(package / (module + '.tla'), directory / (module + '.tla'))
            names = descriptor['invariants']
            tables = ('--------------------------- MODULE PagedTables ---------------------------\n'
                      'EXTENDS SourceTables, AsyncDriverRefinement\nOwnedPredecessors == {ProductStates[' + str(owned + 1) + ']}\n'
                      'RequiredSourceInvariants == ' + ' /\\ '.join(names) + '\n=============================================================================\n')
            (directory / 'PagedTables.tla').write_text(tables)
            cfg = ('SPECIFICATION PageSpec\nCONSTANTS\n StableSuccess = ' + ('TRUE' if descriptor['stable'] else 'FALSE') +
                   '\n Mutation = ' + legacy.tla(descriptor['mutation']) + '\n Witness = FALSE\nINVARIANTS PageAgreement PageSafety\nCHECK_DEADLOCK FALSE\n')
            config = directory / 'paged-one-step.cfg'
            config.write_text(cfg)
            receipt = run_once(tag, 'PagedOneStep', directory, config)
            if receipt['first_failure'] is not None:
                require(receipt['first_failure'] == 'PageSafety' and descriptor['expected'] in descriptor['invariants'], 'unexpected original-action one-step failure')
                first=index.db.execute('SELECT product,receipt FROM finite_failures WHERE family=?',(family,)).fetchone()
                if first is None:
                    with index.execution.quantum('family_finish'):
                        index.db.execute('INSERT INTO finite_failures VALUES(?,?,?)',(family,identity,encoded(receipt)))
                elif first[0]==identity:
                    require(first[1]==encoded(receipt),'retained first invariant failure changed')
            index.budget.check(predecessors=1)
    mapping = strict_json((package / 'paged-outcomes.json').read_bytes())
    for family in mapping['families']:
        name, context = family['family'], family['context']
        properties = family['source_descriptor'].get('properties', [])
        component_receipts = BoundedList(index.budget,'Outcome')
        witness_ref = None
        violated = False
        for prop in properties:
            view = 'seen_goal_false' if prop == 'BothSelectedCompletion' else 'capacity_ok_pending_true'
            for component in index.find('Scc'):
                if component['context'] != context or component['property'] != view:
                    continue
                tag = name + '-component-' + str(component['id'])
                directory = output / 'temporal' / tag
                directory.mkdir(parents=True, exist_ok=True)
                chunks, local_map = component_tables(index, temporal, component)
                bounded_source(directory / 'ComponentTables.tla', chunks, index.budget)
                write_json(directory / 'id-map.json', local_map)
                shutil.copyfile(package / 'PagedComponent.tla', directory / 'PagedComponent.tla')
                shutil.copyfile(package / 'PropertyMonitors.tla', directory / 'PropertyMonitors.tla')
                config = directory / 'paged-component.cfg'
                shutil.copyfile(package / 'paged-component.cfg', config)
                receipt = run_once(tag, 'PagedComponent', directory, config)
                walk = temporal.fair_walk(component)
                require((receipt['first_failure'] == 'EventuallyDischarged') == (walk is not None), 'TLC/composed fair-language disagreement')
                receipt_path = directory / (tag + '.tlc-receipt.json')
                receipt_ref = {'kind': 'tlc_receipt', 'id': str(receipt_path.relative_to(output)), 'sha256': file_sha(receipt_path)}
                index.add_blob('tlc_receipt', receipt_ref['id'], receipt_path, receipt_ref['sha256'])
                component_receipts.append(receipt_ref)
                if len(encoded(component_receipts))>min(index.budget.member_limit(),index.budget.allowances['Outcome']['max_encoded_bytes']):
                    raise Incomplete('whole Outcome component receipt array exceeds allocation')
                if walk is not None:
                    violated = True
                    require(family['expected_first_failure'] == prop, 'unexpected positive-family property violation')
                    if witness_ref is None:
                        prefix, cycle, stutter = walk
                        fairness_value = {'schema_version': 6, 'source': index.source, 'session_id': index.session_id,
                            'component': component['id'], 'property': view, 'obligation': component['obligation'],
                            'full_original_enabledness': True, 'nonstuttering_pair_occurrence': True,
                            'prefix': prefix, 'cycle': cycle, 'stutter_at': stutter}
                        fairness_path = directory / 'fairness-replay.json'
                        write_json(fairness_path, fairness_value)
                        fairness_ref = {'kind': 'fairness_receipt', 'id': str(fairness_path.relative_to(output)), 'sha256': file_sha(fairness_path)}
                        index.add_blob('fairness_receipt', fairness_ref['id'], fairness_path, fairness_ref['sha256'])
                        witness = {'family': name, 'expected_first_failure': prop,
                            'original_initial': index.db.execute('SELECT initial FROM v_roots WHERE context=?', (context,)).fetchone()[0],
                            'prefix': prefix, 'cycle': cycle, 'monitor_component': component['id'],
                            'full_graph_fairness': fairness_ref, 'capacity_prefix_verified': view == 'capacity_ok_pending_true',
                            'source': index.source, 'stutter_at': stutter}
                        temporal.replay_witness(witness)
                        value = envelope('Counterexample', witness)
                        index.propose(value,'family_finish')
                        write_json(directory / 'counterexample.json', value)
                        witness_ref = ref(value)
                index.budget.check()
        finite = index.db.execute('SELECT product,receipt FROM finite_failures WHERE family=?', (name,)).fetchone()
        if family['scope'] == 'sparse_boundary_control':
            receipt_ref = qualify_boundary(index, semantic, output, tlc, family, run_once)
            component_receipts.append(receipt_ref)
            witness_ref, violated = receipt_ref, True
        elif not properties and family['expected_first_failure'] is not None:
            require(finite is not None, 'expected original invariant witness absent')
            witness_path = output / (name + '-finite-witness.json')
            write_json(witness_path, finite_witness(index, semantic, name, finite[0]))
            witness_ref = {'kind': 'causal_receipt', 'id': witness_path.name, 'sha256': file_sha(witness_path)}
            index.add_blob('causal_receipt', witness_ref['id'], witness_path, witness_ref['sha256'])
            violated = True
        if family['expected_first_failure'] is None:
            require(not violated and finite is None, 'positive family violated')
            expected = actual = 'pass'
        elif name == 'nonvacuity':
            require(violated, 'nonvacuity original-Init witness missing')
            expected = actual = 'finite_witness'
        else:
            require(violated, 'required mutation counterexample missing')
            expected = actual = 'named_semantic_counterexample'
        # Include every original-action receipt for this family, not just SCCs.
        for tag, relative in index.db.execute('SELECT tag,relative_path FROM tlc_done ORDER BY tag'):
            if tag.startswith(name + '-one-step-'):
                path = output / relative
                r = {'kind': 'tlc_receipt', 'id': relative, 'sha256': file_sha(path)}
                index.add_blob('tlc_receipt', relative, path, r['sha256'])
                component_receipts.append(r)
        outcome = envelope('Outcome', {'family': name, 'expected': expected, 'actual': actual,
            'component_receipts': component_receipts, 'invocation_count': len(component_receipts), 'witness': witness_ref})
        index.propose(outcome,'family_finish')
        path = output / (name + '-outcome.json')
        write_json(path, outcome)
        artifacts.append(path.name)
    require(index.count('Outcome') == 11, 'eleven semantic outcome families incomplete')
    for context in semantic.checkpoint['contexts']:
        require(context['frontier']==0 and context['reachable_products']==context['expanded_products'],
                'final context publication before source closure')
        index.propose(envelope('ContextRoot',context),'family_finish')
    index.propose(envelope('Counts',index.counts()),'family_finish')
    write_json(output / 'temporal-receipt.json', {'schema_version': 6, 'source': index.source,
        'session_id': index.session_id, 'all_original_predecessors': True, 'all_required_components': True,
        'all_eleven_families': True, 'acceptance': False})
    artifacts.append('temporal-receipt.json')
    return artifacts


def finite_witness(index, semantic, family, target):
    context = legacy.CONFIG_CONTEXT[family]
    root = index.db.execute('SELECT initial FROM v_roots WHERE context=?', (context,)).fetchone()[0]
    key='finite_witness:'+family
    scope=canonical([family,root,target])
    saved=index.db.execute('SELECT scope,state FROM operation_state WHERE operation=?',(key,)).fetchone()
    if saved is None:
        with index.execution.quantum('family_finish'):
            index.db.execute('INSERT INTO finite_paths VALUES(?,?,NULL,NULL)',(scope,root))
            index.db.execute('INSERT INTO finite_queue(scope,node) VALUES(?,?)',(scope,root))
            state={'node':None,'after':0,'found':root==target}
            index.db.execute('INSERT INTO operation_state VALUES(?,?,?)',(key,encoded(scope),encoded(state)))
    else:
        require(saved[0]==encoded(scope),'finite witness target/source scope changed')
        state=strict_json(saved[1])
    while not state['found']:
        with index.execution.quantum('family_finish'):
            if state['node'] is None:
                row=index.db.execute('SELECT ticket,node FROM finite_queue WHERE scope=? ORDER BY ticket LIMIT 1',(scope,)).fetchone()
                require(row is not None,'finite witness not reachable from original Init')
                index.db.execute('DELETE FROM finite_queue WHERE ticket=?',(row[0],))
                state['node'],state['after']=row[1],0
            edge=index.db.execute('SELECT id,dst FROM v_edges WHERE src=? AND id>? ORDER BY id LIMIT 1',(state['node'],state['after'])).fetchone()
            if edge is None:
                state['node']=None
            else:
                index.get('ProductEdge',edge[0])
                if index.db.execute('INSERT OR IGNORE INTO finite_paths VALUES(?,?,?,?)',(scope,edge[1],state['node'],edge[0])).rowcount:
                    index.db.execute('INSERT INTO finite_queue(scope,node) VALUES(?,?)',(scope,edge[1]))
                state['after']=edge[0]
                state['found']=edge[1]==target
            index.db.execute('UPDATE operation_state SET state=? WHERE operation=?',(encoded(state),key))
    edges,node=BoundedList(index.budget,None),target
    while node!=root:
        row=index.db.execute('SELECT parent,edge FROM finite_paths WHERE scope=? AND node=?',(scope,node)).fetchone()
        require(row is not None,'finite original-Init prefix lost its predecessor')
        edges.append(row[1]);node=row[0]
        index.budget.check()
    edges.reverse()
    p = semantic.product(target)
    if family == 'nonvacuity':
        require(all(j['request_done'] for j in semantic.store(p['sql'])['jobs']) and all(w['dropped'] and not w['admission'] for w in p['w']), 'false finite nonvacuity endpoint')
    elif family == 'control-drop-before-join':
        require(any(w['stage'] == 'driver' and w['workStarted'] and not w['joined'] and not w['admission'] for w in p['w']), 'false drop-before-join endpoint')
    else:
        raise Invalid('unknown finite original invariant witness family')
    current = root
    for identity in edges:
        edge = index.get('ProductEdge', identity)
        require(edge['from_product'] == current, 'finite witness branch discontinuity')
        current = edge['to_product']
    require(current == target, 'finite witness wrong endpoint')
    return {'schema_version': 6, 'source': index.source, 'session_id': index.session_id,
            'family': family, 'original_initial': root, 'product_edges': edges, 'target': target,
            'expected_first_failure': legacy.CONFIGS[family]['expected'], 'acceptance': False}


def replay_finite_witness(index,database,family,witness):
    legacy.exact_keys(witness,{'schema_version','source','session_id','family','original_initial','product_edges','target','expected_first_failure','acceptance'},'finite source witness')
    require(witness['schema_version']==6 and witness['source']==index.source and witness['session_id']==index.session_id and
            witness['family']==family and witness['expected_first_failure']==legacy.CONFIGS[family]['expected'] and witness['acceptance'] is False,'finite source witness binding differs')
    context=legacy.CONFIG_CONTEXT[family]
    root=database.execute('SELECT initial FROM v_roots WHERE context=?',(context,)).fetchone()[0]
    require(root==witness['original_initial'],'finite witness restarted away from original Init')
    current=root
    for identity in witness['product_edges']:
        edge=index.get('ProductEdge',identity)
        require(edge['context']==context and edge['from_product']==current,'finite source witness is discontinuous')
        current=edge['to_product']
        index.budget.check()
    require(current==witness['target'],'finite source witness endpoint differs')
    row=database.execute('SELECT payload FROM v_products WHERE id=? AND context=?',(current,context)).fetchone()
    require(row is not None,'finite witness target absent from independently checked source graph')
    product=strict_json(row[0])
    if family=='nonvacuity':
        store=database.execute('SELECT payload FROM v_stores WHERE id=?',(product['sql'],)).fetchone()
        require(store is not None and all(j['request_done'] for j in strict_json(store[0])['jobs']) and
                all(w['dropped'] and not w['admission'] for w in product['w']),'finite nonvacuity endpoint does not violate original named invariant')
    elif family=='control-drop-before-join':
        require(any(w['stage']=='driver' and w['workStarted'] and not w['joined'] and not w['admission'] for w in product['w']),
                'finite mutation endpoint does not violate original AdmissionUntilJoined')
    else:raise Invalid('unregistered finite source witness family')
    return True


def qualify_boundary(index, semantic, output, tlc, family, run_once):
    """Sparse controls keep actual Store provenance, never a fictional SCC."""
    import shutil
    name = family['family']
    # A source-rooted actual accepted completion suffices to expose both sparse
    # original boundary controls. Follow its certified Store replay parents.
    terminal = index.db.execute("SELECT id FROM v_oracle WHERE json_extract(payload,'$.action.op')='complete' AND json_extract(payload,'$.result')='accepted' ORDER BY id LIMIT 1").fetchone()
    require(terminal is not None, 'sparse boundary actual completion witness absent')
    oracle, current, seen = [terminal[0]], semantic.oracle_edge(terminal[0])['from'], set()
    while current != semantic.image['initial_store'] - 1:
        require(current not in seen, 'cyclic Store replay provenance')
        seen.add(current)
        parent = index.db.execute('SELECT parent FROM v_stores WHERE id=?', (current,)).fetchone()[0]
        require(parent is not None, 'unrooted sparse boundary prefix')
        oracle.append(parent)
        current = semantic.oracle_edge(parent)['from']
        if len(oracle) * 64 > index.budget.member_limit():
            raise Incomplete('sparse control prefix exceeds resident admission')
    directory = output / 'temporal' / name
    directory.mkdir(parents=True, exist_ok=True)
    chunks, mapping, _ = local_tables(semantic, None, boundary_edges=oracle)
    bounded_source(directory / 'SourceTables.tla', chunks, index.budget)
    write_json(directory / 'id-map.json', mapping)
    package = Path(__file__).resolve().parent
    shutil.copyfile(package / 'BoundaryControls.tla', directory / 'BoundaryControls.tla')
    config = directory / family['source_config']
    config.write_text(legacy.config(name))
    receipt = run_once(name, 'BoundaryControls', directory, config)
    require(receipt['first_failure'] == family['expected_first_failure'], 'sparse boundary first failure differs')
    receipt_path = directory / (name + '.tlc-receipt.json')
    reference={'kind':'tlc_receipt','id':str(receipt_path.relative_to(output)),'sha256':file_sha(receipt_path)}
    index.add_blob(reference['kind'],reference['id'],receipt_path,reference['sha256'])
    return reference




CATALOG_KINDS = ('PhysicalRows', 'Episode', 'Store', 'Product', 'Applied', 'OracleEvidence',
    'Candidate', 'Demand', 'ProductEdge', 'FairPair', 'Enabledness', 'Expansion', 'Counts',
    'ContextRoot', 'InitialImage', 'Checkpoint', 'Page', 'ManifestNode', 'ResourcePlan', 'Session',
    'WorkUnit', 'MonitorNode', 'MonitorEdge', 'Scc', 'Counterexample', 'Outcome', 'Completion',
    'BoundaryObservation', 'ArtifactReceipt')
LOGICAL_OMITTED = frozenset(('Checkpoint', 'Page', 'ManifestNode', 'Session', 'WorkUnit', 'ArtifactReceipt', 'Completion'))


def validate_restore(request):
    """Independent catalog/page correspondence; never source-closure acceptance.

    M4 already authenticates/reassembles B transport. M1 separately checks all
    private SQL/FIFO/allocator invariants. This reads neither as semantic trust.
    """
    validate_private_request(request,'validate_restore')
    output = Path(request['output_dir'])
    require(output.is_absolute() and not output.is_symlink(), 'owned restore output path')
    output.mkdir(parents=True, exist_ok=True)
    unit = request['work_unit']
    checkpoint_envelope = request['checkpoint']
    checkpoint = checkpoint_envelope['record']
    budget = Budget(request['resource_plan']['record'], output,seconds=request['deadline_monotonic']-time.monotonic())
    index = private_input_view(request)
    index.schema.check(checkpoint_envelope)
    require(checkpoint['source'] == index.source and checkpoint['session_id'] == index.session_id and
            checkpoint['resource_plan_sha256'] == budget.plan['plan_sha256'], 'restore snapshot identity')
    path = Path(request['catalog_path'])
    require(path.is_absolute() and path.is_file() and not path.is_symlink(), 'staged catalog path')
    resolver = sqlite3.connect(Path(request['dependency_index']).as_uri() + '?mode=ro&immutable=1', uri=True)
    catalog = sqlite3.connect(path.as_uri() + '?mode=ro&immutable=1', uri=True)
    try:
        catalog.execute('PRAGMA query_only=ON')
        catalog.execute('PRAGMA temp_store=FILE')
        catalog.execute('PRAGMA cache_size=-2048')
        require(catalog.execute('PRAGMA integrity_check').fetchone() == ('ok',), 'restored SQLite integrity')
        require(catalog.execute('PRAGMA foreign_key_check').fetchone() is None, 'restored SQLite foreign key violation')
        meta = catalog.execute('SELECT current_commit,schema_version,source_json,session_id,resource_plan_sha256 FROM catalog_meta WHERE singleton=1').fetchone()
        require(meta is not None and meta[0] == checkpoint['committed_transaction'] and meta[1] == 6 and
                strict_json(meta[2]) == index.source and meta[3] == index.session_id and meta[4] == budget.plan['plan_sha256'], 'restored catalog exact snapshot/source/session/plan')
        plan_bytes=encoded(request['resource_plan'])
        plan_key=digest(request['resource_plan'])
        plan=catalog.execute('SELECT length(descriptor),admission FROM resource_plans WHERE digest=?',(plan_key,)).fetchone()
        require(plan==(len(plan_bytes),'admitted') and catalog.execute('SELECT count(*) FROM resource_plans').fetchone()==(1,),
                'restored separate admitted plan inventory differs')
        require(catalog.execute('SELECT descriptor FROM resource_plans WHERE digest=?',(plan_key,)).fetchone()==(plan_bytes,),
                'restored separate ResourcePlan full envelope bytes differ')
        binding=encoded({'source':index.source,'session_id':index.session_id,'resource_plan':request['resource_plan']})
        require(catalog.execute("SELECT length(payload) FROM catalog_control WHERE key='binding'").fetchone()==(len(binding),) and
                catalog.execute("SELECT payload FROM catalog_control WHERE key='binding'").fetchone()==(binding,),
                'restored private source/session/plan binding differs')
        scope_state=index.db.execute('SELECT checkpoint_sha256,status FROM input_scopes WHERE scope=?',(index.scope,)).fetchone()
        if scope_state is None or scope_state!=(digest(checkpoint_envelope),'ready'):
            receipt={'schema_version':6,'catalog_sha256':budget_file_sha(path,index.budget),'checkpoint_sha256':digest(checkpoint_envelope),
                'source':index.source,'session_id':index.session_id,'catalog_logical_digest':checkpoint['catalog_logical_digest'],
                'catalog_commit':checkpoint['committed_transaction'],'counts':checkpoint['counts'],'contexts':checkpoint['contexts'],
                'manifest_root':checkpoint['page_root'],'complete':False,'acceptance':False,'scope':'input_preparation'}
            write_json(output/'restore-semantic.json',receipt)
            return receipt
        certificate=index.db.execute("SELECT cursor,status,owner_result FROM input_work WHERE scope=? AND pass='catalog'",(index.scope,)).fetchone()
        require(certificate is not None and certificate[1]=='done','ready input scope lacks catalog correspondence certificate')
        checked=strict_json(certificate[0])
        actual_sha=budget_file_sha(path,index.budget)
        require(checked['checkpoint']==digest(checkpoint_envelope) and checked['phase']=='done' and
                (checked['catalog_sha256'],checked['bytes'])==(actual_sha,path.stat().st_size),
                'restored catalog differs from exact authenticated checked prefix')
        require(index.db.execute("SELECT 1 FROM credit_links WHERE table_name='input_catalog' AND row_key=? AND result_sequence=?",
            (encoded(index.scope),certificate[2])).fetchone() is not None,'catalog certificate lacks actual checked result')
        # The actual result chain and immutable backup/hash preserve the prior
        # complete row comparisons. Rebuilding a second index or comparing the
        # entire prefix again would erase the continuation benefit.
        verify_proof_contract(index)
        receipt={'schema_version':6,'source':index.source,'session_id':index.session_id,
            'checkpoint_sha256':digest(checkpoint_envelope),'catalog_commit':checkpoint['committed_transaction'],
            'catalog_logical_digest':checkpoint['catalog_logical_digest'],'manifest_root':checkpoint['page_root'],
            'catalog_sha256':actual_sha,'counts':checkpoint['counts'],'contexts':checkpoint['contexts'],
            'complete':True,'acceptance':False,
            'scope':'restored catalog/page identity correspondence; production replay and full semantic qualification remain separate'}
        write_json(output/'restore-semantic.json',receipt)
        return receipt
    finally:
        resolver.close()
        catalog.close()
        index.close()



def reparse_retained_tlc(result, progress, resolver, state):
    tlc=result['tlc']
    receipt_path=resolver.member_path(tlc['receipt_member'])
    log_path=resolver.member_path(tlc['log_member'])
    raw=receipt_path.read_bytes()
    require(len(raw)<=8*1024*1024,'retained TLC receipt bound')
    receipt=strict_json(raw)
    require(encoded(receipt)==raw and receipt['source']==state.index.source and receipt['session_id']==state.index.session_id,'retained TLC source/session/canonical identity')
    parsed=parse_tlc_log(log_path,tlc['exit_code'])
    require(all(receipt[k]==v for k,v in parsed.items()),'actual retained TLC log disagrees with receipt')
    for key in ('invocation_key','family','module','module_closure_root','config_sha256','generated_inputs_root',
                'local_id_map_sha256','java_sha256','jar_sha256','argv_sha256','heap_bytes','workers','maximum_seconds',
                'exit_code','first_failure','complete'):
        require(receipt[key]==tlc[key],'retained exact TLC invocation/verdict differs')
    row=state.db.execute('SELECT input_tuple,receipt FROM tlc_done WHERE invocation_key=?',(tlc['invocation_key'],)).fetchone()
    require(row is not None and sha_bytes(row[0])==tlc['invocation_key'] and strict_json(row[1])==receipt,'unattributed/rekeyed private TLC completion')
    identity=strict_json(row[0])
    require(identity['source']==state.index.source and identity['family']==tlc['family'] and identity['module']==tlc['module'],'TLC identity source/family')
    modules=[]
    generated=[]
    map_digest=config_digest=None
    runtime_seen=False
    common={k:result[k] for k in ('source','session_id','unit_ordinal','checkpoint_sha256')}
    for entry in resolver.evidence(result['input_inventory_root'],'inputs',common=common):
        if entry['entry_kind']!='member':
            continue
        member=entry['member']
        name=Path(member['member_path']).name
        path=resolver.member_path(member)
        if member['member_path']=='runtime/'+str(result['unit_ordinal'])+'.json':
            require(not runtime_seen and member['bytes']<=8*1024*1024,'duplicate/oversized original TLC runtime input')
            original=bounded_json(path.read_bytes(),state.budget)
            unit=resolver.auth.execute('SELECT work_unit_json FROM verifier_chain WHERE unit_ordinal=?',(result['unit_ordinal'],)).fetchone()
            require(unit is not None,'original TLC runtime has no authenticated executed unit')
            executed=strict_json(unit[0]);state.index.schema.check(executed)
            executed=executed['record']
            require(set(original)=={'schema_version','source','session_id','unit_ordinal','origin','resource_plan_sha256','runtime','runner'} and
                    original['schema_version']==1 and original['source']==state.index.source and original['session_id']==state.index.session_id and
                    original['unit_ordinal']==result['unit_ordinal'] and original['origin']=={k:executed[k] for k in ('run','job','task','attempt')} and
                    original['resource_plan_sha256']==result['resource_plan_sha256'] and
                    sha_bytes(encoded(original['runtime']))==state.index.source['toolchain_sha256'] and
                    original['runtime']['java']['sha256']==tlc['java_sha256'] and original['runtime']['tlc']['sha256']==tlc['jar_sha256'],
                    'retained actual TLC runtime/source/origin binding differs')
            runtime_seen=True
        elif name.endswith('.tla'):
            modules.append([name,member['sha256']])
            if name in ('SourceTables.tla','ComponentTables.tla','PagedTables.tla'):
                generated.append([name,member['sha256']])
        elif name.endswith('.cfg'):
            config_digest=member['sha256']
        elif name=='id-map.json':
            map_digest=member['sha256']
    require(runtime_seen,'retained TLC evidence omits actual original-unit runtime input')
    require(sha_bytes(encoded(sorted(modules)))==tlc['module_closure_root'] and
            sha_bytes(encoded(sorted(generated)))==tlc['generated_inputs_root'] and
            config_digest==tlc['config_sha256'] and map_digest==tlc['local_id_map_sha256'],'retained TLC module/config/table/map closure differs')
    require(identity['module_closure_root']==tlc['module_closure_root'] and identity['generated_inputs_root']==tlc['generated_inputs_root'] and
            identity['config_sha256']==tlc['config_sha256'] and identity['local_id_map_sha256']==tlc['local_id_map_sha256'],'same tag changed actual TLC inputs')
    position=result['after_cursor']['position']
    expected_scope={'owned_predecessor':position['after_key']} if tlc['module']=='PagedOneStep' else {'component':position['component_id']} if tlc['module']=='PagedComponent' else {'boundary':tlc['family']}
    require(identity['scope']==expected_scope,'retained TLC exact owned scope differs')
    return True


PRIVATE_REQUEST_KEYS={'schema_version','operation','deadline_monotonic','work_unit','resource_plan','initial_image',
    'prior_checkpoint','checkpoint','catalog_path','output_dir','dependency_index','source_files','driver_graph','tlc','runtime',
    'verifier_work_db','prepare_path','reservation_path','previous_verifier_progress_sha256','previous_verifier_backup_sha256'}
PRIVATE_ROLES={'source_tables','module','config','log','receipt','local_id_map','witness','temporal_import',
    'temporal_publication','result','evidence_index','progress','catalog_page','catalog_manifest','initial_image','source_receipt','phase_result'}


def validate_private_request(request,operation,*,private_restore=False):
    extra={'verifier_checkpoint','verifier_backup_root'} if private_restore else set()
    require(type(request) is dict and set(request)==PRIVATE_REQUEST_KEYS|extra,'closed current private verifier request differs')
    require(request['schema_version']==6 and request['operation']==operation,'private verifier operation/version differs')
    runtime=request['runtime']
    require(type(runtime) is dict and set(runtime)=={'rustc','cargo','java','python','tlc','action_node'} and
            sha_bytes(encoded(runtime))==request['work_unit']['source']['toolchain_sha256'],'private runtime/source binding differs')
    for name in ('rustc','cargo','java','python','action_node'):
        tool=runtime[name]
        require(type(tool) is dict and set(tool)=={'path','sha256','argv','version'} and
                tool['argv']==(['-version'] if name=='java' else ['--version']),'closed registered runtime descriptor differs')
    require(type(runtime['tlc']) is dict and set(runtime['tlc'])=={'path','sha256'},'closed TLC descriptor differs')
    require(Path(sys.executable).resolve()==Path(runtime['python']['path']).resolve() and
            file_sha(Path(sys.executable).resolve())==runtime['python']['sha256'],'actual verifier Python binary differs')
    require(request['tlc']=={'java':runtime['java']['path'],'java_sha256':runtime['java']['sha256'],
            'jar':runtime['tlc']['path'],'jar_sha256':runtime['tlc']['sha256']},'private TLC/toolchain binding differs')

    require(type(request['work_unit']) is dict and 'record' not in request['work_unit'],'current WorkUnit must be the frozen record directly')
    from paged_codec import Schema
    Schema().check(request['work_unit'],'WorkUnit')
    require(type(request['deadline_monotonic']) in (int,float) and request['deadline_monotonic']>time.monotonic(),'work-unit deadline is expired')
    for key in ('output_dir','dependency_index','verifier_work_db','prepare_path','reservation_path','catalog_path','driver_graph'):
        require(type(request[key]) is str and Path(request[key]).is_absolute() and not Path(request[key]).is_symlink(),'unsafe private path: '+key)


def begin_private_unit(index,request):
    db=index.db
    unit=request['work_unit']
    maximum=db.execute('SELECT max(unit) FROM unit_work').fetchone()[0]
    require(maximum is None or unit['unit_ordinal']>maximum,'verifier attempted-unit ordinal reused or moved backwards')
    require(unit['unit_ordinal']>request['checkpoint']['record']['unit_ordinal'],'verifier actual attempt does not follow its authenticated input checkpoint')
    origin={k:unit[k] for k in ('repository','pr','event_base','workflow_id','event_name','mode','run','job','task','attempt','stage')}
    before=index.execution.sequence()[0],index.execution.logical_digest(),index.execution.cursor()
    first=index.execution.sequence()[1]+1
    op=before[2]['operation']
    with index.execution.quantum(op,framing=True):
        db.execute('INSERT INTO unit_work VALUES(?,?,?,?,?,?,?,?,0)',(unit['unit_ordinal'],encoded(origin),digest(request['checkpoint']),before[0],before[1],encoded(before[2]),first,encoded(request['checkpoint'])))


def load_current_runtime(index,request):
    """Current source-checked controller input, never historical upload credit."""
    unit=request['work_unit']
    relative='runtime/'+str(unit['unit_ordinal'])+'.json'
    path=Path(request['output_dir'])/relative
    require(path.is_file() and not path.is_symlink() and path.stat().st_size<=8*1024*1024,'fixed current runtime receipt missing/unsafe')
    raw=path.read_bytes();value=strict_json(raw)
    require(encoded(value)==raw and set(value)=={'schema_version','source','session_id','unit_ordinal','origin',
        'resource_plan_sha256','runtime','runner'},'closed current runtime receipt differs')
    require(value['schema_version']==1 and value['source']==index.source and value['session_id']==index.session_id and
        value['unit_ordinal']==unit['unit_ordinal'] and value['origin']=={k:unit[k] for k in ('run','job','task','attempt')} and
        value['resource_plan_sha256']==index.budget.plan['plan_sha256'] and value['runtime']==request['runtime'] and
        sha_bytes(encoded(value['runtime']))==index.source['toolchain_sha256'],'current runtime source/origin/toolchain binding differs')
    require(type(value['runner']) is dict and set(value['runner'])=={'name','os','arch'} and
        all(type(x) is str and x for x in value['runner'].values()),'current actual runner descriptor differs')
    reference={'kind':'runtime_receipt','id':relative,'sha256':sha_bytes(raw)}
    index.add_blob(reference['kind'],reference['id'],path,reference['sha256'])
    index.db.commit()
    index.current_runtime_ref=reference
    resources=Path(request['output_dir'])/'resources.json'
    require(resources.is_file() and not resources.is_symlink() and resources.stat().st_size<=8*1024*1024,'fixed admitted resource receipt missing/unsafe')
    resource_bytes=resources.read_bytes();measurement=strict_json(resource_bytes)
    require(encoded(measurement)==resource_bytes and measurement['runtime_sha256']==index.source['toolchain_sha256'],
            'admitted resource receipt canonical/toolchain binding differs')
    resource_ref={'kind':'runtime_receipt','id':'resources.json','sha256':sha_bytes(resource_bytes)}
    require(index.budget.plan['available_disk_receipt']==resource_ref and
            index.budget.accounting['measurement_sha256']==resource_ref['sha256'],'admitted actual measurement receipt differs')
    index.add_blob(resource_ref['kind'],resource_ref['id'],resources,resource_ref['sha256'])
    index.db.commit()
    return reference


def budget_file_sha(path,budget):
    h=hashlib.sha256()
    with Path(path).open('rb') as stream:
        while chunk:=stream.read(65536):h.update(chunk);budget.check(bytes_=len(chunk))
    return h.hexdigest()


def prepare_catalog_input(index,request):
    # The catalog is read-only until byte correspondence is independently
    # established. This cursor is bound to the exact immutable backup file.
    cp=request['checkpoint'];body=cp['record'];cp_sha=digest(cp)
    state=index.db.execute('SELECT status FROM input_scopes WHERE scope=?',(index.scope,)).fetchone()[0]
    if state=='ready':return
    require(state=='dependencies_ready','catalog comparison before root-bound dependencies complete')
    path=Path(request['catalog_path'])
    require(path.is_file() and not path.is_symlink(),'missing immutable catalog during preparation')
    file_digest=budget_file_sha(path,index.budget);file_bytes=path.stat().st_size
    row=index.db.execute("SELECT cursor,status FROM input_work WHERE scope=? AND pass='catalog'",(index.scope,)).fetchone()
    if row is None:
        with index.input_quantum('check_input') as q:
            cursor={'checkpoint':cp_sha,'catalog_sha256':file_digest,'bytes':file_bytes,'kind':0,'after':[-1,None,None],'phase':'records'}
            index.db.execute('INSERT INTO input_work VALUES(?,?,?,?,?,?,?,?,?)',(index.scope,'catalog','Checkpoint',cp_sha,'catalog',cp_sha,'pending',encoded(cursor),q.sequence))
    else:
        cursor=strict_json(row[0])
        require((cursor['checkpoint'],cursor['catalog_sha256'],cursor['bytes'])==(cp_sha,file_digest,file_bytes),'continued catalog comparison changed checkpoint/file')
    catalog=sqlite3.connect(path.as_uri()+'?mode=ro&immutable=1',uri=True)
    catalog.execute('PRAGMA temp_store=FILE');catalog.execute('PRAGMA cache_size=-2048')
    try:
        require(catalog.execute('PRAGMA integrity_check').fetchone()==('ok',),'preparation catalog integrity')
        require(catalog.execute('PRAGMA foreign_key_check').fetchone() is None,'preparation catalog foreign key violation')
        meta=catalog.execute('SELECT current_commit,schema_version,source_json,session_id,resource_plan_sha256 FROM catalog_meta WHERE singleton=1').fetchone()
        require(meta is not None and meta[:2]==(body['committed_transaction'],6) and strict_json(meta[2])==index.source and
            meta[3:]==(index.session_id,index.budget.plan['plan_sha256']),'preparation catalog identity/snapshot differs')
        require(catalog.execute('SELECT descriptor FROM checkpoints WHERE digest=?',(cp_sha,)).fetchone()==(encoded(cp),),'catalog exact input checkpoint differs')
        plan=request['resource_plan'];pk=digest(plan)
        require(catalog.execute('SELECT descriptor,admission FROM resource_plans WHERE digest=?',(pk,)).fetchone()==(encoded(plan),'admitted') and
            catalog.execute('SELECT count(*) FROM resource_plans').fetchone()==(1,),'catalog separate ResourcePlan differs')
        binding=encoded({'source':index.source,'session_id':index.session_id,'resource_plan':plan})
        require(catalog.execute("SELECT payload FROM catalog_control WHERE key='binding'").fetchone()==(binding,),'catalog source/session binding differs')
        while cursor['kind']<len(CATALOG_KINDS):
            kind=CATALOG_KINDS[cursor['kind']]
            if kind in LOGICAL_OMITTED:
                with index.input_quantum('check_input') as q:
                    cursor['kind']+=1;cursor['after']=[-1,None,None]
                    index.db.execute("UPDATE input_work SET cursor=?,owner_result=? WHERE scope=? AND pass='catalog'",(encoded(cursor),q.sequence,index.scope))
                continue
            if cursor['phase']=='records':
                kt,kn,ks=cursor['after']
                row=catalog.execute('SELECT key_type,key_integer,key_text,key_json,length(payload),digest,created_commit FROM catalog_records WHERE kind=? AND (key_type>? OR key_type=? AND ((key_type=0 AND key_integer>?) OR (key_type=1 AND key_text>?))) ORDER BY key_type,key_integer,key_text LIMIT 1',(kind,kt,kt,kn,ks)).fetchone()
                if row:
                    typ,num,text,key,size,d,commit=row
                    require(commit<=body['committed_transaction'] and typ in (0,1),'post-snapshot/invalid typed catalog key')
                    identity=num if typ==0 else text
                    require(strict_json(key)==identity and key==encoded(identity),'catalog canonical typed key differs')
                    if size>index.budget.member_limit():raise Incomplete('indivisible catalog record exceeds resident allowance')
                    with index.input_quantum('check_input') as q:
                        expected=index.get_envelope(kind,identity)
                        raw=encoded(expected)
                        actual=catalog.execute('SELECT payload FROM catalog_records WHERE kind=? AND key_json=?',(kind,key)).fetchone()
                        require(actual==(raw,) and d in (digest(expected),bytes.fromhex(digest(expected))),'catalog/page complete envelope differs')
                        cursor['after']=[typ,num,text]
                        index.db.execute("UPDATE input_work SET cursor=?,owner_result=? WHERE scope=? AND pass='catalog'",(encoded(cursor),q.sequence,index.scope))
                        q.credit('catalog_record',[index.scope,kind,identity]);index.budget.check(records=1,bytes_=size)
                    continue
                with index.input_quantum('check_input') as q:
                    cursor['phase']='missing';cursor['after']=[-1,None,None]
                    index.db.execute("UPDATE input_work SET cursor=?,owner_result=? WHERE scope=? AND pass='catalog'",(encoded(cursor),q.sequence,index.scope))
                continue
            # Check the other inclusion direction using actual selected members.
            after=cursor['after'][2] or ''
            missing=index.db.execute('SELECT key FROM active_authoritative WHERE kind=? AND key>? ORDER BY key LIMIT 1',(kind,after)).fetchone() if kind!='ResourcePlan' else None
            if missing:
                with index.input_quantum('check_input') as q:
                    key=missing[0]
                    require(catalog.execute('SELECT 1 FROM catalog_records WHERE kind=? AND key_json=?',(kind,key.encode())).fetchone() is not None,'selected page record missing from catalog')
                    cursor['after']=[-1,None,key]
                    index.db.execute("UPDATE input_work SET cursor=?,owner_result=? WHERE scope=? AND pass='catalog'",(encoded(cursor),q.sequence,index.scope))
                    q.credit('catalog_member',[index.scope,kind,strict_json(key)]);index.budget.check(records=1)
                continue
            with index.input_quantum('check_input') as q:
                cursor['kind']+=1;cursor['phase']='records';cursor['after']=[-1,None,None]
                index.db.execute("UPDATE input_work SET cursor=?,owner_result=? WHERE scope=? AND pass='catalog'",(encoded(cursor),q.sequence,index.scope))
        # Required whole logical-root authentication is indivisible, accounted,
        # and may legitimately report resource_blocked. It does not re-add or
        # re-debit the already compared records as newly created records.
        h=hashlib.sha256(b'plurx-paged-catalog-v6\0')
        h.update(encoded({'source':index.source,'session_id':index.session_id,'resource_plan_sha256':index.budget.plan['plan_sha256'],'transaction':body['committed_transaction']}))
        for kind in CATALOG_KINDS:
            if kind in LOGICAL_OMITTED:continue
            for payload, in catalog.execute('SELECT payload FROM catalog_records WHERE kind=? ORDER BY key_type,key_integer,key_text',(kind,)):
                h.update(len(payload).to_bytes(8,'big'));h.update(payload);index.budget.check(bytes_=len(payload))
        counts={field:index.db.execute('SELECT count(*) FROM active_authoritative WHERE kind=?',(kind,)).fetchone()[0] for field,kind in COUNT_KINDS.items()}
        counts['frontier']=counts['products']-counts['expansions']
        require(counts==body['counts'],'catalog/source snapshot exact scoped counts differ')
        contexts=[]
        for name in legacy.CONTEXTS:
            initial=catalog.execute('SELECT initial_product FROM contexts WHERE name=?',(name,)).fetchone()
            require(initial is not None,'catalog initial context missing')
            products=catalog.execute('SELECT count(*) FROM products WHERE context=?',(name,)).fetchone()[0]
            expanded=catalog.execute('SELECT count(*) FROM products WHERE context=? AND expanded=1',(name,)).fetchone()[0]
            pending=catalog.execute("SELECT count(*) FROM frontier f JOIN products p ON p.id=f.product_id WHERE p.context=? AND f.status='pending'",(name,)).fetchone()[0]
            require(products==expanded+pending,'catalog context frontier differs')
            contexts.append({'context':name,'initial_product':initial[0],'reachable_products':products,'expanded_products':expanded,'frontier':pending})
        require(contexts==body['contexts'],'catalog seven context snapshots differ')
        h.update(encoded(counts));h.update(encoded(contexts))
        require(h.hexdigest()==body['catalog_logical_digest'],'catalog exact logical digest differs')
        with index.input_quantum('check_input') as q:
            cursor['phase']='done'
            index.db.execute("UPDATE input_work SET cursor=?,status='done',owner_result=? WHERE scope=? AND pass='catalog'",(encoded(cursor),q.sequence,index.scope))
            index.save_input_cursor('finish',status='ready');q.credit('input_catalog',index.scope);q.credit('input_ready',index.scope)
    finally:catalog.close()


def private_input_view(request):
    unit=request['work_unit'];output=Path(request['output_dir'])
    budget=Budget(request['resource_plan']['record'],output,seconds=request['deadline_monotonic']-time.monotonic())
    index=RecordIndex.readonly_view(Path(request['verifier_work_db']),source=unit['source'],
        session_id=unit['session_id'],budget=budget,resolver_path=request['dependency_index'])
    index.request=request
    row=index.db.execute('SELECT scope FROM input_scopes ORDER BY owner_result DESC LIMIT 1').fetchone()
    if row:index.bind_scope(row[0])
    return index


def validate_restored_verifier(request):
    validate_private_request(request,'validate_restore',private_restore=True)
    output=Path(request['output_dir'])
    require(output.is_absolute() and not output.is_symlink(),'owned private restore output')
    output.mkdir(parents=True,exist_ok=True)
    # Rebuild all excluded paths/input indexes separately. The staged private
    # database remains byte-identical from hash verification through install.
    index=private_input_view(request)
    path=Path(request['verifier_work_db'])
    require(path.is_absolute() and path.is_file() and not path.is_symlink(),'staged verifier backup path')
    before_sha,before_bytes=file_sha(path),path.stat().st_size
    backup_raw=Path(request['verifier_backup_root']).read_bytes()
    backup=strict_json(backup_raw)
    resolver=ContinuationResolver(request['dependency_index'],output,source=index.source,session_id=index.session_id,budget=index.budget)
    resolver.schema.check(backup)
    require(encoded(backup)==backup_raw and backup['kind']=='verifier_backup' and backup['source']==index.source and backup['session_id']==index.session_id,'private backup identity')
    checkpoint=request['verifier_checkpoint']
    require(backup['checkpoint_sha256']==digest(checkpoint) and backup['catalog_commit']==checkpoint['record']['committed_transaction'] and
            backup['catalog_logical_digest']==checkpoint['record']['catalog_logical_digest'] and backup['semantic_manifest_root']==checkpoint['record']['page_root'],'private backup/authoritative catalog binding')
    require(backup['work_db_sha256']==before_sha and backup['work_db_bytes']==before_bytes,'staged private backup file identity')
    state=ExecutionState.__new__(ExecutionState)
    state.index,state.budget,state.active=index,index.budget,False
    state.sql_path=Path(__file__).resolve().parent/'paged-verifier-state.sql'
    state.schema_sha256=file_sha(state.sql_path)
    state.db=index.db
    try:
        state.db.execute('PRAGMA query_only=ON')
        state.db.execute('PRAGMA temp_store=FILE')
        state.db.execute('PRAGMA cache_size=-2048')
        require(state.db.execute('PRAGMA integrity_check').fetchone()==('ok',),'restored private SQLite integrity')
        require(state.db.execute('PRAGMA foreign_key_check').fetchone() is None,'restored private SQLite FK violation')
        state._validate_schema()
        require(state.schema_sha256==backup['verifier_schema_sha256'] and state.sequence()[0]==backup['verifier_commit'] and state.logical_digest()==backup['verifier_logical_digest'],'restored private schema/commit/logical root')
        latest=resolver.reconcile_chain(state,latest_progress_sha256=backup['progress_sha256'],latest_backup_sha256=sha_bytes(backup_raw),tlc_reparser=reparse_retained_tlc)
        chain_hash=hashlib.sha256()
        chain_hash.update(b'[')
        first=True
        for unit,progress_path,backup_path in resolver.auth.execute('SELECT unit_ordinal,progress_path,backup_root_path FROM verifier_chain ORDER BY unit_ordinal'):
            if not first:
                chain_hash.update(b',')
            first=False
            chain_hash.update(encoded([unit,file_sha(progress_path),file_sha(backup_path)]))
        chain_hash.update(b']')
        require(file_sha(path)==before_sha and path.stat().st_size==before_bytes,'private restore validation mutated staged bytes')
        receipt={'schema_version':1,'operation':'validate_restored_verifier','source':index.source,'session_id':index.session_id,
            'unit_ordinal':backup['unit_ordinal'],'checkpoint_sha256':backup['checkpoint_sha256'],
            'resource_plan_sha256':backup['resource_plan_sha256'],'initial_image_sha256':backup['initial_image_sha256'],
            'verifier_schema_sha256':state.schema_sha256,'verifier_commit':state.sequence()[0],
            'verifier_logical_digest':state.logical_digest(),'work_db_sha256':before_sha,'work_db_bytes':before_bytes,
            'progress_sha256':backup['progress_sha256'],'result_chain_root_sha256':chain_hash.hexdigest(),
            'complete':True,'acceptance':False}
        write_json(output/'verifier-restore-validation.json',receipt)
        return receipt
    finally:
        resolver.close()
        index.close()



def complete_verification(request):
    """Reconcile the authenticated complete proof, never local completion flags."""
    validate_private_request(request,'completion_verify')
    output=Path(request['output_dir'])
    output.mkdir(parents=True,exist_ok=True)
    index=private_input_view(request)
    scope=index.db.execute('SELECT status,manifest_root,high_water,checkpoint FROM input_scopes WHERE scope=?',(index.scope,)).fetchone()
    current=request['checkpoint']['record']
    require(scope is not None and scope[:3]==('ready',current['page_root'],current['committed_transaction']) and
            strict_json(scope[3])['record']['counts']==current['counts'] and strict_json(scope[3])['record']['contexts']==current['contexts'],
            'completion before exact final manifest/highwater/count/context input certificate')
    resolver=ContinuationResolver(request['dependency_index'],output,source=index.source,session_id=index.session_id,budget=index.budget)
    state=ExecutionState.__new__(ExecutionState)
    state.index,state.budget,state.active=index,index.budget,False
    state.sql_path=Path(__file__).resolve().parent/'paged-verifier-state.sql'
    state.schema_sha256=file_sha(state.sql_path)
    path=Path(request['verifier_work_db'])
    require(path.is_file() and not path.is_symlink(),'completion private snapshot absent')
    state.db=index.db
    try:
        state.db.execute('PRAGMA query_only=ON')
        state.db.execute('PRAGMA temp_store=FILE')
        state.db.execute('PRAGMA cache_size=-2048')
        state._validate_schema()
        latest=resolver.reconcile_chain(state,latest_progress_sha256=request['previous_verifier_progress_sha256'],
            latest_backup_sha256=request['previous_verifier_backup_sha256'],tlc_reparser=reparse_retained_tlc)
        checkpoint=request['checkpoint']['record']
        require(checkpoint['source']==index.source and checkpoint['session_id']==index.session_id and
                checkpoint['initial_image_id']==request['initial_image']['record']['image_sha256'] and
                checkpoint['resource_plan_sha256']==request['resource_plan']['record']['plan_sha256'],'complete snapshot binding differs')
        counts=index.get('Counts','global')
        require(counts==index.counts()==checkpoint['counts'] and counts['frontier']==0,'complete exact global counts/frontier differs')
        mapping=strict_json((Path(__file__).resolve().parent/'paged-outcomes.json').read_bytes())
        contexts=[]
        for context in mapping['contexts']:
            root=index.get('ContextRoot',context)
            require(root['frontier']==0 and root['reachable_products']==root['expanded_products'] and
                    root in checkpoint['contexts'],'complete context closure/count binding differs')
            contexts.append(root)
        require(len(contexts)==7 and len(checkpoint['contexts'])==7,'all seven exact contexts required')
        require(state.db.execute("SELECT 1 FROM credit_links WHERE table_name='source_closure' AND row_key=?",(encoded(checkpoint['initial_image_id']),)).fetchone() is not None,'complete source closure has no authenticated result')
        require(state.db.execute('SELECT count(*) FROM checked_monitors').fetchone()[0]==index.count('MonitorNode') and
                state.db.execute('SELECT count(*) FROM checked_monitor_edges').fetchone()[0]==index.count('MonitorEdge'),'complete monitor history coverage differs')
        require(state.db.execute('SELECT count(*) FROM verified_sccs').fetchone()[0]==index.count('Scc'),'complete independent SCC coverage differs')
        from paged_scc import views,Temporal
        witness_replay=Temporal.__new__(Temporal)
        witness_replay.index,witness_replay.db,witness_replay.budget,witness_replay.execution=index,state.db,index.budget,None
        for context in mapping['contexts']:
            for _,prop,obligation in views(context):
                view=canonical([context,prop,obligation])
                require(state.db.execute('SELECT 1 FROM verified_views WHERE view=?',(view,)).fetchone() is not None,'complete fixed obligation view absent')
        def authenticated_blob(reference):
            row=(index.blob(reference['kind'],reference['id'])[1],index.blob(reference['kind'],reference['id'])[0])
            require(row is not None and row[0]==reference['sha256'] and file_sha(row[1])==reference['sha256'],'complete external evidence is missing or changed')
        def expect_invocation(tag):
            index.budget.check(records=1,bytes_=len(tag.encode())+64)
            resolver.db.execute('INSERT INTO completion_invocations(tag) VALUES(?)',(tag,))
        outcomes=[]
        for family in mapping['families']:
            outcome=index.get('Outcome',family['family'])
            expected={'pass':'pass','reachable_original_init_witness':'finite_witness',
                'expected_first_failure_with_source_valid_witness':'named_semantic_counterexample'}[family['expected_result']]
            require(outcome['expected']==outcome['actual']==expected and
                    outcome['invocation_count']==len(outcome['component_receipts']) and outcome['invocation_count']>0,'complete named outcome differs')
            resolver.db.execute('CREATE TABLE IF NOT EXISTS completion_invocations(tag TEXT PRIMARY KEY,seen INTEGER DEFAULT 0)')
            resolver.db.execute('DELETE FROM completion_invocations')
            if family['scope']=='sparse_boundary_control':
                expect_invocation(family['family'])
            else:
                for product, in state.db.execute('SELECT id FROM v_products WHERE context=? ORDER BY id',(family['context'],)):
                    expect_invocation(family['family']+'-one-step-'+str(product))
                for prop in family['source_descriptor'].get('properties',[]):
                    view='seen_goal_false' if prop=='BothSelectedCompletion' else 'capacity_ok_pending_true'
                    for component in index.find('Scc'):
                        if component['context']==family['context'] and component['property']==view:
                            expect_invocation(family['family']+'-component-'+str(component['id']))
            for receipt in outcome['component_receipts']:
                require(receipt['kind']=='tlc_receipt','outcome supplies a substituted component receipt')
                authenticated_blob(receipt)
                row=state.db.execute('SELECT invocation_key,receipt,tag FROM tlc_done WHERE relative_path=?',(receipt['id'],)).fetchone()
                require(row is not None and sha_bytes(row[1])==receipt['sha256'] and
                        state.db.execute("SELECT 1 FROM credit_links WHERE table_name='tlc_invocation' AND row_key=?",(encoded(row[0]),)).fetchone() is not None,'outcome lacks exact authenticated TLC execution')
                require(resolver.db.execute('UPDATE completion_invocations SET seen=1 WHERE tag=? AND seen=0',(row[2],)).rowcount==1,'duplicate/unrequired outcome invocation')
            require(resolver.db.execute('SELECT 1 FROM completion_invocations WHERE seen=0 LIMIT 1').fetchone() is None,'omitted exact original predecessor/component invocation')
            if outcome['witness'] is not None:
                witness=outcome['witness']
                if witness['kind']=='Counterexample':
                    value=index.get('Counterexample',witness['id'])
                    require(digest(envelope('Counterexample',value))==witness['sha256'],'complete counterexample digest differs')
                    require(value['family']==family['family'] and value['expected_first_failure']==family['expected_first_failure'],'complete counterexample named family differs')
                    witness_replay.replay_witness(value)
                else:
                    authenticated_blob(witness)
                    if witness['kind']=='causal_receipt' and family['scope']!='sparse_boundary_control':
                        file=index.blob(witness['kind'],witness['id'])[0]
                        replay_finite_witness(index,state.db,family['family'],strict_json(Path(file).read_bytes()))
            outcomes.append(outcome)
            index.budget.check()
        require(index.count('Outcome')==len(outcomes)==11,'complete eleven-outcome inventory differs')
        # This helper is fixed source, not request-selected code or an argv seam.
        repository=Path(__file__).resolve().parents[2]
        for name in ('validation/async_refinement_session.py','scripts/check-async-driver-refinement'):
            require(file_sha(repository/name)==request['source_files'].get(name),'backend receipt parser source is not registered')
        sys.path.insert(0,str(repository))
        from validation import async_refinement_session as backend_parser
        require(Path(backend_parser.__file__).resolve()==(repository/'validation/async_refinement_session.py').resolve(),'backend parser resolved outside fixed source closure')
        backend=backend_parser.verify_backend_qualification(request)
        receipt={'manifest_root':checkpoint['page_root'],'counts':counts,'contexts':contexts,
                 'outcomes':outcomes,'backend_causal_numeric_eligibility':backend,'complete':True}
        write_json(output/'complete-verification.json',receipt)
        return receipt
    finally:
        resolver.close()
        index.close()


def export_temporal(index, request, stage_complete):
    output=Path(request['output_dir'])
    kinds=('MonitorNode','MonitorEdge','Scc','Counterexample','Outcome','ContextRoot','Counts')
    if request['work_unit']['stage']=='temporal_components' and stage_complete:
        require(index.count('Outcome')==11 and index.count('Counts')==1,'final Counts must already be atomically sealed after all eleven outcomes')
    path=output/'temporal-import.ndjson'
    count=total=0
    exhausted=False
    limit=min(index.budget.member_limit(),index.budget.plan['page_bytes'])
    with path.open('wb') as stream:
        for kind in kinds:
            for row in index.db.execute('SELECT key,payload FROM records r WHERE r.kind=? AND NOT EXISTS (SELECT 1 FROM page_members p WHERE p.kind=r.kind AND p.key=r.key) ORDER BY number,text_key',(kind,)):
                if total+len(row[1])+1>limit or count>=index.budget.plan['unit_record_allowance']:
                    exhausted=True
                    break
                stream.write(row[1]+b'\n')
                total+=len(row[1])+1
                count+=1
            if exhausted:
                break
    if not count:
        path.unlink()
        return None
    final=(not exhausted and request['work_unit']['stage']=='temporal_components' and stage_complete and index.count('Outcome')==11)
    checkpoint=request['checkpoint']
    sidecar={'schema_version':6,'source':index.source,'session_id':index.session_id,
        'resource_plan_sha256':index.budget.plan['plan_sha256'],'input_checkpoint_sha256':digest(checkpoint),
        'input_catalog_commit':checkpoint['record']['committed_transaction'],
        'ndjson_sha256':file_sha(path),'ndjson_bytes':total,'record_count':count,'final':final,'stage_complete':stage_complete and not exhausted}
    write_json(output/'temporal-publication.json',sidecar)
    index.budget.check(bytes_=total)
    return sidecar


def member_role(path):
    name=Path(path).name
    if name=='temporal-import.ndjson':return 'temporal_import'
    if name in ('temporal-publication.json','temporal-external-roots.ndjson'):return 'temporal_publication'
    if name=='phase-result.json':return 'phase_result'
    if Path(path).parts[:1]==('runtime',) or name in ('runtime-external-roots.ndjson','resources.json'):return 'source_receipt'
    if name in ('SourceTables.tla','ComponentTables.tla','PagedTables.tla'):return 'source_tables'
    if name.endswith('.tla'):return 'module'
    if name.endswith('.cfg'):return 'config'
    if name.endswith('.log'):return 'log'
    if name=='id-map.json':return 'local_id_map'
    if 'witness' in name or 'counterexample' in name:return 'witness'
    return 'receipt'


def registered_staged_member(relative):
    """Only source-defined verifier outputs enter the late-export inventory."""
    parts=Path(relative).parts
    top={'phase-result.json','semantic-receipt.json','temporal-structure-receipt.json',
         'temporal-receipt.json','incomplete.json','temporal-import.ndjson','temporal-publication.json','temporal-external-roots.ndjson'}
    if len(parts)==2 and parts[0]=='runtime':
        number=Path(parts[1]).stem
        return parts[1].endswith('.json') and number.isascii() and number.isdecimal() and str(int(number))==number
    if len(parts)==1:
        return relative in ('runtime-external-roots.ndjson','resources.json') or relative in top or any(relative==family+suffix for family in legacy.CONFIGS for suffix in ('-outcome.json','-finite-witness.json'))
    if len(parts)!=3 or parts[0]!='temporal':return False
    tag,name=parts[1:]
    if tag in legacy.CONFIGS:
        family=tag
    elif '-one-step-' in tag or '-component-' in tag:
        separator='-one-step-' if '-one-step-' in tag else '-component-'
        family,number=tag.rsplit(separator,1)
        if family not in legacy.CONFIGS or not number.isascii() or not number.isdecimal() or str(int(number))!=number:return False
    else:return False
    return name in {'SourceTables.tla','ComponentTables.tla','PagedTables.tla','PagedOneStep.tla',
                    'PagedComponent.tla','PropertyMonitors.tla','AsyncDriverRefinement.tla','BoundaryControls.tla',
                    'id-map.json','paged-one-step.cfg','paged-component.cfg','fairness-replay.json','counterexample.json',
                    family+'.cfg',tag+'.tlc.log',tag+'.tlc-receipt.json'}


def prepare_unit(request):
    validate_private_request(request,'unit')
    require(request['work_unit']['stage']!='complete' or request['work_unit']['mode']=='unit','final preparation must run in ordinary unit mode')
    output=Path(request['output_dir'])
    output.mkdir(parents=True,exist_ok=True)
    path=Path(request['verifier_work_db'])
    if path.exists():
        require(request['previous_verifier_progress_sha256'] is not None,'unuploaded local verifier state has no continuation authority')
        auth=sqlite3.connect(Path(request['dependency_index']).as_uri()+'?mode=ro&immutable=1',uri=True)
        try:
            row=auth.execute('SELECT backup_root_path FROM verifier_chain ORDER BY unit_ordinal DESC LIMIT 1').fetchone()
            require(row is not None,'missing authenticated verifier snapshot chain')
            backup=strict_json(Path(row[0]).read_bytes())
            cp=auth.execute("SELECT path FROM objects WHERE kind='Checkpoint' AND CAST(key AS TEXT)=?",(canonical(backup['checkpoint_sha256']),)).fetchone()
            require(cp is not None,'latest verifier authoritative checkpoint missing')
            restore={**request,'operation':'validate_restore','verifier_checkpoint':strict_json(Path(cp[0]).read_bytes()),'verifier_backup_root':row[0]}
            validate_restored_verifier(restore)
        finally:
            auth.close()
    else:
        require(request['previous_verifier_progress_sha256'] is None and request['previous_verifier_backup_sha256'] is None,'missing required prior verifier DB')
    phase=run_unit(request)
    index=private_input_view(request)
    work=index.db
    # Transport cache reconstruction is separate; state rows remain the actual
    # trusted execution output from this current unit until final sealing.
    state=ExecutionState.__new__(ExecutionState)
    state.index,state.db,state.budget,state.active=index,work,index.budget,False
    state.sql_path=Path(__file__).resolve().parent/'paged-verifier-state.sql'
    state.schema_sha256=file_sha(state.sql_path)
    try:
        state._validate_schema()
        # Derived temporal records are carried in the workDB input/output cache;
        # enumerate them read-only and emit exact immutable envelopes for M1.
        index.execution=state
        preparation_only=state.cursor()['operation'] in ('load_input','check_input')
        require(request['work_unit']['stage']!='complete' or preparation_only,'final preparation cannot export or resume semantic work')
        publication=None if preparation_only else export_temporal(index,request,phase['stage_complete'])
        if publication is not None and not publication['stage_complete']:
            phase['stage_complete']=False
            phase['work_unit']['status']='checkpointed_incomplete'
            write_json(output/'phase-result.json',{'work_unit':phase['work_unit'],'stage_complete':False})
        work.commit()
        staged=[]
        excluded={'unit-result.json','prepare-unit.json','reservation.json','verifier-backup-descriptor.json',
            'verifier-restore-validation.json','restore-semantic.json'}
        for directory,subdirs,names in __import__('os').walk(output):
            subdirs[:]=[d for d in subdirs if d not in ('states','verifier-progress','verifier-results','verifier-evidence','verifier-backup')]
            for name in sorted(names):
                member=Path(directory)/name
                relative=str(member.relative_to(output))
                if name in excluded or name.endswith(('.sqlite','.sqlite-wal','.sqlite-shm','.partial')) or member==path:
                    continue
                if not registered_staged_member(relative):
                    continue
                require(not member.is_symlink(),'symlink staged verifier output')
                staged.append({'path':relative,'bytes':member.stat().st_size,'sha256':file_sha(member),'role':member_role(relative)})
                if len(encoded(staged))>8*1024*1024:
                    raise Incomplete('prepare staged member inventory exceeds metadata allocation')
        root_path=output/'temporal-external-roots.ndjson'
        staged=[m for m in staged if m['path']!=root_path.name]
        staged_by_path={m['path']:m for m in staged}
        root_bytes=0
        with root_path.open('wb') as roots:
            for kind,key,sha in work.execute("SELECT kind,key,digest FROM blobs WHERE kind IN ('tlc_receipt','causal_receipt','fairness_receipt') ORDER BY kind,key"):
                ident=strict_json(key)
                if type(ident) is not str or ident not in staged_by_path:continue
                member=staged_by_path[ident]
                require(member['sha256']==sha,'staged external proof evidence changed')
                line=encoded({'reference':{'kind':kind,'id':ident,'sha256':sha},'member_path':ident})+b'\n'
                root_bytes+=len(line)
                if root_bytes>8*1024*1024:raise Incomplete('external root inventory exceeds bounded metadata allocation')
                roots.write(line)
        if root_bytes:
            staged.append({'path':root_path.name,'bytes':root_bytes,'sha256':file_sha(root_path),'role':'temporal_publication'})
        else:root_path.unlink()
        staged.sort(key=lambda m:m['path'])
        first=work.execute('SELECT first_result FROM unit_work WHERE unit=?',(request['work_unit']['unit_ordinal'],)).fetchone()[0]
        last=state.sequence()[1]+1  # One final atomic sealing quantum.
        reservations=[]
        def metadata_cap(kind):
            require(kind in index.budget.allowances,'missing admitted verifier metadata allocation: '+kind)
            return min(8*1024*1024,index.budget.allowances[kind]['max_encoded_bytes'])
        for sequence in range(first,last+1):
            reservations.append({'path':'verifier-results/'+str(sequence)+'.json','maximum_bytes':metadata_cap('verifier_check_result'),'role':'result'})
            for purpose in ('inputs','outputs'):
                n=work.execute('SELECT count(*) FROM inventory_entries WHERE result_sequence=? AND purpose=?',(sequence,purpose)).fetchone()[0]
                # TLC generated members will be added after their actual part
                # assignment. Reserve their bounded current staged inventory.
                n+=len(staged)
                level=0
                while n:
                    nodes=(n+255)//256
                    for i in range(nodes):
                        reservations.append({'path':f'verifier-evidence/{sequence}-{purpose}-{level}-{i}.json','maximum_bytes':metadata_cap('verifier_evidence_index'),'role':'evidence_index'})
                    if nodes==1:break
                    n=nodes;level+=1
            if len(encoded(reservations))>8*1024*1024:
                raise Incomplete('result/evidence reservation inventory exceeds metadata allocation')
        n=last-first+1
        level=0
        while n:
            nodes=(n+255)//256
            for i in range(nodes):
                reservations.append({'path':f'verifier-evidence/unit-results-{level}-{i}.json','maximum_bytes':metadata_cap('verifier_evidence_index'),'role':'evidence_index'})
            if nodes==1:break
            n=nodes;level+=1
        unit_inputs=work.execute("SELECT count(*) FROM inventory_entries WHERE purpose='inputs' AND result_sequence>=?",(first,)).fetchone()[0]
        publication=output/'temporal-publication.json'
        if publication.is_file():unit_inputs+=strict_json(publication.read_bytes())['record_count']
        for relative, in work.execute("SELECT t.relative_path FROM tlc_done t JOIN credit_links c ON c.table_name='tlc_invocation' AND CAST(c.row_key AS TEXT)=json_quote(t.invocation_key) WHERE c.result_sequence>=?",(first,)):
            directory=str(Path(relative).parent)
            unit_inputs+=1+sum(str(Path(m['path']).parent)==directory and (m['path'].endswith(('.tla','.cfg')) or Path(m['path']).name=='id-map.json') for m in staged)
        level=0
        while unit_inputs:
            nodes=(unit_inputs+255)//256
            for i in range(nodes):
                reservations.append({'path':f'verifier-evidence/unit-inputs-{level}-{i}.json','maximum_bytes':metadata_cap('verifier_evidence_index'),'role':'evidence_index'})
            if nodes==1:break
            unit_inputs=nodes;level+=1
        reservations.append({'path':'verifier-progress/unit.json','maximum_bytes':metadata_cap('verifier_unit_progress'),'role':'progress'})
        if request['work_unit']['stage']=='complete':
            reservations.append({'path':'restore-semantic.json','maximum_bytes':min(8*1024*1024,index.budget.member_limit()),'role':'receipt'})
        prepare={'protocol':'verifier_prepare_v1','source':index.source,'session_id':index.session_id,
            'unit_ordinal':request['work_unit']['unit_ordinal'],'resource_plan_sha256':index.budget.plan['plan_sha256'],
            'initial_image_sha256':request['initial_image']['record']['image_sha256'],
            'input_checkpoint_sha256':digest(request['checkpoint']),'input_manifest_root':request['checkpoint']['record']['page_root'],
            'input_catalog_commit':request['checkpoint']['record']['committed_transaction'],
            'input_catalog_logical_digest':request['checkpoint']['record']['catalog_logical_digest'],
            'before_verifier_commit':state.sequence()[0],'before_logical_digest':state.logical_digest(),'before_cursor':state.cursor(),
            'staged_members':staged,'metadata_reservations':reservations}
        write_json(request['prepare_path'],prepare)
        return prepare
    finally:
        work.close()


def metadata_write(output, path, value, reservation):
    require(path in reservation,'unreserved future verifier metadata path')
    from paged_codec import Schema
    Schema(Path(__file__).resolve().parent/'paged-verifier-continuation.schema.json').check(value)
    data=encoded(value)
    r=reservation[path]
    if len(data)>min(8*1024*1024,r['maximum_bytes']):
        raise Incomplete('actual verifier metadata exceeds immutable reservation: '+path)
    destination=Path(output)/path
    require(destination.resolve().is_relative_to(Path(output).resolve()) and not destination.is_symlink(),'unsafe metadata publication path')
    write_json(destination,value)
    return {'unit_ordinal':value['unit_ordinal'],'part_ordinal':r['part_ordinal'],'member_path':path,
            'bytes':len(data),'sha256':sha_bytes(data)}


def build_evidence(entries,purpose,prefix,common,output,reservations,db):
    db.execute('DELETE FROM export_evidence WHERE scope=?',(prefix,))
    group=[]
    ordinal=first=node=0
    def emit(group,level,first,count,node):
        path=f'verifier-evidence/{prefix}-{level}-{node}.json'
        value={'schema_version':1,'kind':'verifier_evidence_index',**common,'purpose':purpose,
            'level':level,'first_ordinal':first,'item_count':count,'entries':group}
        member=metadata_write(output,path,value,reservations)
        item={'member':member,'level':level,'first_ordinal':first,'item_count':count}
        db.execute('INSERT INTO export_evidence VALUES(?,?,?,?)',(prefix,level,node,encoded(item)))
        return item
    for entry in entries:
        group.append(entry)
        ordinal+=1
        if len(group)==256:
            emit(group,0,first,len(group),node)
            first=ordinal;node+=1;group=[]
    if group:
        emit(group,0,first,len(group),node)
        node+=1
    if ordinal==0:
        return None
    level=0
    while node>1:
        next_count=0
        group=[]
        first=0
        items=0
        for payload, in db.execute('SELECT payload FROM export_evidence WHERE scope=? AND level=? ORDER BY ordinal',(prefix,level)):
            child=strict_json(payload)
            if not group:first=child['first_ordinal']
            group.append(child);items+=child['item_count']
            if len(group)==256:
                emit(group,level+1,first,items,next_count)
                next_count+=1;group=[];items=0
        if group:
            emit(group,level+1,first,items,next_count);next_count+=1
        node=next_count;level+=1
    return strict_json(db.execute('SELECT payload FROM export_evidence WHERE scope=? AND level=?',(prefix,level)).fetchone()[0])


def finalize_unit(request):
    validate_private_request(request,'finalize_unit')
    output=Path(request['output_dir'])
    require(Path(request['prepare_path']).stat().st_size<=8*1024*1024,'prepare descriptor exceeds bounded metadata limit')
    raw=Path(request['prepare_path']).read_bytes()
    prepare=strict_json(raw)
    require(encoded(prepare)==raw and set(prepare)=={'protocol','source','session_id','unit_ordinal','resource_plan_sha256',
        'initial_image_sha256','input_checkpoint_sha256','input_manifest_root','input_catalog_commit','input_catalog_logical_digest',
        'before_verifier_commit','before_logical_digest','before_cursor','staged_members','metadata_reservations'} and prepare['protocol']=='verifier_prepare_v1','closed prepare preimage differs')
    require(Path(request['reservation_path']).stat().st_size<=8*1024*1024,'reservation exceeds bounded metadata limit')
    reservation_raw=Path(request['reservation_path']).read_bytes()
    reservation=strict_json(reservation_raw)
    require(encoded(reservation)==reservation_raw and set(reservation)=={'protocol','prepare_sha256','source','session_id','unit_ordinal','resource_plan_sha256',
        'initial_image_sha256','input_checkpoint_sha256','final_checkpoint','members','metadata_reservations','fixed_prefix_part_count',
        'backup_first_part','upload_slot_count','maximum_backup_parts'} and reservation['protocol']=='verifier_reservation_v1','closed late reservation differs')
    require(reservation['prepare_sha256']==sha_bytes(raw) and reservation['upload_slot_count']==32 and
            reservation['fixed_prefix_part_count']<=reservation['backup_first_part'] and
            1<=reservation['backup_first_part']<32 and reservation['backup_first_part']+reservation['maximum_backup_parts']<=32,
            'late reservation token/slot boundary differs')
    for key in ('source','session_id','unit_ordinal','resource_plan_sha256','initial_image_sha256','input_checkpoint_sha256'):
        require(prepare[key]==reservation[key],'late reservation binding differs: '+key)
    require(reservation['final_checkpoint']==request['checkpoint'],'finalize authoritative checkpoint differs')
    cp=request['checkpoint']['record']
    preparation_only=prepare['before_cursor']['operation'] in ('load_input','check_input')
    if preparation_only:
        require(digest(request['checkpoint'])==prepare['input_checkpoint_sha256'] and
                cp['committed_transaction']==prepare['input_catalog_commit'] and cp['page_root']==prepare['input_manifest_root'],
                'preparation-only unit changed authoritative checkpoint')
    else:
        require(cp['previous_checkpoint']==prepare['input_checkpoint_sha256'] and cp['committed_transaction']>=prepare['input_catalog_commit'] and
                cp['source']==prepare['source'] and cp['session_id']==prepare['session_id'] and
                cp['resource_plan_sha256']==prepare['resource_plan_sha256'] and cp['initial_image_id']==prepare['initial_image_sha256'],
                'final checkpoint is not registered append-only successor')
    member_map={}
    for m in reservation['members']:
        require(set(m)=={'path','part_ordinal','bytes','sha256','role'} and m['role'] in PRIVATE_ROLES and
                type(m['part_ordinal']) is int and 0<=m['part_ordinal']<reservation['fixed_prefix_part_count'] and m['path'] not in member_map,'invalid/duplicate fixed-prefix member reservation')
        path=output/m['path']
        from paged_codec import safe_path
        safe_path(output,m['path'])
        require(path.stat().st_size==m['bytes'] and file_sha(path)==m['sha256'],'reserved actual member bytes changed')
        member_map[m['path']]=m
    for m in prepare['staged_members']:
        require(m['path'] in member_map and {k:member_map[m['path']][k] for k in ('path','bytes','sha256','role')}==m,'prepared member changed/renumbered at late reservation')
    metadata={}
    for m in reservation['metadata_reservations']:
        require(set(m)=={'path','part_ordinal','maximum_bytes','role'} and (m['role'] in ('result','evidence_index','progress') or m['role']=='receipt' and m['path']=='restore-semantic.json' and request['work_unit']['stage']=='complete' and request['work_unit']['mode']=='unit' or m['role']=='source_receipt' and m['path'] in ('restore-semantic-execution.json','restore-semantic.log') and request['work_unit']['stage']=='complete' and request['work_unit']['mode']=='unit') and
                m['path'] not in metadata and 0<=m['part_ordinal']<reservation['fixed_prefix_part_count'] and 0<m['maximum_bytes']<=8*1024*1024,'invalid bounded future metadata reservation')
        metadata[m['path']]=m
    for m in prepare['metadata_reservations']:
        require(m['path'] in metadata and all(metadata[m['path']][k]==m[k] for k in ('path','maximum_bytes','role')),'future metadata reservation changed')
    index=private_input_view(request)
    # Validate only this unit's bounded actual publication. Historical raw
    # records remain the authenticated read-only private input view.
    published=RecordIndex(output/'publication-index.sqlite',source=index.source,session_id=index.session_id,budget=index.budget)
    try:
        for m in reservation['members']:
            if m['role'] in ('catalog_page','catalog_manifest'):
                require(not preparation_only,'preparation-only unit published catalog pages')
                published.load_member(output/m['path'])
        if not preparation_only:
            current=published.db.execute("SELECT payload FROM records WHERE kind='ManifestNode' AND key=?",(canonical(cp['page_root']),)).fetchone()
            prior=index.db.execute("SELECT payload FROM records WHERE kind='ManifestNode' AND key=?",(canonical(cp['page_root']),)).fetchone()
            if current is not None and prior is not None:require(current==prior,'immutable prior manifest full-byte conflict')
            require(current is not None or cp['page_root']==prepare['input_manifest_root'] and prior is not None,
                    'current publication lacks actual final manifest or exact authenticated unchanged root')
    except BaseException:
        published.close();index.close();raise
    for m in metadata.values():
        if m['path']=='restore-semantic.json':
            require(m['maximum_bytes']<=min(8*1024*1024,index.budget.member_limit()),'readonly readiness receipt exceeds fixed metadata reservation')
            continue
        if m['path'] in ('restore-semantic-execution.json','restore-semantic.log'):
            require(m['maximum_bytes']<=8*1024*1024,'controller readonly execution evidence exceeds fixed metadata ceiling')
            continue
        kind={'result':'verifier_check_result','evidence_index':'verifier_evidence_index','progress':'verifier_unit_progress'}[m['role']]
        require(kind in index.budget.allowances and m['maximum_bytes']<=index.budget.allowances[kind]['max_encoded_bytes'],'reserved metadata exceeds admitted operational byte allocation')
    state_db=sqlite3.connect(Path(request['verifier_work_db']))
    state=ExecutionState.__new__(ExecutionState)
    state.index,state.db,state.budget,state.active=index,state_db,index.budget,False
    state.sql_path=Path(__file__).resolve().parent/'paged-verifier-state.sql';state.schema_sha256=file_sha(state.sql_path)
    try:
        state._validate_schema()
        require((state.sequence()[0],state.logical_digest(),state.cursor())==
                (prepare['before_verifier_commit'],prepare['before_logical_digest'],prepare['before_cursor']),'private work state changed after prepare')
        # No source relation changes are permitted after independent closure.
        source=state_db.execute("SELECT state FROM operation_state WHERE operation='source_closure_binding'").fetchone()
        if source:
            original=strict_json(source[0])['record']
            for field in ('physical_rows','episodes','stores','oracle_edges','products','candidates','demands','product_edges','fair_pairs','enabledness','expansions','boundaries'):
                require(cp['counts'][field]==original['counts'][field],'temporal publication changed frozen source graph')
            require(cp['contexts']==original['contexts'],'temporal publication changed seven closed contexts')
        publication=[]
        import_path=output/'temporal-import.ndjson'
        if import_path.is_file():
            with import_path.open('rb') as stream:
                for line in stream:
                    require(len(line)<=index.budget.member_limit(),'indivisible temporal publication record')
                    value=strict_json(line)
                    from paged_codec import record_key
                    actual=published.get_envelope(value['kind'],record_key(value))
                    require(encoded(actual)==encoded(value),'M1 did not publish byte-identical proposed temporal record')
                    publication.append({'entry_kind':'record','record_kind':value['kind'],'key':record_key(value),'sha256':digest(value)})
                    if len(encoded(publication))>index.budget.member_limit():raise Incomplete('publication input inventory exceeds resident budget')
        unit=request['work_unit'];ordinal=unit['unit_ordinal']
        row=state_db.execute('SELECT before_commit,before_digest,before_cursor,first_result,input_snapshot,origin FROM unit_work WHERE unit=?',(ordinal,)).fetchone()
        require(row is not None and sha_bytes(encoded(strict_json(row[4])))==prepare['input_checkpoint_sha256'],'private unit input checkpoint changed')
        original_input=strict_json(row[4])['record']
        if preparation_only:
            require(request['checkpoint']==strict_json(row[4]),'preparation-only checkpoint differs from actual unit input')
        else:
            require(prepare['unit_ordinal']==ordinal==cp['unit_ordinal'] and cp['unit_ordinal']>original_input['unit_ordinal'] and
                    cp['sequence']==original_input['sequence']+1,'final checkpoint actual attempted-unit/sequence successor differs')

        final_sequence=state.sequence()[1]+1
        before_cursor=state.cursor();after_cursor={**before_cursor,'quantum':before_cursor['quantum']+1,
            'operation':'publish_temporal' if publication else before_cursor['operation']}
        common={'source':index.source,'session_id':index.session_id,'unit_ordinal':ordinal,'checkpoint_sha256':digest(request['checkpoint'])}
        phase=strict_json((output/'phase-result.json').read_bytes())
        require(not preparation_only or not phase['stage_complete'] and not publication,'partial preparation claimed semantic completion/publication')
        require(set(phase)=={'work_unit','stage_complete'} and phase['work_unit']['progress']==before_cursor['position'],'phase after-position changed before seal')
        # Seal only after every fixed part assignment and authoritative import
        # has been validated. Failure rolls back result/credit finalization.
        state_db.execute('BEGIN IMMEDIATE')
        try:
            coverage={'record_kind':None,'lower_exclusive':None,'upper_inclusive':None,'count':len(publication),'scope':before_cursor['scope']}
            claim='temporal_publication_checked' if publication else 'algorithm_checkpoint'
            state_db.execute('INSERT INTO check_results VALUES(?,?,?,?,?,?,?,?,NULL,NULL)',(final_sequence,claim,encoded(coverage),'',encoded(before_cursor),encoded(after_cursor),after_cursor['quantum'],1))
            for i,entry in enumerate(publication):state_db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)',(final_sequence,'inputs',i,encoded(entry)))
            for i,prepared in enumerate(prepare['staged_members']):
                m=member_map[prepared['path']]
                member={'unit_ordinal':ordinal,'part_ordinal':m['part_ordinal'],'member_path':m['path'],'bytes':m['bytes'],'sha256':m['sha256']}
                state_db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)',(final_sequence,'outputs',i,encoded({'entry_kind':'member','member':member})))
            for sequence in range(row[3],final_sequence+1):
                local=state_db.execute('SELECT claim,coverage,before_cursor,after_cursor,first_quantum,quantum_count FROM check_results WHERE sequence=?',(sequence,)).fetchone()
                require(local is not None,'omitted private journal result')
                tlc=None
                if local[0]=='tlc_completed':
                    owned=state_db.execute("SELECT row_key FROM credit_links WHERE table_name='tlc_invocation' AND result_sequence=?",(sequence,)).fetchone()
                    require(owned is not None,'TLC result lacks exact invocation credit')
                    invocation=strict_json(owned[0])
                    retained=state_db.execute('SELECT receipt,relative_path FROM tlc_done WHERE invocation_key=?',(invocation,)).fetchone()
                    require(retained is not None,'TLC credit lacks retained receipt')
                    receipt=strict_json(retained[0]);directory=str(Path(retained[1]).parent)
                    for path,m in sorted(member_map.items()):
                        if str(Path(path).parent)==directory and (path.endswith(('.tla','.cfg')) or Path(path).name=='id-map.json'):
                            member={'unit_ordinal':ordinal,'part_ordinal':m['part_ordinal'],'member_path':path,'bytes':m['bytes'],'sha256':m['sha256']}
                            entry=encoded({'entry_kind':'member','member':member})
                            n=state_db.execute("SELECT count(*) FROM inventory_entries WHERE result_sequence=? AND purpose='inputs'",(sequence,)).fetchone()[0]
                            state_db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)',(sequence,'inputs',n,entry))
                    def bound_member(path):
                        require(path in member_map,'TLC retained member was not reserved')
                        m=member_map[path]
                        return {'unit_ordinal':ordinal,'part_ordinal':m['part_ordinal'],'member_path':path,'bytes':m['bytes'],'sha256':m['sha256']}
                    runtime_member=bound_member('runtime/'+str(ordinal)+'.json')
                    n=state_db.execute("SELECT count(*) FROM inventory_entries WHERE result_sequence=? AND purpose='inputs'",(sequence,)).fetchone()[0]
                    state_db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)',(sequence,'inputs',n,encoded({'entry_kind':'member','member':runtime_member})))
                    receipt_member=bound_member(retained[1]);log_member=bound_member(str(Path(retained[1]).with_name(Path(retained[1]).name.replace('.tlc-receipt.json','.tlc.log'))))
                    tlc={k:receipt[k] for k in ('invocation_key','family','module','module_closure_root','config_sha256','generated_inputs_root','local_id_map_sha256','java_sha256','jar_sha256','argv_sha256','heap_bytes','workers','maximum_seconds','exit_code','first_failure','complete')}
                    tlc.update(receipt_member=receipt_member,log_member=log_member)
                    for i,member in enumerate((receipt_member,log_member)):
                        state_db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)',(sequence,'outputs',i,encoded({'entry_kind':'member','member':member})))
                    require(parse_tlc_log(output/log_member['member_path'],tlc['exit_code'])['log_sha256']==log_member['sha256'],'current actual TLC log changed before seal')
                roots={}
                h=hashlib.sha256(b'plurx-verifier-input-inventory-v1\0')
                for purpose in ('inputs','outputs'):
                    def entries(purpose=purpose):
                        for data, in state_db.execute('SELECT entry FROM inventory_entries WHERE result_sequence=? AND purpose=? ORDER BY ordinal',(sequence,purpose)):
                            if purpose=='inputs':h.update(len(data).to_bytes(8,'big'));h.update(data)
                            yield strict_json(data)
                    roots[purpose]=build_evidence(entries(),purpose,str(sequence)+'-'+purpose,common,output,metadata,state_db)
                result={'schema_version':1,'kind':'verifier_check_result',**common,'result_sequence':sequence,
                    'resource_plan_sha256':prepare['resource_plan_sha256'],'initial_image_sha256':prepare['initial_image_sha256'],
                    'input_root':h.hexdigest(),'before_cursor':strict_json(local[2]),'after_cursor':strict_json(local[3]),
                    'first_quantum':local[4],'quantum_count':local[5],'claim':local[0],'coverage':strict_json(local[1]),
                    'input_inventory_root':roots['inputs'],'output_inventory_root':roots['outputs'],'tlc':tlc}
                member=metadata_write(output,'verifier-results/'+str(sequence)+'.json',result,metadata)
                state_db.execute('UPDATE check_results SET input_root=?,record=?,published_unit=? WHERE sequence=?',(h.hexdigest(),encoded(result),ordinal,sequence))
                state_db.execute('UPDATE execution_commits SET input_digest=? WHERE first_result=?',(h.hexdigest(),sequence))
                state_db.execute('INSERT INTO export_result_members VALUES(?,?,?)',(ordinal,sequence,encoded(member)))
            unit_input_root=build_evidence((strict_json(entry) for entry, in state_db.execute("SELECT entry FROM inventory_entries WHERE purpose='inputs' AND result_sequence>=? AND result_sequence<=? ORDER BY result_sequence,ordinal",(row[3],final_sequence))),
                'inputs','unit-inputs',common,output,metadata,state_db)
            private_commit=state.sequence()[0]+1
            root=state_db.execute('SELECT input_root FROM check_results WHERE sequence=?',(final_sequence,)).fetchone()[0]
            state_db.execute('INSERT INTO execution_commits VALUES(?,?,?,?,?,?,1,?)',(private_commit,after_cursor['operation'],encoded(before_cursor),encoded(after_cursor),root,final_sequence,int(bool(publication))))
            state_db.execute('UPDATE verifier_meta SET private_commit=?,last_result=?,cursor=? WHERE singleton=1',(private_commit,final_sequence,encoded(after_cursor)))
            state_db.execute('UPDATE unit_work SET finalized=1 WHERE unit=?',(ordinal,))
            result_root=build_evidence(({'entry_kind':'result','result_sequence':seq,'member':strict_json(member)} for seq,member in state_db.execute('SELECT sequence,member FROM export_result_members WHERE unit=? ORDER BY sequence',(ordinal,))),
                'results','unit-results',common,output,metadata,state_db)
            useful=state_db.execute('SELECT coalesce(sum(useful),0) FROM execution_commits WHERE sequence>? AND sequence<=?',(row[0],private_commit)).fetchone()[0]
            if useful==0:raise Incomplete('resource_blocked: no useful committed verifier quantum; refuse zero-progress publication')
            progress={'schema_version':1,'kind':'verifier_unit_progress',**common,
                'resource_plan_sha256':prepare['resource_plan_sha256'],'initial_image_sha256':prepare['initial_image_sha256'],
                'previous_progress_sha256':request['previous_verifier_progress_sha256'],'previous_backup_manifest_sha256':request['previous_verifier_backup_sha256'],
                'origin':strict_json(row[5]),'input_checkpoint_sha256':prepare['input_checkpoint_sha256'],'input_catalog_commit':prepare['input_catalog_commit'],
                'input_catalog_logical_digest':prepare['input_catalog_logical_digest'],'input_manifest_root':prepare['input_manifest_root'],
                'input_inline_counts_sha256':sha_bytes(encoded(strict_json(row[4])['record']['counts'])),
                'input_contexts_sha256':sha_bytes(encoded(strict_json(row[4])['record']['contexts'])),
                'source_closure_checkpoint_sha256':state_db.execute('SELECT source_closure_checkpoint FROM verifier_meta WHERE singleton=1').fetchone()[0],
                'input_inventory_root':unit_input_root,'verifier_schema_sha256':state.schema_sha256,'verifier_algorithm_version':1,
                'before_verifier_commit':row[0],'after_verifier_commit':state.sequence()[0],
                'before_logical_digest':row[1],'after_logical_digest':state.logical_digest(),'before_cursor':strict_json(row[2]),'after_cursor':state.cursor(),
                'result_index_root':result_root,'first_result_sequence':row[3],'result_count':final_sequence-row[3]+1,'committed_quantum_count':useful,
                'disposition':'closed_unqualified' if phase['stage_complete'] else 'checkpointed_incomplete','stage_complete':phase['stage_complete'],'acceptance':False}
            progress_member=metadata_write(output,'verifier-progress/unit.json',progress,metadata)
            state.prospective_commit()
            state_db.commit()
        except BaseException:
            state_db.rollback();raise
        state.budget.check()
        backup_path=output/'verifier-backup.sqlite'
        if backup_path.exists():raise Invalid('refuse overwrite existing finalized verifier backup')
        descriptor=state.backup(backup_path)
        descriptor.update(source=index.source,session_id=index.session_id,unit_ordinal=ordinal,checkpoint_sha256=digest(request['checkpoint']),
            previous_checkpoint_sha256=request['work_unit']['expected_prior_checkpoint'],resource_plan_sha256=prepare['resource_plan_sha256'],initial_image_sha256=prepare['initial_image_sha256'],
            catalog_commit=cp['committed_transaction'],catalog_logical_digest=cp['catalog_logical_digest'],semantic_manifest_root=cp['page_root'],
            previous_verifier_progress_sha256=request['previous_verifier_progress_sha256'],progress_sha256=progress_member['sha256'],
            snapshot_boundary='post_verifier_commit_pre_upload',format='sqlite3',backup_method='sqlite_backup_api_v1',
            backup_first_part=reservation['backup_first_part'],maximum_backup_parts=reservation['maximum_backup_parts'])
        write_json(output/'verifier-backup-descriptor.json',descriptor)
        unit['progress']=state.cursor()['position'];unit['status']=progress['disposition']
        result={'schema_version':6,'work_unit':unit,'checkpoint':request['checkpoint'],
            'artifacts':['verifier-progress/unit.json','verifier-backup-descriptor.json'],'stage_complete':phase['stage_complete']}
        write_json(output/'unit-result.json',result)
        return result
    finally:
        state_db.close();published.close();index.close()

if __name__ == '__main__':
    try:
        sys.exit(main())
    except Incomplete as error:
        print('incomplete paged proof: resource_blocked: '+str(error),file=sys.stderr)
        sys.exit(3)
    except (Invalid, legacy.Invalid) as error:
        print('invalid paged proof: ' + str(error), file=sys.stderr)
        sys.exit(2)
