"""Land an encode arm at the protocol's authored-rate band by changing only the ABR target.

Rule (protocol-aq-psy-20261112.json, rate_rule): only `b` changes, secant steps, at most
4 encodes per arm and fixture, until the predicted authored packet average is inside
[128.5, 130.0] Mb/s; the last in-band encode is the arm. The rule reads rate only.
Prediction = encoded video-packet average + measured authoring overhead (default 0.03 Mb/s,
the Michael windows' authored-minus-encoded). The chosen encode must still pass the
qualifier's actual authored-rate screen afterwards.

A diagnostic rate rung (protocol-beekeeper-rung-20261112.json) passes --target T: band T +/- 0.75 Mb/s instead.
Usage: land_rate.py --prefix P [--b0 115000] [--target T] -- <encode_roi_control.py args without --prefix>
Writes <output-root>/corrected-encodes/P-landing.json; attempts are P-L1 .. P-L4.
"""
from pathlib import Path
import argparse, json, subprocess, sys

ap = argparse.ArgumentParser()
ap.add_argument('--prefix', required=True)
ap.add_argument('--b0', type=int, default=115000, help='first ABR target, kb/s')
ap.add_argument('--overhead', type=float, default=0.03)
ap.add_argument('--target', type=float, help='diagnostic rung: land at TARGET +/- 0.75 Mb/s (default band 128.5-130.0)')
ap.add_argument('--driver', default=str(Path(__file__).with_name('encode_roi_control.py')))
ap.add_argument('rest', nargs=argparse.REMAINDER)
a = ap.parse_args()
LO, HI, TARGET = (a.target - 0.75, a.target + 0.75, a.target) if a.target else (128.5, 130.0, 129.25)
rest = a.rest[1:] if a.rest[:1] == ['--'] else a.rest
root = Path(rest[rest.index('--output-root') + 1]).resolve()
FFPROBE = '/opt/homebrew/bin/ffprobe'


def packet_mbps(mp4, recipe):
    num, den = (int(x) for x in recipe['cadence'].split('/'))
    pk = json.loads(subprocess.check_output([FFPROBE, '-v', 'error', '-select_streams', 'v:0', '-show_entries', 'packet=size', '-of', 'json', str(mp4)]))['packets']
    assert len(pk) == 48
    return sum(int(x['size']) for x in pk) * 8 / (48 * den / num) / 1e6


attempts, b = [], a.b0
for k in range(1, 5):
    pre = f'{a.prefix}-L{k}'
    r = subprocess.run([sys.executable, a.driver, *rest, '--prefix', pre, '--override', f'b={b}k'])
    rec = json.loads((root / 'corrected-encodes' / f'{pre}-recipe.json').read_text())
    if r.returncode or rec['status'] != 'encoded':
        attempts.append({'prefix': pre, 'b_kbps': b, 'status': rec['status']}); break
    enc = packet_mbps(root / 'corrected-encodes' / f'{pre}-hdr.mp4', rec); pred = enc + a.overhead
    attempts.append({'prefix': pre, 'b_kbps': b, 'encoded_packet_mbps': enc, 'predicted_authored_mbps': pred, 'in_band': LO <= pred <= HI})
    print(json.dumps(attempts[-1]), flush=True)
    if LO <= pred <= HI:
        break
    if len(attempts) == 1:
        nb = b * TARGET / pred
    else:
        p0, p1 = attempts[-2], attempts[-1]
        slope = (p1['predicted_authored_mbps'] - p0['predicted_authored_mbps']) / (p1['b_kbps'] - p0['b_kbps'])
        nb = b + (TARGET - pred) / slope if slope > 0 else b * TARGET / pred
    b = int(round(nb))
chosen = next((x['prefix'] for x in reversed(attempts) if x.get('in_band')), None)
out = {'rule': f'secant on b only, <=4 encodes, predicted authored average in [{LO},{HI}]', 'band': [LO, HI], 'target': TARGET,
       'overhead_mbps': a.overhead, 'attempts': attempts, 'chosen': chosen, 'landed': chosen is not None}
with (root / 'corrected-encodes' / f'{a.prefix}-landing.json').open('x') as f:
    f.write(json.dumps(out, indent=2) + '\n')
print(json.dumps({'prefix': a.prefix, 'chosen': chosen}))
raise SystemExit(0 if chosen else 1)
