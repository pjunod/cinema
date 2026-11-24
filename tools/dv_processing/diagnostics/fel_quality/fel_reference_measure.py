"""Milestone 5.2: today's FEL composition measured against the reference.

Inputs are one layer capture (capture_layers.c: bl.yuv, el.yuv, events.jsonl
and the parsed metadata.jsonl) and the helper's own pq-bilinear-rgb48-v1
render of the same window (segment_decode_render ... fel ... rgb48 NUT),
with the measured frames extracted as frame-NNN.rgb48le.

Three pictures per frame, all carried to PQ BT.2020 R'G'B' by one display
path (bilinear 4:4:4 sampling at the stream's siting, then the RPU's
ycc_to_rgb matrix and offset):

- helper: the shipped renderer's actual output.
- today:  a CPU model of that renderer's composition (every plane sampled
          bilinearly at full resolution, the EL code rounded, LINEAR_DZ in
          real arithmetic, no clamp). It exists only to put today's
          composition; it confirms the renderer's sampling phases and the
          display path (the "validation" block). Today's YCbCr codes are
          taken from the helper's output through the inverse display matrix.
- reference: fel_reference_compose (ETSI GS CCM 001 integer composer,
          fel_reference_composer.h) at 4:2:0, then the same display path.

Reported, by the reference's luminance band (BT.2020 weighted linear Y, the
ledger's bands): 12-bit code error of today against the reference per
component and Delta E ITP (BT.2124), both of the helper's output against the
reference. The go/stop rule is the plan's section 5.2.

Usage:
  python3 fel_reference_measure.py --capture DIR --helper-frames DIR \
      --helper-report stdout.jsonl --oracle fel_reference_compose \
      --work DIR --frames 0 24 --label michael5400 --out result.json
"""
from pathlib import Path
import argparse, hashlib, json, subprocess, sys
sys.dont_write_bytecode = True
import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import vector_metrics as vm  # noqa: E402  (BT.2124 on 16-bit PQ R'G'B')

BANDS = [(0, 0.1), (0.1, 1), (1, 10), (10, 100), (100, None)]
Y_WEIGHTS = np.array((.2627, .6780, .0593))
THRESHOLDS = {'mean_delta_e_itp': 0.01, 'max_delta_e_itp': 0.1, 'band_rms_code': 0.5}


def sha(path):
    with Path(path).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def planes(path, frame, w, h):
    count = w * h * 3 // 2
    data = np.fromfile(path, dtype='<u2', count=count, offset=frame * count * 2)
    assert data.size == count, f'{path}: short frame {frame}'
    y = data[:w * h].reshape(h, w)
    u = data[w * h:w * h * 5 // 4].reshape(h // 2, w // 2)
    v = data[w * h * 5 // 4:].reshape(h // 2, w // 2)
    return y, u, v


def sample(plane, ys, xs):
    """Bilinear sampling, clamp-to-edge, at fractional texel indices; the
    rows depend only on ys and the columns only on xs, so it is separable."""
    p = plane.astype(np.float64)
    def axis(n, c):
        c = np.clip(c, 0, n - 1)
        i0 = np.floor(c).astype(np.int64)
        i1 = np.minimum(i0 + 1, n - 1)
        return i0, i1, c - i0
    x0, x1, fx = axis(p.shape[1], xs)
    rows = p[:, x0] * (1 - fx) + p[:, x1] * fx
    y0, y1, fy = axis(p.shape[0], ys)
    return rows[y0] * (1 - fy)[:, None] + rows[y1] * fy[:, None]


# Texel index sampled for output pixel n of a BL-sized raster, per axis. The
# capture's BL signals top-left chroma. The renderer's EL phases were fitted
# against its own output (the validation block confirms them on every
# measured frame): for an EL at scale s of the BL (0.5 or 1), the EL frame is
# co-sited with the BL horizontally and centre-aligned vertically, with its
# chroma co-sited inside the EL frame (at s = 1 that is the BL's own
# sampling). At s = 0.5 that is also the phase of Annex B's EL filters, so
# today and the reference sample the EL at the same luma positions and
# differ in filter taps.
def bl_chroma_coord(n):
    return n / 2.0


def el_luma_x(n, s):
    return n * s


def el_luma_y(n, s):
    return (n + 0.5) * s - 0.5


def el_chroma_x(n, s):
    return el_luma_x(n, s) / 2.0


def el_chroma_y(n, s):
    return el_luma_y(n, s) / 2.0


def params_text(meta):
    hd, mp = meta['header'], meta['mapping']
    assert mp['num_x_partitions'] == 1 and mp['num_y_partitions'] == 1 and mp['nlq_method_idc'] == 0
    out = ['frc1', hd['coef_log2_denom'], hd['bl_bit_depth'], hd['el_bit_depth'],
           hd['vdr_bit_depth'], hd['disable_residual_flag']]
    for curve in mp['curves']:
        out += [len(curve['pivots']), *curve['pivots']]
        for piece in curve['pieces']:
            # RPU field -> CCM 001 parameter (reading R9): mapping_idc,
            # poly_order = poly_order_minus1 + 1, fp poly coefficients.
            c = list(piece['poly_coef']) + [0] * (3 - len(piece['poly_coef']))
            if piece['mapping_idc'] == 0:
                out += [0, piece['poly_order'], *c[:3], 1, 0] + [0] * 21
            else:
                # mmr_order = mmr_order_minus1 + 1; fp coefficients [order][7].
                assert 'mmr_coef' in piece, 'MMR pieces need their coefficients in the capture'
                out += [1, 1, 0, 0, 0, piece['mmr_order'], piece['mmr_constant'], *piece['mmr_coef']]
        out += [curve['nlq_offset'], curve['slope'], curve['threshold'], curve['vdr_in_max']]
    return ' '.join(map(str, out)) + '\n'


def today_composition(meta, bl, el, w, h, ew):
    """CPU model of the shipped renderer: normalised float YCbCr at w x h."""
    hd, curves = meta['header'], meta['mapping']['curves']
    scale = ew / w
    denom = float(1 << hd['coef_log2_denom'])
    cols, rows = np.arange(w), np.arange(h)
    out = []
    for c in range(3):
        curve = curves[c]
        assert len(curve['pieces']) == 1 and curve['pieces'][0]['mapping_idc'] == 0
        if c == 0:
            b = bl[0].astype(np.float64)
            e = sample(el[0], el_luma_y(rows, scale), el_luma_x(cols, scale))
        else:
            b = sample(bl[c], bl_chroma_coord(rows), bl_chroma_coord(cols))
            e = sample(el[c], el_chroma_y(rows, scale), el_chroma_x(cols, scale))
        s = b * 64 / 65535  # UNORM16 texture holding code << 6
        coef = [x / denom for x in curve['pieces'][0]['poly_coef']]
        mapped = sum(k * s ** i for i, k in enumerate(coef))
        code = np.rint(e)  # the renderer rounds the resampled EL code
        d = code - curve['nlq_offset']
        S, T = curve['slope'] / denom, curve['threshold'] / denom
        residual = np.where(d == 0, 0.0, np.sign(d) * ((np.abs(d) - 0.5) * S + T))
        out.append(mapped + residual)
    return out


def reference_planes(ref, w, h, scale=16):
    """Reference 4:2:0 codes -> normalised float at w x h, chroma sampled
    with the same bilinear display path the renderer uses for BL chroma."""
    y, u, v = ref
    rows, cols = np.arange(h), np.arange(w)
    to_unit = lambda p: p.astype(np.float64) * scale / 65535  # 12-bit code << 4 as UNORM16
    return [to_unit(y),
            sample(to_unit(u), bl_chroma_coord(rows), bl_chroma_coord(cols)),
            sample(to_unit(v), bl_chroma_coord(rows), bl_chroma_coord(cols))]


def display(ycc, meta):
    """RPU ycc_to_rgb with offset -> PQ R'G'B' in 16-bit code units (float)."""
    col = meta['color']
    m = np.array([a / b for a, b in col['ycc_to_rgb_matrix']]).reshape(3, 3)
    off = np.array([a / b for a, b in col['ycc_to_rgb_offset']])
    x = np.stack(ycc, axis=-1) - off
    return np.clip(x @ m.T, 0, 1) * 65535


def helper_frame(directory, index, w, h):
    """One frame of the helper's rgb48 render, extracted as frame-NNN.rgb48le."""
    path = directory / f'frame-{index:03d}.rgb48le'
    data = np.fromfile(path, dtype='<u2')
    assert data.size == w * h * 3, f'{path}: {data.size} samples'
    return data.reshape(h, w, 3).astype(np.float64)


def band_masks(rgb_reference):
    lum = 10000 * vm_eotf(rgb_reference / 65535) @ Y_WEIGHTS
    return {f'[{lo},{hi if hi is not None else "inf"})': (lum >= lo) & (lum < hi if hi is not None else True)
            for lo, hi in BANDS}


def vm_eotf(x):
    p = x ** (1 / vm.M2)
    return (np.maximum(p - vm.C1, 0) / (vm.C2 - vm.C3 * p)) ** (1 / vm.M1)


def code_stats(err):
    a = np.abs(err).ravel()
    s = vm.stats(a)
    if s:
        s['rms'] = float(np.sqrt(np.mean(err.ravel() ** 2)))
        s['signed_mean'] = float(err.mean())
    return s


def measure_frame(args, meta, pair, w, h, ew, eh):
    frame = pair['frame']
    params = args.work / f'params-{frame:03d}.txt'
    params.write_text(params_text(meta))
    raw = args.work / f'reference-{frame:03d}.raw'
    unrounded_raw = args.work / f'reference-unrounded-{frame:03d}.raw'
    subprocess.run([str(args.oracle), str(params), str(args.capture / 'bl.yuv'), str(args.capture / 'el.yuv'),
                    str(frame), str(w), str(h), str(ew), str(eh), str(raw), str(unrounded_raw)],
                   check=True, timeout=600)
    ref = planes(raw, 0, w, h)
    sums = np.fromfile(unrounded_raw, dtype='<i4')
    unrounded = (sums[:w * h].reshape(h, w), sums[w * h:w * h * 5 // 4].reshape(h // 2, w // 2),
                 sums[w * h * 5 // 4:].reshape(h // 2, w // 2))
    raw.unlink()
    unrounded_raw.unlink()
    bl, el = planes(args.capture / 'bl.yuv', frame, w, h), planes(args.capture / 'el.yuv', frame, ew, eh)
    modelled = all(len(cv['pieces']) == 1 and cv['pieces'][0]['mapping_idc'] == 0
                   for cv in meta['mapping']['curves'])
    today = today_composition(meta, bl, el, w, h, ew) if modelled else None
    reference = reference_planes(ref, w, h)
    # The reference before clause 5.4.3.3 rounds to 12 bits: v + r in the
    # 16-bit mapped domain, which the renderer's UNORM16 convention reads as
    # (v + r) / 65535. Comparing the helper with it isolates the EL filters
    # (and composition grid) from the specification's output rounding.
    reference_unrounded = reference_planes(unrounded, w, h, scale=1)
    rgb_reference = display(reference, meta)
    helper = helper_frame(args.helper_frames, frame, w, h)
    masks = band_masks(rgb_reference)

    # The CPU model confirms the renderer's sampling phases and the display
    # path; its residual disagreement is one-code EL rounding ties (the GPU's
    # bilinear weights are not exact), reported as el_rounding_flips.
    # The model covers one polynomial piece per component; other mappings
    # rely on the phases and display path validated on titles it covers.
    validation = None
    if today is not None:
        rgb_today = display(today, meta)
        model_error = rgb_today - helper
        validation = {'rgb16_abs_error': code_stats(model_error),
                      'delta_e_itp': vm.stats(vm.delta(helper, rgb_today)),
                      'el_rounding_flips': float((np.abs(model_error).max(-1) > 10).mean())}

    # Today's YCbCr comes from the helper's own output by inverting the RPU
    # display matrix, exact wherever no channel of either picture clipped.
    col = meta['color']
    m = np.array([a / b for a, b in col['ycc_to_rgb_matrix']]).reshape(3, 3)
    off = np.array([a / b for a, b in col['ycc_to_rgb_offset']])
    helper_ycc = (helper / 65535) @ np.linalg.inv(m).T + off
    unclipped = (((helper > 0) & (helper < 65535)).all(-1)
                 & ((rgb_reference > 0) & (rgb_reference < 65535)).all(-1))

    row = {'frame': frame, 'pts': pair['pts'], 'rpu_sha256': meta['rpu_sha256'], 'validation': validation,
           'unclipped_fraction': float(unclipped.mean()), 'code_error_12bit': {}, 'delta_e_itp': {}}
    for c, name in enumerate(('Y', 'Cb', 'Cr')):
        err = (helper_ycc[..., c] - reference[c]) * 65535 / 16  # 12-bit code units
        row['code_error_12bit'][name] = {'all': code_stats(err[unclipped])} | {
            band: code_stats(err[mask & unclipped]) for band, mask in masks.items() if (mask & unclipped).any()}
    de = vm.delta(rgb_reference, helper)
    row['delta_e_itp'] = {'all': vm.stats(de)} | {
        band: vm.stats(de[mask]) for band, mask in masks.items() if mask.any()}
    # Decomposition: filters only (helper vs unrounded reference) and the
    # specification's 12-bit output rounding alone (reference vs unrounded).
    rgb_unrounded = display(reference_unrounded, meta)
    decomposition = {}
    for part, (a, b) in {'filters_only': (rgb_unrounded, helper),
                         'output_rounding_only': (rgb_reference, rgb_unrounded)}.items():
        d = vm.delta(a, b)
        entry = {'delta_e_itp': {'all': vm.stats(d)} | {
            band: vm.stats(d[mask]) for band, mask in masks.items() if mask.any()}, 'code_error_12bit': {}}
        for c, name in enumerate(('Y', 'Cb', 'Cr')):
            first = helper_ycc[..., c] if part == 'filters_only' else reference[c]
            err = (first - reference_unrounded[c]) * 65535 / 16
            entry['code_error_12bit'][name] = {'all': code_stats(err[unclipped])} | {
                band: code_stats(err[mask & unclipped]) for band, mask in masks.items() if (mask & unclipped).any()}
        decomposition[part] = entry
    row['decomposition'] = decomposition
    row['band_pixels'] = {band: int(mask.sum()) for band, mask in masks.items()}
    return row


def compose(args, capture, meta, frame, w, h, ew, eh, tag):
    """The reference's 12-bit 4:2:0 planes for one captured frame."""
    params = args.work / f'params-{tag}-{frame:03d}.txt'
    params.write_text(params_text(meta))
    raw = args.work / f'reference-{tag}-{frame:03d}.raw'
    subprocess.run([str(args.oracle), str(params), str(capture / 'bl.yuv'), str(capture / 'el.yuv'),
                    str(frame), str(w), str(h), str(ew), str(eh), str(raw)], check=True, timeout=600)
    ref = planes(raw, 0, w, h)
    raw.unlink()
    return ref


def measure_clamp(args, metas, pairs):
    """A clamp-binding capture against the same layers with vdr_in_max at its
    ceiling: what clause 5.4.3.2's clamp changes. Today's helper refuses the
    binding stream, so this is the difference admitting it unclamped would make."""
    events = [json.loads(line) for line in (args.unclamped_capture / 'events.jsonl').read_text().splitlines()]
    other_pairs = {e['frame']: e for e in events if e['kind'] == 'accepted_source_pair'}
    other = {m['frame']: m for m in map(json.loads, (args.unclamped_capture / 'metadata.jsonl').read_text().splitlines())}
    for name in ('bl.yuv', 'el.yuv'):
        assert sha(args.capture / name) == sha(args.unclamped_capture / name), f'{name} differs between the captures'
    rows = []
    for frame in args.frames:
        meta, pair = metas[frame], pairs[frame]
        assert other_pairs[frame]['pts'] == pair['pts']
        w, h, ew, eh = pair['bl_width'], pair['bl_height'], pair['el_width'], pair['el_height']
        clamped = compose(args, args.capture, meta, frame, w, h, ew, eh, 'clamped')
        free = compose(args, args.unclamped_capture, other[frame], frame, w, h, ew, eh, 'unclamped')
        row = {'frame': frame, 'pts': pair['pts'], 'rpu_sha256': meta['rpu_sha256'],
               'vdr_in_max': [cv['vdr_in_max'] for cv in meta['mapping']['curves']],
               'unclamped_vdr_in_max': [cv['vdr_in_max'] for cv in other[frame]['mapping']['curves']],
               'changed_fraction': {}, 'code_change_12bit': {}}
        for c, name in enumerate(('Y', 'Cb', 'Cr')):
            d = free[c].astype(np.float64) - clamped[c]
            row['changed_fraction'][name] = float((d != 0).mean())
            row['code_change_12bit'][name] = code_stats(d)
        a = display(reference_planes(clamped, w, h), meta)
        b = display(reference_planes(free, w, h), other[frame])
        row['delta_e_itp'] = vm.stats(vm.delta(a, b))
        rows.append(row)
        print(json.dumps({'frame': frame, 'changed_fraction': row['changed_fraction'],
                          'delta_e_itp': row['delta_e_itp']}), flush=True)
    return rows


def decide(rows):
    reasons = []
    for row in rows:
        f = row['frame']
        if row['delta_e_itp']['all']['mean'] > THRESHOLDS['mean_delta_e_itp']:
            reasons.append(f'frame {f}: mean Delta E ITP {row["delta_e_itp"]["all"]["mean"]:.4f} > 0.01')
        if row['delta_e_itp']['all']['max'] > THRESHOLDS['max_delta_e_itp']:
            reasons.append(f'frame {f}: max Delta E ITP {row["delta_e_itp"]["all"]["max"]:.4f} > 0.1')
        for comp, bands in row['code_error_12bit'].items():
            for band, s in bands.items():
                if band != 'all' and s and s['rms'] > THRESHOLDS['band_rms_code']:
                    reasons.append(f'frame {f}: {comp} {band} RMS {s["rms"]:.3f} code > 0.5')
    return reasons


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--capture', required=True, type=Path)
    ap.add_argument('--helper-frames', type=Path,
                    help='directory of frame-NNN.rgb48le extracted from the helper render')
    ap.add_argument('--helper-report', required=True, type=Path,
                    help="the helper's stdout (in clamp mode, its stderr refusal)")
    ap.add_argument('--oracle', required=True, type=Path)
    ap.add_argument('--work', required=True, type=Path)
    ap.add_argument('--frames', type=int, nargs='+', default=[0, 24])
    ap.add_argument('--label', required=True)
    ap.add_argument('--out', required=True, type=Path)
    ap.add_argument('--unclamped-capture', type=Path,
                    help='clamp mode: the same layers with vdr_in_max at its ceiling; '
                         'no helper frames are read, since today\'s helper refuses a binding clamp')
    args = ap.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)

    events = [json.loads(line) for line in (args.capture / 'events.jsonl').read_text().splitlines()]
    pairs = {e['frame']: e for e in events if e['kind'] == 'accepted_source_pair'}
    metas = {m['frame']: m for m in map(json.loads, (args.capture / 'metadata.jsonl').read_text().splitlines())}
    if args.unclamped_capture:
        result = {
            'schema': 'plurx-fel-reference-clamp-v1',
            'label': args.label,
            'capture': {name: sha(args.capture / name) for name in ('bl.yuv', 'el.yuv', 'metadata.jsonl', 'events.jsonl')},
            'unclamped_capture': {name: sha(args.unclamped_capture / name) for name in ('metadata.jsonl', 'events.jsonl')},
            'helper': args.helper_report.read_text().strip().splitlines()[-1] if args.helper_report.exists() else None,
            'oracle': 'tools/dv_processing/fel_reference_composer.h via diagnostics/fel_quality/fel_reference_compose.c',
            'frames': measure_clamp(args, metas, pairs),
        }
        args.out.write_text(json.dumps(result, indent=1) + '\n')
        return
    if not args.helper_frames:
        ap.error('--helper-frames is required unless --unclamped-capture is given')
    report = [json.loads(line) for line in args.helper_report.read_text().splitlines() if line.startswith('{')]
    helper_rpus = [r['rpu_sha256'] for r in report if r['kind'] == 'accepted_source_pair']
    rows = []
    for frame in args.frames:
        pair, meta = pairs[frame], metas[frame]
        # Bind the helper's picture to the captured layers by the RPU it rendered.
        assert helper_rpus[frame] == meta['rpu_sha256'] == pair['rpu_sha256'], f'frame {frame} is not the same picture'
        w, h, ew, eh = pair['bl_width'], pair['bl_height'], pair['el_width'], pair['el_height']
        rows.append(measure_frame(args, meta, pair, w, h, ew, eh))
        print(json.dumps({'frame': frame, 'validation': rows[-1]['validation'],
                          'delta_e_itp': rows[-1]['delta_e_itp']['all']}), flush=True)
    reasons = decide(rows)
    result = {
        'schema': 'plurx-fel-reference-measure-v1',
        'label': args.label,
        'capture': {name: sha(args.capture / name) for name in ('bl.yuv', 'el.yuv', 'metadata.jsonl', 'events.jsonl')},
        'helper_frames_sha256': {f'{n:03d}': sha(args.helper_frames / f'frame-{n:03d}.rgb48le') for n in args.frames},
        'oracle': 'tools/dv_processing/fel_reference_composer.h via diagnostics/fel_quality/fel_reference_compose.c',
        'thresholds': THRESHOLDS,
        'frames': rows,
        'above_threshold': reasons,
        'decision_input': 'go' if reasons else 'stop',
    }
    args.out.write_text(json.dumps(result, indent=1) + '\n')


if __name__ == '__main__':
    main()
