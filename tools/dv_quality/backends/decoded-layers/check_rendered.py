"""Scalar matrix/PQ control for the actually decoded synthetic source domain."""
import hashlib, json, math, struct, sys
from pathlib import Path
import check_association
def read_json(path):
    def refuse(value):
        raise ValueError('nonfinite JSON constant')
    return json.loads(path.read_text(), parse_constant=refuse)


root = Path(sys.argv[1])
dm = read_json(root / 'rpu-tags/p7-frame0.json')['vdr_dm_data']
M = [[dm[f'ycc_to_rgb_coef{3 * i + j}'] / 8192 for j in range(3)] for i in range(3)]
L = [[dm[f'rgb_to_lms_coef{3 * i + j}'] / 16384 for j in range(3)] for i in range(3)]
H = [[3.06441879, -2.16597676, 0.10155818], [-0.65612108, 1.78554118, -0.12943749], [0.01736321, -0.04725154, 1.03004253]]
A = [[sum((H[i][k] * L[k][j] for k in range(3))) for j in range(3)] for i in range(3)]
O = [dm[f'ycc_to_rgb_offset{i}'] / 2 ** 28 * 1024 / 1023 for i in range(3)]

def eotf(x):
    p = max(x, 0) ** (32 / 2523)
    return (max(p - 3424 / 4096, 0) / (2413 / 128 - 2392 / 128 * p)) ** (16384 / 2610)

def oetf(x):
    p = max(x, 0) ** (2610 / 16384)
    return ((3424 / 4096 + 2413 / 128 * p) / (1 + 2392 / 128 * p)) ** (2523 / 32)

def render_provenance(root, bframes, frame, pair):
    folder = root / f'b{bframes}-normal/render/frame-{frame}'
    association = folder / 'association.json'
    if json.dumps(read_json(association), sort_keys=True, allow_nan=False) != json.dumps(pair, sort_keys=True, allow_nan=False):
        raise ValueError('render association differs from current accepted decoder inputs')
    probe = folder / 'probe.jsonl'
    events = check_association.events(probe)
    expected_parser = {
        'kind': 'parsed_rpu', 'bytes': (root / pair['rpu_path']).stat().st_size,
        'profile': 7, 'el_type': 'FEL', 'parser_error': False,
        'bl_depth': 10, 'el_depth': 10, 'vdr_depth': 12, 'mapping_segments': 1,
        'nlq_method': 'LINEAR_DZ', 'creative_l2_count': 0, 'creative_l8_count': 0,
        'creative_trims_applied': False,
    }
    expected_environment = {
        'kind': 'environment', 'api': 374, 'output_format': 'rgba32f',
        'format_pixel_size': 16,
        'input_domain': 'actual paired decoded native Main10 components',
        'rpu_input': 'libdovi parsed UNSPEC62 NAL; guarded synthetic subset',
    }
    expected_events = [expected_parser, expected_environment]
    for case in ('zero', 'nonzero', 'omitted', 'shifted', 'disabled'):
        expected_events.append({
            'kind': 'frame', 'case': case, 'pts': pair['pts'], 'duration': pair['duration'],
            'el_bound': case != 'omitted', 'nlq_active': case != 'disabled',
            'rpu_parsed': True, 'direct_dispatch_ok': True, 'render_ok': True,
            'render_errors': 0, 'qualified_fel': False,
        })
    if json.dumps(events, sort_keys=True, allow_nan=False) != json.dumps(expected_events, sort_keys=True, allow_nan=False):
        raise ValueError('actual renderer event PTS/duration/flags differ from accepted input')
    return {key: pair[key] for key in ('bl_sha256', 'el_sha256', 'rpu_sha256', 'pts', 'duration')} | {
        'association_sha256': check_association.sha(association),
        'probe_sha256': check_association.sha(probe),
    }


accepted = {b: check_association.inspect(root, b, 'normal') for b in (0, 2)}
rows = []
for bframes in (0, 2):
    for frame in range(6):
        pair = accepted[bframes]['pairs'][frame]
        if pair['source_frame'] != frame:
            raise ValueError('accepted display source order differs')
        provenance = render_provenance(root, bframes, frame, pair)
        context = json.dumps(accepted[bframes], sort_keys=True, allow_nan=False).encode()
        provenance['accepted_context_sha256'] = hashlib.sha256(context).hexdigest()
        for case in ('zero', 'nonzero', 'omitted', 'shifted', 'disabled'):
            expected = []
            for y in range(64):
                for x in range(64):
                    base = (400 + 4 * frame + 2 * (x // 4) + y // 4) / 1023
                    residual = 0
                    if case in ('nonzero', 'shifted'):
                        xx = (x + 1) % 64 if case == 'shifted' else x
                        residual = (8 + frame) * (1 if (xx // 4 + y // 4 + frame) % 2 else -1) / 1024
                    v = [base + residual, 512 / 1023, 512 / 1023]
                    rgb = [sum((M[i][j] * (v[j] - O[j]) for j in range(3))) for i in range(3)]
                    linear = list(map(eotf, rgb))
                    expected.extend((oetf(sum((A[i][j] * linear[j] for j in range(3)))) for i in range(3)))
            if not all(math.isfinite(value) for value in expected):
                raise ValueError('nonfinite scalar reference')
            for stage in ('reconstruction', 'rendered'):
                folder = root / f'b{bframes}-normal/render/frame-{frame}/outputs'
                raw = (folder / f'{case}-{stage}.rgba32f').read_bytes()
                if len(raw) != 65536:
                    raise ValueError('truncated float output')
                floats = struct.unpack('<16384f', raw)
                if not all((math.isfinite(x) for x in floats)):
                    raise ValueError('nonfinite output')
                actual = [floats[4 * i + c] for i in range(4096) for c in range(3)]
                error = max((abs(a - b) for a, b in zip(actual, expected)))
                raw_rgb = (folder / f'{case}-{stage}.rgb48le').read_bytes()
                if len(raw_rgb) != 24576:
                    raise ValueError('truncated RGB48')
                rgb = struct.unpack('<12288H', raw_rgb)
                code_error = max((abs(a - round(min(1, max(0, b)) * 65535)) for a, b in zip(rgb, expected)))
                if error > 2e-05 or code_error > 2:
                    raise ValueError(f'arithmetic control failed{(bframes, frame, case, stage, error, code_error)}')
                rows.append({'bframes': bframes, 'source_frame': frame, 'case': case, 'stage': stage, 'max_pq_error': error, 'max_code_error': code_error, 'output_sha256': hashlib.sha256(raw_rgb).hexdigest(), **provenance})
print(json.dumps({'scope': 'decoded synthetic scalar arithmetic; not Dolby oracle', 'mapping_target': '10000/.005nits BT2020 clip', 'results': rows, 'full_fel_qualified': False}, indent=2, allow_nan=False))
