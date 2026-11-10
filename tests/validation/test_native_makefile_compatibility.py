"""Native recipe compatibility stays limited to two reviewed Makefiles."""
import unittest
from unittest.mock import patch

from validation import native_python_discovery as python_native
from validation import promotion_unit_retention as retention
from validation import workspace_rust_continuation as rust_native
from validation.python_unit_receipts import ReceiptError


PRIOR = '5ca3abcb0259b401b7da3716c41f6026e2b95415'
MERGED = '8d02a7b58'


class NativeMakefileCompatibilityCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import subprocess
        cls.merged = subprocess.check_output(['git', 'rev-parse', MERGED], text=True).strip()
        cls.old = retention.git_bytes(PRIOR, 'Makefile')
        cls.new = retention.git_bytes(cls.merged, 'Makefile')
        cls.workflow = retention.git_bytes(PRIOR, '.github/workflows/ci.yml').decode()

    def android_inputs(self, makefile):
        def source(commit, path):
            return makefile if path == 'Makefile' else retention.git_bytes(PRIOR, path)
        with patch.object(retention, 'android_device_tree', return_value='fixed-tree'):
            return retention.android_device_inputs(source, PRIOR)

    def rust_changes(self, before, after, affected=()):
        changes = {'Makefile': {'before': retention.digest(before),
                               'after': retention.digest(after),
                               'affected': list(affected),
                               'reason': 'Reviewed Docker-only recipe transition'}}
        record = {'prior': PRIOR, 'changes': changes}
        targets = {'core@src/lib.rs': {'outcomes': {'case': 'pass'}}}
        with patch.object(retention, 'changed', return_value={'Makefile'}), \
                patch.object(retention, 'git_bytes', side_effect=lambda rev, path:
                             before if rev == PRIOR else after):
            return rust_native.check_changes(record, self.merged, targets)

    def test_reviewed_docker_only_make_change_preserves_native_consumers(self):
        for makefile in (self.old, self.new):
            with patch.object(retention, 'git_bytes', return_value=makefile):
                python_native.environment(PRIOR, self.workflow, historical=False)
        self.assertEqual(self.android_inputs(self.old), self.android_inputs(self.new))
        self.assertEqual(self.rust_changes(self.old, self.new), set())

    def test_compatibility_does_not_erase_explicit_rust_body_invalidation(self):
        self.assertEqual(self.rust_changes(self.old, self.new, ['core@src/lib.rs::case']),
                         {'core@src/lib.rs::case'})

    def test_unknown_recipe_global_or_appended_override_stays_incompatible(self):
        for suffix in (b'\noverride ANDROID_IMAGE := other\n',
                       b'\noverride CARGO := other-cargo\n',
                       b'\ninclude unreviewed.mk\n',
                       b'\nandroid-instrumentation-run:\n\tother-runner\n',
                       b'\nvalidation-lint:\n\ttrue\n'):
            changed = self.new + suffix
            with self.subTest(suffix=suffix):
                with patch.object(retention, 'git_bytes', return_value=changed):
                    with self.assertRaises(ReceiptError):
                        python_native.environment(PRIOR, self.workflow, historical=False)
                self.assertNotEqual(self.android_inputs(self.old), self.android_inputs(changed))
                with self.assertRaises(ReceiptError):
                    self.rust_changes(self.old, changed)

    def test_an_unreviewed_historical_makefile_cannot_use_the_transition(self):
        with self.assertRaises(ReceiptError):
            self.rust_changes(self.old + b'\n# unknown source\n', self.new)
