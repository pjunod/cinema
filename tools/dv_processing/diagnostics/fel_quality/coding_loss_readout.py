"""Portable readout for the coding-loss arms (protocol-aq-psy / search-effort / chroma-asym / plane-export /
roi-map-refine, 2026-11-12) from heldout_eval.py outputs: full-resolution frames 0/24 and the all-48 lattice.

For each fixture and arm: landing (chosen attempt and authored rate), strict selected-frame bar rows, the
pooled all-48 full mean per model, frames beating F per model, and the protocols' common accept rule:
  strict bar rows all pass (screen included), pooled full mean <= its landed control (Z6; base for Qbase) - 0.02 in all four models on
  every fixture, and shadow-p99 frames-beating-F >= the control in every model.
Arms scored in several runs must agree exactly (same decoded candidate, same frozen scorer).
A fixture whose name does not start with "michael" is a held-out scene and gets the stack's held-out rule instead.
Usage: coding_loss_readout.py OUT.json FIXTURE=SCOREDIR=LANDINGDIR [...]
"""
from pathlib import Path
import hashlib, json, sys

out, specs = Path(sys.argv[1]), sys.argv[2:]
rec = {'scope': 'offline coding-loss readout; ΔITP four frozen models; no perceptual or conformance claim',
       'script_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), 'fixtures': {}}
for spec in specs:
    fx, sdir, ldir = spec.split('=')
    arms, sources = {}, []
    for kind in ('fullres', 'lattice'):
        # unbound-* files score candidates that were never authored (B-frame probe, the unencoded export).
        for f in sorted(Path(sdir).glob(f'{kind}-*.json')) + sorted(Path(sdir).glob(f'unbound-{kind}-*.json')):
            d = json.loads(f.read_text()); R = d['results']; fr = list(R)
            sources.append({'file': f.name, 'sha256': hashlib.sha256(f.read_bytes()).hexdigest()})
            for arm in R[fr[0]]['A']:
                a = arms.setdefault(arm, {})
                if kind == 'fullres':
                    v = {'frames': {k: {m: {'full_mean': R[k][m][arm]['full']['mean'],
                                            'dark_below_0.1_mean': R[k][m][arm]['bands']['dark_below_0.1'].get('mean'),
                                            'dark_below_0.1_p99': R[k][m][arm]['bands']['dark_below_0.1'].get('p99')} for m in 'ABCD'} for k in fr}}
                    rows = (d.get('selected_frame_bar') or {}).get(arm, {}).get('rows')
                    if arm != 'F' and rows:
                        v['bar_rows_passing'] = [sum(all(r['bar'].values()) for r in rows), len(rows)]
                else:
                    v = {'pooled_full_mean': {m: sum(R[k][m][arm]['full']['mean'] for k in fr) / len(fr) for m in 'ABCD'}, 'frames': len(fr)}
                    if arm != 'F':
                        w = d['per_frame_wins_vs_F']
                        v['frames_beating_F'] = {m: {x: w[f'{m}:{arm}'][x] for x in ('dark_p99_below_F', 'all_three')} for m in 'ABCD'}
                if kind in a:
                    assert a[kind] == v, (fx, arm, kind, 'differs between scoring runs')
                a[kind] = v
    for arm, a in arms.items():
        lj = Path(ldir) / f'{fx}-{arm}-landing.json'
        if lj.exists():
            l = json.loads(lj.read_text()); ch = [t for t in l['attempts'] if t['prefix'] == l['chosen']]
            a['landing'] = {'chosen': l['chosen'], 'attempts': len(l['attempts']), 'predicted_authored_mbps': ch[0]['predicted_authored_mbps'] if ch else None}
    rec['fixtures'][fx] = {'arms': arms, 'sources': sources}

# Each arm is judged against its protocol's control: Qbase (plane-domain export, no map) against base, the rest against Z6.
CONTROL = {'Qbase': 'base'}
verdict = {}
HELDOUT = {fx for fx in rec['fixtures'] if not fx.startswith('michael')}
for arm in sorted(set().union(*[v['arms'] for v in rec['fixtures'].values()]) - {'F', 'Z6', 'base'}):
    per = {}
    for fx, v in rec['fixtures'].items():
        if fx in HELDOUT:
            continue
        a = v['arms'].get(arm)
        if not a or 'lattice' not in a or 'fullres' not in a:
            per[fx] = 'not scored'; continue
        c = v['arms'][CONTROL.get(arm, 'Z6')]
        bar = a['fullres'].get('bar_rows_passing', [0, 1]); dm = {m: a['lattice']['pooled_full_mean'][m] - c['lattice']['pooled_full_mean'][m] for m in 'ABCD'}
        wins = {m: a['lattice']['frames_beating_F'][m]['dark_p99_below_F'] - c['lattice']['frames_beating_F'][m]['dark_p99_below_F'] for m in 'ABCD'}
        per[fx] = {'bar_all_pass': bar[0] == bar[1], 'pooled_mean_minus_control': dm, 'shadow_wins_minus_control': wins,
                   'pass': bar[0] == bar[1] and all(x <= -0.02 for x in dm.values()) and all(x >= 0 for x in wins.values())}
    if all(isinstance(p, str) for p in per.values()):
        continue  # scored on held-out scenes only
    verdict[arm] = {'control': CONTROL.get(arm, 'Z6'), 'per_fixture': per, 'accept': all(isinstance(p, dict) and p['pass'] for p in per.values())}
# Held-out scene (protocol-stack-20261112.json): pooled mean below F in all four models, frames 0 and 24 below F,
# shadow-p99 frames beating F at least the landed Z6's.
for fx in HELDOUT:
    v = rec['fixtures'][fx]['arms']; F, Z = v['F'], v['Z6']; hv = {}
    for arm, a in v.items():
        if arm == 'F' or 'lattice' not in a or 'fullres' not in a:
            continue
        pm = {m: a['lattice']['pooled_full_mean'][m] - F['lattice']['pooled_full_mean'][m] for m in 'ABCD'}
        fm = {k: {m: a['fullres']['frames'][k][m]['full_mean'] - F['fullres']['frames'][k][m]['full_mean'] for m in 'ABCD'} for k in a['fullres']['frames']}
        w = {m: a['lattice']['frames_beating_F'][m]['dark_p99_below_F'] - Z['lattice']['frames_beating_F'][m]['dark_p99_below_F'] for m in 'ABCD'}
        hv[arm] = {'pooled_mean_minus_F': pm, 'frame_mean_minus_F': fm, 'shadow_wins_minus_Z6': w,
                   'pass': all(x < 0 for x in pm.values()) and all(x < 0 for f in fm.values() for x in f.values()) and all(x >= 0 for x in w.values())}
    rec.setdefault('heldout_verdicts', {})[fx] = hv
    for arm, h in hv.items():
        print(fx, arm, 'PASS' if h['pass'] else 'fail', {'dF_B': round(h['pooled_mean_minus_F']['B'], 4), 'wins_vs_Z6_B': h['shadow_wins_minus_Z6']['B']})
rec['accept_rule'] = 'strict bar rows all pass; pooled full mean <= landed control (Z6; base for Qbase) - 0.02 in all four models on every fixture; shadow-p99 frames beating F >= the control in every model'
rec['verdicts'] = verdict
out.write_text(json.dumps(rec, indent=1) + '\n')
for arm, v in verdict.items():
    print(arm, 'ACCEPT' if v['accept'] else 'reject', {fx: (p if isinstance(p, str) else {'dB': round(p['pooled_mean_minus_control']['B'], 4), 'dA': round(p['pooled_mean_minus_control']['A'], 4), 'wB': p['shadow_wins_minus_control']['B']}) for fx, p in v['per_fixture'].items()})
