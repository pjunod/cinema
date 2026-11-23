"""Command-only source-gate envelopes keep default/release authority strict."""
import unittest
from unittest.mock import patch
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from tests.validation.test_promotion_unit_retention import fixture, log


class SelectedPrCommandOnlyEnvelopeCase(unittest.TestCase):
    def document(self):
        row=fixture();row.update(version=2,lanes={},command_units={
            'version':1,'source_commit':'b'*40,'commands':[{'name':'android_jvm'}],
            'logs':{'e'*64:log('original local record')}})
        return row

    def test_command_only_empty_lanes_never_admit_default_release_or_empty_commands(self):
        from validation import qualification_unit_continuation as commands
        for selected_pr,empty in [(False,False),(True,True)]:
            row=self.document()
            if empty:row['command_units']['commands']=[]
            with patch.object(commands,'validate') as validator,self.assertRaises(receipts.ReceiptError):
                retention.validate(row,'b'*40,differences=lambda *_:set(),selected_pr=selected_pr)
            validator.assert_not_called()

    def test_selected_command_only_envelope_requires_real_complete_command_validation(self):
        from validation import qualification_unit_continuation as commands
        row=self.document();expected={'android_jvm':{'mode':'validated-complete-current-union'}}
        with patch.object(commands,'validate',return_value=expected) as validator:
            self.assertEqual(retention.validate(row,'b'*40,differences=lambda *_:set(),selected_pr=True),({},expected))
            validator.assert_called_once_with(row['command_units'],'b'*40,existing={},selected_pr=True)
        with patch.object(commands,'validate',side_effect=receipts.ReceiptError('missing actual coverage')):
            with self.assertRaises(receipts.ReceiptError):
                retention.validate(row,'b'*40,differences=lambda *_:set(),selected_pr=True)
