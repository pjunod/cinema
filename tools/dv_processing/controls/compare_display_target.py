"""Execute actual target shader against independent published-anchor oracle.

Run only in the post-review qualification batch, on native Linux with explicit
pinned binaries/dependencies. No synthetic success records or zero-match passes.
The RGB input is binary32 PQ; the comparison is EVERY Y/U/V 10-bit sample,
including the documented left-sited export filter. No perceptual neighbourhood
allowance can hide a curve error. Source and executable hashes bind the receipt.
"""
import argparse
from array import array
import hashlib
import json
import math
from pathlib import Path
import struct
import subprocess
import sys

from annex_b2 import BUCKETS, from_l1, oetf


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def yuv_reference(rgb):
    # Independent BT.2020 NCL conversion and [1,2,1] x [1,1] left-sited
    # sampling. The matrix is specified by BT.2020, not imported C constants.
    y = [0.2627 * r + 0.6780 * g + 0.0593 * b for r, g, b in rgb]
    out = [math.floor(64 + 876 * v + 0.5) for v in y]
    for channel, factor in [(2, 1.8814), (0, 1.4746)]:
        for row in range(0, 64, 2):
            for col in range(0, 64, 2):
                value = 0.0
                for dy in (0, 1):
                    for dx, weight in ((-1, 1), (0, 2), (1, 1)):
                        at = (row + dy) * 64 + min(63, max(0, col + dx))
                        value += weight * (rgb[at][channel] - y[at]) / 8
                out.append(math.floor(512 + 896 * value / factor + 0.5))
    return out


def run(shader, root):
    root.mkdir()  # Refuse reuse: a stale output is not evidence.
    patterns = {
        'ramp': [(i / 4095,) * 3 for i in range(4096)],
        # Saturated highlight/near-black engineering control, not a display measurement.
        'colors': [(oetf(8000), oetf(150), oetf(1)),
                   (oetf(1), oetf(6000), oetf(100)),
                   (oetf(200), oetf(1), oetf(9000)), (0., 0., 0.)] * 1024,
        'out-of-range': [(-0.1, 0., 1.1), (1.5, -1., 0.5),
                         (-1., -1., -1.), (2., 2., 2.)] * 1024,
    }
    for name, pixels in patterns.items():
        values = [v for pixel in pixels for v in (*pixel, 1.)]
        data = struct.pack('<' + 'f' * len(values), *values)
        (root / f'{name}.rgba32f').write_bytes(data)
        # Oracle uses precisely the binary32 input uploaded, not pre-rounded doubles.
        readback = struct.unpack('<' + 'f' * len(values), data)
        patterns[name] = [readback[i:i+3] for i in range(0, len(values), 4)]
    cases, sensitivity = [], {}
    for nits in BUCKETS:
        for max_pq, avg_pq, floor in [(4095, 1800, 50), (4095, 2500, 50),
                                       (0, 0, 50), (4095, 0, 50),
                                       (4095, 4095, 50), (4095, 1800, 0),
                                       (1000, 500, 50)]:
            curve = from_l1(max_pq, avg_pq, nits, floor)
            for pattern, rgb in patterns.items():
                name = f'{nits}-{max_pq}-{avg_pq}-{floor}-{pattern}'
                output = root / f'{name}.yuv'
                argv = [str(shader), f'bt2020-pq-target-{nits}', str(floor), '4000',
                        str(max_pq), str(avg_pq), str(root / f'{pattern}.rgba32f'), str(output)]
                result = subprocess.run(argv, capture_output=True, timeout=120)
                (root / f'{name}.stdout').write_bytes(result.stdout)
                (root / f'{name}.stderr').write_bytes(result.stderr)
                if result.returncode:
                    raise RuntimeError(f'{name}: shader exit {result.returncode}')
                events = [json.loads(line) for line in result.stdout.splitlines()]
                assert len(events) == 1 and events[0]['kind'] == 'display_target_frame'
                event = events[0]
                assert (event['nits'], event['max_pq'], event['avg_pq'],
                        event['title_min_1e4'], event['title_peak']) == (nits, max_pq, avg_pq, floor, 4000)
                assert event['max0_fallback'] == (max_pq == 0)
                expected_reason = ('annex-b2-rgb' if curve.mode == 'annex-b2-rgb-v1'
                                   else 'no-expansion' if curve.mode == 'no-expansion' else None)
                if expected_reason:
                    assert event['curve'] == expected_reason
                else:
                    assert event['curve'].endswith('-linear')
                actual = array('H', output.read_bytes())
                if sys.byteorder != 'little':
                    actual.byteswap()
                reference = yuv_reference([tuple(curve.pq(v) for v in pixel) for pixel in rgb])
                assert len(actual) == len(reference) == 6144
                worst = max(abs(a - b) for a, b in zip(actual, reference, strict=True))
                assert worst <= 2, (name, worst)
                assert max(actual[:4096]) <= 64 + 876 * oetf(nits) + 1
                if pattern == 'ramp':
                    assert actual[0] == 64, (name, 'zero black shifted')
                    assert all(a <= b for a, b in zip(actual[:4095], actual[1:4096]))
                    if max_pq == 4095 and floor == 50 and avg_pq in (1800, 2500):
                        sensitivity[nits, avg_pq] = sha(output)
                cases.append({'name': name, 'exit': result.returncode, 'samples': len(actual),
                              'maximum_error_codes': worst, 'curve': curve.mode,
                              'trace': event, 'sha256': sha(output)})
        assert sensitivity[nits, 1800] != sensitivity[nits, 2500], 'ignored dynamic L1 average'
    assert cases and all(row['samples'] > 0 for row in cases)
    receipt = {'shader_sha256': sha(shader), 'oracle_sha256': sha(Path(__file__).with_name('annex_b2.py')),
               'controller_sha256': sha(__file__), 'cases': cases, 'case_count': len(cases),
               'scope': 'synthetic post-reconstruction shader; not real-title or display acceptance'}
    (root / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--shader', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    receipt = run(args.shader.resolve(strict=True), args.output.resolve())
    print(json.dumps({'cases': receipt['case_count'], 'receipt': str(args.output / 'receipt.json')}))
