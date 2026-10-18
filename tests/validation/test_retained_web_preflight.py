"""Only the complete, finite original Node capture can adopt two scripts."""
import base64
import copy
from pathlib import Path
import unittest
from unittest.mock import patch
from validation import promotion_unit_retention as retention
from validation import retained_web_preflight as web
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]


def source(commit, path):
    return (ROOT / path).read_bytes()


def capture():
    first, second = web.labels(source)
    return ('node: v22.23.3\n' + ''.join('ok - ' + label + '\n' for label in first)
            + ''.join('PASS ' + label + '\n' for label in second)
            + 'TAP version 13\n# Subtest: ' + web.CASE + '\nok 1 - ' + web.CASE
            + "\n  ---\n  duration_ms: 0.1\n  type: 'test'\n  ...\n1..1\n# tests 1"
            + '\n# suites 0\n# pass 1\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 0'
            + '\n# duration_ms 1.2\n⭐ Run Post setup-node\n🏁  Job succeeded\n')


def blob(raw):
    return {'base64': base64.b64encode(raw.encode()).decode(),
            'sha256': retention.digest(raw.encode())}


def fixture():
    environment = {'platform': 'linux', 'toolchain': 'Node 22.23.3', 'features': [],
                   'runtime': 'original full-CI Node preflight', 'binary_sha256': None,
                   'flags': None, 'per_script_exit': None}
    record = {'sequence': 1, 'source_commit': web.SOURCE, 'environment': environment,
              'origin': {'kind': 'authenticated-ci', 'identity': 'run:4954/job:49307',
                         'command': web.COMMAND}, 'changes': {},
              'outcomes': dict.fromkeys(web.SCRIPTS, 'pass'), 'format': web.FORMAT,
              'log': blob(capture())}
    suite = {'name': 'node-contracts', 'ids': list(web.SCRIPTS), 'environment': environment,
             'inventory': blob('\n'.join(web.SCRIPTS) + '\n'), 'records': [record]}
    return {'version': 1, 'repository': 1, 'pr': 1, 'branch': 'effort/test',
            'base_sha': 'a' * 40, 'source_commit': 'b' * 40, 'windows_waiver': None,
            'lanes': {'web_preflight': [suite]}}


class RetainedWebPreflightCase(unittest.TestCase):
    def test_complete_custom_and_native_outcomes_refuse_missing_duplicate_unknown_failed_or_truncated_rows(self):
        raw = capture()
        self.assertEqual(web.outcomes(raw, source), dict.fromkeys(web.SCRIPTS, 'pass'))
        mutations = [raw.replace('TAP version 13\n', ''),
                     raw.replace('# tests 1', '# tests 2'),
                     raw.replace('# fail 0', '# fail 1'),
                     raw.replace('# skipped 0', '# skipped 1'),
                     raw.replace('# cancelled 0', '# cancelled 1'),
                     raw.replace('ok 1 - ', 'not ok 1 - '),
                     raw.replace('1..1', '1..2'), raw.replace('# todo 0', '# todo 1'),
                     raw.replace('🏁  Job succeeded', '🏁  Job failed'),
                     raw.replace('TAP version 13', 'PASS unknown\nTAP version 13'),
                     raw.replace('TAP version 13', 'TAP version 13\nTAP version 13'),
                     raw[:raw.index('# duration_ms')]]
        first, _ = web.labels(source)
        mutations += [raw.replace('ok - ' + first[0] + '\n', ''),
                      raw.replace('ok - ' + first[1], 'ok - ' + first[0])]
        for changed in mutations:
            with self.subTest(changed=changed[-60:]), self.assertRaises(receipts.ReceiptError):
                web.outcomes(changed, source)

    def test_record_and_original_workflow_binding_refuse_foreign_source_scope_environment_or_job(self):
        document = fixture()
        suite = document['lanes']['web_preflight'][0]
        record = suite['records'][0]
        with patch.object(web, 'LOG', retention.digest(capture().encode())):
            web.record_contract('web_preflight', suite, record, 'b' * 40, capture(), source,
                                retention.digest)
            mutations = [lambda r: r.update(source_commit='c' * 40),
                         lambda r: r.update(format='rust'),
                         lambda r: r['origin'].update(kind='attested-local'),
                         lambda r: r['origin'].update(identity='run:4954/job:49308'),
                         lambda r: r['origin'].update(command=web.COMMAND + '\nnode other.js'),
                         lambda r: r['environment'].update(flags=[]),
                         lambda r: r['environment'].update(per_script_exit=0)]
            for mutate in mutations:
                changed = copy.deepcopy(record)
                mutate(changed)
                with self.subTest(mutate=mutate), self.assertRaises(receipts.ReceiptError):
                    web.record_contract('web_preflight', suite, changed, 'b' * 40, capture(),
                                        source, retention.digest)
            with self.assertRaises(receipts.ReceiptError):
                web.record_contract('rust', suite, record, 'b' * 40, capture(), source, retention.digest)
        original = ('      - name: Check the shared player input contract\n        run: |\n'
                    + ''.join('          node ' + path + '\n' for path in web.SCRIPTS))
        web.authenticate(4954, 49307, {'status': 'success'}, lambda *_: original.encode())
        for workflow in (original.replace('        run: |', "        if: false\n        run: |"),
                         original * 2, original.replace('node tests/web/', 'node --test tests/web/')):
            with self.assertRaises(receipts.ReceiptError):
                web.authenticate(4954, 49307, {'status': 'success'}, lambda *_: workflow.encode())
        with self.assertRaises(receipts.ReceiptError):
            web.authenticate(4954, 49307, {'status': 'failure'}, lambda *_: original.encode())

    def test_current_script_drift_and_consumed_input_invalidation_cannot_supply_applicable_pass(self):
        document = fixture()
        suite = document['lanes']['web_preflight'][0]
        raw = capture()
        with patch.object(web, 'LOG', retention.digest(raw.encode())):
            with self.assertRaisesRegex(receipts.ReceiptError, 'script inventory or body'):
                web.record_contract('web_preflight', suite, suite['records'][0], 'b' * 40,
                                    raw, lambda commit, path: source(commit, path)
                                    + (b'// drift' if commit != web.SOURCE else b''), retention.digest)
            record = suite['records'][0]
            record['changes'] = {'dep.js': {'before': retention.digest(b'old'),
                'after': retention.digest(b'new'), 'affected_ids': [next(iter(web.SCRIPTS))]}}
            read = lambda commit, path: b'old' if commit == web.SOURCE else b'new'
            diff = lambda before, after: set() if before == after else {'dep.js'}
            with patch.object(retention, 'git_bytes', source):
                with self.assertRaisesRegex(receipts.ReceiptError, 'Incomplete or failed applicable'):
                    retention.validate(document, document['source_commit'], source=read, differences=diff)
                record['changes']['dep.js']['affected_ids'] = []
                adopted = retention.validate(document, document['source_commit'], source=read, differences=diff)
                self.assertEqual(adopted['web_preflight']['passed'], 2)

    def test_workflow_guards_only_two_scripts_and_requires_successful_preflight_and_browser_jobs(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        step = workflow.split('      - name: Check the shared player input contract\n')[1].split('\n      - name:')[0]
        self.assertIn('RETAIN_WEB_PREFLIGHT: ${{ needs.scope.outputs.retain_web_preflight }}', step)
        self.assertIn('if [ "$RETAIN_WEB_PREFLIGHT" = true ]; then', step)
        self.assertEqual(step.count('node tests/'), 2)
        self.assertIn('else\n            node tests/playback/player-input-contract.test.js\n'
                      '            node tests/web/player-dom.test.js\n          fi', step)
        self.assertIn('retain_web_preflight: ${{ steps.retention.outputs.retain_web_preflight }}', workflow)
        from validation.qualification import REQUIRED_JOBS
        self.assertTrue({'preflight', 'web_layout', 'vod_web'} <= REQUIRED_JOBS)
        self.assertNotIn('web_preflight', REQUIRED_JOBS)
        self.assertNotIn("retain_web_preflight != 'true'", workflow)
