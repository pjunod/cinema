"""Qualify production L1 -> BT.709 gamma2.4 limited export on a grayscale oracle.

Engineering sample comparison only, never a display/creative-trim reference.
Run once after compilation and independent review, with explicit native binaries.
"""
import argparse
from array import array
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys

from annex_b2 import eotf, from_l1


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(shader, root):
    root.mkdir()  # Exclusive evidence directory; stale output is never a pass.
    values = [v for i in range(4096) for v in (i / 4095, i / 4095, i / 4095, 1.)]
    data = struct.pack('<' + 'f' * len(values), *values)
    source = root / 'ramp.rgba32f'
    source.write_bytes(data)
    rounded = struct.unpack('<' + 'f' * len(values), data)[::4]
    cases = []
    sensitivity = {}
    for max_pq, avg_pq, floor in [(4095, 1800, 50), (4095, 2500, 50),
                                   (0, 0, 50), (1000, 500, 50),
                                   (4095, 0, 50), (4095, 1800, 0)]:
        name = f'{max_pq}-{avg_pq}-{floor}'
        output = root / f'{name}.yuv'
        result = subprocess.run([str(shader), 'bt709-gamma24-target-100', str(floor),
            '4000', str(max_pq), str(avg_pq), str(source), str(output)],
            capture_output=True, timeout=120)
        (root / f'{name}.stdout').write_bytes(result.stdout)
        (root / f'{name}.stderr').write_bytes(result.stderr)
        if result.returncode:
            raise RuntimeError(f'{name}: exit {result.returncode}')
        events = [json.loads(line) for line in result.stdout.splitlines()]
        assert len(events) == 1 and events[0]['kind'] == 'display_target_frame'
        event = events[0]
        assert (event['nits'], event['max_pq'], event['avg_pq'], event['title_min_1e4'], event['title_peak']) == (100, max_pq, avg_pq, floor, 4000)
        actual = array('H', output.read_bytes())
        if sys.byteorder != 'little':
            actual.byteswap()
        curve = from_l1(max_pq, avg_pq, 100, floor, sdr=True)
        # Independent physical equation: pure power EOTF, 1.0 = 100 cd/m².
        expected = [round(64 + 876 * (max(0, min(100, curve.nits(eotf(v)))) / 100) ** (1 / 2.4)) for v in rounded] + [512] * 2048
        assert len(actual) == len(expected) == 6144
        worst = max(abs(a-b) for a,b in zip(actual, expected, strict=True))
        assert worst <= 2, (name, worst)
        assert actual[0] == 64 and max(actual[:4096]) <= 940
        assert all(a <= b for a,b in zip(actual[:4095],actual[1:4096]))
        if max_pq == 4095 and floor == 50 and avg_pq in (1800,2500):
            sensitivity[avg_pq] = sha(output)
        cases.append({'name': name, 'exit': result.returncode, 'samples': len(actual),
            'maximum_error_codes': worst, 'trace': event, 'output_sha256': sha(output)})
    assert len(cases) == 6 and sensitivity[1800] != sensitivity[2500]
    receipt = {'shader_sha256':sha(shader), 'controller_sha256':sha(__file__),
        'oracle_sha256':sha(Path(__file__).with_name('annex_b2.py')), 'cases':cases,
        'scope':'synthetic grayscale L1/gamma/limited export; gamut, real-title and display acceptance remain separate'}
    (root / 'receipt.json').write_text(json.dumps(receipt,indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--shader',required=True,type=Path)
    parser.add_argument('--output',required=True,type=Path)
    args = parser.parse_args()
    receipt = run(args.shader.resolve(strict=True),args.output.resolve())
    print(json.dumps({'cases':len(receipt['cases']),'receipt':str(args.output / 'receipt.json')}))
