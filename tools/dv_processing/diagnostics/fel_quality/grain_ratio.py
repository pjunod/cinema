"""Grain-retention readout for psy arms (protocol-psy-split-20261112.json), report-only.

High-pass luma energy of each decoded candidate over that of the encoder input, on 16x16 blocks whose
encoder-input mean Y' decodes (ST 2084) to 1-100 nit. High-pass = Y' minus its 3x3 box mean, in 10-bit
codes; energy = mean square over the selected blocks' pixels. A ratio below 1 means the encode removed
texture energy the input had; ΔITP cannot see this. Not a perceptual metric.
Usage: grain_ratio.py INPUT.yuv OUT.json LABEL=decoded.yuv [...]   (3840x2160 yuv420p10le, 48 frames)
"""
from pathlib import Path
import hashlib, json, sys
import numpy as np

W, H, N, G = 3840, 2160, 48, 16
FB = W * H * 3 // 2


def pq_nits(e):
    m1, m2, c1, c2, c3 = 2610 / 16384, 2523 / 4096 * 128, 3424 / 4096, 2413 / 4096 * 32, 2392 / 4096 * 32
    p = np.power(np.clip(e, 0, 1), 1 / m2)
    return 10000 * np.power(np.maximum(p - c1, 0) / (c2 - c3 * p), 1 / m1)


def hp(y):
    y = y.astype(np.float64); p = np.pad(y, 1, mode='edge')
    box = sum(p[i:i + H, j:j + W] for i in range(3) for j in range(3)) / 9
    return y - box


inp = np.memmap(sys.argv[1], dtype='<u2', mode='r').reshape(N, FB)
cands = {s.split('=', 1)[0]: np.memmap(s.split('=', 1)[1], dtype='<u2', mode='r').reshape(N, FB) for s in sys.argv[3:]}
acc = {k: [0.0, 0.0] for k in cands}; frames = []
for f in range(N):
    yi = inp[f][:W * H].reshape(H, W)
    nits = pq_nits((yi.reshape(H // G, G, W // G, G).mean(axis=(1, 3)) - 64) / 876)
    m = np.repeat(np.repeat((nits >= 1) & (nits < 100), G, 0), G, 1)
    ei = float((hp(yi)[m] ** 2).sum()); row = {'frame': f, 'pixels': int(m.sum())}
    for k, c in cands.items():
        ec = float((hp(c[f][:W * H].reshape(H, W))[m] ** 2).sum())
        acc[k][0] += ec; acc[k][1] += ei; row[k] = ec / ei if ei else None
    frames.append(row)
rec = {'scope': 'report-only grain proxy: high-pass luma energy ratio decoded/encoder input, 1-100 nit blocks; not perceptual',
       'script_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
       'inputs': {'encoder_input': sys.argv[1], **{k: s.split('=', 1)[1] for k, s in zip(cands, sys.argv[3:])}},
       'pooled_ratio': {k: v[0] / v[1] for k, v in acc.items()}, 'frames': frames}
Path(sys.argv[2]).write_text(json.dumps(rec, indent=1) + '\n')
print(json.dumps(rec['pooled_ratio']))
