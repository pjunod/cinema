"""The session qualifies against the merge base of the head and Main, not the live Main tip.

Main cannot be frozen for a multi-hour session. `event_base` therefore names the Main commit last
merged into the PR; Main advancing leaves it unchanged and merging Main into the head moves it. The
landing check is what licenses landing the qualified evidence onto a Main that has moved.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from validation import async_refinement_provenance as p
from validation import async_refinement_session as s
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]
PROOF = s.PROOF_DIRECTORY
FORMULAS = {'source_inputs': [{'path': 'crates/a.rs'}, {'path': 'crates/shared.rs'}],
            'record_schemas': [{'path': PROOF + 'paged-proof.schema.json'}],
            'primitive_bindings': [{'source_path': 'crates/primitive.rs'}],
            'init_class_rules': [{'source_path': 'crates/a.rs'}]}
INVENTORY = {'source_bindings': {'oracle': {'path': 'crates/oracle.rs', 'sha256': '0' * 64},
                                 'temporal': {'path': 'spec/Temporal.tla', 'sha256': '0' * 64}}}


class Repository:
    """A real throwaway git repository: the checks run actual `git merge-base` and `git diff`."""

    def __init__(self, test):
        self.root = Path(tempfile.mkdtemp(prefix='merge-base-'))
        test.addCleanup(shutil.rmtree, self.root, True)
        self.run('init', '-q', '-b', 'main')

    def run(self, *arguments):
        return subprocess.check_output(
            ['git', '-c', 'user.name=t', '-c', 'user.email=t@example.invalid', '-c', 'commit.gpgsign=false',
             *arguments], cwd=self.root, text=True, stderr=subprocess.STDOUT).strip()

    def commit(self, message, files=None, remove=()):
        for name, content in (files or {}).items():
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content)
        for name in remove:
            self.run('rm', '-q', name)
        self.run('add', '-A')
        self.run('commit', '-q', '--allow-empty', '-m', message)
        return self.run('rev-parse', 'HEAD')

    def at(self, revision, branch=None):
        self.run('checkout', '-q', *(['-b', branch] if branch else []), revision)


def seed(repo):
    """B: the commit the registration names as event_base."""
    return repo.commit('base', {
        PROOF + 'source-bound-formulas.json': json.dumps(FORMULAS),
        PROOF + 'finite-inventory.json': json.dumps(INVENTORY),
        '.github/workflows/' + p.WORKFLOW: 'workflow: 1\n',
        'validation/async_physical_table_map.json': '{}\n',
        'crates/a.rs': 'a\n', 'crates/unrelated.rs': 'u\n', 'docs/readme.md': 'r\n'})


def registration(event_base, source_commit, **extra):
    reg = dict.fromkeys(s.REGISTRATION_KEYS)
    reg.update(repository=1, pr=7, event_base=event_base, source={'commit': source_commit}, **extra)
    return reg


def live(head, base, merge_base, **extra):
    value = {'number': 7, 'state': 'open', 'draft': False, 'head': {'sha': head, 'repo': {'id': 1}},
             'base': {'ref': 'main', 'sha': base, 'repo': {'id': 1}}, 'merge_base': merge_base}
    value.update(extra)
    return value


class Api:
    def __init__(self, value):
        self.value = value

    def get(self, path):
        return self.value


class MovedMainFixture(unittest.TestCase):
    """B <- H (the PR head); B <- M1 (Main moved on, the live base); H2 = H with M1 merged in."""

    def setUp(self):
        self.repo = Repository(self)
        self.b = seed(self.repo)
        self.repo.at(self.b, 'pr')
        self.h = self.repo.commit('pr change', {'crates/a.rs': 'a2\n'})
        self.repo.at(self.b, 'main-moved')
        self.m1 = self.repo.commit('main moves', {'docs/readme.md': 'r2\n'})
        self.repo.at(self.h, 'pr-merged')
        self.repo.run('merge', '-q', '--no-ff', '-m', 'merge main', self.m1)
        self.h2 = self.repo.run('rev-parse', 'HEAD')
        self.assertEqual(self.repo.run('merge-base', self.h, self.m1), self.b)
        self.assertEqual(self.repo.run('merge-base', self.h2, self.m1), self.m1)


class LivePullRequestTests(MovedMainFixture):
    def test_moved_main_tip_with_unchanged_merge_base_passes(self):
        reg = registration(self.b, self.h)
        value = live(self.h, self.m1, self.b)
        self.assertNotEqual(value['base']['sha'], reg['event_base'])
        self.assertIs(s.live_pr(Api(value), reg), value)

    def test_merging_main_into_the_head_moves_the_merge_base_and_refuses(self):
        reg = registration(self.b, self.h)
        with self.assertRaises(receipts.ReceiptError):
            s.live_pr(Api(live(self.h, self.m1, self.m1)), reg)

    def test_a_pr_object_without_a_merge_base_is_never_trusted_on_its_base_sha(self):
        reg = registration(self.b, self.h)
        value = live(self.h, self.b, self.b)  # base sha equals event_base, as the old check demanded
        del value['merge_base']
        with self.assertRaises(receipts.ReceiptError):
            s.live_pr(Api(value), reg)
        with self.assertRaises(receipts.ReceiptError):
            s.live_pr(Api(live(self.h, self.m1, None)), reg)

    def test_every_other_condition_still_refuses(self):
        reg = registration(self.b, self.h)
        bad = (live(self.m1, self.m1, self.b), live(self.h, self.m1, self.b, draft=True),
               live(self.h, self.m1, self.b, state='closed'), live(self.h, self.m1, self.b, number=8))
        for value in bad:
            with self.subTest(value=value), self.assertRaises(receipts.ReceiptError):
                s.live_pr(Api(value), reg)
        value = live(self.h, self.m1, self.b)
        value['base']['ref'] = 'release'
        with self.assertRaises(receipts.ReceiptError):
            s.live_pr(Api(value), reg)
        value = live(self.h, self.m1, self.b)
        value['base']['repo']['id'] = 2
        with self.assertRaises(receipts.ReceiptError):
            s.live_pr(Api(value), reg)


class EventMergeBaseTests(MovedMainFixture):
    def event(self, head, base):
        return {'pull_request': {'head': {'sha': head}, 'base': {'sha': base}}}

    def test_the_job_derives_the_merge_base_from_history_not_from_the_event_base_sha(self):
        self.assertEqual(s.event_merge_base(self.repo.root, self.event(self.h, self.m1)), self.b)
        self.assertEqual(s.event_merge_base(self.repo.root, self.event(self.h2, self.m1)), self.m1)

    def test_missing_history_refuses_rather_than_guessing(self):
        with self.assertRaises(receipts.ReceiptError):
            s.event_merge_base(self.repo.root, self.event(self.h, 'e' * 40))
        with self.assertRaises(receipts.ReceiptError):
            s.event_merge_base(self.repo.root, self.event(self.h, 'not-a-commit'))
        with self.assertRaises(receipts.ReceiptError):
            s.event_merge_base(self.repo.root, {'pull_request': {}})

    def test_criss_cross_history_with_several_merge_bases_refuses(self):
        repo = Repository(self)
        base = repo.commit('base', {'f': '0\n'})
        repo.at(base, 'x')
        x = repo.commit('x', {'x': '1\n'})
        repo.at(base, 'y')
        y = repo.commit('y', {'y': '1\n'})
        repo.at(x, 'xy')
        repo.run('merge', '-q', '--no-ff', '-m', 'x takes y', y)
        xy = repo.run('rev-parse', 'HEAD')
        repo.at(y, 'yx')
        repo.run('merge', '-q', '--no-ff', '-m', 'y takes x', x)
        yx = repo.run('rev-parse', 'HEAD')
        self.assertEqual(len(repo.run('merge-base', '--all', xy, yx).split()), 2)
        with self.assertRaises(receipts.ReceiptError):
            s.event_merge_base(repo.root, self.event(xy, yx))


class MainAuthenticationTests(MovedMainFixture):
    """The Main fast-lane event gates reach the next check only for the registered merge base."""

    class Reached(Exception):
        pass

    def authenticate(self, head, base, event_base):
        reg = registration(event_base, head)
        event = {'pull_request': {'head': {'sha': head}, 'base': {'sha': base}}}
        api = Api({'commit_sha': head})
        with patch.object(s.adoption, 'bind_event', return_value=event), \
             patch.object(s.adoption, 'validate_live_pr', side_effect=self.Reached):
            s.authenticate_main(api, reg, 5, self.repo.root, verified={})

    def test_moved_main_with_unchanged_merge_base_passes_the_event_gate(self):
        with self.assertRaises(self.Reached):
            self.authenticate(self.h, self.m1, self.b)

    def test_changed_merge_base_is_refused_at_the_event_gate(self):
        with self.assertRaises(receipts.ReceiptError):
            self.authenticate(self.h2, self.m1, self.b)

    def test_the_raw_event_base_sha_no_longer_decides(self):
        with self.assertRaises(receipts.ReceiptError):
            self.authenticate(self.h, self.m1, self.m1)

    def completion(self, head, base, event_base):
        reg = registration(event_base, head)
        event = {'number': 7, 'repository': {'id': 1}, 'pull_request': {'head': {'sha': head}, 'base': {'sha': base}}}
        run = {'commit_sha': head, 'event': 'workflow_dispatch', 'status': 'success', 'id': 9,
               'event_payload': p.canonical({'inputs': {'mode': 'complete', 'control': p.canonical(
                   {'registration': {}}).decode()}}).decode()}
        api = type('A', (), {'get': lambda self, path: {}, 'pages': lambda self, *a, **k: [run]})()
        self.repo.at(head)
        environment = {'GITHUB_EVENT_NAME': 'pull_request', 'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_EVENT_PATH': 'event'}
        with patch.dict(os.environ, environment), patch.object(p, 'read', return_value=event), \
             patch.object(s, 'api_environment', return_value=api), \
             patch.object(s.adoption, 'validate_live_pr'), patch.object(s, 'registration', return_value=reg), \
             patch.object(s, 'authenticate_completion', side_effect=self.Reached):
            s.require_completion(self.repo.root, self.repo.root / 'out')

    def test_ordinary_completion_gate_follows_the_merge_base(self):
        with self.assertRaises(self.Reached):
            self.completion(self.h, self.m1, self.b)
        with self.assertRaises(receipts.ReceiptError):
            self.completion(self.h2, self.m1, self.b)
        with self.assertRaises(receipts.ReceiptError):
            self.completion(self.h, self.m1, self.m1)


class CarrierPostcheckTests(unittest.TestCase):
    def test_carrier_schema_binds_a_merge_base_beside_the_audit_base(self):
        schema = json.loads((ROOT / PROOF / 'paged-control-carrier.schema.json').read_text())
        postcheck = schema['properties']['postcheck']
        self.assertIn('merge_base', postcheck['properties'])
        self.assertIn('merge_base', postcheck['required'])
        self.assertIn('current_base', postcheck['required'])


class LandingCheckTests(MovedMainFixture):
    def land(self, **changes):
        """Move Main forward from M1 with `changes`, then check the landing at the new tip."""
        self.repo.at(self.m1, 'main-tip')
        tip = self.repo.commit('main again', changes.get('files'), changes.get('remove', ()))
        return s.landing_check(registration(self.b, self.h), tip, self.repo.root), tip

    def test_unrelated_main_changes_license_the_landing_with_a_receipt(self):
        receipt, tip = self.land(files={'crates/unrelated.rs': 'u2\n', 'docs/new.md': 'n\n'})
        self.assertTrue(receipt['compatible'])
        self.assertEqual((receipt['event_base'], receipt['main_tip'], receipt['source']['commit']),
                         (self.b, tip, self.h))
        self.assertEqual(receipt['main_changed_files'], 3)  # docs/readme.md since B, plus the two new ones
        self.assertEqual(receipt['closure_files'], len(s.proof_closure(self.repo.root, self.h)))

    def test_every_kind_of_closure_file_refuses_when_main_touched_it(self):
        touched = {
            'formula source input': 'crates/shared.rs',
            'formula source path': 'crates/primitive.rs',
            'formula record schema': PROOF + 'paged-proof.schema.json',
            'finite-inventory binding': 'crates/oracle.rs',
            'finite-inventory temporal binding': 'spec/Temporal.tla',
            'proof contract': PROOF + 'PAGED-CONTRACT.md',
            'proof contract list': PROOF + 'source-bound-formulas.json',
            'async workflow': '.github/workflows/' + p.WORKFLOW,
            'table map': 'validation/async_physical_table_map.json',
            'accounting method file': 'validation/async_refinement_session.py',
        }
        self.assertEqual(len(touched), 10)
        for kind, name in touched.items():
            with self.subTest(kind=kind):
                self.repo.at(self.m1, 'tip-' + str(abs(hash(name))))
                tip = self.repo.commit('touch', {name: 'changed on main\n'})
                with self.assertRaises(receipts.ReceiptError) as raised:
                    s.landing_check(registration(self.b, self.h), tip, self.repo.root)
                self.assertIn(name, str(raised.exception))

    def test_a_closure_file_changed_before_the_session_base_is_not_a_conflict(self):
        # Main changed a closure file, but the PR already merged it (event_base is after it).
        self.repo.at(self.b, 'earlier')
        before = self.repo.commit('closure change merged into the PR', {'crates/a.rs': 'main a\n'})
        reg = registration(before, self.h)
        self.repo.at(before, 'later')
        tip = self.repo.commit('unrelated', {'docs/readme.md': 'later\n'})
        self.assertTrue(s.landing_check(reg, tip, self.repo.root)['compatible'])

    def test_deleting_or_renaming_a_closure_file_on_main_refuses(self):
        self.repo.at(self.m1, 'remove')
        tip = self.repo.commit('remove a closure file', remove=('crates/a.rs',))
        with self.assertRaises(receipts.ReceiptError):
            s.landing_check(registration(self.b, self.h), tip, self.repo.root)
        self.repo.at(self.m1, 'renamed')
        self.repo.run('mv', 'crates/a.rs', 'crates/renamed.rs')
        tip = self.repo.commit('rename a closure file')
        with self.assertRaises(receipts.ReceiptError):
            s.landing_check(registration(self.b, self.h), tip, self.repo.root)

    def test_a_tip_that_does_not_descend_from_event_base_refuses(self):
        self.repo.run('checkout', '-q', '--orphan', 'other')
        self.repo.run('rm', '-rfq', '.')
        tip = self.repo.commit('unrelated root', {'x': '1\n'})
        with self.assertRaises(receipts.ReceiptError):
            s.landing_check(registration(self.b, self.h), tip, self.repo.root)

    def test_malformed_identities_refuse(self):
        with self.assertRaises(receipts.ReceiptError):
            s.landing_check(registration(self.b, self.h), 'main', self.repo.root)
        with self.assertRaises(receipts.ReceiptError):
            s.landing_check({'pr': 7}, self.m1, self.repo.root)

    def test_cli_wires_control_registration_and_tip_and_prints_the_receipt(self):
        control = self.repo.root.parent / (self.repo.root.name + '-control.json')
        self.addCleanup(control.unlink, True)
        control.write_text(json.dumps({'registration': {'pr': 7}}))
        with patch.object(s, 'landing_check', return_value={'compatible': True}) as check, \
             patch('builtins.print') as printed:
            s.main(['landing-check', '--control', str(control), '--main-tip', self.m1])
        self.assertEqual(check.call_args.args[:2], ({'pr': 7}, self.m1))
        printed.assert_called_once_with('{"compatible":true}')


class RealClosureTests(unittest.TestCase):
    def test_closure_is_derived_from_the_codes_own_lists(self):
        commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        closure = set(s.proof_closure(ROOT, commit))
        formulas = json.loads((ROOT / PROOF / 'source-bound-formulas.json').read_text())
        inventory = json.loads((ROOT / PROOF / 'finite-inventory.json').read_text())
        self.assertTrue({row['path'] for row in formulas['source_inputs']} <= closure)
        self.assertTrue({b['path'] for b in inventory['source_bindings'].values()} <= closure)
        self.assertTrue({PROOF + name for name in s.PROOF_CONTRACT_NAMES} <= closure)
        self.assertTrue(set(s.ACCOUNTING_METHOD_FILES) <= closure)
        self.assertIn(s.ACCOUNTING_METHOD['table_map'], closure)
        self.assertIn('.github/workflows/' + p.WORKFLOW, closure)
        self.assertTrue(all((ROOT / name).is_file() for name in closure), 'closure names a missing file')
        self.assertNotIn('docs/streaming/DV-DELIVERY-FINDINGS.md', closure)


if __name__ == '__main__':
    unittest.main()
