"""Ceiling of an RPU re-fit (protocol-rpu-refit-20261112.json): how close can the untouched BL get to the
FEL composition R through single-layer mapping curves alone, with no encode?

Fit (stride-8 phase-0 lattice of the active ROI, all 48 frames of the window, BL/EL/RPU only):
  luma   1024-bin conditional mean of R's 12-bit composed luma given the BL luma code (Fp_lut), and
         8 equal pieces of 2nd-order polynomials fitted to it with sample-count weights (Fp_poly);
  chroma 3rd-order MMR on normalised BL (y,u,v): constant + {y,u,v,yu,yv,uv,yuv} to powers 1..3, least squares
         per chroma channel; Fp_poly rounds every coefficient to 2^-23.
  Fp_frame repeats Fp_lut per frame (oracle upper bound).
  Fp_cs is Fp_lut with the MMR's luma input at the chroma sample site (rounded mean of the co-located 2x2
  BL luma block) in fit and application (protocol amendment_1).
Score: each arm's composed 12-bit planes go through the unchanged four models (sample_codes, model_rgb,
vector_metrics.delta) against R exactly as heldout_eval.py scores F, on the lattice (--stride 8) or full
resolution (--stride 1, --frames 0,24). Also reports mean composed luma per frame (flicker) for R and each arm.
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
ap.add_argument('--frames', default='all')
ap.add_argument('--bands-json', required=True, type=Path)
ap.add_argument('--output', required=True, type=Path)
ap.add_argument('--rpu-list', type=Path, help='with --write-plan: the ordered source RPU list (path/sha256 entries)')
ap.add_argument('--write-plan', type=Path, help='write the rpu_refit_write plan for Fp_poly (normative domain) and exit after fitting')
ap.add_argument('--rpu-out-dir', type=Path, help='with --write-plan: where the 8.1 RPUs go')
ap.add_argument('--readback', type=Path, help="rpu_refit_write's output: adds arm Fp_rpu (amendment_2)")
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
bl = np.memmap(a.bl, dtype='<u2', mode='r').reshape(48, fb)
el = np.memmap(a.el, dtype='<u2', mode='r').reshape(48, efb)

# Same metadata assertions as heldout_eval.py: identity mapping, NLQ linear, one colour matrix.
obs = [json.loads(x) for x in a.metadata_jsonl.read_text().splitlines()]
assert [o['frame'] for o in obs] == list(range(48))
c0 = obs[0]['color']
for o in obs:
    hd, mp, c = o['header'], o['mapping'], o['color']
    assert (hd['coef_data_type'], hd['coef_log2_denom'], hd['bl_bit_depth'], hd['el_bit_depth'], hd['vdr_bit_depth'], hd['disable_residual_flag']) == (0, 23, 10, 10, 12, 0)
    for curve in mp['curves']:
        assert curve['pivots'] == [0, 1023] and curve['pieces'] == [{'mapping_idc': 0, 'poly_order': 1, 'poly_coef': [0, 8388608, 0]}]
    assert (c['ycc_to_rgb_matrix'], c['ycc_to_rgb_offset'], c['rgb_to_lms_matrix']) == (c0['ycc_to_rgb_matrix'], c0['ycc_to_rgb_offset'], c0['rgb_to_lms_matrix'])
matrix = np.array([x[0] for x in c0['ycc_to_rgb_matrix']], dtype=np.int64).reshape(3, 3)
offsets = np.array([x[0] for x in c0['ycc_to_rgb_offset']], dtype=np.int64)
lms = fm.HPE_INVERSE @ (np.array([x[0] for x in c0['rgb_to_lms_matrix']], dtype=np.float64).reshape(3, 3) / 16384)

proto = json.loads(a.bands_json.read_text())
if 'reference_luminance_bands_nits' in proto:
    bands = [(f'[{lo},{hi if hi is not None else "infinity"})', lo, hi) for lo, hi in proto['reference_luminance_bands_nits']]
    bands += [(f'dark_below_{b}', 0, b) for b in proto.get('dark_aggregates_nits', [])]
else:
    bands = [(f'[{lo},{hi if hi is not None else "inf"})', lo, hi) for lo, hi in proto['luminance_bands_reference_weighted_linear_Y_nits']]


def planes(raw, i, ww, hh):
    x = raw[i]
    return (x[:ww * hh].reshape(hh, ww), x[ww * hh:ww * hh * 5 // 4].reshape(hh // 2, ww // 2), x[ww * hh * 5 // 4:].reshape(hh // 2, ww // 2))


def lattice(phases):
    xs = np.array(sorted({x0 + p + 8 * k for p in phases for k in range((x1 - x0) // 8 + 1) if x0 + p + 8 * k < x1}))
    ys = np.array(sorted({y0 + p + 8 * k for p in phases for k in range((y1 - y0) // 8 + 1) if y0 + p + 8 * k < y1}))
    return xs, ys


def chunks(f, xs, ys):
    """(base, centered) of R at the given lattice, in 64-row chunks, as the frozen tools pair them."""
    b = planes(bl, f, w, h); e = planes(el, f, ew, eh)
    for lo in range(0, h, 64):
        hi = min(lo + 64, h); rows = ys[(ys >= lo) & (ys < hi)]
        if len(rows):
            base, centered = fm.sample_codes(b, e, b, w, h, lo, hi)['R']
            # Luma at each pixel's chroma sample site, paired exactly as sample_codes pairs chroma.
            cy = np.minimum((rows + 1) // 2, h // 2 - 1); cx = np.minimum((xs + 1) // 2, w // 2 - 1)
            Y = b[0].astype(np.int64)
            ycs = (Y[2 * cy[:, None], 2 * cx] + Y[2 * cy[:, None], 2 * cx + 1] + Y[2 * cy[:, None] + 1, 2 * cx] + Y[2 * cy[:, None] + 1, 2 * cx + 1] + 2) // 4
            yield base[rows - lo][:, xs], centered[rows - lo][:, xs], ycs


def mmr_basis(base, ycs=None):
    y, u, v = ((base[..., k] if k or ycs is None else ycs).astype(np.float64) / 1023 for k in range(3))
    t = [y, u, v, y * u, y * v, u * v, y * u * v]
    return np.stack([np.ones_like(y)] + [x ** p for p in (1, 2, 3) for x in t], -1)


class Fit:
    def __init__(self):
        self.ysum = np.zeros(1024); self.ycnt = np.zeros(1024); self.xtx = np.zeros((22, 22)); self.xty = np.zeros((22, 2))

    def add(self, base, centered, ycs=None):
        comp = fm.composition12(base, centered)
        np.add.at(self.ysum, base[..., 0].ravel(), comp[..., 0].ravel()); np.add.at(self.ycnt, base[..., 0].ravel(), 1)
        X = mmr_basis(base, ycs).reshape(-1, 22); Y = comp[..., 1:].reshape(-1, 2).astype(np.float64) / 4095
        self.xtx += X.T @ X; self.xty += X.T @ Y

    def solve(self, quantize=False):
        filled = self.ycnt > 0
        lut = np.interp(np.arange(1024), np.flatnonzero(filled), self.ysum[filled] / self.ycnt[filled])
        mmr = np.linalg.lstsq(self.xtx + 1e-9 * np.eye(22), self.xty, rcond=None)[0]
        poly = []
        for p in range(8):
            xsel = np.arange(128 * p, 128 * (p + 1)); wts = self.ycnt[xsel]
            if (wts > 0).sum() >= 3:
                xn = xsel / 1023; A = np.stack([np.ones_like(xn), xn, xn * xn], -1) * np.sqrt(wts)[:, None]
                cf = np.linalg.lstsq(A, lut[xsel] / 4095 * np.sqrt(wts), rcond=None)[0]
            else:
                cf = np.array([0.0, 4 * 1023 / 4095, 0.0])  # identity piece where the window has no samples
            poly.append(cf)
        poly = np.array(poly)
        if quantize:
            q = 2.0 ** 23; poly = np.round(poly * q) / q; mmr = np.round(mmr * q) / q
        return {'lut': lut, 'poly': poly, 'mmr': mmr}

    def solve_safe(self):
        """amendment_3: monotone luma target continued by identity slope, count+floor weighted pieces; ridge MMR toward identity."""
        cnt = self.ycnt; pop = np.flatnonzero(cnt > 0); m = self.ysum[pop] / cnt[pop]
        # Pool-adjacent-violators: count-weighted non-decreasing fit of the conditional mean over populated codes.
        blocks = []
        for v, wt in zip(m, cnt[pop]):
            blocks.append([v * wt, wt, 1])
            while len(blocks) > 1 and blocks[-2][0] / blocks[-2][1] > blocks[-1][0] / blocks[-1][1]:
                b = blocks.pop(); blocks[-1][0] += b[0]; blocks[-1][1] += b[1]; blocks[-1][2] += b[2]
        iso = np.concatenate([np.full(n, sv / sw) for sv, sw, n in blocks])
        codes = np.arange(1024); target = np.interp(codes, pop, iso)
        lo, hi = pop[0], pop[-1]
        target[:lo] = 4 * codes[:lo] + (iso[0] - 4 * lo); target[hi + 1:] = 4 * codes[hi + 1:] + (iso[-1] - 4 * hi)
        target = np.clip(target, 0, 4095)
        floor = 0.01 * cnt[pop].mean(); poly = []
        for p_ in range(8):
            xsel = codes[128 * p_:128 * (p_ + 1)]; wts = cnt[xsel] + floor
            xn = xsel / 1023; A = np.stack([np.ones_like(xn), xn, xn * xn], -1) * np.sqrt(wts)[:, None]
            poly.append(np.linalg.lstsq(A, target[xsel] / 4095 * np.sqrt(wts), rcond=None)[0])
        poly = np.array(poly)
        ident = np.zeros((22, 2)); ident[2, 0] = ident[3, 1] = 4 * 1023 / 4095  # Cb <- u, Cr <- v
        lam = 1e-5 * cnt.sum()
        mmr = np.linalg.solve(self.xtx + lam * np.eye(22), self.xty + lam * ident)
        q = 2.0 ** 23
        return {'lut': target, 'poly': np.round(poly * q) / q, 'mmr': np.round(mmr * q) / q, 'target': target}


def diagnostics(fit):
    """Worst departure from identity over every luma code and a 17^3 (y,u,v) grid; monotonicity; largest coefficient."""
    codes = np.arange(1024); cf = fit['poly'][np.minimum(codes // 128, 7)]; xn = codes / 1023
    yl = (cf[:, 0] + cf[:, 1] * xn + cf[:, 2] * xn * xn) * 4095
    g = np.linspace(0, 1023, 17).round().astype(np.int64)
    grid = np.stack(np.meshgrid(g, g, g, indexing='ij'), -1).reshape(-1, 3)
    co = mmr_basis(grid) @ fit['mmr'] * 4095
    return {'luma_max_abs_minus_identity_codes12': float(np.abs(yl - 4 * codes).max()),
            'luma_monotone': bool(np.all(np.diff(yl) >= -1e-6)),
            'luma_max_jump_at_pivots_codes12': float(max(abs(yl[128 * k] - yl[128 * k - 1]) for k in range(1, 8))),
            'chroma_max_abs_minus_identity_codes12_on_grid': float(np.abs(co - 4 * grid[:, 1:]).max()),
            'max_abs_coefficient': float(max(np.abs(fit['poly']).max(), np.abs(fit['mmr']).max()))}


def apply(fit, kind, base, ycs=None):
    yb = base[..., 0]
    if kind == 'poly':
        cf = fit['poly'][np.minimum(yb // 128, 7)]; xn = yb / 1023
        yo = (cf[..., 0] + cf[..., 1] * xn + cf[..., 2] * xn * xn) * 4095
    else:
        yo = fit['lut'][yb]
    co = mmr_basis(base, ycs) @ fit['mmr'] * 4095
    comp = np.clip(np.floor(np.concatenate([yo[..., None], co], -1) + .5), 0, 4095).astype(np.int64)
    d = comp - 4 * base.astype(np.int64)
    return np.where(d < 0, d - 1, d)  # centered code whose composition12 equals comp exactly


# Fit on the phase-0 stride-8 lattice of all 48 frames (window) and per frame (oracle).
fxs, fys = lattice((0,))
win = Fit(); wcs = Fit(); per = []; halves = [Fit(), Fit()]
for f in range(48):
    pf = Fit()
    for base, centered, ycs in chunks(f, fxs, fys):
        win.add(base, centered); pf.add(base, centered); wcs.add(base, centered, ycs); halves[f // 24].add(base, centered)
    per.append(pf.solve())
fits = {'Fp_lut': (win.solve(), 'lut'), 'Fp_poly': (win.solve(quantize=True), 'poly'), 'Fp_cs': (wcs.solve(), 'lut'),
        'Fp_safe': (win.solve_safe(), 'poly')}
oos = {'Fp_poly_oos': [halves[1].solve(quantize=True), halves[0].solve(quantize=True)],  # frames 0-23 use the 24-47 fit and vice versa
       'Fp_safe_oos': [halves[1].solve_safe(), halves[0].solve_safe()]}
fit_diag = {k: diagnostics(v[0]) for k, v in fits.items() if k in ('Fp_poly', 'Fp_safe')}
fit_diag.update({f'{k}_half{i}': diagnostics(v[i]) for k, v in oos.items() for i in (0, 1)})
print(json.dumps({'fitted': True, 'samples': int(win.ycnt.sum())}), flush=True)

# Composer domain (amendment_2): x = code / 1024 in, s * 4096 out; the fit used code/1023 in, value/4095 out.
K, RS = 4095 / 4096, 1024 / 1023
DEG = [0] + [g * p for p in (1, 2, 3) for g in (1, 1, 1, 2, 2, 2, 3)]
if a.write_plan:
    pf = fits['Fp_safe'][0]  # amendment_3: only the safe fit is written
    assert all(abs(v) < 63 for v in pf['poly'].ravel()) and all(abs(v) < 63 for v in pf['mmr'].ravel()), 'coefficient outside the writable range'
    luma = [[K * c[0], K * c[1] * RS, K * c[2] * RS * RS] for c in pf['poly'].tolist()]
    chroma = []
    for ch in range(2):
        m = [K * pf['mmr'][i, ch] * RS ** DEG[i] for i in range(22)]
        chroma.append({'constant': m[0], 'coef': [m[1 + 7 * j:8 + 7 * j] for j in range(3)]})
    srcs = json.loads(a.rpu_list.read_text()); assert len(srcs) == 48
    a.rpu_out_dir.mkdir(parents=True, exist_ok=False)
    for e in srcs:
        assert sha(e['path']) == e['sha256'], e['path']
    plan = {'frames': [{'src': e['path'], 'out': str(a.rpu_out_dir / f'frame-{i:03d}.nal')} for i, e in enumerate(srcs)],
            'luma': {'pivot_deltas': [0] + [128] * 7 + [127], 'coef': luma}, 'chroma': chroma}
    a.write_plan.write_text(json.dumps(plan, indent=1) + '\n'); print(json.dumps({'plan': str(a.write_plan)})); sys.exit(0)

rb = None
if a.readback:
    rf = json.loads(a.readback.read_text())['frames']; assert len(rf) == 48
    r0 = rf[0]
    assert all((x['luma_coef'], x['chroma'], x['luma_pivot_deltas']) == (r0['luma_coef'], r0['chroma'], r0['luma_pivot_deltas']) for x in rf), 'per-window curves differ between frames'
    assert (r0['profile'], r0['bl_bit_depth'], r0['vdr_bit_depth']) == (8, 10, 12)
    piv = np.cumsum(r0['luma_pivot_deltas'])
    rb = {'piv': piv, 'luma': np.array(r0['luma_coef']), 'mmr': [np.array([c['constant']] + [v for row in c['coef'] for v in row]) for c in r0['chroma']]}


def apply_rpu(base):
    """libplacebo pl_shader_dovi_reshape semantics on 10-bit BL codes; returns the centered code sample_codes would give."""
    x = np.clip(base.astype(np.float64) / 1024, 0, 1023 / 1024)
    yb = base[..., 0]
    idx = np.clip(np.searchsorted(rb['piv'][1:-1], yb, side='right'), 0, len(rb['piv']) - 2)
    c = rb['luma'][idx]; s0 = c[..., 0] + c[..., 1] * x[..., 0] + c[..., 2] * x[..., 0] ** 2
    y, u, v = x[..., 0], x[..., 1], x[..., 2]; t = [y, u, v, y * u, y * v, u * v, y * u * v]
    X = np.stack([np.ones_like(y)] + [tt ** p for p in (1, 2, 3) for tt in t], -1)
    out = np.stack([s0, X @ rb['mmr'][0], X @ rb['mmr'][1]], -1)
    comp = np.clip(np.floor(np.clip(out, 0, 1) * 4096 + .5), 0, 4095).astype(np.int64)
    d = comp - 4 * base.astype(np.int64)
    return np.where(d < 0, d - 1, d)

if a.stride == 1:
    xs, ys = np.arange(x0, x1), np.arange(y0, y1)
else:
    xs, ys = lattice((0, 1))
frames = list(range(48)) if a.frames == 'all' else [int(x) for x in a.frames.split(',')]
ARMS = ['F', 'Fp_lut', 'Fp_poly', 'Fp_frame', 'Fp_cs', 'Fp_safe', 'Fp_poly_oos', 'Fp_safe_oos'] + (['Fp_rpu'] if rb else [])


def score(model, f):
    des = {k: [] for k in ARMS}; lums = []; ymean = {k: [0.0, 0] for k in ['R'] + ARMS}
    for base, centered, ycs in chunks(f, xs, ys):
        R = fm.model_rgb(model, base, centered, matrix, offsets, lms)
        lums.append((10000 * fm.eotf(R.astype(np.float64) / 65535) @ fm.Y_WEIGHTS).ravel())
        arm_d = {'F': None, 'Fp_lut': apply(fits['Fp_lut'][0], 'lut', base), 'Fp_poly': apply(fits['Fp_poly'][0], 'poly', base),
                 'Fp_frame': apply(per[f], 'lut', base), 'Fp_cs': apply(fits['Fp_cs'][0], 'lut', base, ycs),
                 'Fp_safe': apply(fits['Fp_safe'][0], 'poly', base),
                 'Fp_poly_oos': apply(oos['Fp_poly_oos'][f // 24], 'poly', base), 'Fp_safe_oos': apply(oos['Fp_safe_oos'][f // 24], 'poly', base)}
        if rb:
            arm_d['Fp_rpu'] = apply_rpu(base)
        for k, d in arm_d.items():
            des[k].append(vm.delta(R, fm.model_rgb(model, base, d, matrix, offsets, lms)).ravel())
            yc = fm.composition12(base, d)[..., 0]; ymean[k][0] += float(yc.sum()); ymean[k][1] += yc.size
        yr = fm.composition12(base, centered)[..., 0]; ymean['R'][0] += float(yr.sum()); ymean['R'][1] += yr.size
    lum = np.concatenate(lums); out = {}
    for arm, parts in des.items():
        de = np.concatenate(parts); entry = {'full': vm.stats(de), 'bands': {}}
        for name, lo_, hi_ in bands:
            m = lum >= lo_
            if hi_ is not None:
                m &= lum < hi_
            entry['bands'][name] = vm.stats(de[m]) if m.any() else {'count': 0}
        out[arm] = entry
    out['_mean_composed_luma'] = {k: s / n for k, (s, n) in ymean.items()}
    return out


results = {}
for f in frames:
    results[f] = {model: score(model, f) for model in fm.MODEL_IDS}
    print(json.dumps({'frame': f, 'scored': True}), flush=True)

dark = next((nm for nm, lo_, hi_ in bands if (lo_, hi_) == (0, 0.1)), 'dark_below_0.1')
summary = {}
for model in fm.MODEL_IDS:
    s = {}
    for arm in ARMS:
        pooled = sum(results[f][model][arm]['full']['mean'] for f in frames) / len(frames)
        s[arm] = {'pooled_full_mean': pooled, 'worst_frame_p99': max(results[f][model][arm]['full']['p99'] for f in frames),
                  'worst_frame_max': max(results[f][model][arm]['full']['max'] for f in frames)}
        if arm != 'F':
            s[arm]['frames_full_mean_below_F'] = sum(results[f][model][arm]['full']['mean'] < results[f][model]['F']['full']['mean'] for f in frames)
            s[arm]['frames_dark_p99_below_F'] = sum(results[f][model][arm]['bands'].get(dark, {}).get('p99', 0) < results[f][model]['F']['bands'].get(dark, {}).get('p99', 0) for f in frames)
            s[arm]['fraction_of_F_error_removed'] = 1 - pooled / s['F']['pooled_full_mean']
    summary[model] = s
report = {'scope': 'RPU re-fit ceiling (protocol-rpu-refit-20261112.json); frozen four-model operators; ' + ('full resolution' if a.stride == 1 else 'stride-8 lattice') + '; no encode, no shipped RPU, no device claim.',
          'script_sha256': sha(Path(__file__).resolve()), 'frozen_tools': {Path(s['path']).name: s['sha256'] for s in manifest['sources']},
          'inputs': {k: sha(p) for k, p in (('bl', a.bl), ('el', a.el), ('metadata_jsonl', a.metadata_jsonl), ('bands', a.bands_json))},
          'roi': a.roi, 'stride': a.stride, 'frames': frames, 'fit_samples': int(win.ycnt.sum()),
          'fit_diagnostics': fit_diag,
          'fit': {'Fp_safe_luma_pieces_normalised': fits['Fp_safe'][0]['poly'].tolist(), 'Fp_safe_chroma_mmr_normalised': fits['Fp_safe'][0]['mmr'].tolist(),
                  'Fp_poly_luma_pieces_normalised': fits['Fp_poly'][0]['poly'].tolist(), 'Fp_poly_chroma_mmr_normalised': fits['Fp_poly'][0]['mmr'].tolist()},
          'summary': summary, 'results': {str(f): results[f] for f in frames}}
a.output.write_text(json.dumps(report, indent=1) + '\n')
print(json.dumps({m: {k: round(v['pooled_full_mean'], 4) for k, v in s.items()} for m, s in summary.items()}))
