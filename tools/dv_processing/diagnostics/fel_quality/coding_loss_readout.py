"""Portable readout for the coding-loss arms (protocol-aq-psy / search-effort / chroma-asym / plane-export /
roi-map-refine, 2026-11-12) from heldout_eval.py outputs: full-resolution frames 0/24 and the all-48 lattice.

For each fixture and arm: landing (chosen attempt and authored rate), strict selected-frame bar rows, the
pooled all-48 full mean per model, frames beating F per model, and the protocols' common accept rule:
  strict bar rows all pass (screen included), pooled full mean <= its landed control (Z6; base for Qbase) - 0.02 in all four models on
  every fixture, and shadow-p99 frames-beating-F >= the control in every model.
Arms scored in several runs must agree exactly (same decoded candidate, same frozen scorer).
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
        for f in sorted(Path(sdir).glob(f'{kind}-*.json')):
            d = json.loads(f.read_text()); R = d['results']; fr = list(R)
            sources.append({'file': f.name, 'sha256': hashlib.sha256(f.read_bytes()).hexdigest()})
            for arm in R[fr[0]]['A']:
                a = arms.setdefault(arm, {})
                if kind == 'fullres':
                    v = {'frames': {k: {m: {'full_mean': R[k][m][arm]['full']['mean'],
                                            'dark_below_0.1_mean': R[k][m][arm]['bands']['dark_below_0.1'].get('mean'),
                                            'dark_below_0.1_p99': R[k][m][arm]['bands']['dark_below_0.1'].get('p99')} for m in 'ABCD'} for k in fr}}
                    if arm != 'F':
                        rows = d['selected_frame_bar'][arm]['rows']
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
for arm in sorted(set().union(*[v['arms'] for v in rec['fixtures'].values()]) - {'F', 'Z6', 'base'}):
    per = {}
    for fx, v in rec['fixtures'].items():
        a = v['arms'].get(arm)
        if not a or 'lattice' not in a or 'fullres' not in a:
            per[fx] = 'not scored'; continue
        c = v['arms'][CONTROL.get(arm, 'Z6')]
        bar = a['fullres']['bar_rows_passing']; dm = {m: a['lattice']['pooled_full_mean'][m] - c['lattice']['pooled_full_mean'][m] for m in 'ABCD'}
        wins = {m: a['lattice']['frames_beating_F'][m]['dark_p99_below_F'] - c['lattice']['frames_beating_F'][m]['dark_p99_below_F'] for m in 'ABCD'}
        per[fx] = {'bar_all_pass': bar[0] == bar[1], 'pooled_mean_minus_control': dm, 'shadow_wins_minus_control': wins,
                   'pass': bar[0] == bar[1] and all(x <= -0.02 for x in dm.values()) and all(x >= 0 for x in wins.values())}
    verdict[arm] = {'control': CONTROL.get(arm, 'Z6'), 'per_fixture': per, 'accept': all(isinstance(p, dict) and p['pass'] for p in per.values())}
rec['accept_rule'] = 'strict bar rows all pass; pooled full mean <= landed control (Z6; base for Qbase) - 0.02 in all four models on every fixture; shadow-p99 frames beating F >= the control in every model'
rec['verdicts'] = verdict
out.write_text(json.dumps(rec, indent=1) + '\n')
for arm, v in verdict.items():
    print(arm, 'ACCEPT' if v['accept'] else 'reject', {fx: (p if isinstance(p, str) else {'dB': round(p['pooled_mean_minus_control']['B'], 4), 'dA': round(p['pooled_mean_minus_control']['A'], 4), 'wB': p['shadow_wins_minus_control']['B']}) for fx, p in v['per_fixture'].items()})
