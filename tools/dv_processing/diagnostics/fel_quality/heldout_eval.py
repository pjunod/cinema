"""Score a fixture with no retained parsed metadata on the frozen four-model operators.

Same operators as four_model_audit.py / score_reference_bands.py / lattice_eval.py,
imported unchanged (sample_codes, model_rgb, eotf, Y_WEIGHTS, vector_metrics.delta
and stats). Colour metadata comes from the native metadata observer's JSONL
instead of retained parsed metadata; every frame's header, mapping, NLQ and colour
fields are asserted, and the matrices must be identical on all frames.

--stride 1 gives full-resolution statistics over the active ROI (the selected-frame
bar); --stride 8 gives the phase-0/1 lattice (sampled, labelled as such). Rows are
processed in 64-row chunks from the top of the raster, in the same row order as the
frozen tools, so full-resolution statistics are directly comparable.

Validate before trusting: run --stride 1 on a fixture whose retained full-resolution
results exist and compare with --compare-summary (exact equality required).
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


ap = argparse.ArgumentParser()
ap.add_argument('--tools', required=True, type=Path)
ap.add_argument('--bl', required=True, type=Path); ap.add_argument('--el', required=True, type=Path)
ap.add_argument('--metadata-jsonl', required=True, type=Path)
ap.add_argument('--roi', required=True, type=int, nargs=4)
ap.add_argument('--stride', required=True, type=int, choices=[1, 8])
ap.add_argument('--frames', default='all', help='comma list or "all"')
ap.add_argument('--bands-json', required=True, type=Path, help='QUALITY_PROTOCOL.v1.json or a lattice protocol')
ap.add_argument('--candidate', action='append', default=[], help='LABEL=decoded48.yuv[=qualification.json]')
ap.add_argument('--compare-summary', type=Path, help='retained quality summary.json to equal exactly (stride 1, frames 0/24)')
ap.add_argument('--compare-label', help='candidate label whose C the retained summary scored')
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

w, h, ew, eh = 3840, 2160, 1920, 1080
x0, y0, x1, y1 = a.roi
fb, efb = w * h * 3 // 2, ew * eh * 3 // 2
bl = np.memmap(a.bl, dtype='<u2', mode='r'); n = bl.size // fb; bl = bl.reshape(n, fb)
el = np.memmap(a.el, dtype='<u2', mode='r').reshape(n, efb)
assert n == 48

obs = [json.loads(x) for x in a.metadata_jsonl.read_text().splitlines()]
assert [o['frame'] for o in obs] == list(range(48))
c0 = obs[0]['color']
for o in obs:
    hd, mp, c = o['header'], o['mapping'], o['color']
    assert (hd['coef_data_type'], hd['coef_log2_denom'], hd['bl_bit_depth'], hd['el_bit_depth'], hd['vdr_bit_depth'], hd['disable_residual_flag']) == (0, 23, 10, 10, 12, 0)
    assert (mp['color_space'], mp['chroma_format_idc'], mp['nlq_method_idc'], mp['num_x_partitions'], mp['num_y_partitions'], len(mp['curves'])) == (0, 0, 0, 1, 1, 3)
    assert (c['signal_bit_depth'], c['signal_color_space'], c['signal_chroma_format'], c['signal_full_range_flag']) == (12, 0, 0, 1)
    for curve in mp['curves']:
        assert curve['pivots'] == [0, 1023] and curve['pieces'] == [{'mapping_idc': 0, 'poly_order': 1, 'poly_coef': [0, 8388608, 0]}]
        assert (curve['nlq_offset'], curve['vdr_in_max'], curve['slope'], curve['threshold']) == (512, 1048576, 2048, 0)
    assert all(x[1] == 8192 for x in c['ycc_to_rgb_matrix']) and all(x[1] == 2 ** 28 for x in c['ycc_to_rgb_offset']) and all(x[1] == 16384 for x in c['rgb_to_lms_matrix'])
    assert (c['ycc_to_rgb_matrix'], c['ycc_to_rgb_offset'], c['rgb_to_lms_matrix']) == (c0['ycc_to_rgb_matrix'], c0['ycc_to_rgb_offset'], c0['rgb_to_lms_matrix'])
# Same conversion as four_model_audit.fixture_metadata.
matrix = np.array([x[0] for x in c0['ycc_to_rgb_matrix']], dtype=np.int64).reshape(3, 3)
offsets = np.array([x[0] for x in c0['ycc_to_rgb_offset']], dtype=np.int64)
lms = fm.HPE_INVERSE @ (np.array([x[0] for x in c0['rgb_to_lms_matrix']], dtype=np.float64).reshape(3, 3) / 16384)

proto = json.loads(a.bands_json.read_text())
if 'reference_luminance_bands_nits' in proto:
    bands = [(f'[{lo},{hi if hi is not None else "infinity"})', lo, hi) for lo, hi in proto['reference_luminance_bands_nits']]
    bands += [(f'dark_below_{b}', 0, b) for b in proto.get('dark_aggregates_nits', [])]
else:
    bands = [(f'[{lo},{hi if hi is not None else "inf"})', lo, hi) for lo, hi in proto['luminance_bands_reference_weighted_linear_Y_nits']]

cands = {}
for spec_ in a.candidate:
    parts = spec_.split('=')
    label, path = parts[0], parts[1]
    mm = np.memmap(path, dtype='<u2', mode='r'); assert mm.size == 48 * fb, label; mm = mm.reshape(48, fb)
    rec = {'path': path, 'sha256': sha(path)}
    if len(parts) > 2:
        q = json.loads(Path(parts[2]).read_text())
        for item in q['native_frames']:
            assert hashlib.sha256(np.ascontiguousarray(mm[item['source_ordinal']]).tobytes()).hexdigest() == item['sha256'], (label, item['source_ordinal'])
        rec.update(qualification_sha256=sha(parts[2]), authored_sha256=q['authored_sha256'], authored_video_packet_mbps=q['authored_video_packet_mbps'],
                   coarse_rate_admitted=q['authored_video_packet_mbps'] <= 130, native_0_24_bound=True)
    cands[label] = (mm, rec)

if a.stride == 1:
    xs = np.arange(x0, x1); ys = np.arange(y0, y1)
else:
    xs = np.array(sorted({x0 + p + 8 * k for p in (0, 1) for k in range((x1 - x0) // 8 + 1) if x0 + p + 8 * k < x1}))
    ys = np.array(sorted({y0 + p + 8 * k for p in (0, 1) for k in range((y1 - y0) // 8 + 1) if y0 + p + 8 * k < y1}))
frames = list(range(48)) if a.frames == 'all' else [int(x) for x in a.frames.split(',')]


def planes(raw, i, ww, hh):
    x = raw[i]
    return (x[:ww * hh].reshape(hh, ww), x[ww * hh:ww * hh * 5 // 4].reshape(hh // 2, ww // 2), x[ww * hh * 5 // 4:].reshape(hh // 2, ww // 2))


def score(model, f):
    b = planes(bl, f, w, h); e = planes(el, f, ew, eh)
    des = {k: [] for k in ['F'] + list(cands)}; lums = []
    for lo in range(0, h, 64):
        hi = min(lo + 64, h); rows = ys[(ys >= lo) & (ys < hi)]
        if not len(rows):
            continue
        R = None
        for idx, (label, (mm, _)) in enumerate(cands.items()):
            codes = fm.sample_codes(b, e, planes(mm, f, w, h), w, h, lo, hi)
            def rgb(arm):
                base, centered = codes[arm]
                return fm.model_rgb(model, base[rows - lo][:, xs], None if centered is None else centered[rows - lo][:, xs], matrix, offsets, lms)
            if idx == 0:
                R = rgb('R'); des['F'].append(vm.delta(R, rgb('F')).ravel())
                lums.append((10000 * fm.eotf(R.astype(np.float64) / 65535) @ fm.Y_WEIGHTS).ravel())
            des[label].append(vm.delta(R, rgb('C')).ravel())
    lum = np.concatenate(lums); out = {}
    for arm, parts in des.items():
        de = np.concatenate(parts); entry = {'full': vm.stats(de), 'bands': {}}
        for name, lo_, hi_ in bands:
            m = lum >= lo_
            if hi_ is not None:
                m &= lum < hi_
            entry['bands'][name] = vm.stats(de[m]) if m.any() else {'count': 0}
        out[arm] = entry
    return out


results = {}
for f in frames:
    results[f] = {model: score(model, f) for model in fm.MODEL_IDS}
    print(json.dumps({'frame': f, 'scored': True}), flush=True)

dark = next(nm for nm, lo_, hi_ in bands if (lo_, hi_) == (0, 0.1)) if any((lo_, hi_) == (0, 0.1) for _, lo_, hi_ in bands) else 'dark_below_0.1'
validation = None
if a.compare_summary:
    assert a.stride == 1 and set(frames) >= {0, 24} and a.compare_label in cands
    summ = json.loads(a.compare_summary.read_text()); checks = 0
    for row in summ['rows']:
        mine = results[row['frame']][row['model']]
        for arm, key in (('F', 'R__F'), (a.compare_label, 'R__C')):
            ref = row['full_active'][key]['full_resolution_active_ROI_deltaITP']
            assert mine[arm]['full'] == ref, ('full', row['frame'], row['model'], arm, mine[arm]['full'], ref)
            checks += 1
        for name, rb in row['reference_bands'].items():
            if not rb['count']:
                continue
            for arm, key in (('F', 'F'), (a.compare_label, 'C')):
                for stat in ('count', 'mean', 'p99', 'max'):
                    assert mine[arm]['bands'][name][stat] == rb[key][stat], ('band', name, row['frame'], row['model'], arm, stat)
                checks += 1
    validation = {'summary': {'path': str(a.compare_summary), 'sha256': sha(a.compare_summary)}, 'exact_equal_checks': checks}
    print(json.dumps({'validation': 'exact', 'checks': checks}), flush=True)

bar = None
if a.stride == 1 and set(frames) >= {0, 24}:
    bar = {}
    for label in cands:
        rows = []
        for f in (0, 24):
            for model in fm.MODEL_IDS:
                m = results[f][model]; c, F = m[label], m['F']
                t = {'full_mean_advantage_F': c['full']['mean'] < F['full']['mean'],
                     'dark_below_.1_mean_advantage_F': c['bands'][dark]['mean'] < F['bands'][dark]['mean'],
                     'dark_below_.1_p99_advantage_F': c['bands'][dark]['p99'] < F['bands'][dark]['p99'],
                     'coarse_rate_admitted': cands[label][1].get('coarse_rate_admitted')}
                rows.append({'frame': f, 'model': model, 'bar': t})
        bar[label] = {'rows': rows, 'pass_count': sum(all(r['bar'].values()) for r in rows)}

wins = None
if len(frames) == 48:
    wins = {}
    for model in fm.MODEL_IDS:
        for label in cands:
            k = {'full_mean_below_F': 0, 'dark_mean_below_F': 0, 'dark_p99_below_F': 0, 'all_three': 0}
            for f in frames:
                m = results[f][model]; c, F = m[label], m['F']; d = c['bands'][dark]; dF = F['bands'][dark]
                t = [c['full']['mean'] < F['full']['mean'], d.get('mean', 0) < dF.get('mean', 0), d.get('p99', 0) < dF.get('p99', 0)]
                for key, v in zip(['full_mean_below_F', 'dark_mean_below_F', 'dark_p99_below_F'], t):
                    k[key] += int(v)
                k['all_three'] += int(all(t))
            wins[f'{model}:{label}'] = k

report = {'scope': 'Frozen four-model operators with observer metadata; ' + ('full resolution' if a.stride == 1 else 'sampled stride-8 lattice, not full-resolution p99') + '; no admission change.',
          'script': {'path': str(Path(__file__).resolve()), 'sha256': sha(Path(__file__).resolve())},
          'frozen_tools': {s['path']: s['sha256'] for s in manifest['sources']},
          'inputs': {'bl': {'path': str(a.bl), 'sha256': sha(a.bl)}, 'el': {'path': str(a.el), 'sha256': sha(a.el)},
                     'metadata_jsonl': {'path': str(a.metadata_jsonl), 'sha256': sha(a.metadata_jsonl)}, 'bands': {'path': str(a.bands_json), 'sha256': sha(a.bands_json)}},
          'roi': a.roi, 'stride': a.stride, 'samples_per_frame': int(len(xs) * len(ys)), 'frames': frames,
          'candidates': {k: v[1] for k, v in cands.items()}, 'validation': validation, 'selected_frame_bar': bar, 'per_frame_wins_vs_F': wins,
          'results': {str(f): r for f, r in results.items()}}
with a.output.open('x') as fh:
    fh.write(json.dumps(report, indent=1) + '\n')
print(json.dumps({'status': 'complete', 'bar': {k: v['pass_count'] for k, v in (bar or {}).items()}, 'wins': wins and {k: v['dark_p99_below_F'] for k, v in wins.items()}}))
