#!/usr/bin/env python3
"""GPU shader vs CPU reference parity for dv_dm.h, on real-frame crops and synthetic rasters.
Runs dm_shader_control for each case; prints max |GPU-CPU| (PQ) and mean/max dE_ITP.

usage: run_dm_parity.py DM_SHADER_CONTROL SCRATCH_DIR SRC_F32
SRC_F32: at least five 3840x2160 frames of float32 BT.2020/PQ RGB (the decoded master).
Real crops (real pixels, real or synthetic metadata) must match to dE_ITP max 0.05 and 2e-4 PQ.
The synthetic raster is a 9^3 lattice reaching 10000-nit fully saturated BT.2020 corners, where the
shader's float32 arithmetic loses digits against the double reference; it must match to dE_ITP mean
0.02 and max 0.5 (well under one JND) and 0.05 PQ in any single channel."""
import json, os, subprocess, sys
import numpy as np
CTL = sys.argv[1]; WORK = sys.argv[2]; SRC = sys.argv[3]  # control binary, scratch dir, real 4K frames (src.yuv)
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

def run(name, params, rgb, w, h, synthetic=False):
    rgba = np.concatenate([rgb.reshape(h, w, 3), np.ones((h, w, 1))], -1).astype(np.float32)
    pf, inf, go, co = (os.path.join(WORK, name + s) for s in ('.params', '.in', '.gpu', '.cpu'))
    open(pf, 'w').write(params); rgba.tofile(inf)
    for f in (go, co):
        if os.path.exists(f): os.remove(f)
    r = subprocess.run([CTL, pf, inf, str(w), str(h), go, co], capture_output=True, text=True)
    if r.returncode: print(name, 'FAILED', r.stderr.strip()); return False
    g = np.fromfile(go, np.float32).reshape(-1, 4)[:, :3].astype(float); c = np.fromfile(co, np.float32).reshape(-1, 4)[:, :3].astype(float)
    d = np.abs(g - c).max(); e = de(g, c)
    print('%-28s %s  max|GPU-CPU| %.2e PQ  dE mean %.4f max %.4f' % (name, r.stdout.strip(), d, e.mean(), e.max()))
    if synthetic:
        return e.mean() < 0.02 and e.max() < 0.5 and d < 0.05
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
# a black frame (L1 0,0,0) maps instead of refusing
black = 'l1 0 0 0\nl6 50 4000\n'
ok &= run('cm29_black_1000_p1', black + 'target 1000 0 1 4000\n', np.zeros((H, W, 3)), W, H)
ok &= run('cm4_black_1000_p1', black + 'l254\ntarget 1000 0 1 4000\n', np.zeros((H, W, 3)), W, H)
print('PASS' if ok else 'FAIL')
sys.exit(0 if ok else 1)
