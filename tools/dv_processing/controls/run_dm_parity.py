#!/usr/bin/env python3
"""GPU shader vs CPU reference parity for dv_dm.h, on real-frame crops and synthetic rasters.
Runs dm_shader_control for each case; prints max |GPU-CPU| (PQ) and mean/max dE_ITP.

usage: run_dm_parity.py DM_SHADER_CONTROL SCRATCH_DIR SRC_F32 [CASE ...]
SRC_F32: at least five 3840x2160 frames of float32 BT.2020/PQ RGB (the decoded master).
Optional CASE names rerun only those cases; unknown names fail.
Real crops (real pixels, real or synthetic metadata) must match to dE_ITP max 0.05 and 2e-4 PQ;
CM4 real crops and black frames must match per channel to 1.5e-6 PQ OR four binary32 epsilons
of that pixel's peak in linear light, and max dE_ITP 0.05. Float32 matrix cancellation near a zero
channel can exceed the PQ bound while its light error is below two epsilons. The precise ST 2084
helpers have a separate bit-level check in run_pq_helper_exact.py, including unfused lavapipe.
The synthetic raster is a 9^3 lattice reaching 10000-nit fully saturated BT.2020 corners, where the
shader's float32 arithmetic loses digits against the double reference; it must match to dE_ITP mean
0.02 and max 0.5 (well under one JND). Each synthetic channel must differ by less than
0.05 PQ OR 1e-4 of the actual target peak in linear light. This bounded extension keeps the
original PQ acceptance while allowing small light errors near zero that PQ greatly magnifies
(e.g. inverse-LMS cancellation next to a peak-clipped channel). It is an acceptance tolerance,
not proof that all such discrepancies are float32 noise. Both outputs must be finite in [0,1]."""
import json, os, subprocess, sys
import numpy as np
from cm4_reference_controls import MS_REFERENCE_CASES, spatial_texture
CTL = sys.argv[1]; WORK = sys.argv[2]; SRC = sys.argv[3]  # control binary, scratch dir, real 4K frames (src.yuv)
ONLY, SEEN = set(sys.argv[4:]), set()
os.makedirs(WORK, exist_ok=True)
M1, M2, C1, C2, C3 = 0.1593017578125, 78.84375, 0.8359375, 18.8515625, 18.6875
def eotf(e):
    p = np.clip(e, 0, 1) ** (1 / M2); return 10000 * (np.maximum(p - C1, 0) / (C2 - C3 * p)) ** (1 / M1)
def oetf(y):
    p = np.clip(y / 10000, 0, 1) ** M1; return ((C1 + C2 * p) / (1 + C3 * p)) ** M2
R2L = np.array([[1688, 2146, 262], [683, 2951, 462], [99, 309, 3688]]) / 4096
L2I = np.array([[2048, 2048, 0], [6610, -13613, 7003], [17933, -17390, -543]]) / 4096
def de(a, b):
    A = oetf(np.clip(eotf(a) @ R2L.T, 0, None)) @ L2I.T; B = oetf(np.clip(eotf(b) @ R2L.T, 0, None)) @ L2I.T
    return 720 * np.sqrt((A[:, 0] - B[:, 0]) ** 2 + 0.25 * (A[:, 1] - B[:, 1]) ** 2 + (A[:, 2] - B[:, 2]) ** 2)

def synthetic_channel_ok(g, c, linear_g, linear_c, target_peak):
    # Scalar and NumPy-array predicate; range comparisons also reject NaN/Inf.
    valid = (g >= 0) & (g <= 1) & (c >= 0) & (c <= 1)
    return valid & ((abs(g - c) < 0.05) | (abs(linear_g - linear_c) < target_peak * 1e-4))

def precise_cm4_channel_ok(g, c, linear_g, linear_c, pixel_peak):
    # A three-component float matrix can leave a tiny positive residue where
    # the double reference cancels to zero. PQ magnifies that residue; the
    # transfer helper can be bit-exact and this PQ comparison can still fail.
    # Keep the tenth-code budget, with a four-epsilon *pixel-light* alternative.
    # This is an empirical arithmetic tolerance, not a formal error bound.
    valid = ((g >= 0) & (g <= 1) & (c >= 0) & (c <= 1)
             & (pixel_peak >= 0) & (pixel_peak < float('inf'))
             & (linear_g >= 0) & (linear_c >= 0)
             & (linear_g <= pixel_peak) & (linear_c <= pixel_peak))
    return valid & ((abs(g - c) < 1.5e-6)
                    | (abs(linear_g - linear_c) <= 4 * (2.0 ** -23) * pixel_peak))

def run(name, params, rgb, w, h, synthetic=False, expected_weight=None, reference_samples=None):
    SEEN.add(name)
    if ONLY and name not in ONLY: return True
    rgba = np.concatenate([rgb.reshape(h, w, 3), np.ones((h, w, 1))], -1).astype(np.float32)
    pf, inf, go, co = (os.path.join(WORK, name + s) for s in ('.params', '.in', '.gpu', '.cpu'))
    open(pf, 'w').write(params); rgba.tofile(inf)
    for f in (go, co):
        if os.path.exists(f): os.remove(f)
    r = subprocess.run([CTL, pf, inf, str(w), str(h), go, co], capture_output=True, text=True)
    if r.returncode: print(name, 'FAILED', r.stderr.strip()); return False
    if expected_weight is not None:
        try:
            weight = json.loads(r.stdout)['weight']
        except (ValueError, KeyError, TypeError):
            print(name, 'FAILED: missing spatial-weight status'); return False
        if not np.isfinite(weight) or abs(weight - expected_weight) > 1e-6:
            print(name, 'FAILED: L8 spatial weight', weight, 'expected', expected_weight); return False

    g = np.fromfile(go, np.float32).reshape(-1, 4)[:, :3].astype(float); c = np.fromfile(co, np.float32).reshape(-1, 4)[:, :3].astype(float)
    if not (np.isfinite(g).all() and np.isfinite(c).all()
            and ((g >= 0) & (g <= 1)).all() and ((c >= 0) & (c <= 1)).all()):
        print(name, 'FAILED: non-finite or out-of-domain PQ output'); return False

    if reference_samples is not None:
        indexes = np.array([y * w + x for x, y, _ in reference_samples])
        reference = np.array([rgb for _, _, rgb in reference_samples])
        golden_error = max(de(g[indexes], reference).max(), de(c[indexes], reference).max())
        if golden_error >= 0.01:
            print(name, 'FAILED: licensed spatial reference mismatch, max dE', golden_error); return False
    d = np.abs(g - c).max(); e = de(g, c)
    print('%-28s %s  max|GPU-CPU| %.2e PQ  dE mean %.4f max %.4f' % (name, r.stdout.strip(), d, e.mean(), e.max()))
    if synthetic:
        target_peak = float(next(line.split()[1] for line in params.splitlines() if line.startswith('target ')))
        lg, lc = eotf(g), eotf(c)
        print('  max linear channel error / target peak %.3e' % (np.abs(lg - lc).max() / target_peak))
        return e.mean() < 0.02 and e.max() < 0.5 and np.all(synthetic_channel_ok(g, c, lg, lc, target_peak))
    if name.startswith('cm4_'):
        lg, lc = eotf(g), eotf(c)
        peak = np.maximum(lg.max(axis=1), lc.max(axis=1))[:, None]
        channels_ok = precise_cm4_channel_ok(g, c, lg, lc, peak)
        if not channels_ok.all():
            print('  FAIL: CM4 channel exceeds both 1.5e-6 PQ and four binary32 epsilons of pixel peak light')
            return False
        return e.max() < 0.05
    return e.max() < 0.05 and d < 2e-4

W, H = 512, 256
frames = []
n4k = 3840 * 2160 * 3
for k in (1, 2, 4):
    a = np.fromfile(SRC, np.float32, count=n4k, offset=k * n4k * 4).reshape(2160, 3840, 3)
    frames.append(a[1000:1000 + H, 1600:1600 + W].astype(float))
g = np.linspace(0, 1, 9)
lat = np.array([[r, gg, b] for r in g for gg in g for b in g])
syn = np.zeros((H, W, 3)); syn[:, :, :] = 0.3
syn.reshape(-1, 3)[:len(lat) * 4:4] = lat
syn[100:140, 200:260] = (0.7, 0.2, 0.1); syn[30:60, 30:400] = (0.2, 0.6, 0.8)
ok = True
up = """l1 5 1229 2269
l3 2048 1488 2048
l6 50 4000
l9 0
l254
l2 2081 1049 2030 850 2048 1884 2048
l2 2851 2075 2049 1795 2048 2048 2048
l2 3079 2069 2049 1862 2048 2048 2048
l8 1 10 2028 2048 1929 2048 1884 2048 2048 2048 128 128 128 128 128 128 128 128 128 128 128 128
l8 27 10 2048 2048 2048 2048 2048 2048 2048 2048 128 128 128 128 128 128 128 128 128 128 128 128
"""
for tgt in ("300 0 1 4000", "100 0.005 1 4000", "1000 0 1 4000", "600 0 0 4000"):
    ok &= run('cm4_real_' + tgt.split()[0] + '_p' + tgt.split()[2], up + 'target ' + tgt + '\n', frames[0], W, H)
syn4 = """l1 0 1600 3500
l3 2048 2048 2048
l6 50 4000
l9 2
l254
l8 1 25 2300 2100 1800 2300 1700 2048 2400 2200 160 120 128 100 140 128 128 150 110 128 128 140
l8 48 25 1900 2000 2200 1900 2300 2048 1800 1900 120 128 140 128 128 110 140 128 128 120 128 128
"""
for tgt in ("400 0.005 1 4000", "1000 0 1 4000", "2000 0 0 4000", "100 0.005 2 4000"):
    ok &= run('cm4_syn_' + tgt.split()[0] + '_p' + tgt.split()[2], syn4 + 'target ' + tgt + '\n', syn, W, H, True)
    ok &= run('cm4_synreal_' + tgt.split()[0] + '_p' + tgt.split()[2], syn4 + 'target ' + tgt + '\n', frames[1], W, H)
# L8 MS is a spatial parameter: flat tiles cannot detect a missing decode or
# wrong interpolation domain. Check real parser status and independent oracle
# samples, in addition to CPU/GPU parity, on a periodic sinusoid/step texture.
ms_image = spatial_texture(W, H)
for case in MS_REFERENCE_CASES:
    ok &= run(case['name'], case['params'], ms_image, W, H,
              expected_weight=case['weight'], reference_samples=case['samples'])
# CM4 L5 active areas: asymmetric bars over real pixels (bar content takes the pointwise mapping),
# the synthetic lattice, a one-row and a one-column active picture. These select the boundary-aware
# shaders; the full-frame cases above keep the original literals.
# Bars carry an adversarial bright/dark checker so a filter tap that leaks one row or column past the
# boundary moves the active result far beyond the real-crop limits.
def with_bars(img, left, right, top, bottom):
    out = img.copy(); yy, xx = np.mgrid[0:H, 0:W]
    bars = (xx < left) | (xx >= W - right) | (yy < top) | (yy >= H - bottom)
    out[bars] = np.where(((xx + yy) % 2)[bars, None] == 1, 0.92, 0.0001)
    return out
for name, l5, img, synthetic in (('real', '7 11 20 13', frames[0], False), ('syn', '0 0 31 29', syn, True),
                                 ('row', '0 0 128 127', frames[1], False), ('col', '255 256 0 0', frames[1], False)):
    img = with_bars(img, *map(int, l5.split()))
    params = (syn4 if synthetic else up) + 'l5 ' + l5 + '\n'
    for tgt in ("1000 0 1 4000", "100 0.005 2 4000"):
        ok &= run('cm4_l5_' + name + '_' + tgt.split()[0] + '_p' + tgt.split()[2], params + 'target ' + tgt + '\n', img, W, H, synthetic)
cm29 = """l1 80 1528 3728
l6 50 4000
l2 2081 2076 2030 2000 2020 2030 512
l2 2851 2000 2040 2150 2048 2294 512
l2 3079 2010 2180 2075 2050 2179 512
"""
for tgt in ("600 0 1 4000", "1000 0 1 4000", "100 0.005 2 4000", "2000 0 0 4000"):
    ok &= run('cm29_' + tgt.split()[0] + '_p' + tgt.split()[2], cm29 + 'target ' + tgt + '\n', frames[2], W, H)
    ok &= run('cm29_syn_' + tgt.split()[0] + '_p' + tgt.split()[2], cm29 + 'target ' + tgt + '\n', syn, W, H, True)
# CM v2.9 without chroma weight (single detail plane) and with the default MS weight (raw -1)
cm29b = cm29.replace('2020 2030 512', '2048 2030 65535', 1).replace('2048 2294 512', '2048 2294 65535', 1).replace('2050 2179 512', '2048 2179 65535', 1)
for tgt in ("1000 0 1 4000", "100 0.005 2 4000"):
    ok &= run('cm29b_' + tgt.split()[0] + '_p' + tgt.split()[2], cm29b + 'target ' + tgt + '\n', frames[2], W, H)
# CM v2.9 L5 active areas, as for CM4 above: both detail planes (cm29: live chroma weight) and the
# single plane (cm29b) must stay inside the rectangle on both separable passes; bars take the
# pointwise mapping. The adversarial bar checker turns a one-row or one-column leak into a failure.
for fam, meta in (('cm29', cm29), ('cm29b', cm29b)):
    for name, l5, img, synthetic in (('real', '7 11 20 13', frames[2], False), ('syn', '0 0 31 29', syn, True),
                                     ('row', '0 0 128 127', frames[2], False), ('col', '255 256 0 0', frames[2], False)):
        img = with_bars(img, *map(int, l5.split()))
        for tgt in ("1000 0 1 4000", "100 0.005 2 4000"):
            ok &= run(fam + '_l5_' + name + '_' + tgt.split()[0] + '_p' + tgt.split()[2], meta + 'l5 ' + l5 + '\ntarget ' + tgt + '\n', img, W, H, synthetic)
# raw ms_weight 0 is MS weight -1: the detail stage is skipped (single-pass shader path)
cm29c = cm29.replace(' 512\n', ' 0\n')
ok &= run('cm29_nodetail_1000_p1', cm29c + 'target 1000 0 1 4000\n', frames[2], W, H)
# CM v2.9 near black (cm29_dark_colour): saturated and part-desaturated colours on eight hues from
# PQ 0 to 0.25, so the near-black chroma gain, its offset above the target black (0 and 0.005 nit)
# and the intensity-ratio cap run on the GPU. Real-crop limits for P3 and BT.709; the BT.2020 target
# keeps fully saturated BT.2020 colours, where float32 loses digits as on the lattice (synthetic limits).
def cm29_dark_colour():
    ramp = np.linspace(0, 0.25, W)
    hues = np.array([[1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 1, 0], [0, 1, 1], [1, 0, 1], [1, .5, 0], [.3, .6, 1]])
    img = np.zeros((H, W, 3))
    for r in range(H):
        mix = (r // len(hues)) / (H // len(hues))
        img[r] = ramp[:, None] * (hues[r % len(hues)] * (1 - mix) + 0.5 * mix)
    return img
dark = cm29_dark_colour()
for tgt in ("600 0 1 4000", "1000 0.005 1 4000", "1000 0 0 4000", "100 0.005 2 4000"):
    ok &= run('cm29_dark_colour_' + tgt.split()[0] + '_f' + tgt.split()[1] + '_p' + tgt.split()[2], cm29 + 'target ' + tgt + '\n', dark, W, H, tgt.split()[2] == '0')
# Exact measured targets exercise every uploaded-table axis on real and dark
# colour rasters. Fallback/other-black cases above remain independent controls.
for peak in (600, 1000, 2000):
    params = cm29 + 'target %d 0.005 1 4000\n' % peak
    ok &= run('cm29_measured_real_%d_p1' % peak, params, frames[2], W, H)
    ok &= run('cm29_measured_dark_%d_p1' % peak, params, dark, W, H)
ok &= run('cm29_measured_l5_1000_p1', cm29 + 'l5 7 11 20 13\ntarget 1000 0.005 1 4000\n',
          with_bars(frames[2], 7, 11, 20, 13), W, H)

# CM v2.9 detail at the top clip (cm29_top_clip): fields from below to above the intensity where the
# tone curve reaches its top value (L1 max 3728 = PQ 0.910), with gray and coloured specks darker and
# brighter than the field on a 16x8 grid. Detail there comes from the unclamped mapped intensity and
# only the finished value is held to the peak, on both detail planes (cm29: live chroma weight).
def cm29_top_clip():
    img = np.zeros((H, W, 3)); rng = np.random.default_rng(29)
    fields = np.linspace(0.84, 1.0, 8)
    for b, v in enumerate(fields):
        r0, r1 = b * H // len(fields), (b + 1) * H // len(fields)
        img[r0:r1] = v
        ys, xs = np.meshgrid(np.arange(r0 + 4, r1 - 4, 8), np.arange(8, W - 8, 16), indexing='ij')
        n = ys.size
        speck = np.clip(v + rng.uniform(-0.2, 0.06, (n, 1)) + rng.uniform(-0.05, 0.05, (n, 3)) * (rng.uniform(0, 1, (n, 1)) < 0.5), 0, 1)
        img[ys.ravel(), xs.ravel()] = speck
    return img
clip = cm29_top_clip()
# The 4000-nit target sits above the 3728 source max's intensity, so the curve clips at (or just above)
# the source maximum: the no-clip path. A uniform left at 0 would hold every pixel at 0 and the GPU
# would leave the CPU reference by a wide margin.
for fam, meta in (('cm29', cm29), ('cm29b', cm29b)):
    for tgt in ("600 0 1 4000", "1000 0.005 1 4000", "4000 0 1 4000"):
        ok &= run(fam + '_top_clip_' + tgt.split()[0] + '_p' + tgt.split()[2], meta + 'target ' + tgt + '\n', clip, W, H)
# The chroma-weighted plane at large L2 chroma weights (raw 1000: cw -0.51, raw 3000: +0.46,
# on the 600-nit target's exact L2): fields above the clip with chroma-rich specks on the 16x8 grid.
def cm29_top_clip_chroma():
    img = np.zeros((H, W, 3)); rng = np.random.default_rng(2929)
    fields = np.linspace(0.84, 1.0, 8)
    for b, v in enumerate(fields):
        r0, r1 = b * H // len(fields), (b + 1) * H // len(fields)
        img[r0:r1] = v
        ys, xs = np.meshgrid(np.arange(r0 + 4, r1 - 4, 8), np.arange(8, W - 8, 16), indexing='ij')
        img[ys.ravel(), xs.ravel()] = rng.uniform(0.45, 1.0, (ys.size, 3))
    return img
clipc = cm29_top_clip_chroma()
for raw in (1000, 3000):
    meta = cm29.replace('2000 2040 2150 2048 2294 512', '2000 2040 2150 %d 2294 512' % raw, 1)
    assert meta != cm29
    ok &= run('cm29_top_clip_chroma%d_600_p1' % raw, meta + 'target 600 0 1 4000\n', clipc, W, H)
# a black frame (L1 0,0,0) maps instead of refusing
# CM v2.9 L2 selection and the unclamped curve (the licensed values are pinned in
# test_dm_active_area.c): gray and tinted ramps over the whole PQ range, so pixels run under the L1
# crush and over the L1 clip, with specks for the detail planes. An L2 at 100 nits only (interpolated
# towards neutral at the mastering peak, darkening: gradation above the clip), a high L1 minimum
# with a lifting exact L2 over a non-zero black (the curve runs under the target black), an L2 at
# the 1000-nit mastering peak bracketing 600, and an exact L2 above the mastering peak.
def cm29_l2_ramp():
    img = np.zeros((H, W, 3)); rng = np.random.default_rng(2902)
    ramp = np.linspace(0, 1, W)
    for b, tint in enumerate(((0, 0, 0), (0.03, -0.02, 0.0), (-0.04, 0.0, 0.05), (0.0, 0.04, -0.06))):
        img[b * H // 4:(b + 1) * H // 4] = np.clip(ramp[:, None] + np.array(tint), 0, 1)
    ys, xs = np.meshgrid(np.arange(4, H - 4, 8), np.arange(8, W - 8, 16), indexing='ij')
    img[ys.ravel(), xs.ravel()] = np.clip(img[ys.ravel(), xs.ravel()] + rng.uniform(-0.1, 0.05, (ys.size, 1)), 0, 1)
    return img
l2ramp = cm29_l2_ramp()
for name, meta, tgt in (
        ('interp', 'l1 2 2048 3697\nl6 50 4000\nl2 2081 1500 2010 1300 2048 2048 2048\n', '600 0 1 4000'),
        ('interp_cw', 'l1 2 2048 3697\nl6 50 4000\nl2 2081 1500 2010 1300 2500 1900 512\n', '1000 0.005 1 4000'),
        ('crush', 'l1 205 1229 2457\nl6 50 4000\nl2 2851 2200 2060 2300 2048 2048 2048\n', '600 0.005 1 4000'),
        ('crush_untrimmed', 'l1 205 1229 2457\nl6 50 4000\n', '600 0.005 1 4000'),
        ('atpeak', 'l1 2 2048 3697\nl6 50 1000\nl2 3079 1400 2000 1400 2048 2048 2048\n', '600 0 1 1000'),
        ('above', 'l1 2 2048 3697\nl6 50 1000\nl2 3388 2150 2052 1900 2048 2048 2048\n', '2000 0 1 1000')):
    ok &= run('cm29_l2_' + name, meta + 'target ' + tgt + '\n', l2ramp, W, H)
black = 'l1 0 0 0\nl6 50 4000\n'
ok &= run('cm29_black_1000_p1', black + 'target 1000 0 1 4000\n', np.zeros((H, W, 3)), W, H)
ok &= run('cm4_black_1000_p1', black + 'l254\ntarget 1000 0 1 4000\n', np.zeros((H, W, 3)), W, H)
if ONLY - SEEN:
    print('Unknown parity cases: ' + ', '.join(sorted(ONLY - SEEN)), file=sys.stderr)
    sys.exit(2)
print('PASS' if ok else 'FAIL')
sys.exit(0 if ok else 1)
