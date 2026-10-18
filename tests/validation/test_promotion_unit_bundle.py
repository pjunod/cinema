"""Bundle composition cannot broaden a child's authenticated unit authority."""
import base64
import copy
import json
import unittest
from unittest.mock import patch
from validation import promotion_unit_bundle as bundle
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation.unit_evidence_objects import Objects, StructuralBudget


def blob(value):
    raw = json.dumps(value).encode()
    sha = retention.digest(raw)
    return sha, {'sha256': sha, 'base64': base64.b64encode(raw).decode()}


class PromotionBundleCase(unittest.TestCase):
    def fixture(self):
        header = {'repository': 1, 'pr': 1045, 'branch': 'effort/test',
                  'base_sha': 'a' * 40, 'source_commit': 'b' * 40}
        children, descriptors = [], []
        for number, lane in enumerate(('rust', 'apple'), 2):
            sha, item = blob({'name': lane})
            child = {**header, 'version': 2, 'windows_waiver': None,
                     'lanes': {lane: [{'sha256_ref': sha}]},
                     'command_units': {'version': 2, 'source_commit': header['source_commit'],
                                       'commands': [], 'logs': {sha: item}}}
            raw = json.dumps(child).encode()
            children.append(raw)
            descriptors.append({'sha256': retention.digest(raw), 'attachment_id': number,
                                'attachment_uuid': str(number)})
        manifest = {**header, 'version': 3, 'proofs': descriptors}
        binding = {'base_sha': header['base_sha'], 'pull_request': header['pr']}
        return manifest, children, binding

    def run_bundle(self, manifest, children, binding, outcomes=None):
        raw = json.dumps(manifest).encode()
        if outcomes is None:
            outcomes = [{'lanes': {'rust': {'mode': 'retained-unit-evidence'}},
                         'command_units': {'validation': {}}},
                        {'lanes': {'apple': {'mode': 'retained-unit-evidence'}},
                         'command_units': {'wal': {}}}]
        with patch.object(retention, 'verify_identity', return_value=(manifest, {}, 1, [9])), \
                patch.object(retention, 'attachment_bytes', side_effect=children), \
                patch.object(retention, 'verify', side_effect=outcomes) as verify:
            result = bundle.verify_bundle(object(), raw, retention.digest(raw), binding,
                                          manifest['source_commit'], 1, 'parent')
            return result, verify.call_args_list

    def repack(self, manifest, children, index, mutate):
        child = json.loads(children[index])
        mutate(child)
        children[index] = json.dumps(child).encode()
        manifest['proofs'][index]['sha256'] = retention.digest(children[index])

    def test_complete_children_are_independently_verified_with_shared_budgets_and_local_tables(self):
        manifest, children, binding = self.fixture()
        result, calls = self.run_bundle(manifest, children, binding)
        self.assertEqual(set(result['lanes']), {'rust', 'apple'})
        self.assertEqual(set(result['command_units']), {'validation', 'wal'})
        self.assertEqual(len(result['bundle_proofs']), 2)
        self.assertIs(calls[0].kwargs['objects'].budget, calls[1].kwargs['objects'].budget)
        self.assertIs(calls[0].kwargs['live_logs'], calls[1].kwargs['live_logs'])
        self.assertNotEqual(calls[0].kwargs['objects'].logs, calls[1].kwargs['objects'].logs)
        for call, raw, descriptor in zip(calls, children, manifest['proofs']):
            self.assertEqual(call.args[1:3], (raw, descriptor['sha256']))
            self.assertEqual(call.args[-2:], (descriptor['attachment_id'], descriptor['attachment_uuid']))

    def test_child_identity_recursion_digest_and_repeated_enrollments_refuse(self):
        mutations = [lambda c: c.update(source_commit='c' * 40),
                     lambda c: c.update(base_sha='c' * 40),
                     lambda c: c.update(repository=2), lambda c: c.update(pr=1046),
                     lambda c: c.update(branch='effort/other'), lambda c: c.update(version=3)]
        for mutate in mutations:
            manifest, children, binding = self.fixture()
            self.repack(manifest, children, 0, mutate)
            with self.subTest(mutate=mutate), self.assertRaises(receipts.ReceiptError):
                self.run_bundle(manifest, children, binding)
        for field in ('sha256', 'attachment_id'):
            manifest, children, binding = self.fixture()
            manifest['proofs'][1][field] = manifest['proofs'][0][field]
            with self.subTest(field=field), self.assertRaises(receipts.ReceiptError):
                self.run_bundle(manifest, children, binding)
        manifest, children, binding = self.fixture()
        children[0] += b' '
        with self.assertRaisesRegex(receipts.ReceiptError, 'digest differs'):
            self.run_bundle(manifest, children, binding)

    def test_foreign_structural_reference_and_conflicting_log_alias_refuse(self):
        manifest, children, binding = self.fixture()
        other = json.loads(children[1])['lanes']['apple'][0]
        self.repack(manifest, children, 0, lambda c: c['lanes'].update(rust=[other]))
        with self.assertRaisesRegex(receipts.ReceiptError, 'outside its own child'):
            self.run_bundle(manifest, children, binding)
        manifest, children, binding = self.fixture()
        sha, value = next(iter(json.loads(children[0])['command_units']['logs'].items()))
        value = copy.deepcopy(value)
        value['base64'] += 'AAAA'
        self.repack(manifest, children, 1, lambda c: c['command_units']['logs'].update({sha: value}))
        with self.assertRaisesRegex(receipts.ReceiptError, 'Conflicting'):
            self.run_bundle(manifest, children, binding)

    def test_inline_slots_and_aggregate_suite_command_and_table_bounds_refuse_before_adoption(self):
        mutations = [lambda c: c['lanes'].update(rust=[{'name': 'inline'}]),
                     lambda c: c['lanes'].update(rust=c['lanes']['rust'] * 8),
                     lambda c: c['command_units'].update(commands=c['lanes']['rust'] * 513),
                     lambda c: c['command_units']['logs'].update({str(i): {} for i in range(1024)})]
        for mutate in mutations:
            manifest, children, binding = self.fixture()
            self.repack(manifest, children, 0, mutate)
            with self.subTest(mutate=mutate), self.assertRaises(receipts.ReceiptError):
                self.run_bundle(manifest, children, binding)

    def test_overlapping_lane_command_jvm_and_failed_child_never_publish_a_union(self):
        cases = [
            [{'lanes': {'rust': {}}, 'command_units': {}}, {'lanes': {'rust': {}}, 'command_units': {}}],
            [{'lanes': {'rust': {}}, 'command_units': {'wal': {}}},
             {'lanes': {'apple': {}}, 'command_units': {'wal': {}}}],
            [{'lanes': {'android_jvm': {}}, 'command_units': {}},
             {'lanes': {'apple': {}}, 'command_units': {'android_jvm': {}}}],
            [{'lanes': {'rust': {}}, 'command_units': {}}, receipts.ReceiptError('child refused')],
        ]
        for outcomes in cases:
            manifest, children, binding = self.fixture()
            with self.subTest(outcomes=outcomes), self.assertRaises(receipts.ReceiptError):
                self.run_bundle(manifest, children, binding, outcomes)

    def test_actual_structural_byte_and_node_budgets_accumulate_across_child_local_decoders(self):
        first_sha, first = blob({'name': 'first', 'rows': list(range(9))})
        second_sha, second = blob({'name': 'second', 'rows': list(range(10))})
        for field, limit in [('STRUCTURAL_BYTES', 100), ('STRUCTURAL_NODES', 20)]:
            budget = StructuralBudget()
            left, right = Objects({first_sha: first}, budget), Objects({second_sha: second}, budget)
            with self.subTest(field=field), patch('validation.unit_evidence_objects.' + field, limit):
                left.resolve({'sha256_ref': first_sha}, True)
                with self.assertRaises(receipts.ReceiptError):
                    right.resolve({'sha256_ref': second_sha}, True)


if __name__ == '__main__':
    unittest.main()
