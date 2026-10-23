"""Qualify production L1 -> BT.709 gamma2.4 limited export on grayscale/colors.

Engineering sample comparison only, never a display/creative-trim reference.
Four constant-color frames prove primary conversion independently of the neutral
ramp. Every color is strictly inside BT.709 after linear conversion, so the
production pl_gamut_map_clip policy is inactive. These controls do not accept
out-of-gamut hue preservation, optical/display accuracy or creative trims.
Run once after compilation and independent review, with explicit native binaries.
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

from annex_b2 import eotf, from_l1, oetf


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()



# Primary source: ITU-R BT.2407-0 (2017), section 2.2, equation (1), page 2.
# https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2407-2017-PDF-E.pdf
# The report explicitly rounds the XYZ matrices to four decimal places.
# Solve M709 * rgb709 = M2020 * rgb2020 here; no candidate/library matrix,
# shader constant or library-generated LUT supplies the expected conversion.
XYZ_2020 = ((.6370, .1446, .1689), (.2627, .6780, .0593), (0., .0281, 1.0610))
XYZ_709 = ((.4124, .3576, .1805), (.2126, .7152, .0722), (.0193, .1192, .9505))
# BT.709-6 (2015), Part 1 items 3.2--3.4: luma, color difference and codes.
# https://www.itu.int/dms_pubrec/itu-r/rec/bt/R-REC-BT.709-6-201506-I!!PDF-E.pdf
# Gamma 2.4 here is the requested display EOTF, not BT.709's camera OETF.


def rgb2020_to_709(rgb):
    xyz = [sum(a*b for a,b in zip(row,rgb,strict=True)) for row in XYZ_2020]
    rows = [list(row) + [value] for row,value in zip(XYZ_709,xyz,strict=True)]
    for col in range(3):
        pivot = max(range(col,3),key=lambda row:abs(rows[row][col]))
        rows[col],rows[pivot] = rows[pivot],rows[col]
        divisor = rows[col][col]
        if abs(divisor) < 1e-12:
            raise ValueError('singular public XYZ matrix')
        rows[col] = [v/divisor for v in rows[col]]
        for row in range(3):
            if row != col:
                scale = rows[row][col]
                rows[row] = [a-scale*b for a,b in zip(rows[row],rows[col],strict=True)]
    return tuple(row[3] for row in rows)


def limited_709_codes(rgb_nits):
    # In-gamut proof is an obligation of each color input, not an oracle clip.
    assert all(0 < value < 100 for value in rgb_nits), rgb_nits
    r,g,b = (value/100 for value in rgb_nits)
    r,g,b = (value ** (1/2.4) for value in (r,g,b))
    y = .2126*r + .7152*g + .0722*b
    cb,cr = (b-y)/1.8556,(r-y)/1.5748
    return tuple(math.floor(code+.5) for code in (64+876*y,512+896*cb,512+896*cr))


def color_controls(shader,root):
    cases = []
    # Non-neutral linear BT.2020 nits, bounded below this L1 max (~92 nits).
    # The L1 mapping is no-expansion, isolating primaries/gamma/YUV conversion.
    patches = {'warm':(50.,20.,15.),'green':(20.,40.,15.),
               'blue':(15.,20.,50.),'muted':(45.,40.,30.)}
    for name,linear in patches.items():
        name = 'color-' + name
        pq = tuple(oetf(value) for value in linear)
        data = struct.pack('<4f',*pq,1.) * 4096
        source = root / f'{name}.rgba32f'; source.write_bytes(data)
        rounded = struct.unpack('<4f',data[:16])[:3]
        # Reconstruct exactly the binary32 values uploaded by the adapter.
        source_nits = tuple(eotf(value) for value in rounded)
        curve = from_l1(2048,1800,100,50,sdr=True)
        assert curve.mode == 'no-expansion' and max(source_nits) < curve.peak
        mapped = tuple(curve.nits(value) for value in source_nits)
        converted = rgb2020_to_709(mapped)
        expected_codes = limited_709_codes(converted)
        # An export that only tags BT.2020 RGB as BT.709 must fail strongly.
        wrong_codes = limited_709_codes(mapped)
        wrong_error = max(abs(a-b) for a,b in zip(expected_codes,wrong_codes,strict=True))
        assert wrong_error >= 8, (name,'weak primary-conversion witness',wrong_error)
        expected = [expected_codes[0]]*4096 + [expected_codes[1]]*1024 + [expected_codes[2]]*1024
        output = root / f'{name}.yuv'
        result = subprocess.run([str(shader),'bt709-gamma24-target-100','50','4000',
            '2048','1800',str(source),str(output)],capture_output=True,timeout=120)
        (root / f'{name}.stdout').write_bytes(result.stdout)
        (root / f'{name}.stderr').write_bytes(result.stderr)
        if result.returncode:
            raise RuntimeError(f'{name}: exit {result.returncode}')
        events = [json.loads(line) for line in result.stdout.splitlines()]
        assert len(events) == 1 and events[0]['kind'] == 'display_target_frame'
        event = events[0]
        assert (event['nits'],event['max_pq'],event['avg_pq'],event['curve']) == (100,2048,1800,'no-expansion')
        actual = array('H',output.read_bytes())
        if sys.byteorder != 'little': actual.byteswap()
        assert len(actual) == len(expected) == 6144
        worst = max(abs(a-b) for a,b in zip(actual,expected,strict=True))
        assert worst <= 2,(name,worst,expected_codes)
        assert all(64 <= value <= 940 for value in actual[:4096])
        assert all(64 <= value <= 960 for value in actual[4096:])
        assert max(abs(value-512) for value in actual[4096:]) > 8
        cases.append({'name':name,'exit':result.returncode,'samples':len(actual),
            'linear_bt2020_nits':source_nits,'linear_bt709_nits':converted,
            'expected_yuv_codes':expected_codes,'wrong_primaries_yuv_codes':wrong_codes,
            'wrong_primaries_error_codes':wrong_error,'maximum_error_codes':worst,
            'gamut_policy':'production clip; inactive for these strictly in-gamut colors',
            'trace':event,'output_sha256':sha(output)})
    assert len(cases) == 4
    return cases

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
    cases.extend(color_controls(shader,root))
    assert len(cases) == 10
    receipt = {'shader_sha256':sha(shader), 'controller_sha256':sha(__file__),
        'oracle_sha256':sha(Path(__file__).with_name('annex_b2.py')), 'cases':cases,
        'scope':'synthetic grayscale and four in-gamut color L1/primaries/gamma/limited export controls; out-of-gamut quality, real-title, device and creative-trim acceptance remain separate'}
    (root / 'receipt.json').write_text(json.dumps(receipt,indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--shader',required=True,type=Path)
    parser.add_argument('--output',required=True,type=Path)
    args = parser.parse_args()
    receipt = run(args.shader.resolve(strict=True),args.output.resolve())
    print(json.dumps({'cases':len(receipt['cases']),'receipt':str(args.output / 'receipt.json')}))
