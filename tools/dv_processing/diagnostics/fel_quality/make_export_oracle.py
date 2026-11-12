"""X0 (protocol-export-floor-20261112.json): oracle 10-bit 4:2:0 exports built from the composition.

For every frame of a canonical BL/EL capture this writes, as 48-frame yuv420p10le files:
  Q   luma (composition12 + 2) >> 2; chroma at each BL chroma sample = round(mean/4) of the
      composition12 chroma over the luma positions that four_model_audit.sample_codes maps to it
  T   the same with truncation (floor) instead of rounding
  PREY_QUV / QY_PREUV  component swaps between the retained export (PRE) and Q
plus signed PRE-Q statistics per plane (mean, RMS, x/y parity of luma). It uses the frozen
sample_codes / composition12 and nothing else. Q is an oracle floor built from the scoring
model's own composition, not a normative Dolby composer.
Usage: make_export_oracle.py TOOLS BL EL PRE.yuv OUTDIR
"""
from pathlib import Path
import hashlib, importlib.util, json, sys
sys.dont_write_bytecode = True
import numpy as np

tools, blp, elp, prep, outdir = (Path(x) for x in sys.argv[1:6])
spec = importlib.util.spec_from_file_location('four_model_audit', tools / 'four_model_audit.py')
sys.path.insert(0, str(tools)); fm = importlib.util.module_from_spec(spec); spec.loader.exec_module(fm)
w, h, ew, eh = 3840, 2160, 1920, 1080
fb, efb, Y, C = w * h * 3 // 2, ew * eh * 3 // 2, w * h, (w // 2) * (h // 2)
bl = np.memmap(blp, dtype='<u2', mode='r').reshape(48, fb)
el = np.memmap(elp, dtype='<u2', mode='r').reshape(48, efb)
pre = np.memmap(prep, dtype='<u2', mode='r').reshape(48, fb)
outdir.mkdir(parents=True, exist_ok=False)
outs = {k: np.memmap(outdir / f'{k}.yuv', dtype='<u2', mode='w+', shape=(48, fb)) for k in ('Q', 'T', 'PREY_QUV', 'QY_PREUV')}

# Chroma-sample index each luma row/column maps to, exactly as sample_codes clamps it.
cy = np.minimum((np.arange(h) + 1) // 2, h // 2 - 1); cx = np.minimum((np.arange(w) + 1) // 2, w // 2 - 1)
cnt = np.zeros((h // 2, w // 2)); np.add.at(cnt, (cy[:, None], cx[None, :]), 1)


def planes(raw, i, ww, hh):
    x = raw[i]
    return (x[:ww * hh].reshape(hh, ww), x[ww * hh:ww * hh * 5 // 4].reshape(hh // 2, ww // 2), x[ww * hh * 5 // 4:].reshape(hh // 2, ww // 2))


stats = {p: {'sum': 0.0, 'sq': 0.0, 'n': 0} for p in ('Y', 'Cb', 'Cr')}
parity = {f'{a}{b}': {'sum': 0.0, 'sq': 0.0, 'n': 0} for a in (0, 1) for b in (0, 1)}
for f in range(48):
    b, e = planes(bl, f, w, h), planes(el, f, ew, eh)
    qy = np.empty((h, w), np.int64); ty = np.empty((h, w), np.int64)
    csum = [np.zeros((h // 2, w // 2)) for _ in range(2)]
    for lo in range(0, h, 64):
        hi = min(lo + 64, h)
        base, centered = fm.sample_codes(b, e, planes(pre, f, w, h), w, h, lo, hi)['R']
        r12 = fm.composition12(base, centered)
        qy[lo:hi] = np.clip((r12[..., 0] + 2) >> 2, 0, 1023); ty[lo:hi] = np.clip(r12[..., 0] >> 2, 0, 1023)
        for k in range(2):
            np.add.at(csum[k], (cy[lo:hi, None], cx[None, :]), r12[..., k + 1])
    qc = [np.clip(np.floor(s / cnt / 4 + 0.5), 0, 1023).astype(np.int64) for s in csum]
    tc = [np.clip(np.floor(s / cnt / 4), 0, 1023).astype(np.int64) for s in csum]
    p = pre[f]
    outs['Q'][f] = np.concatenate([qy.ravel(), qc[0].ravel(), qc[1].ravel()])
    outs['T'][f] = np.concatenate([ty.ravel(), tc[0].ravel(), tc[1].ravel()])
    outs['PREY_QUV'][f] = np.concatenate([p[:Y], qc[0].ravel(), qc[1].ravel()])
    outs['QY_PREUV'][f] = np.concatenate([qy.ravel(), p[Y:]])
    d = {'Y': p[:Y].reshape(h, w).astype(np.int64) - qy, 'Cb': p[Y:Y + C].astype(np.int64) - qc[0].ravel(), 'Cr': p[Y + C:].astype(np.int64) - qc[1].ravel()}
    for k, v in d.items():
        stats[k]['sum'] += float(v.sum()); stats[k]['sq'] += float((v.astype(np.float64) ** 2).sum()); stats[k]['n'] += v.size
    for a in (0, 1):
        for c in (0, 1):
            v = d['Y'][a::2, c::2]
            parity[f'{a}{c}']['sum'] += float(v.sum()); parity[f'{a}{c}']['sq'] += float((v.astype(np.float64) ** 2).sum()); parity[f'{a}{c}']['n'] += v.size
    print(json.dumps({'frame': f}), flush=True)
for o in outs.values():
    o.flush()
fin = lambda s: {'mean_codes': s['sum'] / s['n'], 'rms_codes': (s['sq'] / s['n']) ** 0.5, 'samples': s['n']}
rec = {'scope': 'X0 oracle export floor; oracle from the scoring composition, not normative Dolby',
       'inputs': {k: {'path': str(v), 'sha256': hashlib.sha256(Path(v).read_bytes()).hexdigest()} for k, v in (('bl', blp), ('el', elp), ('pre', prep))},
       'script_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
       'outputs': {k: str(outdir / f'{k}.yuv') for k in outs},
       'PRE_minus_Q': {k: fin(v) for k, v in stats.items()}, 'PRE_minus_Q_luma_parity_yx': {k: fin(v) for k, v in parity.items()}}
(outdir / 'export-oracle.json').write_text(json.dumps(rec, indent=2) + '\n')
print(json.dumps(rec['PRE_minus_Q']))
