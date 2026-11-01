"""Unrun temporal continuation protocol fixtures, pending full delta review.

TLC and source graphs are synthetic here. Actual log parsing, exact invocation
identity, member authentication, result-chain reconciliation and SQLite backup
are exercised; these fixtures grant no selected-operation proof acceptance.
"""
from __future__ import annotations

from collections import Counter
import copy
import hashlib
from pathlib import Path
import shutil
import sqlite3
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_paged_temporal as fixture
import paged_checker as checker
import paged_codec as codec


class PagedTemporalContinuationFixtures(unittest.TestCase):
    def setUp(self):
        base = fixture.PACKAGE.parents[1] / '.async-proof-sessions' / 'paged-fixtures'
        base.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=base)
        self.root = Path(self.directory.name)
        self.runtime = {'java': {'sha256': 'a' * 64}, 'tlc': {'sha256': 'b' * 64}}
        self.source = {**fixture.SOURCE, 'toolchain_sha256': codec.digest(self.runtime)}
        self.plan = copy.deepcopy(fixture.admitted(self.root).plan)
        self.plan.update(source=self.source, unit_predecessor_allowance=2, unit_record_allowance=25)
        if self._testMethodName.startswith('test_aggregate_'):
            self.plan.update(resident_bytes=16 * 1024 * 1024, unit_predecessor_allowance=100000,
                             unit_record_allowance=100000)
        self.plan['plan_sha256'] = codec.digest(codec.envelope('ResourcePlan', self.plan))
        self.index = codec.RecordIndex(self.root / 'work.sqlite', source=self.source,
            session_id=fixture.SESSION, budget=fixture.granted_budget(self.plan, self.root))
        self.state = codec.ExecutionState(self.index, '6' * 64)
        self.index.execution = self.state
        self.mapping = codec.strict_json((fixture.PACKAGE / 'paged-outcomes.json').read_bytes())
        self.contexts = [{'context': context, 'initial_product': n * 3 + 1,
            'reachable_products': 3, 'expanded_products': 3, 'frontier': 0}
            for n, context in enumerate(self.mapping['contexts'])]
        self.checkpoint = codec.envelope('Checkpoint', {'session_id': fixture.SESSION, 'source': self.source,
            'sequence': 0, 'initial_image_id': '6' * 64, 'resource_plan_sha256': self.plan['plan_sha256'],
            'unit_ordinal': 0, 'previous_checkpoint': None, 'committed_transaction': 1,
            'oldest_pending_ticket': None, 'counts': {**{kind: 0 for kind in codec.COUNT_KINDS}, 'frontier': 0},
            'contexts': self.contexts, 'page_root': '7' * 64, 'catalog_logical_digest': '8' * 64})
        self.auth = self.root / 'authenticated.sqlite'
        with sqlite3.connect(self.auth) as db:
            db.execute('CREATE TABLE authenticated_members(unit_ordinal INTEGER,member_path TEXT,bytes INTEGER,sha256 TEXT,local_path TEXT,PRIMARY KEY(unit_ordinal,member_path))')
            db.execute('CREATE TABLE verifier_chain(unit_ordinal INTEGER PRIMARY KEY,progress_path TEXT,backup_root_path TEXT,work_unit_json BLOB,execution_origin_json BLOB)')
        self.previous_progress = self.previous_backup = self.previous_manifest = None
        self.before = self.state.sequence()[0], self.state.logical_digest(), self.state.cursor()
        self.first = 1
        self.calls = []
        self.generated = []
        self.references = []
        self.restores = 0
        self.unit = 0
        self.interrupted_component = False
        self.seed_source_fixture()
        self.semantic = SimpleNamespace(checkpoint={'contexts': self.contexts})
        self.temporal = SimpleNamespace(fair_walk=self.fair_walk, replay_witness=lambda witness: None)
        self.tlc = {'java': '/synthetic/java', 'jar': '/synthetic/tla2tools.jar',
                    'java_sha256': 'a' * 64, 'jar_sha256': 'b' * 64}
        self.next_unit()

    def tearDown(self):
        self.index.close()
        self.directory.cleanup()

    def seed_source_fixture(self):
        # A legal source-pinned private journal supplies fixture input readiness.
        # It is intentionally a micrograph, not production semantic acceptance.
        scope = codec.sha_bytes(codec.encoded([self.source, fixture.SESSION, self.plan['plan_sha256'],
            '6' * 64, codec.digest(self.checkpoint), []]))
        resume = {'stage': 'semantic_verify', 'operation': 'materialize_store',
                  'scope': codec.empty_scope(), 'position': codec.empty_position(), 'quantum': 0}
        with self.state.quantum('load_input'):
            self.index.db.execute('INSERT INTO unit_work VALUES(1,?,?,?,?,?,?,?,0)',
                (codec.encoded(None), codec.digest(self.checkpoint), self.before[0], self.before[1],
                 codec.encoded(self.before[2]), 1, codec.encoded(self.checkpoint)))
            self.index.db.execute('INSERT INTO input_scopes VALUES(?,?,?,?,?,?,?,?,?,?,?)',
                (scope, codec.encoded(self.checkpoint), codec.digest(self.checkpoint), 1, '7' * 64,
                 codec.encoded([]), None, 'ready', codec.encoded(resume), codec.encoded({}), 1))
        self.index.scope = scope
        with self.state.quantum('check_input', scope={**codec.empty_scope(), 'input_root': scope}) as quantum:
            certificate = {'checkpoint': codec.digest(self.checkpoint), 'catalog_sha256': '8' * 64,
                'bytes': 1, 'kind': 0, 'after': ['', '', ''], 'phase': 'done'}
            self.index.db.execute('INSERT INTO input_work VALUES(?,?,?,?,?,?,?,?,?)',
                (scope, 'catalog', '', '', '', '', 'done', codec.encoded(certificate), 2))
            quantum.credit('input_catalog', scope)
            quantum.credit('input_ready', scope)
        with self.state.quantum('materialize_store', stage=resume['stage'], scope=resume['scope'], position=resume['position']):
            for n, context in enumerate(self.mapping['contexts']):
                for product in range(n * 3 + 1, n * 3 + 4):
                    self.index.db.execute('INSERT INTO v_products(id,context) VALUES(?,?)', (product, context))
                self.index.db.execute('INSERT INTO v_roots VALUES(?,?)', (context, n * 3 + 1))
                for view in ('seen_goal_false', 'capacity_ok_pending_true'):
                    identity = n * 2 + (1 if view == 'seen_goal_false' else 2)
                    component = {'id': identity, 'context': context, 'property': view, 'monitor_root': 1,
                        'members': [1], 'forward_tree': [], 'reverse_tree': [], 'condensation_rank': 0,
                        'outgoing_dependencies': [], 'obligation': None if view == 'seen_goal_false' else
                            {'node': 0, 'job': 0, 'claim_id': fixture.SESSION}}
                    self.index.propose(codec.envelope('Scc', component), 'materialize_store')
        self.index.scope = ''
        for operation in ('materialize_oracle', 'materialize_product', 'seed_fifo', 'check_expansion',
                          'source_reachability', 'source_finish', 'stage_finish'):
            with self.state.quantum(operation):
                if operation == 'source_finish':
                    self.index.db.execute("INSERT INTO operation_state VALUES('source_closure_binding',?,?)",
                        (codec.encoded(None), codec.encoded(self.checkpoint)))

    def next_unit(self):
        self.unit += 1
        self.output = self.root / ('unit-' + str(self.unit))
        self.output.mkdir()
        self.index.budget = self.state.budget = fixture.granted_budget(self.plan, self.output)
        self.index.request = {'dependency_index': str(self.auth), 'output_dir': str(self.output)}
        self.index.resolver_path = self.auth
        self.index.input_snapshot = self.checkpoint
        self.work_unit = {'session_id': fixture.SESSION, 'unit_ordinal': self.unit,
            'expected_prior_checkpoint': None, 'source': self.source, 'pr': 1, 'repository': 1,
            'event_base': '1' * 40, 'workflow_id': 'async-proof-session.yml', 'event_name': 'workflow_dispatch',
            'mode': 'unit', 'run': self.unit, 'job': self.unit, 'task': self.unit, 'attempt': 1,
            'resource_plan_sha256': self.plan['plan_sha256'], 'runner_identity_utf8': '{}',
            'status': 'checkpointed_incomplete', 'artifact_roots': [], 'stage': 'temporal_components',
            'registered_cases': [], 'progress': self.state.cursor()['position']}
        # The authenticated Origin of the execution that produced this unit: the WorkUnit's identity plus the
        # operation and dispatch ordinal the controller reserved for it (the WorkUnit no longer carries them).
        self.origin = {**{key: self.work_unit[key] for key in ('repository', 'pr', 'event_base', 'workflow_id', 'event_name',
                                                                'mode', 'run', 'job', 'task', 'attempt')},
                       'operation': 'execute', 'dispatch_ordinal': self.unit - 1}
        runtime = {'schema_version': 1, 'source': self.source, 'session_id': fixture.SESSION,
            'unit_ordinal': self.unit, 'origin': {k: self.work_unit[k] for k in ('run', 'job', 'task', 'attempt')},
            'resource_plan_sha256': self.plan['plan_sha256'], 'runtime': self.runtime, 'runner': {}}
        checker.write_json(self.output / 'runtime' / (str(self.unit) + '.json'), runtime)

    def local_tables(self, semantic, identity):
        self.generated.append((self.unit, identity))
        return iter(['---- MODULE SourceTables ----\n====\n']), {'owned': identity}, 0

    def component_tables(self, index, temporal, component):
        return iter(['---- MODULE ComponentTables ----\n====\n']), {'component': component['id']}

    def invoke(self, index, directory, module, config, tlc, *, tag):
        identity, key = checker.tlc_identity(index, directory, module, config, tlc, tag)
        self.calls.append((self.unit, tag))
        descriptor = checker.legacy.CONFIGS[identity['family']]
        if module == 'PagedOneStep':
            failure = 'PageSafety' if descriptor['expected'] in descriptor['invariants'] else None
        elif module == 'BoundaryControls':
            failure = descriptor['expected']
        else:
            failure = 'EventuallyDischarged' if descriptor['expected'] in descriptor.get('properties', []) else None
        log = directory / (tag + '.tlc.log')
        code = (13 if module == 'PagedComponent' else 12) if failure else 0
        text = ('Error: Temporal properties were violated.\nState 1:\nStuttering\n' if code == 13 else
                'Error: Invariant ' + failure + ' is violated.\nState 1:\n' if failure else
                'Model checking completed. No error has been found.\n')
        log.write_text(text + '2 states generated, 2 distinct states found, 0 states left on queue\n')
        receipt = {'schema_version': 6, 'source': self.source, 'session_id': fixture.SESSION,
            'module_sha256': checker.file_sha(directory / (module + '.tla')),
            'source_tables_sha256': checker.file_sha(directory / ('ComponentTables.tla' if module == 'PagedComponent' else 'SourceTables.tla')),
            **identity, 'invocation_key': key, **checker.parse_tlc_log(log, code), 'acceptance': False}
        checker.write_json(directory / (tag + '.tlc-receipt.json'), receipt)
        return receipt

    def fair_walk(self, component):
        if not self.interrupted_component:
            self.interrupted_component = True
            raise codec.Incomplete('fixture: restore between component invocation and aggregation')
        state = codec.strict_json(self.index.db.execute("SELECT state FROM operation_state WHERE operation='temporal_qualification'").fetchone()[0])
        family = self.mapping['families'][state['family']]
        return ([], [], 1) if family['expected_first_failure'] in family['source_descriptor'].get('properties', []) else None

    def boundary(self, index, semantic, output, tlc, family, run_once):
        name = family['family']
        directory = output / 'temporal' / name
        directory.mkdir(parents=True)
        (directory / 'SourceTables.tla').write_text('---- MODULE SourceTables ----\n====\n')
        checker.write_json(directory / 'id-map.json', {'boundary': name})
        shutil.copyfile(fixture.PACKAGE / 'BoundaryControls.tla', directory / 'BoundaryControls.tla')
        config = directory / family['source_config']
        config.write_text(checker.legacy.config(name))
        run_once(name, 'BoundaryControls', directory, config)
        return checker.temporal_receipt_ref(index, name)

    def qualify(self):
        original_reference = checker.temporal_receipt_ref
        def reference(index, tag):
            self.references.append(tag)
            return original_reference(index, tag)
        with patch.object(checker, 'temporal_receipt_ref', side_effect=reference), \
             patch.object(checker, 'local_tables', side_effect=self.local_tables), \
             patch.object(checker, 'component_tables', side_effect=self.component_tables), \
             patch.object(checker, 'invoke_tlc', side_effect=self.invoke), \
             patch.object(checker, 'qualify_boundary', side_effect=self.boundary), \
             patch.object(checker, 'finite_witness', return_value={'fixture_only': True}):
            return checker.qualify_temporal(self.index, self.semantic, self.temporal, self.output, self.tlc)

    def member(self, relative, value=None):
        path = self.output / relative
        if value is not None:
            checker.write_json(path, value)
        data = path.read_bytes()
        member = {'unit_ordinal': self.unit, 'member_path': relative,
                  'bytes': len(data), 'sha256': codec.sha_bytes(data)}
        with sqlite3.connect(self.auth) as db:
            db.execute('INSERT OR REPLACE INTO authenticated_members VALUES(?,?,?,?,?)',
                (self.unit, relative, len(data), member['sha256'], str(path)))
        return member

    def evidence(self, entries, purpose, name, common):
        if not entries:
            return None
        self.assertLessEqual(len(entries), 256)
        value = {'schema_version': 2, 'kind': 'verifier_evidence_index', **common, 'purpose': purpose,
            'level': 0, 'first_ordinal': 0, 'item_count': len(entries), 'entries': entries}
        codec.Schema(fixture.PACKAGE / 'paged-verifier-continuation.schema.json').check(value)
        member = self.member('verifier-evidence/' + name + '.json', value)
        return {'member': member, 'level': 0, 'first_ordinal': 0, 'item_count': len(entries)}

    def authenticated_restore(self):
        # Seal a producer-shaped micrograph unit and authenticate its exact
        # member/result chain. No parser, identity or reconciliation is mocked.
        self.index.budget = self.state.budget = fixture.granted_budget(self.plan, self.output)
        common = {'source': self.source, 'session_id': fixture.SESSION, 'unit_ordinal': self.unit,
                  'checkpoint_sha256': codec.digest(self.checkpoint)}
        results, unit_inputs = [], []
        last = self.state.sequence()[1]
        for sequence in range(self.first, last + 1):
            claim, coverage, before, after, first_quantum, quantum_count = self.index.db.execute(
                'SELECT claim,coverage,before_cursor,after_cursor,first_quantum,quantum_count FROM check_results WHERE sequence=?', (sequence,)).fetchone()
            tlc = None
            if claim == 'tlc_completed':
                owned = self.index.db.execute("SELECT row_key FROM credit_links WHERE table_name='tlc_invocation' AND result_sequence=?", (sequence,)).fetchone()[0]
                raw, relative = self.index.db.execute('SELECT receipt,relative_path FROM tlc_done WHERE invocation_key=?', (codec.strict_json(owned),)).fetchone()
                receipt = codec.strict_json(raw)
                directory = Path(relative).parent
                inputs = [self.member(str(path.relative_to(self.output))) for path in sorted((self.output / directory).iterdir())
                    if path.suffix in ('.tla', '.cfg') or path.name == 'id-map.json']
                inputs.append(self.member('runtime/' + str(self.unit) + '.json'))
                for ordinal, member in enumerate(inputs):
                    self.index.db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)',
                        (sequence, 'inputs', ordinal, codec.encoded({'entry_kind': 'member', 'member': member})))
                receipt_member = self.member(relative)
                log_member = self.member(relative.replace('.tlc-receipt.json', '.tlc.log'))
                for ordinal, member in enumerate((receipt_member, log_member)):
                    self.index.db.execute('INSERT INTO inventory_entries VALUES(?,?,?,?)',
                        (sequence, 'outputs', ordinal, codec.encoded({'entry_kind': 'member', 'member': member})))
                fields = ('invocation_key', 'family', 'module', 'module_closure_root', 'config_sha256',
                    'generated_inputs_root', 'local_id_map_sha256', 'java_sha256', 'jar_sha256', 'argv_sha256',
                    'heap_bytes', 'workers', 'maximum_seconds', 'exit_code', 'first_failure', 'complete')
                tlc = {key: receipt[key] for key in fields}
                tlc.update(receipt_member=receipt_member, log_member=log_member)
            entries = {purpose: [codec.strict_json(row[0]) for row in self.index.db.execute(
                'SELECT entry FROM inventory_entries WHERE result_sequence=? AND purpose=? ORDER BY ordinal', (sequence, purpose))]
                for purpose in ('inputs', 'outputs')}
            h = hashlib.sha256(b'plurx-verifier-input-inventory-v1\0')
            for entry in entries['inputs']:
                raw = codec.encoded(entry); h.update(len(raw).to_bytes(8, 'big')); h.update(raw)
            unit_inputs.extend(entries['inputs'])
            result = {'schema_version': 2, 'kind': 'verifier_check_result', **common,
                'result_sequence': sequence, 'resource_plan_sha256': self.plan['plan_sha256'], 'initial_image_sha256': '6' * 64,
                'input_root': h.hexdigest(), 'before_cursor': codec.strict_json(before), 'after_cursor': codec.strict_json(after),
                'first_quantum': first_quantum, 'quantum_count': quantum_count, 'claim': claim, 'coverage': codec.strict_json(coverage),
                'input_inventory_root': self.evidence(entries['inputs'], 'inputs', str(sequence) + '-in', common),
                'output_inventory_root': self.evidence(entries['outputs'], 'outputs', str(sequence) + '-out', common), 'tlc': tlc}
            codec.Schema(fixture.PACKAGE / 'paged-verifier-continuation.schema.json').check(result)
            results.append({'entry_kind': 'result', 'result_sequence': sequence,
                            'member': self.member('verifier-results/' + str(sequence) + '.json', result)})
            self.index.db.execute('UPDATE check_results SET input_root=?,record=?,published_unit=? WHERE sequence=?',
                (h.hexdigest(), codec.encoded(result), self.unit, sequence))
            self.index.db.execute('UPDATE execution_commits SET input_digest=? WHERE first_result=?', (h.hexdigest(), sequence))
        self.index.db.commit()
        result_root = self.evidence(results, 'results', 'results', common)
        input_root = self.evidence(unit_inputs, 'inputs', 'unit-inputs', common)
        useful = self.index.db.execute('SELECT sum(useful) FROM execution_commits WHERE sequence>?', (self.before[0],)).fetchone()[0]
        self.assertGreater(useful, 0)
        progress = {'schema_version': 2, 'kind': 'verifier_unit_progress', **common,
            'resource_plan_sha256': self.plan['plan_sha256'], 'initial_image_sha256': '6' * 64,
            'previous_progress_sha256': self.previous_progress, 'previous_backup_manifest_sha256': self.previous_backup,
            'origin': self.origin,
            'input_checkpoint_sha256': common['checkpoint_sha256'], 'input_catalog_commit': 1,
            'input_catalog_logical_digest': '8' * 64, 'input_manifest_root': '7' * 64,
            'input_inline_counts_sha256': codec.digest(self.checkpoint['record']['counts']),
            'input_contexts_sha256': codec.digest(self.contexts), 'source_closure_checkpoint_sha256': None,
            'input_inventory_root': input_root, 'verifier_schema_sha256': self.state.schema_sha256,
            'verifier_algorithm_version': 1, 'before_verifier_commit': self.before[0],
            'after_verifier_commit': self.state.sequence()[0], 'before_logical_digest': self.before[1],
            'after_logical_digest': self.state.logical_digest(), 'before_cursor': self.before[2],
            'after_cursor': self.state.cursor(), 'result_index_root': result_root, 'first_result_sequence': self.first,
            'result_count': last - self.first + 1, 'committed_quantum_count': useful,
            'disposition': 'checkpointed_incomplete', 'stage_complete': False, 'acceptance': False}
        codec.Schema(fixture.PACKAGE / 'paged-verifier-continuation.schema.json').check(progress)
        progress_member = self.member('verifier-progress/unit.json', progress)
        backup_path = self.output / 'work-backup.sqlite'
        descriptor = self.state.backup(backup_path)
        self.assertEqual(checker.file_sha(backup_path), descriptor['work_db_sha256'])
        # The backup root's own fields and its chain binding are checked here; the content-addressed chunk
        # manifest it names is covered by the snapshot transport fixtures, so the manifest reference is synthetic.
        manifest_root = {'kind': 'snapshot_manifest_node', 'sha256': codec.sha_bytes(b'fixture manifest ' + str(self.unit).encode()),
                         'bytes': 1}
        chunk_limit = 1048576
        backup = {'schema_version': 2, 'kind': 'verifier_backup', **common,
            'previous_checkpoint_sha256': None, 'resource_plan_sha256': self.plan['plan_sha256'],
            'initial_image_sha256': '6' * 64, 'catalog_commit': 1, 'catalog_logical_digest': '8' * 64,
            'semantic_manifest_root': '7' * 64, **{key: value for key, value in descriptor.items() if key != 'path'},
            'previous_verifier_progress_sha256': self.previous_progress, 'progress_sha256': progress_member['sha256'],
            'snapshot_boundary': 'post_verifier_commit_pre_upload', 'format': 'sqlite3', 'backup_method': 'sqlite_backup_api_v1',
            'chunk_bytes_limit': chunk_limit, 'chunk_count': max(1, -(-descriptor['work_db_bytes'] // chunk_limit)),
            'execution_origin': self.origin, 'predecessor_root': self.previous_manifest, 'manifest_root': manifest_root}
        codec.Schema(fixture.PACKAGE / 'paged-verifier-continuation.schema.json').check(backup)
        backup_member = self.member('verifier-backup/root.json', backup)
        with sqlite3.connect(self.auth) as db:
            db.execute('INSERT INTO verifier_chain VALUES(?,?,?,?,?)', (self.unit,
                str(self.output / 'verifier-progress/unit.json'), str(self.output / 'verifier-backup/root.json'),
                codec.encoded(codec.envelope('WorkUnit', self.work_unit)), codec.encoded(self.origin)))
        self.index.close()
        restored = self.root / ('restored-' + str(self.unit) + '.sqlite')
        shutil.copyfile(backup_path, restored)
        self.assertEqual(checker.file_sha(restored), descriptor['work_db_sha256'])
        self.index = codec.RecordIndex(restored, source=self.source, session_id=fixture.SESSION,
            budget=fixture.granted_budget(self.plan, self.output))
        self.state = codec.ExecutionState(self.index, '6' * 64)
        self.index.execution = self.state
        resolver = codec.ContinuationResolver(self.auth, codec.auxiliary_directory(self.output, create=True), source=self.source,
            session_id=fixture.SESSION, budget=self.index.budget)
        try:
            resolver.reconcile_chain(self.state, latest_progress_sha256=progress_member['sha256'],
                latest_backup_sha256=backup_member['sha256'], tlc_reparser=checker.reparse_retained_tlc)
        finally:
            resolver.close()
        self.previous_progress, self.previous_backup = progress_member['sha256'], backup_member['sha256']
        self.previous_manifest = manifest_root
        self.before = self.state.sequence()[0], self.state.logical_digest(), self.state.cursor()
        self.first = self.state.sequence()[1] + 1
        self.restores += 1

    def test_temporal_prefix_crosses_authenticated_restores_and_family_boundaries(self):
        phases = set()
        for _ in range(80):
            start = len(self.calls)
            try:
                self.qualify()
                complete = True
            except codec.Incomplete:
                complete = False
            current_calls = self.calls[start:]
            self.assertLessEqual(len(current_calls), self.plan['unit_predecessor_allowance'])
            emitted = list((self.output / 'temporal').glob('*/*.tlc-receipt.json'))
            self.assertEqual(len(emitted), len(current_calls), 'completed-prefix receipt was recopied')
            state = codec.strict_json(self.index.db.execute("SELECT state FROM operation_state WHERE operation='temporal_qualification'").fetchone()[0])
            phases.add((state['phase'], state['family']))
            if complete:
                self.authenticated_restore()
                break
            self.authenticated_restore()
            self.next_unit()
        else:
            self.fail('temporal cursor failed to make finite progress')
        self.assertGreaterEqual(self.restores, 2)
        self.assertTrue(any(phase == 'component_walk' for phase, _ in phases))
        self.assertGreater(len({family for _, family in phases}), 2)
        self.assertEqual(self.index.count('Outcome'), 11)
        expected = []
        for family in self.mapping['families']:
            name = family['family']
            if family['scope'] == 'sparse_boundary_control':
                tags = [name]
            else:
                tags = [name + '-one-step-' + str(product) for product, in self.index.db.execute(
                    'SELECT id FROM v_products WHERE context=?', (family['context'],))]
                for prop in family['source_descriptor'].get('properties', []):
                    view = 'seen_goal_false' if prop == 'BothSelectedCompletion' else 'capacity_ok_pending_true'
                    tags += [name + '-component-' + str(component['id']) for component in self.index.find('Scc')
                        if component['context'] == family['context'] and component['property'] == view]
            expected.extend(tags)
            outcome = self.index.get('Outcome', name)
            actual = [Path(reference['id']).name.removesuffix('.tlc-receipt.json') for reference in outcome['component_receipts']]
            self.assertEqual(Counter(actual), Counter(tags))
            self.assertEqual(outcome['invocation_count'], len(tags))
        self.assertGreater(len(expected), self.plan['unit_predecessor_allowance'])
        self.assertEqual(Counter(tag for _, tag in self.calls), Counter(expected))
        self.assertTrue(all(count == 1 for count in Counter(tag for _, tag in self.calls).values()))
        self.assertEqual(len(self.generated), sum('-one-step-' in tag for _, tag in self.calls))
        self.assertEqual(Counter(tag for tag in self.references if '-one-step-' in tag),
                         Counter(tag for tag in expected if '-one-step-' in tag),
                         'aggregation restarted a completed receipt prefix')

    def test_same_tag_changed_inputs_rejected_after_authenticated_restore(self):
        with self.assertRaises(codec.Incomplete):
            self.qualify()
        first_unit, tag = self.calls[0]
        directory = self.root / ('unit-' + str(first_unit)) / 'temporal' / tag
        self.authenticated_restore()
        self.next_unit()
        for name in ('paged-one-step.cfg', 'SourceTables.tla', 'id-map.json'):
            path = directory / name
            original = path.read_bytes()
            try:
                path.write_bytes(original + b'\nchanged exact input\n')
                with self.subTest(member=name), self.assertRaisesRegex(codec.Invalid, 'same TLC tag'):
                    checker.run_temporal_tlc(self.index, self.output, self.tlc, tag, 'PagedOneStep', directory,
                        directory / 'paged-one-step.cfg', completed=lambda receipt: self.fail('changed input acquired cursor credit'))
            finally:
                path.write_bytes(original)
        with patch.object(checker, 'invoke_tlc', side_effect=AssertionError('retained invocation reran')):
            checker.run_temporal_tlc(self.index, self.output, self.tlc, tag, 'PagedOneStep', directory,
                directory / 'paged-one-step.cfg', completed=lambda receipt: None)
        self.assertFalse((self.output / 'temporal' / tag).exists(), 'retained reuse copied old evidence')

    def test_invocation_and_temporal_cursor_roll_back_together(self):
        with self.assertRaises(codec.Incomplete):
            self.qualify()
        # Reset only the unit budget; retain its exact committed private prefix.
        self.index.budget = self.state.budget = fixture.granted_budget(self.plan, self.output)
        before = self.state.sequence(), self.state.cursor(), self.state.logical_digest()
        count = self.index.db.execute('SELECT count(*) FROM tlc_done').fetchone()[0]
        original_save = checker.TemporalQualification.save
        def fail_active_cursor(cursor):
            if cursor.state['after_product'] == 3:
                raise codec.Incomplete('fixture: interrupt after actual invocation, before cursor commit')
            original_save(cursor)
        with patch.object(checker.TemporalQualification, 'save', fail_active_cursor), self.assertRaises(codec.Incomplete):
            self.qualify()
        self.assertEqual(self.index.db.execute('SELECT count(*) FROM tlc_done').fetchone()[0], count)
        self.assertEqual((self.state.sequence(), self.state.cursor(), self.state.logical_digest()), before)

    def test_aggregate_boundary_reserves_before_growth_and_restores_same_plan(self):
        expected = []
        interrupted = False
        boundary = False
        encoded_checks = []
        failed_reservations = []
        cleanup = next(i for i, family in enumerate(self.mapping['families']) if family['family'] == 'cleanup')
        original_encoded = checker.encoded
        for batch in range(80):
            with checker.TemporalQualification(self.index, self.mapping) as cursor:
                if batch == 0:
                    cursor.advance(phase='collect', family=cleanup)
                self.assertEqual(cursor.state['receipts'], expected)
                self.assertGreaterEqual(self.index.budget.working_bytes, codec.decoded_json_allocation(*cursor.footprint()),
                    'loaded aggregate lost its live decoded reservation')
                original_reserve = self.index.budget.reserve_working
                def reserve(amount):
                    before_count = len(cursor.state['receipts'])
                    try:
                        return original_reserve(amount)
                    except codec.Incomplete:
                        failed_reservations.append((amount, before_count))
                        raise
                def encode(value):
                    if value is cursor.state:
                        shape = cursor.footprint()
                        required = codec.decoded_json_allocation(*shape) + cursor.encoding_charge(shape)
                        self.assertGreaterEqual(self.index.budget.working_bytes, required,
                            'whole-state serialization allocated before its temporary-copy reservation')
                        encoded_checks.append(shape)
                    return original_encoded(value)
                for _ in range(16):
                    number = len(expected) + 1
                    tag = 'cleanup-one-step-' + str(number)
                    directory = self.output / 'temporal' / tag
                    directory.mkdir(parents=True, exist_ok=True)
                    for module in ('AsyncDriverRefinement', 'PagedOneStep'):
                        shutil.copyfile(fixture.PACKAGE / (module + '.tla'), directory / (module + '.tla'))
                    for module in ('SourceTables', 'PagedTables'):
                        (directory / (module + '.tla')).write_text('---- MODULE ' + module + ' ----\n====\n')
                    checker.write_json(directory / 'id-map.json', {'owned': number})
                    config = directory / 'paged-one-step.cfg'
                    config.write_text('SPECIFICATION PageSpec\n')
                    before = (self.state.sequence(), self.state.cursor(), self.state.logical_digest(),
                        self.index.db.execute("SELECT state FROM operation_state WHERE operation='temporal_qualification'").fetchone()[0],
                        self.index.db.execute("SELECT count(*) FROM credit_links WHERE table_name='tlc_invocation'").fetchone()[0])
                    before_shape, before_charge = cursor.receipt_shape(), cursor.charged
                    def completed(receipt):
                        cursor.state['receipt_after'] = tag
                        cursor.append(checker.temporal_receipt_ref(self.index, tag))
                        cursor.save()
                        if number == 3 and not interrupted:
                            raise codec.Incomplete('fixture: interrupt after guarded append/save before invocation commit')
                    try:
                        with patch.object(self.index.budget, 'reserve_working', side_effect=reserve), \
                             patch.object(checker, 'encoded', side_effect=encode), \
                             patch.object(checker, 'invoke_tlc', side_effect=self.invoke):
                            checker.run_temporal_tlc(self.index, self.output, self.tlc, tag, 'PagedOneStep',
                                directory, config, completed=completed, cursor=cursor)
                    except codec.Incomplete as error:
                        after = (self.state.sequence(), self.state.cursor(), self.state.logical_digest(),
                            self.index.db.execute("SELECT state FROM operation_state WHERE operation='temporal_qualification'").fetchone()[0],
                            self.index.db.execute("SELECT count(*) FROM credit_links WHERE table_name='tlc_invocation'").fetchone()[0])
                        self.assertEqual(after, before, 'failed aggregate quantum changed saved cursor/credit')
                        self.assertEqual(cursor.state['receipts'], expected, 'rollback retained an uncommitted live append')
                        self.assertEqual(cursor.receipt_shape(), before_shape)
                        self.assertEqual(cursor.charged, before_charge)
                        if 'fixture: interrupt' in str(error):
                            interrupted = True
                            continue
                        self.assertIn('prospective', str(error))
                        self.assertTrue(failed_reservations)
                        self.assertEqual(failed_reservations[-1][1], len(expected), 'unsafe append occurred before admission failed')
                        self.assertGreaterEqual(failed_reservations[-1][0], cursor.encoding_charge(cursor.footprint()),
                            'prospective request omitted temporary serialized copies')
                        # The old encoded-only cap would still admit this state;
                        # prospective live/temporary accounting must stop it first.
                        self.assertLess(len(before[3]), self.index.budget.member_limit())
                        boundary = True
                        break
                    expected.append(checker.temporal_receipt_ref(self.index, tag))
                saved = self.index.db.execute("SELECT state FROM operation_state WHERE operation='temporal_qualification'").fetchone()[0]
            self.assertEqual(self.index.budget.working_bytes, 0, 'aggregate ownership leaked after close')
            same_plan = fixture.granted_budget(copy.deepcopy(self.plan), self.output)
            self.assertEqual(codec.bounded_json(saved, same_plan)['receipts'], expected,
                'committed aggregate cannot pass the unchanged bounded_json restore policy')
            self.authenticated_restore()
            self.next_unit()
            if boundary:
                with checker.TemporalQualification(self.index, self.mapping) as restored:
                    self.assertEqual(restored.state['receipts'], expected)
                    self.assertGreaterEqual(self.index.budget.working_bytes,
                        codec.decoded_json_allocation(*restored.footprint()))
                break
        else:
            self.fail('producer-shaped references never reached the admitted resident boundary')
        self.assertTrue(interrupted)
        self.assertTrue(boundary)
        self.assertGreaterEqual(self.restores, 2)
        self.assertGreater(len(expected), 16)
        self.assertTrue(encoded_checks, 'test never observed guarded whole-state serialization')
        self.assertEqual(len({reference['id'] for reference in expected}), len(expected))
        self.assertEqual(self.index.db.execute('SELECT count(*) FROM tlc_done').fetchone()[0], len(expected))

    def test_aggregate_footprint_matches_exact_decoder_token_policy(self):
        # Escaping, Unicode, punctuation inside strings and scalar/container
        # charges must agree with the real decoder rather than a 32x estimate.
        import io
        values = [None, True, False, -9223372036854775808, 9223372036854775807,
            {'quoted': '"\\\n\t\u0000é🧪,:[]{}', 'nested': [[], {}, {'empty': ''}, 0]}]
        for value in values:
            raw = codec.encoded(value)
            shape = codec.canonical_json_footprint(value)
            self.assertEqual(shape[0], len(raw))
            self.assertEqual(codec.decoded_json_allocation(*shape),
                codec.decoded_stream_reservation(io.BytesIO(raw), len(raw), self.index.budget))


if __name__ == '__main__':
    unittest.main()
