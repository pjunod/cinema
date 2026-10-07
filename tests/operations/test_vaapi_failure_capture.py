"""Synthetic frame bytes exercise evidence plumbing, never an encoder or GPU."""
import array
import hashlib
from pathlib import Path
import runpy
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CAPTURE = ROOT / 'docs/evidence/video-quality-2026-10-03/qualify-vaapi.py'
WRAPPER = ROOT / 'docs/evidence/video-quality-2026-10-03/run-vaapi.sh'


def write_frame(path, luma):
    samples = array.array('H', luma + [512] * (len(luma)//2))
    if sys.byteorder != 'little':
        samples.byteswap()
    path.write_bytes(samples.tobytes())


class VaapiFailureCaptureTests(unittest.TestCase):
    def setUp(self):
        # Generic receipts bind this fixture AST, not transitive imports. Change
        # these witnesses with the harness so only this control family reruns.
        self.assertEqual(hashlib.sha256(CAPTURE.read_bytes()).hexdigest(),
                         'd00241df59f8ea5f8fe0c4f33446b620ad2b937f5f0066baacc7aa1ded92802a')
        self.assertEqual(hashlib.sha256(WRAPPER.read_bytes()).hexdigest(),
                         '47c1dd9804396a87ad05e56fbf8e7280e686422fead85ae9f602a7e692581f40')
        self.capture = runpy.run_path(str(CAPTURE))

    def test_failed_luma_keeps_full_identity_coordinates_and_reference_regions(self):
        capture = self.capture
        with tempfile.TemporaryDirectory() as directory:
            reference, decoded = [Path(directory)/name for name in ('source.yuv', 'decoded.yuv')]
            before = [64] * 16
            after = before.copy()
            after[5] += 35
            after[0] += 35
            write_frame(reference, before)
            write_frame(decoded, after)
            result = capture['measure_frame'](reference, decoded, 4, 4)
            self.assertEqual(result['max_error_10bit_codes'], 35)
            self.assertEqual(result['max_error_pixel_count'], 2)
            self.assertEqual(result['max_error_coordinates'], [
                dict(x=0, y=0, reference=64, decoded=99),
                dict(x=1, y=1, reference=64, decoded=99)])
            self.assertEqual(result['regions']['reference_constant_interior']['pixels'], 4)
            self.assertEqual(result['regions']['reference_constant_interior']['max_error_10bit_codes'], 35)
            self.assertEqual(result['regions']['reference_transition_or_border']['pixels'], 12)
            self.assertEqual(result['mae_10bit_codes'], 70/16)
            identity = capture['artifact'](decoded)
            self.assertEqual(identity['bytes'], 48)
            self.assertEqual(identity['sha256'], hashlib.sha256(decoded.read_bytes()).hexdigest())

    def test_frame_alignment_refuses_truncated_extra_geometry_and_non_tenbit_bytes(self):
        capture = self.capture
        with tempfile.TemporaryDirectory() as directory:
            reference, decoded = [Path(directory)/name for name in ('source.yuv', 'decoded.yuv')]
            write_frame(reference, [64] * 16)
            write_frame(decoded, [64] * 16)
            original = decoded.read_bytes()
            for damaged in (original[:-2], original + original, original[:-1]):
                with self.subTest(bytes=len(damaged)):
                    decoded.write_bytes(damaged)
                    with self.assertRaisesRegex(ValueError, 'exactly one'):
                        capture['measure_frame'](reference, decoded, 4, 4)
            decoded.write_bytes(original)
            for width, height in ((3, 4), (4, 3), (0, 4), (1922, 4), (4, 1082)):
                with self.subTest(width=width, height=height), self.assertRaisesRegex(ValueError, 'geometry'):
                    capture['measure_frame'](reference, decoded, width, height)
            write_frame(decoded, [1024] + [64] * 15)
            with self.assertRaisesRegex(ValueError, 'ten-bit range'):
                capture['measure_frame'](reference, decoded, 4, 4)

    def test_capture_precedes_bar_and_wrapper_owns_cleanup_and_failed_retention(self):
        source = CAPTURE.read_text()
        assertion = "assert report['luma']['mae_10bit_codes']<4 and report['luma']['max_error_10bit_codes']<32"
        self.assertLess(source.index("report['artifacts']={path.name:"), source.index(assertion))
        self.assertLess(source.index("report['output']={'sha256'"), source.index(assertion))
        self.assertLess(source.index("report['luma']=measure_frame"), source.index(assertion))
        wrapper = WRAPPER.read_text()
        self.assertIn('mktemp -d /var/tmp/plurx-vaapi-qualification.XXXXXXXX', wrapper)
        self.assertLess(wrapper.index('trap cleanup EXIT'), wrapper.index('container_id=$(docker run'))
        self.assertIn('docker rm -f "$container_id"', wrapper)
        self.assertIn('--cidfile "$root/container.id"', wrapper)
        self.assertIn('read -r container_id < "$root/container.id"', wrapper)
        self.assertIn('^['+'0-9a-f]{64}$', wrapper)
        self.assertNotIn('docker rm -f "$container"', wrapper)
        self.assertIn('kill "$watcher"', wrapper)
        self.assertIn('[ "$status" -ne 0 ] &&', wrapper)
        self.assertIn('-le 524288', wrapper)
        self.assertIn('datetime.timedelta(hours=48)', wrapper)
        self.assertIn('rm -f -- "$root/source.yuv" "$root/source.mkv" "$root/encoded.mp4" "$root/decoded.yuv"', wrapper)


if __name__ == '__main__':
    unittest.main()
