"""inject_rpu.py IN.hevc NAL_DIR OUT.hevc -- put one RPU NAL (type 62, file as written by rpu_refit_write) at the end of each
access unit of an Annex B HEVC stream without B-frames (decode order = presentation order = RPU order)."""
import sys
from pathlib import Path
src, nd, out = Path(sys.argv[1]).read_bytes(), Path(sys.argv[2]), Path(sys.argv[3])
starts = []; i = 0
while True:
    j = src.find(b'\x00\x00\x01', i)
    if j < 0: break
    starts.append(j + 3); i = j + 3
nals = [src[s:(starts[k + 1] - 3 if k + 1 < len(starts) else len(src))].rstrip(b'\x00') if k + 1 < len(starts) else src[s:] for k, s in enumerate(starts)]
rpus = [nd.joinpath(f'frame-{k:03d}.nal').read_bytes() for k in range(len(list(nd.glob('frame-*.nal'))))]
o = bytearray(); au = -1; seen_vcl = False
def emit(n): o.extend(b'\x00\x00\x00\x01' + n)
for n in nals:
    t = (n[0] >> 1) & 0x3f
    vcl = t < 32
    first = vcl and (n[2] & 0x80)
    if seen_vcl and (first or t in (32, 33, 34, 35, 39)):
        emit(rpus[au]); seen_vcl = False
    if first:
        au += 1
    if t == 62:
        continue
    emit(n)
    seen_vcl = seen_vcl or vcl
if seen_vcl: emit(rpus[au])
assert au + 1 == len(rpus), (au + 1, len(rpus))
out.write_bytes(bytes(o)); print('aus', au + 1)
