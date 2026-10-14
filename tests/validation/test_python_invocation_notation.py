"""A finite original invocation notation does not become general alias trust."""
import copy
import json
from pathlib import Path
import unittest
from validation import nonrust_unit_cohort as cohort
from validation import promotion_unit_retention as retention


class PythonInvocationNotationCase(unittest.TestCase):
    def test_only_original_binding_source_log_kind_and_invocation_admit_the_recorded_notation(self):
        root = Path(__file__).resolve().parent / 'fixtures'
        binding_raw = (root / 'r32-original-python-alias-binding.json').read_bytes()
        raw = (root / 'r32-original-python-alias.log').read_bytes()
        binding = json.loads(binding_raw)
        self.assertEqual(retention.digest(binding_raw),
                         'd9213642172d63ab1b2bded499712418936a04026091dc0b20bcd5767f14e135')
        self.assertEqual(retention.digest(raw),
                         'e9fd6d5f078f77f7a842908037f4f121196cd545ec0669356f0341f8316a45f6')
        command = binding['argv']
        self.assertTrue(cohort.named_python_entrypoint('validation', command, binding, binding_raw, raw))
        for kind, captured, log in [('operations', binding_raw, raw),
                                    ('validation', binding_raw + b' ', raw),
                                    ('validation', binding_raw, raw + b' ')]:
            with self.subTest(kind=kind, captured=len(captured), log=len(log)):
                self.assertFalse(cohort.named_python_entrypoint(kind, command, binding, captured, log))
        for field, value in [('source_commit', 'a' * 40),
                             ('python_executable', '/unrecorded/python')]:
            changed = copy.deepcopy(binding)
            changed[field] = value
            self.assertFalse(cohort.named_python_entrypoint('validation', command, changed, binding_raw, raw))
        changed = copy.deepcopy(command)
        changed[0] = '/unrecorded/alias'
        self.assertFalse(cohort.named_python_entrypoint('validation', changed, binding, binding_raw, raw))
        changed = copy.deepcopy(command)
        changed.append('tests.validation.other.Case.test_unrecorded')
        self.assertFalse(cohort.named_python_entrypoint('validation', changed, binding, binding_raw, raw))
        strict = [binding['python_executable'], '-m', 'unittest', '-v', 'tests.validation.other.Case.test_current']
        self.assertTrue(cohort.named_python_entrypoint('validation', strict, binding, b'current', b'current'))
