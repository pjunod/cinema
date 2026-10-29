import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('affected_core_replay', Path(__file__).with_name('affected_core_replay.py'))
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)


class ReplayTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='affected-core-tests-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source'
        self.campaign = self.root / 'campaign'
        self.expanded = self.campaign / 'expanded-1'
        self.expanded.mkdir(parents=True)
        self.metadata = {'workspace_root': str(self.source), 'packages': [
            {'id': name, 'name': name, 'manifest_path': str(self.source / 'crates' / name / 'Cargo.toml')}
            for name in ('plurx-core', 'plurxd')]}
        (self.campaign / 'test-exit-status.txt').write_text('0\n')
        self.before = self.artifacts(False)
        self.after = self.artifacts(True)
        self.write_messages(self.campaign / 'artifacts.jsonl', self.before)
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        for directory, name in ((self.campaign / 'objects-default', 'default-core'),
                                (self.expanded / 'objects', 'augmented-core')):
            directory.mkdir()
            binary = self.root / name
            binary.write_bytes(b'\x7fELF' + name.encode())
            receipt = {'source_sha256': 'a' * 64, 'targets': [{
                'package': 'plurx-core', 'target': 'plurx_core', 'kind': ['lib'],
                'test_profile': True, 'path': str(binary)}], 'objects': [{
                    'path': str(binary), 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}]}
            (directory / 'objects.json').write_text(json.dumps(receipt))
        self.default = {'cluster::old', 'store::hiqlite::same', 'store::hiqlite_sessions::same',
                        'sharing::unchanged', 'root::ignored'}
        self.augmented = self.default | {'unrelated::new', 'other::new_ignored'}

    def artifacts(self, augmented):
        result = []
        for name in ('plurx-core', 'plurxd', 'hiqlite', 'unchanged-dependency'):
            manifest = self.source / ('vendor' if name == 'hiqlite' else 'crates') / name / 'Cargo.toml'
            features = ['base'] + (['sqlite'] if name == 'hiqlite' else [])
            if augmented:
                features += sorted(replay.EXPECTED_ADDITIONS.get(name, set()))
            result.append({'reason': 'compiler-artifact', 'package_id': name,
                           'manifest_path': str(manifest), 'features': features})
        return result + [{'reason': 'build-finished', 'success': True}]

    def write_messages(self, path, values):
        path.write_text(''.join(json.dumps(value) + '\n' for value in values))

    def audit(self):
        return replay.audit_feature_delta(self.campaign / 'artifacts.jsonl',
                                         self.expanded / 'artifacts.jsonl', self.metadata)

    def listed(self, binary, cwd, env, ignored=False):
        if ignored:
            return {'root::ignored'} if binary.name == 'default-core' else {'root::ignored', 'other::new_ignored'}
        return self.default if binary.name == 'default-core' else self.augmented

    def prepare(self):
        with patch.object(replay, 'list_core', side_effect=self.listed):
            return replay.prepare_selection(self.campaign, self.expanded, self.metadata, {})

    def test_exact_feature_delta(self):
        self.assertEqual({row['package'] for row in self.audit()}, set(replay.EXPECTED_ADDITIONS))

    def test_unexpected_dependency_feature_refuses(self):
        self.after[3]['features'].append('unexpected')
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        with self.assertRaisesRegex(ValueError, 'unreviewed feature'):
            self.audit()

    def test_missing_feature_refuses(self):
        self.after[0]['features'].remove('cluster-validation')
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        with self.assertRaisesRegex(ValueError, 'unreviewed feature'):
            self.audit()

    def test_package_graph_change_refuses(self):
        del self.after[3]
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        with self.assertRaisesRegex(ValueError, 'package graph'):
            self.audit()

    def test_failed_build_refuses(self):
        self.after[-1]['success'] = False
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        with self.assertRaisesRegex(ValueError, 'build failed'):
            self.audit()

    def test_mixed_profile_features_are_preserved(self):
        for records in (self.before, self.after):
            extra = dict(records[3], features=['alternative-profile'])
            records.insert(-1, extra)
        self.write_messages(self.campaign / 'artifacts.jsonl', self.before)
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        self.audit()
        self.after[-2]['features'].append('unreviewed-profile-change')
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        with self.assertRaisesRegex(ValueError, 'unreviewed feature'):
            self.audit()

    def test_feature_redistribution_between_profiles_refuses(self):
        for records in (self.before, self.after):
            records[3]['target'] = {'name': 'dependency', 'kind': ['lib']}
            records[3]['profile'] = {'test': False}
            records.insert(-1, dict(records[3], features=['alternative-profile'], profile={'test': True}))
        self.after[3]['features'], self.after[-2]['features'] = self.after[-2]['features'], self.after[3]['features']
        self.write_messages(self.campaign / 'artifacts.jsonl', self.before)
        self.write_messages(self.expanded / 'artifacts.jsonl', self.after)
        with self.assertRaisesRegex(ValueError, 'target/profile'):
            self.audit()

    def test_default_already_augmented_refuses(self):
        self.before[0]['features'].append('cluster-validation')
        self.write_messages(self.campaign / 'artifacts.jsonl', self.before)
        with self.assertRaisesRegex(ValueError, 'already contains'):
            self.audit()

    def test_selection_and_receipt_preserve_ignored_status(self):
        result = self.prepare()
        self.assertEqual(set(result['selected_names']), {
            'cluster::old', 'store::hiqlite::same', 'store::hiqlite_sessions::same',
            'unrelated::new', 'other::new_ignored'})
        self.assertEqual(result['selected_ignored_names'], ['other::new_ignored'])
        self.assertEqual(result['omitted_unaffected_names'], ['root::ignored', 'sharing::unchanged'])
        self.assertEqual(json.loads((self.expanded / 'core-replay-selection.json').read_text()), result)

    def source_review(self):
        from source_scope import input_digest
        self.source.mkdir()
        for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'):
            (self.source / name).write_text('reviewed input')
        schema = self.source / 'crates/plurx-core/src/schema.sql'
        schema.parent.mkdir(parents=True)
        schema.write_text('CREATE TABLE reviewed(id INTEGER);')
        review = self.root / 'reviewed-scope.json'
        review.write_text(json.dumps(input_digest(self.source)))
        return review, schema

    def test_reviewed_source_keeps_affected_selection(self):
        review, _ = self.source_review()
        with patch.object(replay, 'list_core', side_effect=self.listed):
            result = replay.prepare_selection(self.campaign, self.expanded, self.metadata, {}, review)
        self.assertTrue(result['source_scope_audit']['matches'])
        self.assertEqual(result['prefixes'], list(replay.PREFIXES))
        self.assertIn('sharing::unchanged', result['omitted_unaffected_names'])

    def test_sql_source_drift_forces_full_replay_without_prefix_claim(self):
        review, schema = self.source_review()
        schema.write_text('CREATE TABLE changed(id INTEGER);')
        with patch.object(replay, 'list_core', side_effect=self.listed):
            result = replay.prepare_selection(self.campaign, self.expanded, self.metadata, {}, review)
        self.assertFalse(result['source_scope_audit']['matches'])
        self.assertEqual(set(result['selected_names']), self.augmented)
        self.assertEqual(result['omitted_unaffected_names'], [])
        self.assertEqual(result['prefixes'], [])
        self.assertEqual(result['execution'], 'full')
        self.assertTrue(result['scope'].startswith('full augmented core run'))

    def test_failed_default_refuses(self):
        (self.campaign / 'test-exit-status.txt').write_text('1\n')
        with self.assertRaisesRegex(ValueError, 'full default suite'):
            self.prepare()

    def test_source_mismatch_refuses(self):
        path = self.expanded / 'objects/objects.json'
        receipt = json.loads(path.read_text()); receipt['source_sha256'] = 'b' * 64
        path.write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, 'identical frozen source'):
            self.prepare()

    def test_changed_binary_refuses(self):
        (self.root / 'default-core').write_bytes(b'overwritten')
        with self.assertRaisesRegex(ValueError, 'changed after Cargo'):
            self.prepare()

    def test_removed_default_test_refuses(self):
        self.augmented.remove('sharing::unchanged')
        with self.assertRaisesRegex(ValueError, 'removed default'):
            self.prepare()

    def test_existing_ignore_change_refuses(self):
        original = self.listed
        def changed(binary, cwd, env, ignored=False):
            if ignored and binary.name == 'augmented-core':
                return {'other::new_ignored'}
            return original(binary, cwd, env, ignored)
        self.listed = changed
        with self.assertRaisesRegex(ValueError, 'changed ignored status'):
            self.prepare()

    def test_empty_inventory_refuses(self):
        with self.assertRaisesRegex(ValueError, 'empty'):
            replay.choose_names(set(), self.augmented)

    def test_lists_require_unique_names_and_summary(self):
        self.assertEqual(replay.listed_names('a: test\nb: test\n\n2 tests, 0 benchmarks\n'), {'a', 'b'})
        self.assertEqual(replay.listed_names('0 tests, 0 benchmarks\n'), set())
        for output in ('a: test\na: test\n2 tests, 0 benchmarks\n', 'a: test\n'):
            with self.assertRaises(ValueError):
                replay.listed_names(output)


if __name__ == '__main__':
    unittest.main()
