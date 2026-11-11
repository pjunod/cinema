"""Build component-swap counterfactual YUV for the lattice evaluator.

Diagnostic only (not an authored stream or an implementable fix): writes
<A>y<B>uv.yuv with the luma plane of A and both chroma planes of B, for every
frame of two equal-length yuv420p10le files. Used for the Michael5400 all-48
swap readout (encoded Z luma + exact export UV, and the reverse).
Usage: make_component_swaps.py WIDTH HEIGHT FRAMES A.yuv B.yuv OUT_A_Y_B_UV.yuv OUT_B_Y_A_UV.yuv
"""
import sys
import numpy as np

w, h, n = (int(x) for x in sys.argv[1:4])
fb, y = w * h * 3 // 2, w * h
a = np.memmap(sys.argv[4], dtype='<u2', mode='r').reshape(n, fb)
b = np.memmap(sys.argv[5], dtype='<u2', mode='r').reshape(n, fb)
for path, (ysrc, uvsrc) in ((sys.argv[6], (a, b)), (sys.argv[7], (b, a))):
    out = np.memmap(path, dtype='<u2', mode='w+', shape=(n, fb))
    out[:, :y] = ysrc[:, :y]
    out[:, y:] = uvsrc[:, y:]
    out.flush()
