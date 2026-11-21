"""Require distinct native profile outcomes and exact formal smoke inputs."""
import unittest
from unittest.mock import patch

from validation import python_unit_receipts as r
from validation import selected_apple_preflight as apple
from validation import selected_formal_preflight as formal


class SelectedApplePreflightTests(unittest.TestCase):
    def profile_log(self, ipad_status='skipped'):
        top = ['Xcode 27.2', 'Build version 27B52']
        top = ['', *top]
        blocks = []
        boot = 'A' * 36
        device = {'iphone': ('iPhone', 'iOS'), 'ipad': ('iPad', 'iOS'), 'tvos': ('Apple TV', 'tvOS')}
        for profile, (label, os_name) in device.items():
            top.append(f'Monitoring boot status for plurx CI {label} 10-1 ({boot}).')
            case = apple.PIP_CASE.replace('/', ' ')
            start = f"Test Case '-[plurx_{os_name}Tests.{case}]'"
            flag = ipad_status if profile == 'ipad' else 'passed'
            lines = [
                'Command line invocation:',
                f'    /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -project plurx.xcodeproj '
                f'-scheme plurx-{os_name} -destination "platform={os_name} Simulator,id={boot}" '
                '-derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=NO test-without-building',
                start + ' started.',
            ]
            if flag == 'skipped':
                lines.append('This regression exercises a device without PiP support')
            lines.extend([start + f' {flag} (0.01 seconds).', "Test Suite 'All tests' passed at time",
                          'Executed 1 tests, with ' + ('1 test skipped and ' if flag == 'skipped' else '')
                          + '0 failures (0 unexpected)', 'output.xcresult', '** TEST EXECUTE SUCCEEDED **'])
            blocks.extend(lines)
        return '\n'.join(top + blocks).encode()

    def test_apple_profiles_keep_capability_skip_separate_from_positive_results(self):
        counts = {'iphone': ('iPhone', 'iOS', 1, 0), 'ipad': ('iPad', 'iOS', 1, 1),
                  'tvos': ('Apple TV', 'tvOS', 1, 0)}
        with patch.object(apple, 'SHARED_PROFILES', counts):
            records = apple.shared_outcomes(self.profile_log(), 10)
            self.assertEqual(records['iphone']['pass_count'], 1)
            self.assertEqual(records['ipad']['pass_count'], 0)
            self.assertEqual(records['ipad']['ignored'], [apple.PIP_CASE])
            cases = (self.profile_log('passed'),
                     self.profile_log().replace(b'TEST EXECUTE SUCCEEDED', b'TEST EXECUTE FAILED', 1),
                     self.profile_log().replace(b'exercises a device', b'failed for a device'))
            for log in cases:
                with self.subTest(log=log[-100:]), self.assertRaises(r.ReceiptError):
                    apple.shared_outcomes(log, 10)

    def test_only_the_exact_original_native_fragment_may_be_rejoined(self):
        prefix = "Test Case '-[plurx_iOSTests.Case testKnown"
        suffix = "Method]' passed (0.1 seconds)."
        raw = prefix + 'interleaved native diagnostic' + '\n' + suffix
        span = (prefix, suffix, 2, apple.retention.digest(raw.encode()))
        with patch.object(apple, 'FRAGMENTS', (span,)):
            self.assertEqual(apple.rejoin_fragments(raw), prefix + suffix)
            changed = raw.replace('passed', 'failed')
            self.assertEqual(apple.rejoin_fragments(changed), changed)
            with self.assertRaises(r.ReceiptError):
                apple.rejoin_fragments(raw + '\n' + raw)

    def test_desktop_proof_requires_both_complete_named_xctest_suites(self):
        def suite(namespace, count):
            lines = ["Test Suite 'All tests' started at time"]
            for case in range(count):
                event = f"Test Case '-[{namespace}.ExampleTests testCase{case}]'"
                lines.extend([event + ' started.', event + ' passed (0.01 seconds).'])
            lines.extend(["Test Suite 'All tests' passed at time",
                          f'Executed {count} tests, with 0 failures (0 unexpected)'])
            return '\n'.join(lines).encode() + b'\n'

        ui, core = suite('PlurxTrayUITests', 14), suite('PlurxTrayCoreTests', 61)
        result = apple.desktop_outcomes(ui + core)
        self.assertEqual({key: row['pass_count'] for key, row in result.items()},
                         {'PlurxTrayUITests': 14, 'PlurxTrayCoreTests': 61})
        for incomplete in (core, ui, ui + core.replace(b'testCase60', b'testCase59'),
                           ui.replace(b'0 failures', b'1 failures') + core):
            with self.subTest(incomplete=incomplete[-100:]), self.assertRaises(r.ReceiptError):
                apple.desktop_outcomes(incomplete)

    def test_formal_reuse_requires_complete_exact_original_and_current_input_hashes(self):
        commit, candidate = 'a' * 40, 'b' * 40
        hashes = {'worker-refinement.cfg': 'c' * 64}
        pin = formal.retention.digest(b'fixture')
        reported = {'package': formal.PACKAGE, 'schema_version': 1, 'source_drift': [],
                    'input_sha256': hashes, 'mapping_sha256': pin, 'affected': False}
        receipt = {'source_git': {'commit': commit}, 'input_sha256': hashes, 'mapping_sha256': pin,
                   'runner_sha256': pin, 'toolchain': {'manifest_sha256': pin}}
        with patch.object(formal.retention, 'git_bytes', return_value=b'fixture'):
            formal.matches_current(receipt, reported, reported, candidate, commit)
            for current in ({**reported, 'affected': True}, {**reported, 'input_sha256': {}},
                            {**reported, 'mapping_sha256': 'd' * 64},
                            {**reported, 'source_drift': ['changed reviewed source']}):
                with self.subTest(current=current), self.assertRaises(r.ReceiptError):
                    formal.matches_current(receipt, reported, current, candidate, commit)
