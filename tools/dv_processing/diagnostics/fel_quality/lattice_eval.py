"""All-48-frame fixed-lattice evaluation on the frozen four-model operators.

Implements lattice-michael1800/protocol.json (stride 8, phases 0/1 in x and y
relative to the active-ROI origin). Sampled statistics are labelled as such:
they do not replace full-resolution p99 and do not change the admission rule.

Operators are imported, not reimplemented: four_model_audit.sample_codes,
model_rgb, fixture_metadata, eotf, Y_WEIGHTS and vector_metrics.delta/stats
from the frozen, hash-checked quality-tools copy.

Fail-closed order before any frame other than 0/24 is scored:
 1. frozen sources match the manifest;
 2. canonical BL/EL frames 0/24 hash-equal the bound selected-frame inputs;
 3. every frame's observed metadata has the same matrix/offset/LMS numerators
    as the retained parsed 0/24 metadata;
 4. each decoded candidate's frames 0/24 hash-equal its qualified native frames;
 5. lattice R/F/C RGB16 at frames 0/24, all four models, equal the retained
    full-resolution RGB16 at the lattice coordinates.
No encoding, authoring, playback or packaging change.
"""
from pathlib import Path
import argparse, hashlib, importlib.util, json, os, sys
sys.dont_write_bytecode = True
os.environ.setdefault('OPENBLAS_NUM_THREADS', '1'); os.environ.setdefault('VECLIB_MAXIMUM_THREADS', '1')
import numpy as np


def sha(p):
    with Path(p).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def frame_hash(mm, i):
    return hashlib.sha256(np.ascontiguousarray(mm[i]).tobytes()).hexdigest()


ap = argparse.ArgumentParser()
ap.add_argument('--tools', required=True, type=Path, help='frozen quality-tools directory')
ap.add_argument('--request', required=True, type=Path, help='bound selected-frame request whose case supplies the 0/24 bindings')
ap.add_argument('--protocol', required=True, type=Path)
ap.add_argument('--bl', required=True, type=Path); ap.add_argument('--el', required=True, type=Path)
ap.add_argument('--metadata-jsonl', required=True, type=Path)
ap.add_argument('--candidate', action='append', default=[], help='LABEL=decoded.yuv=qualification.json (or LABEL=decoded.yuv=- for PRE)')
ap.add_argument('--retained-dir', required=True, type=Path, help='quality output case dir with {model}-frame-NNN-{R,F,C}.rgb48le')
ap.add_argument('--retained-candidate', required=True, help='label whose C is the retained C')
ap.add_argument('--output', required=True, type=Path)
a = ap.parse_args()
assert not a.output.exists()

manifest = json.loads((a.tools / 'frozen-source-manifest.json').read_text())
for s in manifest['sources']:
    assert sha(s['path']) == s['sha256'], s['path']
sys.path.insert(0, str(a.tools))
spec = importlib.util.spec_from_file_location('four_model_audit', a.tools / 'four_model_audit.py')
fm = importlib.util.module_from_spec(spec); spec.loader.exec_module(fm)
vm = fm.vm

protocol = json.loads(a.protocol.read_text())
req = json.loads(a.request.read_text()); case = req['cases'][0]
w, h = case['raster']; ew, eh = case['el_raster']; x0, y0, x1, y1 = case['roi']
assert [x0, y0, x1, y1] == protocol['active_roi_xyxy'] and [w, h] == protocol['raster']
stride = protocol['fixed_lattice']['stride']; phases = protocol['fixed_lattice']['x_phases_relative_to_roi_origin']
assert phases == protocol['fixed_lattice']['y_phases_relative_to_roi_origin'] == [0, 1] and stride == 8
xs = np.array(sorted({x0 + p + stride * k for p in phases for k in range((x1 - x0) // stride + 1) if x0 + p + stride * k < x1}))
ys = np.array(sorted({y0 + p + stride * k for p in phases for k in range((y1 - y0) // stride + 1) if y0 + p + stride * k < y1}))

fb = w * h * 3 // 2; efb = ew * eh * 3 // 2
bl = np.memmap(a.bl, dtype='<u2', mode='r'); n = bl.size // fb; bl = bl.reshape(n, fb)
el = np.memmap(a.el, dtype='<u2', mode='r').reshape(n, efb)
assert n == 48
for f, ordinal in ((0, 0), (24, 1)):
    assert frame_hash(bl, f) == case['bl']['frame_sha256'][ordinal], ('BL', f)
    assert frame_hash(el, f) == case['el']['frame_sha256'][ordinal], ('EL', f)

meta = {f: fm.fixture_metadata(fm.bound(case['metadata'][str(f)])) for f in (0, 24)}
assert all(np.array_equal(meta[0][i], meta[24][i]) for i in range(3))
matrix, offsets, lms, _ = meta[0]
observed = [json.loads(x) for x in a.metadata_jsonl.read_text().splitlines()]
assert [o['frame'] for o in observed] == list(range(48))
for o in observed:
    c = o['color']
    assert [x[0] for x in c['ycc_to_rgb_matrix']] == matrix.ravel().tolist() and all(x[1] == 8192 for x in c['ycc_to_rgb_matrix'])
    assert [x[0] for x in c['ycc_to_rgb_offset']] == offsets.tolist() and all(x[1] == 2 ** 28 for x in c['ycc_to_rgb_offset'])
    assert [x[0] for x in c['rgb_to_lms_matrix']] == meta[0][3]['rgb_to_lms_rawQ14'] and all(x[1] == 16384 for x in c['rgb_to_lms_matrix'])
    for curve in o['mapping']['curves']:
        assert curve['pieces'] == [{'mapping_idc': 0, 'poly_order': 1, 'poly_coef': [0, 8388608, 0]}] and curve['nlq_offset'] == 512

cands = {}
for spec_ in a.candidate:
    label, path, qual = spec_.split('=')
    mm = np.memmap(path, dtype='<u2', mode='r'); assert mm.size == 48 * fb, label; mm = mm.reshape(48, fb)
    rec = {'path': path, 'sha256': sha(path)}
    if qual != '-':
        q = json.loads(Path(qual).read_text())
        for item in q['native_frames']:
            assert frame_hash(mm, item['source_ordinal']) == item['sha256'], (label, item['source_ordinal'])
        rec.update(qualification={'path': qual, 'sha256': sha(qual)}, authored_sha256=q['authored_sha256'],
                   authored_video_packet_mbps=q['authored_video_packet_mbps'], native_0_24_bound=True)
    cands[label] = (mm, rec)
assert a.retained_candidate in cands


def planes(raw, i, ww, hh):
    x = raw[i]
    return (x[:ww * hh].reshape(hh, ww), x[ww * hh:ww * hh * 5 // 4].reshape(hh // 2, ww // 2), x[ww * hh * 5 // 4:].reshape(hh // 2, ww // 2))


def lattice_rgb(model, f):
    """RGB16 per arm on the lattice, plus absolute (y, x) of each sample."""
    b = planes(bl, f, w, h); e = planes(el, f, ew, eh)
    out = {}
    for lo in range(0, h, 64):
        hi = min(lo + 64, h); rows = ys[(ys >= lo) & (ys < hi)]
        if not len(rows):
            continue
        first = True
        for label, (mm, _) in cands.items():
            codes = fm.sample_codes(b, e, planes(mm, f, w, h), w, h, lo, hi)
            arms = ['R', 'F', 'C'] if first else ['C']
            for arm in arms:
                base, centered = codes[arm]
                sub = (base[rows - lo][:, xs], None if centered is None else centered[rows - lo][:, xs])
                rgb = fm.model_rgb(model, sub[0], sub[1], matrix, offsets, lms)
                out.setdefault(arm if arm != 'C' else label, []).append(rgb)
            first = False
    return {k: np.concatenate(v, axis=0) for k, v in out.items()}


# Gate 5: exact equality against retained full-resolution RGB at frames 0/24.
exact = []
for f in (0, 24):
    for model in fm.MODEL_IDS:
        lat = lattice_rgb(model, f)
        for arm, key in (('R', 'R'), ('F', 'F'), ('C', a.retained_candidate)):
            full = np.memmap(a.retained_dir / f'{model}-frame-{f:03}-{arm}.rgb48le', dtype='<u2', mode='r', shape=(h, w, 3))
            ref = np.asarray(full[ys][:, xs])
            assert np.array_equal(ref, lat[key]), ('lattice RGB differs from retained full RGB', f, model, arm)
            exact.append({'frame': f, 'model': model, 'arm': arm, 'samples': int(ref.shape[0] * ref.shape[1]), 'equal': True})
print(json.dumps({'gate': 'lattice_equals_retained_full_RGB', 'checks': len(exact)}), flush=True)

bands = [(f'[{lo},{hi if hi is not None else "inf"})', lo, hi) for lo, hi in protocol['luminance_bands_reference_weighted_linear_Y_nits']]
arms = ['F'] + list(cands)
pyy, pxx = np.meshgrid(ys % 2, xs % 2, indexing='ij')
pooled = {m: {arm: {b[0]: [] for b in bands} | {'all': []} for arm in arms} for m in fm.MODEL_IDS}
frames_out = []
for f in range(48):
    row = {'frame': f, 'rpu_sha256': observed[f]['rpu_sha256'], 'models': {}}
    for model in fm.MODEL_IDS:
        lat = lattice_rgb(model, f); R = lat['R']
        lum = 10000 * fm.eotf(R.astype(np.float64) / 65535) @ fm.Y_WEIGHTS
        mrow = {}
        for arm in arms:
            de = vm.delta(R, lat[arm])
            entry = {'all': vm.stats(de), 'parities_yx': {f'{yp}{xp}': vm.stats(de[(pyy == yp) & (pxx == xp)]) for yp in (0, 1) for xp in (0, 1)}, 'bands': {}}
            pooled[model][arm]['all'].append(de.ravel())
            for name, lo, hi in bands:
                mask = lum >= lo
                if hi is not None:
                    mask &= lum < hi
                entry['bands'][name] = vm.stats(de[mask]) if mask.any() else {'count': 0}
                pooled[model][arm][name].append(de[mask])
            mrow[arm] = entry
        row['models'][model] = mrow
    frames_out.append(row)
    print(json.dumps({'frame': f, 'scored': True}), flush=True)

dark = bands[0][0]
summary = {}
for model in fm.MODEL_IDS:
    summary[model] = {arm: {k: vm.stats(np.concatenate(v)) for k, v in d.items()} for arm, d in pooled[model].items()}
comparisons = {}
for model in fm.MODEL_IDS:
    for label in cands:
        wins = {'full_mean_below_F': 0, 'dark_mean_below_F': 0, 'dark_p99_below_F': 0, 'all_three': 0}
        for row in frames_out:
            m = row['models'][model]; c = m[label]; F = m['F']
            t = [c['all']['mean'] < F['all']['mean'],
                 c['bands'][dark].get('mean', 0) < F['bands'][dark].get('mean', 0),
                 c['bands'][dark].get('p99', 0) < F['bands'][dark].get('p99', 0)]
            for k, v in zip(['full_mean_below_F', 'dark_mean_below_F', 'dark_p99_below_F'], t):
                wins[k] += int(v)
            wins['all_three'] += int(all(t))
        comparisons[f'{model}:{label}'] = wins
report = {'scope': protocol['scope'] + '. Sampled lattice statistics; not full-resolution p99; no admission change.',
          'protocol': {'path': str(a.protocol), 'sha256': sha(a.protocol)}, 'request': {'path': str(a.request), 'sha256': sha(a.request)},
          'script': {'path': str(Path(__file__).resolve()), 'sha256': sha(Path(__file__).resolve())},
          'frozen_tools': {s['path']: s['sha256'] for s in manifest['sources']},
          'lattice': {'stride': stride, 'phases': phases, 'xs': len(xs), 'ys': len(ys), 'samples_per_frame': int(len(xs) * len(ys))},
          'inputs': {'bl': {'path': str(a.bl), 'sha256': sha(a.bl)}, 'el': {'path': str(a.el), 'sha256': sha(a.el)},
                     'metadata_jsonl': {'path': str(a.metadata_jsonl), 'sha256': sha(a.metadata_jsonl)}},
          'candidates': {k: v[1] for k, v in cands.items()},
          'gates': {'frozen_sources': True, 'bl_el_0_24_bound': True, 'metadata_all48_equal_retained_0_24_numerators': True,
                    'candidates_0_24_bound_to_qualified_native': True, 'lattice_equals_retained_full_RGB': exact},
          'per_frame_wins_vs_F_sampled': comparisons, 'pooled_all48': summary, 'frames': frames_out}
with a.output.open('x') as f:
    f.write(json.dumps(report, indent=1) + '\n')
print(json.dumps({'status': 'complete', 'output': str(a.output), 'wins': comparisons}))
