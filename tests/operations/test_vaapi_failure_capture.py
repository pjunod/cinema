"""Synthetic frame bytes exercise evidence plumbing, never an encoder or GPU."""
import array
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import signal
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
                         '7df472326256a68074b0a5f26621f50a7b03897c6183f5ed09d6667e8076cf5f')
        self.assertEqual(hashlib.sha256(WRAPPER.read_bytes()).hexdigest(),
                         '3a21930760fef315e25467eda3c4b96984331b41877e9aeda08900fada0b59ab')
        self.capture = runpy.run_path(str(CAPTURE))

    def test_fixed_signal_selection_preserves_ramp_and_original_sharp_reference(self):
        capture = self.capture
        ramp, width, height, count = capture['reference_frame']()
        self.assertEqual((width, height, count, len(ramp)), (1920, 1080, 96, 6220800))
        self.assertEqual(hashlib.sha256(ramp).hexdigest(),
                         '3f78739c3a34985743d3070753a42c4327c54f16999b465ff9b9e83a587417a4')
        sharp, sw, sh, sc = capture['reference_frame']('original-sharp-panels')
        self.assertEqual((sw, sh, sc, len(sharp)), (width, height, count, len(ramp)))
        self.assertEqual(hashlib.sha256(sharp).hexdigest(),
                         '777e283050d76a5d51c723ee0a23d04f2abee492aa02046803441715fb6b17f9')
        values = array.array('H')
        values.frombytes(sharp)
        if sys.byteorder != 'little':
            values.byteswap()
        def pq(nits):
            p = (nits/10000)**(2610/16384)
            return round(64+876*((3424/4096+2413/128*p)/(1+2392/128*p))**(2523/32))
        for panel in range(3):
            for y in (panel*360, panel*360+359):
                for x in (0, 959, 1919):
                    t = x/1919
                    nits = 1000*t if panel == 0 else 10*t if panel == 1 else 20+t
                    self.assertEqual(values[y*width+x], pq(nits))
        self.assertEqual(len(values), width*height*3//2)
        self.assertTrue(all(value == 512 for value in values[width*height:]))
        for invalid in ('', '/tmp/source.yuv', 'sharp', 'continuous-pq-ramp '):
            with self.subTest(signal=invalid), self.assertRaisesRegex(ValueError, 'unsupported'):
                capture['reference_frame'](invalid)
        wrapper = WRAPPER.read_text()
        self.assertIn('signal=${2-continuous-pq-ramp}', wrapper)
        self.assertNotIn('signal=${2:-', wrapper)
        self.assertLess(wrapper.index('case "$signal" in'), wrapper.index('root=$(mktemp'))
        self.assertLess(wrapper.index('case "$signal" in'), wrapper.index('curl -fsS'))
        self.assertLess(wrapper.index('case "$signal" in'), wrapper.index('docker inspect'))
        self.assertIn('recipe=${1:-', wrapper)
        self.assertIn('"$container_id" "$signal"', wrapper)
        source = CAPTURE.read_text()
        self.assertLess(source.index('signal = validate_signal(sys.argv[3]'),
                        source.index("recipe = json.loads"))
        self.assertIn("'diagnostic_signal': {'requested': signal, 'actual': None}", source)
        self.assertIn("report['diagnostic_signal']['actual'] = signal", source)

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
        self.assertIn('for job in $(jobs -pr) $(jobs -ps)', wrapper)
        self.assertNotIn('for job in $(jobs -p);', wrapper)
        self.assertNotIn('for job in $(jobs -pr);', wrapper)
        self.assertIn('if watcher_running; then kill "$watcher"', wrapper)
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

    def test_watcher_job_states_exclude_completed_but_keep_running_and_stopped(self):
        # Exercise the actual Bash helper only: no wrapper admission, Docker,
        # HTTP, media or GPU. Completed exit status must survive the later wait.
        wrapper = WRAPPER.read_text()
        function = wrapper[wrapper.index('watcher_running() {'):wrapper.index('cleanup() {')]
        script = '''set -euo pipefail
watcher=''
cleanup_test() {
 for owned in $(jobs -pr) $(jobs -ps); do
  [ "$owned" != "$watcher" ] || kill -KILL "$owned" 2>/dev/null || true
 done
 [ -z "$watcher" ] || wait "$watcher" 2>/dev/null || true
}
trap cleanup_test EXIT
''' + function + '''
(exit 7) & watcher=$!
for ((n=0;n<200;n++)); do
 kill -0 "$watcher" 2>/dev/null || break
 sleep 0.01
done
if kill -0 "$watcher" 2>/dev/null; then exit 10; fi
if watcher_running; then exit 11; fi
if wait "$watcher"; then exit 12; else [ "$?" -eq 7 ]; fi
watcher=''
echo completed_false_reaped_7
sleep 30 & watcher=$!
watcher_running || exit 13
echo running_true
kill -STOP "$watcher"
stopped=false
for ((n=0;n<200;n++)); do
 # Bash 3.2 without job control can leave a stopped child in jobs -pr.
 # Prove the OS stop independently, then require the actual helper to count it.
 state=$(ps -o stat= -p "$watcher")
 [[ "$state" = *T* ]] && stopped=true
 [ "$stopped" = false ] || break
 sleep 0.01
done
[ "$stopped" = true ] || exit 14
watcher_running || exit 15
echo stopped_true
kill -KILL "$watcher"
if wait "$watcher" 2>/dev/null; then exit 16; else [ "$?" -eq 137 ]; fi
watcher=''
echo stopped_reaped_137
'''
        process = subprocess.Popen(['bash', '-c', script], stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True,
                                   env={'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'LC_ALL': 'C'})
        try:
            stdout, stderr = process.communicate(timeout=8)
        finally:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=2)
            process.stdout.close()
            process.stderr.close()
        self.assertEqual(process.returncode, 0, stderr.decode(errors='replace'))
        self.assertEqual(stdout.decode().splitlines(), [
            'completed_false_reaped_7', 'running_true', 'stopped_true', 'stopped_reaped_137'])


if __name__ == '__main__':
    unittest.main()
