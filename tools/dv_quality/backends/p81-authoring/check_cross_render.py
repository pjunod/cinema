"""Bind GPU output evidence before separately evaluating numeric diagnostics."""
from fractions import Fraction
from pathlib import Path
import hashlib
import json
import struct

ROOT = Path(__file__).resolve().parent
FRAME_BYTES = 64 * 64 * 6
YUV_FRAME_BYTES = 64 * 64 * 3
STAGES = ('reconstruction', 'rendered')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def codes(data):
    if len(data) != FRAME_BYTES:
        raise ValueError('cross-render RGB48LE length changed')
    return struct.unpack('<12288H', data)


def verify(root=ROOT):
    upstream = json.loads((root / 'cross-render/upstream-results.json').read_text())
    definition = json.loads((root / 'source-definition.json').read_text())
    reference = (root / 'analytic-hdr10-reference.rgb48le').read_bytes()
    decoded = (root / 'decoded.yuv420p10le').read_bytes()
    if len(reference) != 4 * FRAME_BYTES or len(decoded) != 4 * YUV_FRAME_BYTES:
        raise ValueError('reference/decoded sequence length changed')
    expected_keys = {(frame, stage) for frame in range(4) for stage in STAGES}
    stage_rows = {}
    for row in upstream['results']:
        key = (row['frame_index'], row['stage'])
        if type(row['frame_index']) is not int or key not in expected_keys:
            raise ValueError('unexpected GPU frame/stage receipt')
        if key in stage_rows:
            raise ValueError('duplicate GPU frame/stage receipt')
        stage_rows[key] = row
    if set(stage_rows) != expected_keys:
        raise ValueError('missing GPU frame/stage receipt')

    results = []
    for frame in range(4):
        directory = root / 'cross-render' / f'frame-{frame}'
        expected_input = decoded[frame * YUV_FRAME_BYTES:(frame + 1) * YUV_FRAME_BYTES]
        association = definition['frames'][frame]
        for label in ('hdr10', 'dv-on'):
            if (directory / f'{label}-decoded-input.yuv420p10le').read_bytes() != decoded:
                raise ValueError('cross-render decoded-input association changed')
            if (directory / f'{label}-reconstruction.rgb48le').read_bytes() != (directory / f'{label}-rendered.rgb48le').read_bytes():
                raise ValueError('premap/fullrender differs in declared no-map target')
        output_hashes = {}
        for stage in STAGES:
            row = stage_rows[(frame, stage)]
            expected_tag = dict(association['synthetic_l1'])
            actual_tag = {key: row['parsed_l1'][key] for key in expected_tag}
            if row['decoded_frame_sha256'] != digest(expected_input) or row['rpu_sha256'] != association['rpu_sha256'] or actual_tag != expected_tag:
                raise ValueError('actual GPU input/RPU/parsed-tag association changed')
            if Fraction(row['pts']) != Fraction(frame, 24):
                raise ValueError('external GPU frame index time changed')
            baseline = (directory / f'hdr10-{stage}.rgb48le').read_bytes()
            candidate = (directory / f'dv-on-{stage}.rgb48le').read_bytes()
            # Receipt binding precedes tolerance checks. A close substitute is
            # still not the recorded output and must be rejected.
            if digest(candidate) != row['candidate_sha256'] or digest(baseline) != row['baseline_sha256']:
                raise ValueError('actual GPU output hash differs from frame/stage receipt')
            delta = max(abs(a - b) for a, b in zip(codes(baseline), codes(candidate)))
            if delta != row['max_code_delta']:
                raise ValueError('GPU receipt numeric delta differs from retained output')
            output_hashes[stage] = {'candidate_sha256': digest(candidate), 'baseline_sha256': digest(baseline)}
        baseline = (directory / 'hdr10-rendered.rgb48le').read_bytes()
        candidate = (directory / 'dv-on-rendered.rgb48le').read_bytes()
        analytic = reference[frame * FRAME_BYTES:(frame + 1) * FRAME_BYTES]
        same_delta = max(abs(a - b) for a, b in zip(codes(baseline), codes(candidate)))
        scalar_hdr10 = max(abs(a - b) for a, b in zip(codes(baseline), codes(analytic)))
        scalar_dv = max(abs(a - b) for a, b in zip(codes(candidate), codes(analytic)))
        # Observed-run replay caps only; no preregistered picture fidelity threshold.
        if same_delta > 25 or scalar_hdr10 > 1 or scalar_dv > 25:
            raise ValueError('cross-render diagnostic replay cap changed')
        results.append({
            'frame_index': frame,
            'decoded_input_file_sha256': digest(decoded),
            'selected_decoded_frame_sha256': digest(expected_input),
            'actual_rpu_sha256': association['rpu_sha256'],
            'actual_parsed_l1': association['synthetic_l1'],
            'frame_selection_method': 'GPU helper fseek(frame_index*64*64*3), explicit index',
            'bound_output_hashes_by_stage': output_hashes,
            'same_renderer_dv_on_vs_hdr10_max_code_delta': same_delta,
            'gpu_hdr10_vs_independent_inverse_ncl_max_code_delta': scalar_hdr10,
            'gpu_dv_on_vs_independent_inverse_ncl_max_code_delta': scalar_dv,
            'dv_on_sha256': digest(candidate), 'hdr10_sha256': digest(baseline),
        })
    return {
        'scope': 'analytic controls; separate implementations, no independent DV picture reference',
        'same_renderer': 'Both DV-on/off outputs use same pinned libplacebo backend',
        'threshold_selection': '25 RGBcode DV diagnostic replay cap selected after observing24; not qualification threshold',
        'scalar_reference_scope': 'Independent analytic BT2020 inverseNCL arithmetic only',
        'double_mapping_limit': 'Original P7 reshape is identity; cannot detect double application of arbitrary nonidentity reshaping',
        'unmet_next_operation': 'Nonidentity-reshape P7 reconstructed-base authoring control',
        'creative_trims_applied': False, 'results': results,
        'p81_conformance': None, 'independent_dv_picture_reference': None,
        'reference_fidelity_improvement': None, 'result': 'diagnostic-control-pass',
    }


if __name__ == '__main__':
    receipt = verify()
    (ROOT / 'cross-render-check-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))
