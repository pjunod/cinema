#!/usr/bin/env python3
"""GPU composition stage (fel_compose.h) against the CCM 001 reference
(fel_reference_composer.h), code for code.

Each case runs controls/fel_compose_control and
diagnostics/fel_quality/fel_reference_compose on the same parameters and
layers and compares the composed 4:2:0 planes (hdr_bit_depth codes).
Acceptance: the composed codes are identical on every sample of every case.
The plan's milestone 5.3 allows 1 code (12-bit); this control is stricter
because the stage is integer arithmetic, and because a 1-code allowance
passes a stage that drops clause 5.4.3.3's rounding or the floor of negative
residuals (both found by mutating the shader), which is the half-code bias
milestone 5.2 measured in today's composition.

Cases:
- synthetic: seeded vectors that span what the stage admits: 2-9 pivots,
  polynomial and MMR (orders 1-3) pieces, coefficients up to clause 5.3.3's
  integer-part bounds (polynomial -64..63, MMR -65536..65535) and cases one
  step outside them, which both must refuse, a clamp that binds and one that
  cannot, an EL at
  BL size and at half size, 8- and 10-bit layers, 10- and 12-bit output,
  denominators 15-32, pictures whose edges exercise every filter tap, and
  (odd seeds) each layer's Cb and Cr in one texture, as a mapped decoder
  surface delivers them.
- capture: real frames from a layer capture (capture_layers.c bl.yuv,
  el.yuv, events.jsonl, metadata.jsonl with MMR coefficients); needs numpy.

--control may be the GPU control (controls/fel_compose_control.c) or the
shader's CPU port (controls/fel_compose_cpu_port.py), which takes the same
arguments.

usage: run_fel_compose_parity.py --control BIN --oracle BIN --work DIR
           [--synthetic N] [--capture DIR --frames F ...]... --out RESULT.json
"""
import argparse
import array
import json
import random
import subprocess
import sys
from pathlib import Path

try:  # numpy speeds up 4K captures; synthetic cases need only the standard library
    import numpy as np
except ImportError:
    np = None

ACCEPT_CODES = 0
PLAN_ACCEPT_CODES = 1


def frc1(p):
    """frc1 text from a synthetic parameter dict (the oracle's CLI format)."""
    out = ['frc1', p['denom'], p['bl'], p['el'], p['hdr'], 0]
    for c in range(3):
        m = p['map'][c]
        out += [len(m['pivots']), *m['pivots']]
        for piece in m['pieces']:
            if piece['idc'] == 0:
                coef = piece['coef'] + [0] * (3 - len(piece['coef']))
                out += [0, piece['order'], *coef[:3], 1, 0] + [0] * 21
            else:
                out += [1, 1, 0, 0, 0, piece['order'], piece['coef'][0], *piece['coef'][1:22]]
        n = p['nlq'][c]
        out += [n['offset'], n['slope'], n['threshold'], n['max']]
    return ' '.join(map(str, out)) + '\n'


def synthetic_case(seed):
    rng = random.Random(seed)
    bl = rng.choice([10, 10, 10, 8])
    el = rng.choice([10, 10, 10, 8])
    hdr = rng.choice([12, 12, 12, 10])
    denom = rng.choice([23, 23, 23, el + 5, 32])
    one = 1 << denom
    extreme = rng.random() < 0.2   # coefficients anywhere inside clause 5.3.3's ranges
    def coefficient(scale, mmr=False):
        if extreme:
            bound = (65536 if mmr else 64) * one
            return rng.choice([-bound, bound - 1, rng.randint(-bound, bound - 1)])
        return int(rng.uniform(-scale, scale) * one)
    p = {'denom': denom, 'bl': bl, 'el': el, 'hdr': hdr, 'map': [], 'nlq': []}
    top = (1 << bl) - 1
    for c in range(3):
        n = rng.randint(2, 9)
        inner = sorted(rng.sample(range(1, top), n - 2))
        lo = rng.choice([0, 0, rng.randint(0, 40)])
        hi = rng.choice([top, top, rng.randint(top - 40, top)])
        pivots = sorted([lo] + [min(max(v, lo), hi) for v in inner] + [hi])
        pieces = []
        for _ in range(n - 1):
            if c > 0 and rng.random() < 0.5:
                order = rng.randint(1, 3)
                # The constant carries the level; small cross terms keep most
                # samples inside 0..0xFFFF, so clipping does not hide them.
                constant = int(rng.uniform(0.1, 0.9) * one) if not extreme else coefficient(1, True)
                coef = [constant] + [coefficient(0.15, True) for _ in range(7 * order)]
                pieces.append({'idc': 1, 'order': order, 'coef': coef + [0] * (22 - len(coef))})
            else:
                order = rng.randint(1, 2)
                coef = [coefficient(0.3), int(rng.uniform(0.5, 1.5) * one) if not extreme else coefficient(1)]
                if order == 2:
                    coef.append(coefficient(0.5))
                pieces.append({'idc': 0, 'order': order, 'coef': coef})
        p['map'].append({'pivots': pivots, 'pieces': pieces})
    # One coefficient one step past its clause 5.3.3 bound: both must refuse.
    p['out_of_spec'] = rng.random() < 0.08
    if p['out_of_spec']:
        piece = rng.choice([q for m in p['map'] for q in m['pieces']])
        piece['coef'][0] = (65536 if piece['idc'] else 64) * one
    for c in range(3):
        eltop = (1 << el) - 1
        up = 11 - el
        bind = rng.random() < 0.4
        p['nlq'].append({'offset': rng.choice([1 << (el - 1), rng.randint(0, eltop)]),
                         'slope': rng.randint(0, (1 << denom) - 1) if denom < 51 else rng.randint(0, 1 << 40),
                         'threshold': rng.randint(0, (1 << max(denom - 2, 1)) - 1),
                         'max': rng.randint(1, 1 << (denom - 8)) if bind else (1 << (denom + 1)) - 1})
        assert p['nlq'][-1]['threshold'] < 1 << (63 - up)
    scale = rng.choice([1, 2])
    w, h = rng.choice([(64, 64), (96, 64), (128, 72), (36, 20), (40, 8)])
    if scale == 2:
        w, h = 4 * (w // 4), 4 * (h // 4)
    ew, eh = w // scale, h // scale
    def picture(width, height, depth, centre=None, spread=None):
        top_code = (1 << depth) - 1
        planes = []
        for c in range(3):
            pw, ph = (width, height) if c == 0 else (width // 2, height // 2)
            if centre is None:
                plane = [rng.randint(0, top_code) for _ in range(pw * ph)]
            else:
                plane = [min(max(centre[c] + rng.randint(-spread, spread), 0), top_code) for _ in range(pw * ph)]
            planes += plane
        return array.array('H', planes)
    blp = picture(w, h, bl)
    centres = [n['offset'] for n in p['nlq']]
    elp = picture(ew, eh, el, centres if rng.random() < 0.7 else None, rng.choice([2, 20, 300]))
    return p, (w, h, ew, eh), blp, elp


def read_codes(path):
    if np is not None:
        return np.fromfile(path, dtype='<u2').astype(np.int64)
    codes = array.array('H')
    codes.frombytes(path.read_bytes())
    if sys.byteorder != 'little':
        codes.byteswap()
    return codes


def compare(a, b, top):
    if np is not None:
        d = np.abs(a - b)
        return {'samples': int(b.size), 'max_code_diff': int(d.max()), 'differing': int((d > 0).sum()),
                'distinct_codes': int(np.unique(b).size), 'clipped_fraction': float(((b == 0) | (b == top)).mean())}
    d = [abs(x - y) for x, y in zip(a, b)]
    return {'samples': len(b), 'max_code_diff': max(d), 'differing': sum(1 for x in d if x),
            'distinct_codes': len(set(b)), 'clipped_fraction': sum(1 for x in b if x in (0, top)) / len(b)}


def run(args, name, params, bl_path, el_path, frame, geometry, expect_refusal=False, interleaved=False):
    w, h, ew, eh = geometry
    work = args.work
    pfile = work / f'{name}.params'
    pfile.write_text(params)
    gpu_out, ref_out = work / f'{name}.gpu.raw', work / f'{name}.ref.raw'
    common = [str(pfile), str(bl_path), str(el_path), str(frame), str(w), str(h), str(ew), str(eh)]
    ref = subprocess.run([str(args.oracle), *common, str(ref_out)], capture_output=True, text=True, timeout=900)
    # interleaved: each layer's Cb and Cr in one texture, as a mapped P010
    # decoder surface reaches the stage.
    gpu = subprocess.run([str(args.control), *common, str(gpu_out)] + (['interleaved'] if interleaved else []),
                         capture_output=True, text=True, timeout=900)
    if ref.returncode or gpu.returncode or expect_refusal:
        # Out-of-range parameters must be refused by both, and only those.
        both = bool(ref.returncode) and bool(gpu.returncode)
        for path in (gpu_out, ref_out, pfile):
            path.unlink(missing_ok=True)
        return {'case': name, 'expected_refusal': expect_refusal,
                'status': 'both refused' if both and expect_refusal else 'FAIL',
                'reference': ref.stderr.strip() or 'composed', 'stage': gpu.stderr.strip() or 'composed'}
    a, b = read_codes(gpu_out), read_codes(ref_out)
    gpu_out.unlink()
    ref_out.unlink()
    pfile.unlink()
    n = w * h
    row = {'case': name, 'width': w, 'height': h, 'el_width': ew, 'el_height': eh,
           'interleaved_chroma': interleaved, 'planes': {}}
    for plane, (lo, hi) in {'Y': (0, n), 'Cb': (n, n * 5 // 4), 'Cr': (n * 5 // 4, n * 3 // 2)}.items():
        top = (1 << int(params.split()[4])) - 1
        # How much of the picture the case exercises: a match on a picture
        # clipped flat would prove little.
        row['planes'][plane] = compare(a[lo:hi], b[lo:hi], top)
    worst = max(v['max_code_diff'] for v in row['planes'].values())
    row['status'] = 'match' if worst == 0 else 'within tolerance' if worst <= ACCEPT_CODES else 'FAIL'
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--control', required=True, type=Path)
    ap.add_argument('--oracle', required=True, type=Path)
    ap.add_argument('--work', required=True, type=Path)
    ap.add_argument('--synthetic', type=int, default=0, help='number of seeded synthetic cases')
    ap.add_argument('--capture', type=Path, action='append', default=[])
    ap.add_argument('--frames', type=int, nargs='+', action='append', default=[])
    ap.add_argument('--out', required=True, type=Path)
    args = ap.parse_args()
    if len(args.frames) != len(args.capture):
        ap.error('give --frames once after each --capture')
    args.work.mkdir(parents=True, exist_ok=True)
    rows = []
    for seed in range(args.synthetic):
        p, geometry, blp, elp = synthetic_case(seed)
        bl_path, el_path = args.work / 'synthetic.bl.yuv', args.work / 'synthetic.el.yuv'
        for picture, path in ((blp, bl_path), (elp, el_path)):
            if sys.byteorder != 'little':
                picture.byteswap()   # captures are little-endian
            path.write_bytes(picture.tobytes())
        row = run(args, f'synthetic-{seed:03d}', frc1(p), bl_path, el_path, 0, geometry, p['out_of_spec'],
                  interleaved=seed % 2 == 1)
        row['kind'] = 'synthetic'
        row['seed'] = seed
        rows.append(row)
        print(json.dumps({k: row[k] for k in ('case', 'status')}), flush=True)
    if args.capture:
        if np is None:
            ap.error('--capture needs numpy')
        sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'diagnostics' / 'fel_quality'))
        from fel_reference_measure import params_text  # the 5.2 RPU -> frc1 conversion
    for capture, frames in zip(args.capture, args.frames):
        events = [json.loads(line) for line in (capture / 'events.jsonl').read_text().splitlines()]
        pairs = {e['frame']: e for e in events if e['kind'] == 'accepted_source_pair'}
        metas = {m['frame']: m for m in map(json.loads, (capture / 'metadata.jsonl').read_text().splitlines())}
        for frame in frames:
            pair = pairs[frame]
            geometry = (pair['bl_width'], pair['bl_height'], pair['el_width'], pair['el_height'])
            label = f'{capture.parent.name}-{capture.name}'   # captures are often named alike
            row = run(args, f'{label}-{frame:03d}', params_text(metas[frame]),
                      capture / 'bl.yuv', capture / 'el.yuv', frame, geometry, interleaved=frame % 2 == 1)
            row['kind'] = 'capture'
            row['rpu_sha256'] = metas[frame]['rpu_sha256']
            rows.append(row)
            print(json.dumps({k: row[k] for k in ('case', 'status')}), flush=True)
    failed = [r['case'] for r in rows if r['status'] not in ('match', 'within tolerance', 'both refused')]
    result = {'schema': 'plurx-fel-compose-parity-v1', 'method': 'ccm001-integer-v1',
              'acceptance_codes': ACCEPT_CODES, 'plan_acceptance_codes': PLAN_ACCEPT_CODES, 'cases': rows, 'failed': failed,
              'verdict': 'pass' if rows and not failed else 'fail'}
    args.out.write_text(json.dumps(result, indent=1) + '\n')
    print(json.dumps({'verdict': result['verdict'], 'cases': len(rows), 'failed': failed}))
    return 0 if result['verdict'] == 'pass' else 1


if __name__ == '__main__':
    raise SystemExit(main())
