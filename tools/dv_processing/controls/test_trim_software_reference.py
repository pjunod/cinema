"""Authored boundary/color-QC regressions; coordinator runs only after review.

These engineered checks exercise QC arithmetic/associations, not vendor output.
Scratch must be an explicitly supplied durable, owned directory.
"""
import json
import math
import os
from pathlib import Path
import struct
import tempfile
import unittest

from annex_b2 import oetf
from compare_trim_software_reference import check_selected
from trim_software_metrics import (acquisition, bind, comparison, metrics,
    non_l8_rpu_identity, packet_file, rgb16, rgba32, xml_contract, xyz)


class SoftwareReferenceTests(unittest.TestCase):
    def test_baseline_error_is_separate_from_trim_delta(self):
        candidate_zero, reference_zero = [(0.1, .2, .3)], [(.2, .3, .4)]
        result = comparison([(.3, .4, .5)], [(.4, .5, .6)], candidate_zero, reference_zero)
        self.assertGreater(result['absolute_error']['channels'][0]['maximum_absolute'], .09)
        self.assertLess(result['trim_delta_error']['channels'][0]['maximum_absolute'], 1e-15)
        zero = comparison(candidate_zero, reference_zero, candidate_zero, reference_zero)
        self.assertEqual(zero['candidate_trim_delta']['channels'][0]['maximum_absolute'], 0)
        self.assertEqual(zero['reference_trim_delta']['channels'][0]['maximum_absolute'], 0)
        with self.assertRaises(ValueError):
            comparison([], [], [], [])

    def test_xyz_uses_public_primaries_and_absolute_pq_light(self):
        # Independent D65 chromaticity witness: white has X/Y=x/y, Z/Y=(1-x-y)/y.
        nits = 100
        out = xyz([(oetf(nits),) * 3])[0]
        self.assertAlmostEqual(out[0], nits*.3127/.329, places=7)
        self.assertAlmostEqual(out[1], nits, places=7)
        self.assertAlmostEqual(out[2], nits*(1-.3127-.329)/.329, places=7)
        # Red/green/blue must remain different XYZ directions, not Y-only scores.
        basis = xyz([(oetf(100), 0, 0), (0, oetf(100), 0), (0, 0, oetf(100))])
        self.assertGreater(basis[0][0], basis[0][1])
        self.assertGreater(basis[1][1], basis[1][0])
        self.assertGreater(basis[2][2], basis[2][1])

    def test_raster_length_order_and_nonfinite_refuse(self):
        self.assertEqual(rgb16(struct.pack('<HHH', 0, 32768, 65535), 1, 1),
                         [(0, 32768/65535, 1)])
        with self.assertRaises(ValueError):
            rgb16(b'partial', 2, 2)
        with self.assertRaises(ValueError):
            rgba32(struct.pack('<ffff', math.nan, 0, 0, 1), 1, 1)
        with self.assertRaises(ValueError):
            metrics([])

    def test_candidate_must_select_converted_actual_rpu(self):
        block = {'target_display_index': 255, 'trim_slope': 2048, 'trim_offset': 2048,
                 'trim_power': 2048, 'trim_chroma_weight': 2048,
                 'trim_saturation_gain': 2048, 'ms_weight': 2048}
        parsed = {'vdr_dm_data': {'cmv40_metadata': {'ext_metadata_blocks': [
            {'Level8': block}, {'Level10': {'target_display_index': 255, 'target_max_pq': 3079}}]}}}
        event = {'trim_words': [1, 8, 3079, 255, 10] + [2048]*6}
        check_selected(event, parsed)
        event['trim_words'][3] = 49
        with self.assertRaises(ValueError):
            check_selected(event, parsed)

    def test_non_l8_metadata_must_stay_frozen(self):
        parsed = {'rpu_data_crc32': 1, 'vdr_dm_data': {'cmv40_metadata': {'ext_metadata_blocks': [
            {'Level8': {'trim_slope': 2048}}, {'Level3': {'avg_pq_offset': 1500}}]}}}
        baseline = non_l8_rpu_identity(parsed)
        parsed['rpu_data_crc32'] = 2
        parsed['vdr_dm_data']['cmv40_metadata']['ext_metadata_blocks'][0]['Level8']['trim_slope'] = 2100
        self.assertEqual(non_l8_rpu_identity(parsed), baseline)
        parsed['vdr_dm_data']['cmv40_metadata']['ext_metadata_blocks'][1]['Level3']['avg_pq_offset'] = 1501
        self.assertNotEqual(non_l8_rpu_identity(parsed), baseline)

    def test_acquired_xml_target_and_frame_association_are_frozen(self):
        import xml.etree.ElementTree as ET
        original = Path(os.environ['PLURX_DV_REFERENCE_PACKET'])/'cases/zero/metadata.xml'
        xml_contract(original, [0]*6)
        with tempfile.TemporaryDirectory(dir=os.environ['PLURX_DV_CONTROL_SCRATCH']) as folder:
            tree = ET.parse(original)
            # Same frame bytes with a different Record.In are not the same source association.
            tree.getroot().find('.//{*}Shot/{*}Record/{*}In').text = '1'
            path = Path(folder)/'mutated.xml'; tree.write(path)
            with self.assertRaises(ValueError):
                xml_contract(path, [0]*6)

    def test_hash_and_true_exit_are_required(self):
        with tempfile.TemporaryDirectory(dir=os.environ['PLURX_DV_CONTROL_SCRATCH']) as folder:
            root = Path(folder)
            (root/'data').write_bytes(b'synthetic test artifact')
            bound = bind(root/'data')
            packet_file(root, 'data', bound)
            (root/'data').write_bytes(b'mutated test artifact')
            with self.assertRaises(ValueError):
                packet_file(root, 'data', bound)
            result = {'status': 'pass', 'commands': [{'name': 'synthetic-test',
                'exit_code': 0, 'binary_sha256': '0'*64, 'argv': ['synthetic-test']}]}
            (root/'RESULT.json').write_text(json.dumps(result))
            (root/'OUTER-RESULT.json').write_text('{"exit_code":1}')
            transfer = {name: bind(root/name) for name in ('RESULT.json', 'OUTER-RESULT.json')}
            (root/'TRANSFER-SHA256.json').write_text(json.dumps(transfer))
            with self.assertRaises(ValueError):
                acquisition(root)


if __name__ == '__main__':
    unittest.main()
