"""Actual process failure after complete decoder writes must not become status0."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest
import check_timeline

SOURCE = Path(sys.argv[1]).resolve()


class DriverTest(unittest.TestCase):
    def run_control(self, case, fault):
        root = SOURCE / 'driver-controls' / f'{case}-{fault}'
        root.mkdir(parents=True, exist_ok=False)
        for path in SOURCE.glob('compound-*.mkv'):
            shutil.copyfile(path, root / path.name)
        wrapper = root / 'fault-decoder.sh'
        wrapper.write_text('''#!/bin/sh
set -eu
/work/decode_timeline "$@"
if [ "$2" = "$TIMELINE_FAULT_CASE/frames" ]; then
    if [ "$TIMELINE_FAULT_KIND" = crash ]; then kill -SEGV $$; fi
    exit 37
fi
''')
        wrapper.chmod(0o755)
        environment = dict(os.environ, TIMELINE_TEST_DECODER=str(wrapper),
                           TIMELINE_FAULT_CASE=case, TIMELINE_FAULT_KIND=fault)
        with (root / 'driver.stdout').open('wb') as out, (root / 'driver.stderr').open('wb') as err:
            result = subprocess.run(['sh', '/work/decode_cases.sh'], cwd=root, env=environment,
                                    stdout=out, stderr=err)
        wanted = 139 if fault == 'crash' else 37
        self.assertEqual(result.returncode, wanted)
        actual_status = int((root / case / 'decode-status.txt').read_text())
        self.assertEqual(actual_status, wanted)
        self.assertFalse((root / 'decoder-driver-passed.txt').exists())
        with self.assertRaises(check_timeline.TimelineError):
            check_timeline.assess(root, case)
        trace = root / case / 'decode.jsonl'
        reference = SOURCE / case / 'decode.jsonl'
        self.assertEqual(trace.read_bytes(), reference.read_bytes())
        artifacts = sorted((root / case / 'frames').glob('*'))
        self.assertGreater(len(artifacts), 0)
        evidence = {'case': case, 'fault': fault, 'driver_exit': result.returncode,
                    'actual_decode_status': actual_status, 'complete_trace_matches_real_decode': True,
                    'trace_sha256': hashlib.sha256(trace.read_bytes()).hexdigest(),
                    'artifacts': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in artifacts}}
        (root / 'receipt.json').write_text(json.dumps(evidence, indent=2, allow_nan=False) + '\n')

    def test_implicit_nonzero_after_complete_writes(self):
        self.run_control('implicit-seek', 'nonzero')

    def test_implicit_crash_after_complete_writes(self):
        self.run_control('implicit-seek', 'crash')

    def test_swapped_nonzero_after_complete_writes(self):
        self.run_control('swapped-rpu', 'nonzero')

    def test_swapped_crash_after_complete_writes(self):
        self.run_control('swapped-rpu', 'crash')


if __name__ == '__main__':
    unittest.main(argv=[sys.argv[0]])
