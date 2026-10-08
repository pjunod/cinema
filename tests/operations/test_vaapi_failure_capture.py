"""Synthetic frame bytes exercise evidence plumbing, never an encoder or GPU."""
import array
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
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
                         '3c5a1bb3d3308ec3460eda45f24519bcf1fd5c5e6196a8a1f68b0f94c3f11ada')
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
        self.assertIn('[ ! -L "$root/container.id" ] && [ -f "$root/container.id" ]', wrapper)
        self.assertIn('^[0-9a-f]{64}$', wrapper)
        self.assertNotIn('docker rm -f "$container"', wrapper)
        self.assertIn('kill "$watcher"', wrapper)
        self.assertIn('for job in $(jobs -p)', wrapper)
        self.assertNotIn('jobs -pr', wrapper)
        self.assertIn('[ "$status" -ne 0 ] &&', wrapper)
        self.assertIn('-le 524288', wrapper)
        self.assertIn('datetime.timedelta(hours=48)', wrapper)
        self.assertIn('rm -f -- "$root/source.yuv" "$root/source.mkv" "$root/encoded.mp4" "$root/decoded.yuv"', wrapper)
        # Run only startup/cleanup with synthetic CLI shims. Docker run fails
        # before watcher, timeout/python qualification, media or device work.
        self.assertIsNotNone(shutil.which('timeout'), 'CI cleanup control needs GNU timeout')
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            for kind in ('regular', 'symlink'):
                with self.subTest(cidfile=kind):
                    case = directory / kind
                    fakebin = case / 'bin'
                    scratch = case / 'scratch'
                    fakebin.mkdir(parents=True)
                    scratch.mkdir(mode=0o700)
                    scripts = {
                        'curl': '#!/bin/sh\necho plurx_transcode_sessions_active 0\n',
                        'stat': '#!/bin/sh\necho 0\n',
                        'mktemp': '#!/bin/sh\nprintf "%s\\n" "$FAKE_CAPTURE_ROOT"\n',
                        'docker': '''#!/usr/bin/env python3
import json,os,pathlib,sys
args=sys.argv[1:]
with open(os.environ['FAKE_CALLS'],'a') as log:
    log.write(json.dumps(args)+'\\n')
if args[0]=='inspect': print('synthetic-image-only')
elif args[0]=='exec': print('0'*64+'  synthetic-ffmpeg')
elif args[0]=='run':
    cidfile=pathlib.Path(args[args.index('--cidfile')+1])
    cid='a'*64
    if os.environ['FAKE_CID_KIND']=='symlink':
        target=cidfile.parent.parent/'untrusted-cid'
        target.write_text(cid+'\\n')
        cidfile.symlink_to(target)
    else: cidfile.write_text(cid+'\\n')
    print('nonempty-partial-cid')
    sys.exit(9)
elif args[0]=='rm': sys.exit(17)
else: sys.exit(99)
''',
                    }
                    for name, content in scripts.items():
                        tool = fakebin / name
                        tool.write_text(content)
                        tool.chmod(0o700)
                    calls = case / 'calls.jsonl'
                    environment = dict(os.environ, PATH=str(fakebin)+os.pathsep+os.environ['PATH'],
                                       FAKE_CAPTURE_ROOT=str(scratch), FAKE_CALLS=str(calls),
                                       FAKE_CID_KIND=kind)
                    result = subprocess.run(['bash', str(WRAPPER)], env=environment,
                                            capture_output=True, timeout=15)
                    self.assertEqual(result.returncode, 9, result.stderr.decode(errors='replace'))
                    receipt = json.loads((scratch/'retention.json').read_text())
                    self.assertEqual(receipt['exit_status'], 9)
                    self.assertFalse(receipt['media_retained'])
                    self.assertFalse(receipt['cleanup']['confirmed'])
                    self.assertEqual(receipt['cleanup']['watcher_result'], 'not_started')
                    self.assertFalse((scratch/'qualification.json').exists())
                    commands = [json.loads(line) for line in calls.read_text().splitlines()]
                    removals = [command for command in commands if command[0]=='rm']
                    if kind == 'regular':
                        self.assertEqual(removals, [['rm', '-f', 'a'*64]])
                        self.assertEqual(receipt['cleanup']['container_id'], 'a'*64)
                        self.assertEqual(receipt['cleanup']['identity_source'], 'private_cidfile')
                        self.assertEqual(receipt['cleanup']['container_result'], 'failed_or_unconfirmed')
                        self.assertEqual(receipt['cleanup']['remove_exit'], 17)
                        self.assertTrue((scratch/'container.id').is_file())
                    else:
                        self.assertEqual(removals, [])
                        self.assertIsNone(receipt['cleanup']['container_id'])
                        self.assertEqual(receipt['cleanup']['container_result'], 'identity_unresolved')


if __name__ == '__main__':
    unittest.main()
