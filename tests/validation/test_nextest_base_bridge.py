"""A source bridge retains only reviewed Rust bodies and their actual origins."""

import base64
import hashlib
import unittest
from unittest.mock import patch

from validation import nextest_base_bridge as bridge
from validation.python_unit_receipts import ReceiptError


class NextestBaseBridgeTest(unittest.TestCase):
    def test_source_bridge_requires_the_exact_main_and_reviewed_compiler_inputs(self):
        base = 'b' * 40
        prior = 'a' * 40
        candidate = 'c' * 40
        path = 'crates/plurx-core/src/fmp4.rs'
        profile = {'ci_current_main': base, 'source_before': prior,
                   'changes': {path: {'before': 'old', 'after': 'new'}}}
        with patch.object(bridge.paperwork, 'ancestor', return_value=True), \
             patch.object(bridge, 'source_range', return_value={path}) as changes, \
             patch.object(bridge, 'file_digest', side_effect=lambda source, _: 'old' if source == prior else 'new'), \
             patch.object(bridge.nextest, 'source_selection', return_value={'crates': {}}):
            self.assertEqual(bridge.current_projection(profile, candidate, base), {'crates': {}})
            with self.assertRaises(ReceiptError):
                bridge.current_projection(profile, candidate, 'd' * 40)
            changes.return_value.add('vendor/hiqlite/src/lib.rs')
            with self.assertRaises(ReceiptError):
                bridge.current_projection(profile, candidate, base)

    def test_external_inputs_refuse_an_unreviewed_embedded_or_executable_path(self):
        old, current = 'a' * 40, 'b' * 40
        rust = {'crates/plurxd/src/dv_tools.rs'}
        allowed = rust | {'.github/workflows/ci.yml', 'tests/validation/test_nextest_base_bridge.py'}
        with patch.object(bridge.retention, 'changed', return_value=allowed) as changes:
            bridge.require_external_inputs(old, current, rust)
            for extra in ('tools/dv_processing/publication.c', 'Makefile', 'tests/playback/other-fixture.json'):
                changes.return_value = allowed | {extra}
                with self.subTest(extra=extra), self.assertRaises(ReceiptError):
                    bridge.require_external_inputs(old, current, rust)

    def test_upstream_log_without_its_original_digest_never_supplies_a_positive(self):
        authentic = b'archived source and real runner outcomes'
        profile = {'upstream_linux_log_sha256': hashlib.sha256(authentic).hexdigest()}
        changed = base64.b64encode(authentic + b'\ntest synthetic ... ok\n').decode()
        with self.assertRaises(ReceiptError):
            bridge.upstream_linux_cases(profile, changed, 'c' * 40)
