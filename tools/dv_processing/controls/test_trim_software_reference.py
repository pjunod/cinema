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
from compare_trim_software_reference import check_selected, neutral_l3_authoring
from trim_software_metrics import (acquisition, bind, comparison, metrics,
    non_l8_rpu_identity, packet_file, rgb16, rgba32, sha, xml_contract, xyz, MAX_PACKET_FILE_BYTES)


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

    def test_companion_l2_is_variable_only_in_explicit_mixed_scope(self):
        parsed = {'rpu_data_crc32': 1, 'vdr_dm_data': {'cmv40_metadata': {'ext_metadata_blocks': [
            {'Level2': {'trim_slope': 2048}}, {'Level8': {'trim_slope': 2048}},
            {'Level3': {'avg_pq_offset': 1500}}]}}}
        strict = non_l8_rpu_identity(parsed)
        mixed = non_l8_rpu_identity(parsed, ('Level2', 'Level8'))
        parsed['vdr_dm_data']['cmv40_metadata']['ext_metadata_blocks'][0]['Level2']['trim_slope'] = 2100
        self.assertNotEqual(non_l8_rpu_identity(parsed), strict)
        self.assertEqual(non_l8_rpu_identity(parsed, ('Level2', 'Level8')), mixed)
        parsed['vdr_dm_data']['cmv40_metadata']['ext_metadata_blocks'][2]['Level3']['avg_pq_offset'] = 1501
        self.assertNotEqual(non_l8_rpu_identity(parsed, ('Level2', 'Level8')), mixed)

    def test_neutral_l3_authoring_accepts_only_four_offsets(self):
        def dynamic():
            return ('<PluginNode><DVDynamicData><Level1><ImageCharacter>1</ImageCharacter></Level1>'
                    '<Level2><Trim>2</Trim></Level2>'
                    '<Level3><L1Offset>0 -0.178274 0</L1Offset></Level3>'
                    '<Level8><L8Trim>3</L8Trim></Level8></DVDynamicData></PluginNode>')
        parent = '<Root><Video><Track><Shot>' + dynamic() + ''.join(
            '<Frame>' + dynamic() + '</Frame>' for _ in range(3)) + '</Shot></Track></Video></Root>'
        neutral = parent.replace('0 -0.178274 0', '0 0 0')
        record = {'replaced_l3_nodes': 4, 'input_l3_offsets': ['0 -0.178274 0'] * 4,
                  'output_l3_offsets': ['0 0 0'] * 4}
        with tempfile.TemporaryDirectory(dir=os.environ['PLURX_DV_CONTROL_SCRATCH']) as folder:
            before, after = Path(folder)/'before.xml', Path(folder)/'after.xml'
            before.write_text(parent)
            after.write_text(neutral)
            self.assertEqual(neutral_l3_authoring(before, after, record), ['0 0 0'] * 4)
            mutations = [
                neutral.replace('<Level3><L1Offset>0 0 0</L1Offset></Level3>', '', 1),
                neutral.replace('<Level3><L1Offset>0 0 0</L1Offset></Level3>',
                                '<Level3><L1Offset>0 0 0</L1Offset></Level3>' * 2, 1),
                neutral.replace('<L1Offset>0 0 0</L1Offset>', '<L1Offset>0 0.1 0</L1Offset>', 1),
                neutral.replace('<ImageCharacter>1</ImageCharacter>', '<ImageCharacter>9</ImageCharacter>', 1),
                neutral.replace('<Trim>2</Trim>', '<Trim>9</Trim>', 1),
                neutral.replace('<L8Trim>3</L8Trim>', '<L8Trim>9</L8Trim>', 1),
            ]
            for xml in mutations:
                after.write_text(xml)
                with self.assertRaises(ValueError):
                    neutral_l3_authoring(before, after, record)

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
                'exit_code': 0, 'binary_sha256': '0'*64, 'argv': ['synthetic-test']}],
                'files': {'data': bind(root/'data')}}
            (root/'RESULT.json').write_text(json.dumps(result))
            (root/'OUTER-RESULT.json').write_text('{"exit_code":1}')
            transfer = {name: bind(root/name) for name in ('RESULT.json', 'OUTER-RESULT.json', 'data')}
            (root/'TRANSFER-SHA256.json').write_text(json.dumps(transfer))
            with self.assertRaises(ValueError):
                acquisition(root)

    def source_packet(self, root):
        # Valid uncompressed 1x1 RGB16 TIFFs; acquisition checks happen before
        # image decoding, so the counterexample changes actual pixel payloads.
        tags = [(256, 4, 1, 1), (257, 4, 1, 1), (258, 3, 3, 134),
                (259, 3, 1, 1), (262, 3, 1, 2), (273, 4, 1, 140),
                (277, 3, 1, 3), (278, 4, 1, 1), (279, 4, 1, 6), (284, 3, 1, 1)]
        image = b'II' + struct.pack('<HIH', 42, 8, len(tags))
        image += b''.join(struct.pack('<HHII', *tag) for tag in tags)
        image += struct.pack('<IHHHHHH', 0, 16, 16, 16, 0, 32768, 65535)
        (root/'inputs').mkdir()
        for index in range(3):
            (root/'inputs'/f'frame_{index:06}.tif').write_bytes(image)
        (root/'INPUT-MANIFEST.json').write_text(json.dumps({
            'source_sha256': sha(root/'inputs/frame_000000.tif')}))
        files = {str(p.relative_to(root)): bind(p) for p in root.rglob('*') if p.is_file()}
        result = {'status': 'pass', 'files': files, 'commands': [{
            'name': 'synthetic-artifact-boundary-test', 'exit_code': 0,
            'binary_sha256': '0'*64, 'argv': ['synthetic-artifact-boundary-test']}]}
        (root/'RESULT.json').write_text(json.dumps(result))
        (root/'OUTER-RESULT.json').write_text('{"exit_code":0}')
        self.transfer_packet(root)

    def transfer_packet(self, root):
        files = {str(p.relative_to(root)): bind(p) for p in root.rglob('*')
                 if p.is_file() and p.name != 'TRANSFER-SHA256.json'}
        (root/'TRANSFER-SHA256.json').write_text(json.dumps(files))

    def test_recomputed_transfer_cannot_replace_acquired_source_pixels(self):
        with tempfile.TemporaryDirectory(dir=os.environ['PLURX_DV_CONTROL_SCRATCH']) as folder:
            root = Path(folder); self.source_packet(root)
            acquisition(root)  # The original source/receipt/transport tuple is valid.
            receipt = sha(root/'RESULT.json')
            for index in range(3):
                path = root/'inputs'/f'frame_{index:06}.tif'
                path.write_bytes(path.read_bytes()[:-6] + struct.pack('<HHH', 100, 200, 300))
            manifest = root/'INPUT-MANIFEST.json'
            manifest.write_text(json.dumps({'source_sha256': sha(root/'inputs/frame_000000.tif')}))
            self.transfer_packet(root)  # Attacker updates every transport binding.
            self.assertEqual(sha(root/'RESULT.json'), receipt)
            with self.assertRaisesRegex(ValueError, 'immutable acquisition receipt'):
                acquisition(root)

    def test_outer_result_requires_explicit_transfer_coverage(self):
        with tempfile.TemporaryDirectory(dir=os.environ['PLURX_DV_CONTROL_SCRATCH']) as folder:
            root = Path(folder); self.source_packet(root)
            path = root/'TRANSFER-SHA256.json'
            transfer = json.loads(path.read_text()); del transfer['OUTER-RESULT.json']
            path.write_text(json.dumps(transfer))
            self.assertEqual(json.loads((root/'OUTER-RESULT.json').read_text()), {'exit_code': 0})
            with self.assertRaisesRegex(ValueError, 'coverage requires'):
                acquisition(root)

    def test_packet_records_bound_size_and_reject_parent_paths(self):
        with tempfile.TemporaryDirectory(dir=os.environ['PLURX_DV_CONTROL_SCRATCH']) as folder:
            root = Path(folder); (root/'data').write_bytes(b'x')
            record = bind(root/'data')
            with self.assertRaisesRegex(ValueError, 'contained'):
                packet_file(root, '../data', record)
            record['size'] = MAX_PACKET_FILE_BYTES + 1
            with self.assertRaisesRegex(ValueError, 'bounded'):
                packet_file(root, 'data', record)


if __name__ == '__main__':
    unittest.main()
