"""Exact check of the helper's F+ composition against the frozen tools.

Reads `segment_decode_render --dump-composed` output (plurx-composed-v1, see
tools/dv_processing/refit_mode.h) and, for every dumped lattice sample, the same
pixel of the frozen layer captures (bl.yuv/el.yuv: 10-bit 4:2:0 planar,
little-endian, one frame after another; frames matched by presentation time
through the capture's events.jsonl). The frozen four_model_audit.sample_codes
pairs the base and enhancement codes and composition12 composes them. Every
base code and every composed code must be equal; any difference is a failure.

composition12 is the general LINEAR_DZ formula specialised to identity mapping
curves and these fixtures' NLQ (offset 512, slope 2048/2^23, threshold 0):
floor((B/1024 + sign(d)(|d| - 1/2)/4096)·4096 + 1/2) = 4B + d (d > 0),
4B + d + 1 (d < 0), 4B (d = 0). test_refit_compose.c proves the helper's
general formula reduces to it for every (B, E).

Usage:
  segment_decode_render --dump-composed SOURCE 0 FIRST LAST [--area L R T B] |
    python3 compare_composed.py --tools FROZEN_QUALITY_TOOLS --bl bl.yuv --el el.yuv \
      --events events.jsonl [--roi X0 Y0 X1 Y1] --dump -
Prints one JSON result line; exit 0 only with zero mismatches over all
captured frames.
"""
from fractions import Fraction
from pathlib import Path
import argparse, hashlib, importlib.util, json, os, sys
sys.dont_write_bytecode = True
os.environ.setdefault('OPENBLAS_NUM_THREADS', '1'); os.environ.setdefault('VECLIB_MAXIMUM_THREADS', '1')
import numpy as np


def sha(p):
    with Path(p).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


ap = argparse.ArgumentParser()
ap.add_argument('--tools', required=True, type=Path, help='frozen quality-tools directory (four_model_audit.py)')
ap.add_argument('--bl', required=True, type=Path)
ap.add_argument('--el', required=True, type=Path)
ap.add_argument('--events', required=True, type=Path, help="the capture's events.jsonl (accepted_source_pair pts per frame)")
ap.add_argument('--roi', type=int, nargs=4, help='X0 Y0 X1 Y1: also require the dumped area to be this offline region')
ap.add_argument('--dump', required=True, help="the helper's dump, or - for stdin")
a = ap.parse_args()

manifest = json.loads((a.tools / 'frozen-source-manifest.json').read_text())
for s in manifest['sources']:
    assert sha(s['path']) == s['sha256'], s['path']
sys.path.insert(0, str(a.tools))
spec = importlib.util.spec_from_file_location('four_model_audit', a.tools / 'four_model_audit.py')
fm = importlib.util.module_from_spec(spec); spec.loader.exec_module(fm)

pairs = sorted((e for e in map(json.loads, a.events.read_text().splitlines()) if e['kind'] == 'accepted_source_pair'),
               key=lambda e: e['frame'])
assert [e['frame'] for e in pairs] == list(range(len(pairs))) and pairs
capture_time = {Fraction(e['pts']): e['frame'] for e in pairs}
dims = {(e['bl_width'], e['bl_height'], e['el_width'], e['el_height']) for e in pairs}
assert len(dims) == 1
w, h, ew, eh = dims.pop()
count = len(pairs)
bl = np.memmap(a.bl, dtype='<u2', mode='r'); el = np.memmap(a.el, dtype='<u2', mode='r')
assert bl.size == count * w * h * 3 // 2 and el.size == count * ew * eh * 3 // 2, 'capture size is not the events frame count'
bl = bl.reshape(count, -1); el = el.reshape(count, -1)


def planes(raw, i, ww, hh):
    x = raw[i]
    return (x[:ww * hh].reshape(hh, ww), x[ww * hh:ww * hh * 5 // 4].reshape(hh // 2, ww // 2), x[ww * hh * 5 // 4:].reshape(hh // 2, ww // 2))


stream = sys.stdin.buffer if a.dump == '-' else open(a.dump, 'rb')
header = json.loads(stream.readline())
assert header['kind'] == 'composed_dump' and header['format'] == 'plurx-composed-v1' and header['record_bytes'] == 16
time_base = Fraction(header['time_base'])
seen, areas = set(), set()
totals = {'fit': [0, 0], 'held_out': [0, 0]}  # samples, mismatching samples
position_errors = 0
complete = None
for line in iter(stream.readline, b''):
    frame = json.loads(line)
    if frame['kind'] == 'dump_complete':
        complete = frame; break
    assert frame['kind'] == 'composed_frame'
    assert frame['bl'] == [w, h] and frame['el'] == [ew, eh]
    kf, kh = frame['fit_samples'], frame['held_out_samples']
    data = stream.read(16 * (kf + kh)); assert len(data) == 16 * (kf + kh), 'truncated dump'
    records = np.frombuffer(data, dtype='<u2').reshape(-1, 8).astype(np.int64)
    t = Fraction(frame['pts']) * time_base
    if t not in capture_time:
        continue  # a dumped frame outside the captured window
    i = capture_time[t]; assert i not in seen; seen.add(i)
    left, right, top, bottom = frame['active_area']; areas.add((left, right, top, bottom))
    x0, x1, y0, y1 = left, w - right, top, h - bottom
    if a.roi:
        assert (x0, y0, x1, y1) == tuple(a.roi), f'dumped area {(x0, y0, x1, y1)} is not the ROI {a.roi}'
    b = planes(bl, i, w, h); e = planes(el, i, ew, eh)
    lattices = (('fit', 0, records[:kf]), ('held_out', 4, records[kf:]))
    grids = {}
    for name, phase, part in lattices:
        xs = np.arange(x0 + phase, x1, 8); ys = np.arange(y0 + phase, y1, 8)
        assert len(part) == len(xs) * len(ys), f'{name} sample count'
        want_xy = np.stack((np.tile(xs, len(ys)), np.repeat(ys, len(xs))), -1)
        position_errors += int(np.any(part[:, :2] != want_xy, axis=1).sum())
        grids[name] = (xs, ys, [], [])
    for lo in range(0, h, 64):
        hi = min(lo + 64, h)
        if not any(((ys >= lo) & (ys < hi)).any() for xs, ys, _, _ in grids.values()):
            continue
        base, centered = fm.sample_codes(b, e, b, w, h, lo, hi)['R']
        for xs, ys, bases, comps in grids.values():
            rows = ys[(ys >= lo) & (ys < hi)]
            if len(rows):
                sb = base[rows - lo][:, xs]; sc = centered[rows - lo][:, xs]
                bases.append(sb.reshape(-1, 3)); comps.append(fm.composition12(sb, sc).reshape(-1, 3))
    for name, phase, part in lattices:
        xs, ys, bases, comps = grids[name]
        want = np.concatenate((np.concatenate(bases), np.concatenate(comps)), -1)
        totals[name][0] += len(part)
        totals[name][1] += int(np.any(part[:, 2:] != want, axis=1).sum())
result = {'kind': 'compare_composed', 'frames_compared': len(seen), 'capture_frames': count,
          'dump_complete': complete is not None, 'areas': sorted(areas),
          'fit': {'samples': totals['fit'][0], 'mismatches': totals['fit'][1]},
          'held_out': {'samples': totals['held_out'][0], 'mismatches': totals['held_out'][1]},
          'position_errors': position_errors,
          'inputs': {'bl': str(a.bl), 'el': str(a.el), 'events_sha256': sha(a.events)},
          'frozen_tools': {Path(s['path']).name: s['sha256'] for s in manifest['sources']}}
result['exact'] = (result['dump_complete'] and len(seen) == count and position_errors == 0
                   and totals['fit'][1] == 0 and totals['held_out'][1] == 0 and totals['fit'][0] > 0)
print(json.dumps(result))
sys.exit(0 if result['exact'] else 1)
