"""Structural sharing of the HDR quality inverse; native controls have a media lane."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
FILTER = ROOT / 'tools/dv_quality/bt2020_pq_rgb48.filter'


class DvRgbConversionSharingTests(unittest.TestCase):
    def test_standalone_proof_uses_the_same_conversion(self):
        import importlib.util
        path = ROOT / 'tools/dv_processing/controls/fel_composition_proof.py'
        spec = importlib.util.spec_from_file_location('fel_composition_proof', path)
        proof = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(proof)
        self.assertEqual(FILTER.read_text().strip(), proof.rgb48_filter('bt2020'))
        self.assertEqual('format=rgb48le', proof.rgb48_filter(None))
        with self.assertRaises(ValueError):
            proof.rgb48_filter('bt709')


if __name__ == '__main__':
    unittest.main()
