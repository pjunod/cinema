"""The session qualifies against the merge base of the head and Main, not the live Main tip.

Main cannot be frozen for a multi-hour session. `event_base` therefore names the Main commit last
merged into the PR; Main advancing leaves it unchanged and merging Main into the head moves it. The
landing check is what licenses landing the qualified evidence onto a Main that has moved.
"""
from __future__ import annotations

import contextlib
import importlib.machinery
import io
import json
import os
from pathlib import Path
import re
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


# A stand-in for scripts/check-async-driver-refinement: the closure reads proof_inputs() from it. It names one
# vendored crate the proof compiles (`vendor/hiqlite/`); `vendor/other/` and `crates/core/daemon/` are build
# inputs outside the proof.
GOVERNANCE = ("FILES = ('Cargo.lock', 'scripts/check-async-driver-refinement')\n"
              "def proof_inputs(root=None):\n"
              "    return FILES, {'crates/core/store/', 'vendor/hiqlite/', '" + PROOF + "'}\n")


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
        'scripts/check-async-driver-refinement': GOVERNANCE, 'Cargo.lock': 'lock\n',
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


class RoutedApi:
    """The trusted Forgejo API: the PR object and the current `main` branch tip."""

    def __init__(self, pr, tip):
        self.pr, self.tip = pr, tip

    def get(self, path):
        if path.startswith('/pulls/'):
            return self.pr
        if path == '/branches/main':
            return {'name': 'main', 'commit': {'id': self.tip}}
        raise AssertionError('unexpected API path ' + path)


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

    def completion(self, head, base, *event_bases, expired=()):
        """Run `require_completion` over one earlier complete run per event base, in the given order."""
        event = {'number': 7, 'repository': {'id': 1}, 'pull_request': {'head': {'sha': head}, 'base': {'sha': base}}}
        runs = [{'commit_sha': head, 'event': 'workflow_dispatch', 'status': 'success', 'id': 9 + index,
                 'event_payload': p.canonical({'inputs': {'mode': 'complete', 'control': p.canonical(
                     {'registration': {'event_base': event_base}}).decode()}}).decode()}
                for index, event_base in enumerate(event_bases)]
        api = type('A', (), {'get': lambda self, path: {}, 'pages': lambda self, *a, **k: runs})()

        def validate(raw, root):
            if raw['event_base'] in expired:
                raise receipts.ReceiptError('registration expired')
            return registration(raw['event_base'], head)
        self.repo.at(head)
        environment = {'GITHUB_EVENT_NAME': 'pull_request', 'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_EVENT_PATH': 'event'}
        with patch.dict(os.environ, environment), patch.object(p, 'read', return_value=event), \
             patch.object(s, 'api_environment', return_value=api), \
             patch.object(s.adoption, 'validate_live_pr'), patch.object(s, 'registration', side_effect=validate), \
             patch.object(s, 'authenticate_completion', side_effect=self.Reached) as consumed:
            try:
                s.require_completion(self.repo.root, self.repo.root / 'out')
            finally:
                self.consumed = consumed

    def test_ordinary_completion_gate_follows_the_merge_base(self):
        with self.assertRaises(self.Reached):
            self.completion(self.h, self.m1, self.b)
        with self.assertRaises(receipts.ReceiptError):
            self.completion(self.h2, self.m1, self.b)
        with self.assertRaises(receipts.ReceiptError):
            self.completion(self.h, self.m1, self.m1)
        self.consumed.assert_not_called()

    def test_a_stale_complete_run_for_the_same_head_is_skipped_not_fatal(self):
        # Main merged a sibling branch and the operator re-registered at the new merge base: the older
        # complete run for the same head names another event base and must not mask the current one.
        with self.assertRaises(self.Reached):
            self.completion(self.h, self.m1, self.m1, self.b)
        self.assertEqual(self.consumed.call_args.args[2], 10)
        with self.assertRaises(self.Reached):
            self.completion(self.h, self.m1, self.m1, self.b, expired=(self.m1,))  # stale never even validated

    def test_only_stale_complete_runs_leave_the_gate_pending(self):
        with self.assertRaises(receipts.ReceiptError) as raised:
            self.completion(self.h, self.m1, self.m1, self.h2)
        self.assertIn('pending', str(raised.exception))
        self.consumed.assert_not_called()


class CarrierPostcheckTests(unittest.TestCase):
    def test_carrier_schema_binds_a_merge_base_beside_the_audit_base(self):
        schema = json.loads((ROOT / PROOF / 'paged-control-carrier.schema.json').read_text())
        postcheck = schema['properties']['postcheck']
        self.assertIn('merge_base', postcheck['properties'])
        self.assertIn('merge_base', postcheck['required'])
        self.assertIn('current_base', postcheck['required'])


class LandingCheckTests(MovedMainFixture):
    def advance(self, files=None, remove=(), start=None, branch='main-tip'):
        self.repo.at(start or self.m1, branch)
        return self.repo.commit('main again', files, remove)

    def check(self, tip, reg=None, api=None, at=None):
        """Run the check from a checkout of the registered head, as the operator must."""
        self.repo.at(at or self.h)
        reg = reg or registration(self.b, self.h)
        api = api or RoutedApi(live(reg['source']['commit'], tip, reg['event_base']), tip)
        return s.landing_check(reg, api, self.repo.root)

    def land(self, **changes):
        tip = self.advance(changes.get('files'), changes.get('remove', ()))
        return self.check(tip), tip

    def test_unrelated_main_changes_license_the_landing_with_a_receipt(self):
        receipt, tip = self.land(files={'crates/unrelated.rs': 'u2\n', 'docs/new.md': 'n\n',
                                        'crates/core/store_sibling/x.rs': 'not under the governed prefix\n'})
        self.assertTrue(receipt['compatible'])
        self.assertEqual((receipt['event_base'], receipt['main_tip'], receipt['head_commit_id']), (self.b, tip, self.h))
        self.assertEqual(receipt['source']['commit'], self.h)
        self.assertEqual(receipt['main_changed_files'], 4)  # docs/readme.md since B, plus the three new ones
        closure = s.proof_closure(self.repo.root, self.h)
        self.assertEqual((receipt['closure_files'], receipt['closure_prefixes']),
                         (len(closure['files']), len(closure['prefixes'])))

    def test_main_changes_to_build_only_inputs_do_not_refuse_the_landing(self):
        # source_identity() hashes the whole daemon scope and all of vendor/, but only proof inputs may refuse.
        receipt, _ = self.land(files={'crates/core/daemon/handler.rs': 'main\n',
                                      'vendor/other/src/lib.rs': 'main\n'})
        self.assertTrue(receipt['compatible'])

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
            'governance FILES entry': 'Cargo.lock',
            'governance script': 'scripts/check-async-driver-refinement',
            'TLC temporal model': PROOF + 'PagedComponent.tla',
            'TLC model': PROOF + 'PagedOneStep.tla',
            'TLC config': PROOF + 'paged-component.cfg',
            'compiled catalog source': 'crates/plurx-core/src/store/async_driver_catalog/sqlite.rs',
            'compiled catalog root': 'crates/plurx-core/src/store/async_driver_catalog.rs',
            'stage registry': PROOF + 'store-tests.json',
            'Main fast lane workflow': '.github/workflows/main-fast-lane.yml',
            'runner route': 'validation/runner-fleet.toml',
            'capability action': '.github/actions/async-proof-capability/main.js',
            'scope selector': 'validation/ci_scope.py',
            'scope selector dependency': 'validation/points.toml',
        }
        for kind, name in touched.items():
            with self.subTest(kind=kind):
                tip = self.advance({name: 'changed on main\n'}, branch='tip-' + str(abs(hash(name))))
                with self.assertRaises(receipts.ReceiptError) as raised:
                    self.check(tip)
                self.assertIn(name, str(raised.exception))

    def test_a_file_main_adds_under_a_governed_prefix_refuses(self):
        # source_identity() and the Rust build walk these prefixes, so a path no list names is still bound.
        added = {'governed crate prefix': 'crates/core/store/brand_new_module.rs',
                 'vendor tree the proof compiles': 'vendor/hiqlite/src/new_module.rs',
                 'proof directory': PROOF + 'NewModel.tla'}
        for kind, name in added.items():
            with self.subTest(kind=kind):
                tip = self.advance({name: 'new on main\n'}, branch='tip-' + str(abs(hash(name))))
                with self.assertRaises(receipts.ReceiptError) as raised:
                    self.check(tip)
                self.assertIn(name, str(raised.exception))

    def test_a_closure_file_changed_before_the_session_base_is_not_a_conflict(self):
        # Main changed a closure file, but the PR already merged it (event_base is after it).
        self.repo.at(self.b, 'earlier')
        before = self.repo.commit('closure change merged into the PR', {'crates/a.rs': 'main a\n'})
        reg = registration(before, self.h)
        tip = self.advance({'docs/readme.md': 'later\n'}, start=before, branch='later')
        self.assertTrue(self.check(tip, reg)['compatible'])

    def test_deleting_or_renaming_a_closure_file_on_main_refuses(self):
        tip = self.advance(remove=('crates/a.rs',), branch='remove')
        with self.assertRaises(receipts.ReceiptError):
            self.check(tip)
        self.repo.at(self.m1, 'renamed')
        self.repo.run('mv', 'crates/a.rs', 'crates/renamed.rs')
        tip = self.repo.commit('rename a closure file')
        with self.assertRaises(receipts.ReceiptError):
            self.check(tip)

    def test_a_tip_that_does_not_descend_from_event_base_refuses(self):
        self.repo.run('checkout', '-q', '--orphan', 'other')
        self.repo.run('rm', '-rfq', '.')
        tip = self.repo.commit('unrelated root', {'x': '1\n'})
        with self.assertRaises(receipts.ReceiptError):
            self.check(tip)

    def test_the_tip_comes_from_the_api_and_a_caller_cannot_choose_it(self):
        import inspect
        self.assertEqual(list(inspect.signature(s.landing_check).parameters), ['reg', 'api', 'root'])
        stale = self.advance({'docs/new.md': 'n\n'}, branch='tip-0')
        current = self.advance({'crates/shared.rs': 'main moved a proof input\n'}, start=stale, branch='tip-1')
        # Operator checked at T0 (compatible); Main then moved to T1; the check reads T1 itself and refuses.
        self.assertEqual(self.check(stale)['main_tip'], stale)
        with self.assertRaises(receipts.ReceiptError):
            self.check(current)
        parser_error = io.StringIO()
        with contextlib.redirect_stderr(parser_error), self.assertRaises(SystemExit):
            s.main(['landing-check', '--control', 'c.json', '--output', 'o.json', '--main-tip', stale])
        self.assertIn('--main-tip', parser_error.getvalue())

    def test_a_tip_the_api_reports_is_verified_with_git(self):
        for bogus in ('e' * 40, 'main', None, 'A' * 40):
            with self.subTest(tip=bogus), self.assertRaises(receipts.ReceiptError):
                self.check(bogus, api=RoutedApi(live(self.h, self.m1, self.b), bogus))
        branch = {'name': 'release', 'commit': {'id': self.m1}}
        api = type('A', (), {'get': lambda this, path: branch if path.startswith('/branches')
                             else live(self.h, self.m1, self.b)})()
        with self.assertRaises(receipts.ReceiptError):
            self.check(self.m1, api=api)

    def test_the_receipt_binds_a_pr_that_is_still_at_the_head_and_merge_base(self):
        for value in (live(self.m1, self.m1, self.b), live(self.h, self.m1, self.m1), live(self.h, self.m1, self.b, draft=True)):
            with self.subTest(value=value), self.assertRaises(receipts.ReceiptError):
                self.check(self.m1, api=RoutedApi(value, self.m1))

    def test_the_check_must_run_from_a_checkout_of_the_registered_head(self):
        with self.assertRaises(receipts.ReceiptError):
            self.check(self.m1, at=self.m1)

    def test_malformed_identities_and_sources_refuse_with_a_receipt_error(self):
        api = RoutedApi(live(self.h, self.m1, self.b), self.m1)
        for source in ('abc', None, [], {}, {'commit': 7}, {'commit': 'main'}):
            with self.subTest(source=source), self.assertRaises(receipts.ReceiptError):
                s.landing_check(registration(self.b, self.h) | {'source': source}, api, self.repo.root)
        with self.assertRaises(receipts.ReceiptError):
            s.landing_check(registration('main', self.h), api, self.repo.root)
        with self.assertRaises(receipts.ReceiptError):
            s.landing_check({'pr': 7}, api, self.repo.root)

    def test_missing_governance_refuses_rather_than_closing_over_less(self):
        self.repo.at(self.h)
        self.repo.run('rm', '-q', 'scripts/check-async-driver-refinement')
        with self.assertRaises(receipts.ReceiptError):
            s.proof_closure(self.repo.root, self.h)

    def test_cli_derives_the_api_from_the_environment_and_writes_the_receipt(self):
        control = self.repo.root.parent / (self.repo.root.name + '-control.json')
        output = self.repo.root.parent / (self.repo.root.name + '-landing') / 'receipt.json'
        self.addCleanup(control.unlink, True)
        self.addCleanup(shutil.rmtree, output.parent, True)
        control.write_text(json.dumps({'registration': {'pr': 7}}))
        with patch.object(s, 'landing_check', return_value={'compatible': True}) as check, \
             patch.object(s, 'api_environment', return_value='api') as environment, \
             patch('builtins.print') as printed:
            output.parent.mkdir()
            s.main(['landing-check', '--control', str(control), '--output', str(output)])
        self.assertEqual(check.call_args.args[:2], ({'pr': 7}, 'api'))
        environment.assert_called_once_with(output.parent / 'landing-api')
        self.assertEqual(json.loads(output.read_text()), {'compatible': True})
        printed.assert_called_once_with('{"compatible":true}')


class AffectedSelectorTests(MovedMainFixture):
    """`affected` decides whether Main's async_refinement job must run; Main's own changes must not fire it."""

    @classmethod
    def script(cls):
        loader = importlib.machinery.SourceFileLoader('affected_runner', str(ROOT / 'scripts/check-async-driver-refinement'))
        return loader.load_module()

    def affected(self, base, at):
        script = self.script()
        self.repo.at(at)

        def run(argv, *, env=None, timeout=1800):
            return subprocess.run(argv, cwd=self.repo.root, text=True, capture_output=True, check=False), 0
        out = io.StringIO()
        with patch.object(script, 'run', run), contextlib.redirect_stdout(out):
            script.affected(type('Args', (), {'base': base})())
        return json.loads(out.getvalue())

    def moved(self):
        """B <- H (docs only) and B <- M1 (Main changed Cargo.lock, a governed input)."""
        self.repo.at(self.b, 'main-lock')
        m = self.repo.commit('main bumps a governed input', {'Cargo.lock': 'bumped on main\n'})
        self.repo.at(self.b, 'pr-docs')
        h = self.repo.commit('pr changes docs only', {'docs/readme.md': 'pr\n'})
        return m, h

    def test_a_main_only_change_to_a_governed_path_does_not_fire_the_trigger(self):
        main, head = self.moved()
        result = self.affected(main, head)
        self.assertFalse(result['affected'])
        self.assertEqual(result['changed_paths'], ['docs/readme.md'])

    def fires(self, *paths):
        self.repo.at(self.b, 'pr-' + str(abs(hash(paths))))
        head = self.repo.commit('pr change', {path: 'changed\n' for path in paths})
        return self.affected(self.b, head)['affected']

    def test_a_plurxd_change_outside_the_proof_modules_does_not_fire_the_trigger(self):
        for path in ('crates/plurxd/src/main.rs', 'crates/plurxd/src/state.rs', 'crates/plurx-core/src/lib.rs',
                     'crates/plurx-pgs/src/lib.rs', 'vendor/gemm-common/src/lib.rs', 'vendor/ksni/src/lib.rs',
                     'vendor/s3-simple/src/lib.rs'):
            with self.subTest(path=path):
                self.assertFalse(self.fires(path))

    def test_proof_inputs_still_fire_the_trigger(self):
        for path in ('crates/plurx-core/src/store/new.rs', 'crates/plurx-worker-control/src/new.rs',
                     'crates/plurxd/src/library_work/new.rs', 'crates/plurxd/src/background_jobs/new.rs',
                     'crates/plurx-core/tests/store_contract/new.rs', 'crates/plurx-core/tests/support/new.rs',
                     'verification/async-driver-refinement/PagedComponent.tla', 'rust-toolchain.toml', 'Cargo.toml',
                     'validation/main_preflight_adoption.py', '.github/workflows/main-fast-lane.yml',
                     'vendor/hiqlite/src/new.rs', 'vendor/hiqlite-wal/src/new.rs', 'vendor/rust_decimal/src/new.rs',
                     'vendor/dolby_vision/src/new.rs', 'vendor/bitvec_helpers/src/new.rs'):
            with self.subTest(path=path):
                self.assertTrue(self.fires(path))

    def test_a_change_the_pr_itself_makes_to_a_governed_path_still_fires_it(self):
        main, _ = self.moved()
        self.repo.at(self.b, 'pr-lock')
        head = self.repo.commit('pr bumps a governed input', {'Cargo.lock': 'bumped by the pr\n'})
        self.assertTrue(self.affected(main, head)['affected'])
        self.assertTrue(self.affected(self.b, head)['affected'])


class RealClosureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        cls.closure = s.proof_closure(ROOT, cls.commit)
        cls.script = importlib.machinery.SourceFileLoader(
            'closure_runner', str(ROOT / 'scripts/check-async-driver-refinement')).load_module()

    def test_closure_is_derived_from_the_codes_own_lists(self):
        closure, files = self.closure, set(self.closure['files'])
        formulas = json.loads((ROOT / PROOF / 'source-bound-formulas.json').read_text())
        inventory = json.loads((ROOT / PROOF / 'finite-inventory.json').read_text())
        self.assertTrue({row['path'] for row in formulas['source_inputs']} <= files)
        self.assertTrue({b['path'] for b in inventory['source_bindings'].values()} <= files)
        self.assertTrue({PROOF + name for name in s.PROOF_CONTRACT_NAMES} <= files)
        self.assertTrue(set(s.ACCOUNTING_METHOD_FILES) <= files)
        self.assertTrue(set(s.CAPABILITY_ACTION_FILES) <= files)
        self.assertTrue(set(s.CATALOG_COMPILED_SOURCES) <= files)
        self.assertIn(s.ACCOUNTING_METHOD['table_map'], files)
        self.assertIn('.github/workflows/' + p.WORKFLOW, files)
        explicit = set(s.PROOF_MODEL_FILES) | set(s.REGISTRATION_BOUND_FILES)
        self.assertTrue(explicit <= files)
        self.assertTrue(all((ROOT / name).is_file() for name in files), 'closure names a missing file')
        self.assertFalse(s.in_closure(closure, 'docs/streaming/DV-DELIVERY-FINDINGS.md'))

    def test_closure_covers_everything_the_governance_and_the_checker_bind(self):
        closure = self.closure
        self.assertTrue(set(self.script.FILES) <= set(closure['files']))
        files, prefixes = self.script.proof_inputs()
        self.assertEqual(set(prefixes), set(closure['prefixes']))
        self.assertTrue(set(files) <= set(closure['files']))
        for include_proof in (True, False):
            identity = self.script.source_identity(include_proof)
            # The closure is the proof's inputs, not the build scope: every identity path that is a proof input
            # is in the closure, and the build-only remainder (the wider daemon scope, other vendor) is not.
            proof = [n for n in identity if n in files or any(n.startswith(q) for q in prefixes)]
            self.assertEqual([n for n in proof if not s.in_closure(closure, n)], [])
            self.assertTrue(any(not s.in_closure(closure, n) for n in identity),
                            'source_identity() must keep its wider build scope')
        source = (ROOT / PROOF / 'paged_checker.py').read_text()
        start = source.index("for name in ('checker.py'")
        semantic = re.findall(r"'([^']+)'", source[start:source.index('):', start)])
        self.assertIn('PagedComponent.tla', semantic)
        self.assertEqual([name for name in semantic if not s.in_closure(closure, PROOF + name)], [])
        for name in ('crates/plurx-core/src/store/new/module.rs', 'crates/plurx-worker-control/src/new.rs'):
            self.assertTrue(s.in_closure(closure, name), name)
        self.assertTrue(s.in_closure(closure, 'crates/plurxd/src/library_work/new_module.rs'))
        self.assertTrue(s.in_closure(closure, 'vendor/hiqlite/src/new.rs'))
        for name in ('crates/plurxd/src/main.rs', 'crates/plurxd/src/state.rs', 'crates/plurx-core/src/lib.rs',
                     'vendor/gemm-common/src/lib.rs', 'vendor/ksni/src/lib.rs', 'vendor/s3-simple/src/lib.rs'):
            self.assertFalse(s.in_closure(closure, name), name)


if __name__ == '__main__':
    unittest.main()
