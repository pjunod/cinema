"""dv_patch_check.py -- flat-patch equivalence test of a decoder's Dolby Vision 8.1 reshaping (no capture hardware needed).

make READBACK.json OUTDIR   writes A.yuv (patch codes c) and B.yuv (patch codes round(f(c)/4)), 3840x2160 10-bit 4:2:0,
                            plus patches.json. f is the readback's mapping under libplacebo pl_shader_dovi_reshape semantics.
                            A is played with the re-fit RPU, B and C (= A's pixels) with the identity RPU.
                            A decoder that applies the curve as libplacebo does shows A == B per patch (to the 1/4-code rounding);
                            one that ignores it shows A == C.
score PATCHES.json A.raw B.raw C.raw [B2.raw]
                            per-patch means of CARenderer dumps (dv_display_check.swift ... OUT.raw, rgba16Float 960x540) and the verdict.
"""
import json
import sys
from pathlib import Path

import numpy as np

W, H, NX, NY = 3840, 2160, 8, 6
PW, PH = W // NX, H // NY


def reshape(rb, y, u, v):
    """libplacebo semantics on 10-bit codes -> 12-bit composed (y, u, v)."""
    piv = np.cumsum(rb['luma_pivot_deltas']); luma = np.array(rb['luma_coef'])
    mmr = [np.array([c['constant']] + [x for row in c['coef'] for x in row]) for c in rb['chroma']]
    b = np.stack([y, u, v], -1).astype(np.float64)
    x = np.clip(b / 1024, 0, 1023 / 1024)
    idx = np.clip(np.searchsorted(piv[1:-1], b[..., 0], side='right'), 0, len(piv) - 2)
    c = luma[idx]; s0 = c[..., 0] + c[..., 1] * x[..., 0] + c[..., 2] * x[..., 0] ** 2
    yy, uu, vv = x[..., 0], x[..., 1], x[..., 2]; t = [yy, uu, vv, yy * uu, yy * vv, uu * vv, yy * uu * vv]
    X = np.stack([np.ones_like(yy)] + [tt ** p for p in (1, 2, 3) for tt in t], -1)
    out = np.stack([s0, X @ mmr[0], X @ mmr[1]], -1)
    return np.floor(np.clip(out, 0, 1) * 4096 + .5)


def make(readback, outdir):
    rb = json.loads(Path(readback).read_text())['frames'][0]
    rng = np.random.default_rng(20261112)
    ys = np.linspace(96, 900, NX * NY).round()
    rng.shuffle(ys)
    us = 512 + rng.integers(-72, 73, NX * NY); vs = 512 + rng.integers(-72, 73, NX * NY)
    # nudge each patch (within +-6 codes per plane) to where f(c)/4 lands closest to an integer, so B's rounding stays small
    o = np.stack(np.meshgrid(*[np.arange(-6, 7)] * 3, indexing='ij'), -1).reshape(-1, 3)
    for k in range(NX * NY):
        cand = np.array([ys[k], us[k], vs[k]]) + o
        q = reshape(rb, cand[:, 0], cand[:, 1], cand[:, 2]) / 4
        ys[k], us[k], vs[k] = cand[np.abs(q - np.round(q)).max(1).argmin()]
    comp = reshape(rb, ys, us, vs)
    bcodes = np.clip(np.floor(comp / 4 + .5), 0, 1023)
    assert ((comp > 16) & (comp < 4080)).all(), 'keep every patch away from the clip points'
    outdir = Path(outdir); outdir.mkdir(parents=True, exist_ok=True)
    for name, codes in (('A', np.stack([ys, us, vs], -1)), ('B', bcodes)):
        Y = np.zeros((H, W), np.uint16); C = np.zeros((2, H // 2, W // 2), np.uint16)
        for k in range(NX * NY):
            px, py = k % NX, k // NX
            Y[py * PH:(py + 1) * PH, px * PW:(px + 1) * PW] = codes[k, 0]
            C[:, py * PH // 2:(py + 1) * PH // 2, px * PW // 2:(px + 1) * PW // 2] = codes[k, 1:, None, None]
        (outdir / f'{name}.yuv').write_bytes(Y.tobytes() + C.tobytes())
    (outdir / 'patches.json').write_text(json.dumps({'grid': [NX, NY], 'A': np.stack([ys, us, vs], -1).tolist(), 'B': bcodes.tolist(),
                                                      'predicted_composed12': comp.tolist()}, indent=1))
    print('patches', NX * NY, 'max |B - f(A)/4| in 10-bit codes', float(np.abs(bcodes - comp / 4).max()))


def means(raw):
    px = np.frombuffer(Path(raw).read_bytes(), np.float16).astype(np.float64).reshape(540, 960, 4)[::-1, :, :3]  # CARenderer's texture is bottom-up
    pw, ph = 960 // NX, 540 // NY; out = []
    for k in range(NX * NY):
        x0, y0 = (k % NX) * pw, (k // NX) * ph
        out.append(px[y0 + ph // 4:y0 + 3 * ph // 4, x0 + pw // 4:x0 + 3 * pw // 4].reshape(-1, 3).mean(0))
    return np.array(out)


def score(patches, a, b, c, b2=None):
    A, B, C = means(a), means(b), means(c)
    rel = lambda p, q: np.abs(p - q) / np.maximum(np.abs(q), 1e-4)
    ab, ac = rel(A, B), rel(A, C)
    cb, abd = np.abs(C - B).sum(1), np.abs(A - B).sum(1)
    moved = cb > 0.005  # patches where the curve visibly departs from identity
    rec = 1 - abd[moved] / cb[moved]  # share of the predicted change the decoder reproduced (1 = libplacebo semantics, 0 = RPU ignored)
    rep = {'patches': len(A), 'A_vs_B_rel': {'median': float(np.median(ab)), 'p90': float(np.percentile(ab, 90)), 'max': float(ab.max())},
           'A_vs_C_rel': {'median': float(np.median(ac)), 'p90': float(np.percentile(ac, 90)), 'max': float(ac.max())},
           'patches_closer_to_B': int((abd < np.abs(A - C).sum(1)).sum()),
           'recovered_share_where_curve_moves': {'patches': int(moved.sum()), 'median': float(np.median(rec)), 'p10': float(np.percentile(rec, 10)),
                                                 'min': float(rec.min()), 'above_0.5': int((rec > 0.5).sum())}}
    if b2:
        bb = rel(means(b2), B); rep['B_repeat_rel'] = {'median': float(np.median(bb)), 'max': float(bb.max())}
    rep['per_patch'] = [{'codes_A': json.loads(Path(patches).read_text())['A'][k], 'A': A[k].round(5).tolist(), 'B': B[k].round(5).tolist(), 'C': C[k].round(5).tolist()} for k in range(len(A))]
    print(json.dumps(rep, indent=1))


if __name__ == '__main__':
    {'make': make, 'score': score}[sys.argv[1]](*sys.argv[2:])
