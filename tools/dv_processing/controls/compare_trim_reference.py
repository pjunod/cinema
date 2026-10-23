"""Source-bound matched-device observations for the UNACCEPTED RGB SOP candidate.

This instrument never emits a pass or authorizes production trims. It compares
operator-declared, matched photometric patch observations to candidate output.
It cannot authenticate the operator, instrument, calibration or frame matching.
See DISPLAY-TARGETED-DV-TRIM-CONTRACT.md for those independent obligations.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import struct
import subprocess

from annex_b2 import eotf


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def bound_file(item):
    path = Path(item['path']).resolve(strict=True)
    if not path.is_file() or digest(path) != item['sha256']:
        raise ValueError('file identity does not match packet')
    return path


def finite(value):
    if type(value) not in (int, float) or not math.isfinite(value):
        raise ValueError('finite numeric observation required')
    return value


def patch_luminance(data, width, height, patch):
    x, y, w, h = (patch[key] for key in ('x', 'y', 'width', 'height'))
    if any(type(v) is not int for v in (x, y, w, h)) or not (
            0 <= x < width and 0 <= y < height and w > 0 and h > 0 and
            x + w <= width and y + h <= height):
        raise ValueError('patch must name a nonempty exact native-raster rectangle')
    values = []
    for row in range(y, y + h):
        for col in range(x, x + w):
            rgb = struct.unpack_from('<3f', data, 16 * (row * width + col))
            if any(not math.isfinite(c) or not 0 <= c <= 1 for c in rgb):
                raise ValueError('invalid candidate normalized PQ output')
            # BT.2020 D65 linear-light luminance, in cd/m². The instrument
            # reference must measure the SAME registered spatial patch.
            values.append(sum(weight * eotf(c) for weight, c in
                              zip((0.2627, 0.6780, 0.0593), rgb, strict=True)))
    return math.fsum(values) / len(values)


def observe(packet_path, candidate, output):
    packet_path = packet_path.resolve(strict=True)
    packet = json.loads(packet_path.read_text())
    if packet['schema'] != 'e-trim-reference-v1':
        raise ValueError('unknown packet schema')
    provenance = packet['provenance']
    if provenance['kind'] != 'real-dv-device-photometry' or provenance['synthetic'] is not False:
        raise ValueError('this adapter requires declared real-device photometry')
    for field in ('operator', 'device', 'dv_route', 'capture_utc', 'instrument',
                  'spatial_registration', 'display_mode'):
        if not isinstance(provenance[field], str) or not provenance[field].strip():
            raise ValueError(f'missing capture provenance: {field}')
    bindings = {name: str(bound_file(packet[name])) for name in
                ('source_media', 'reconstruction_receipt', 'capture_artifact',
                 'instrument_calibration', 'source_archive', 'dependency_manifest')}
    width, height, target = (packet[k] for k in ('width', 'height', 'target_nits'))
    if any(type(v) is not int for v in (width, height, target)) or not (
            16 <= width <= 4096 and 16 <= height <= 2160 and
            target in (400, 600, 1000, 1600, 2000, 4000)):
        raise ValueError('unsupported exact raster/target')
    if finite(provenance['measured_display_peak_nits']) <= 0:
        raise ValueError('measured display peak required')
    frames = packet['frames']
    if not 1 <= len(frames) <= 16:
        raise ValueError('1..16 independently matched frames required')
    candidate = candidate.resolve(strict=True)
    argv = [str(candidate), f'bt2020-pq-target-{target}',
            str(packet['title_min_1e4']), str(packet['title_peak']), str(width),
            str(height), 'public-us20210076042-sop-v1', str(output / 'native')]
    seen, frame_bindings = set(), []
    for frame in frames:
        frame_id = frame['frame_id']
        pts = frame['pts']
        if (not isinstance(frame_id, str) or not frame_id or frame_id in seen or
                not isinstance(pts, list) or len(pts) != 2 or
                any(type(n) is not int for n in pts) or pts[0] < 0 or pts[1] <= 0 or
                not frame['patches']):
            raise ValueError('unique matched frame, rational PTS and patches required')
        seen.add(frame_id)
        rpu, rgb = bound_file(frame['rpu']), bound_file(frame['master_rgba32f'])
        if rgb.stat().st_size != width * height * 16:
            raise ValueError('master raster length mismatch')
        argv.extend((str(rpu), str(rgb)))
        frame_bindings.append({'frame_id': frame_id, 'pts': pts,
                               'rpu_sha256': digest(rpu), 'master_sha256': digest(rgb)})
    output.mkdir()  # Fresh attempt; never replace an earlier observation.
    result = subprocess.run(argv, capture_output=True, timeout=600, check=False)
    (output / 'stdout.jsonl').write_bytes(result.stdout)
    (output / 'stderr.txt').write_bytes(result.stderr)
    if result.returncode:
        raise RuntimeError(f'candidate exited {result.returncode}; retained raw logs')
    events = [json.loads(line) for line in result.stdout.splitlines()]
    rows = [e for e in events if e['kind'] == 'trim_candidate_frame']
    if [r['frame'] for r in rows] != list(range(len(frames))):
        raise ValueError('candidate frame coverage mismatch')
    observations = []
    for i, (frame, event) in enumerate(zip(frames, rows, strict=True)):
        if event['acceptance'] != 'unaccepted' or event['creative_trims_applied'] is not False:
            raise ValueError('candidate misrepresents acceptance')
        rendered = output / 'native' / f'frame-{i:03d}.rgba32f'
        data = rendered.read_bytes()
        if len(data) != width * height * 16:
            raise ValueError('candidate output length mismatch')
        for patch in frame['patches']:
            reference = finite(patch['reference_y_nits'])
            uncertainty = finite(patch['instrument_uncertainty_nits'])
            if reference < 0 or uncertainty < 0:
                raise ValueError('invalid photometric observation')
            measured = patch_luminance(data, width, height, patch)
            observations.append({'frame_id': frame['frame_id'], 'patch_id': patch['id'],
                'candidate_y_nits': measured, 'reference_y_nits': reference,
                'candidate_minus_reference_nits': measured - reference,
                'instrument_uncertainty_nits': uncertainty,
                'candidate_output_sha256': digest(rendered),
                'candidate_identity_sha256': event['identity_sha256']})
    report = {'schema': 'e-trim-observations-v1',
        'status': 'observations-only-not-acceptance', 'creative_trims_applied': False,
        'limitations': ['B.3 RGB candidate includes L2 chroma/saturation; L8 chroma/saturation and L3/detail/extended trims unapplied',
                        'public level-specific SOP bridge; separate B.3 RGB candidate is not Dolby rendering conformance',
                        'capture provenance and frame registration are declared, not authenticated',
                        'luminance patches do not establish color/chroma fidelity'],
        'declared_capture_provenance': provenance,
        'packet_sha256': digest(packet_path), 'candidate_sha256': digest(candidate),
        'adapter_sha256': digest(__file__), 'transfer_reference_sha256': digest(Path(__file__).with_name('annex_b2.py')),
        'bound_files': {name: {'path': path, 'sha256': digest(path)} for name, path in bindings.items()},
        'argv': argv, 'exit': result.returncode, 'frames': frame_bindings,
        'frame_count': len(frames), 'patch_count': len(observations),
        'observations': observations}
    (output / 'observations.json').write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--packet', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    report = observe(args.packet, args.candidate, args.output)
    print(f"{report['status']}: {report['frame_count']} frames, {report['patch_count']} patches")
