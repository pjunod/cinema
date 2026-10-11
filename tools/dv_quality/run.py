#!/usr/bin/env python3
"""Bounded, offline RGB48LE comparator; never qualifies a DV playback route.

Usage: python3 tools/dv_quality/run.py compare --candidate C/manifest.json
       --baseline B/manifest.json [--reference R/manifest.json] --output NEWDIR

Manifests have schema=1, role=candidate|baseline|independent-reference,
source={id,sha256[,path]}, renderer={id,revision,artifact_sha256},
picture={width,height,pixel_format:rgb48le,range:full,primaries:bt2020,
transfer:smpte2084,domain:reconstruction|mapped},
target={id,peak_nits,black_nits,gamut:bt2020,mapping,parameters:{}}, and
frames=[{path,sha256,pts:'numerator/denominator',duration:'n/d'}]. All paths
are relative to their manifest, regular files and free of symlink components.
A reference also requires provenance={kind:independent,scope:analytic-control|
picture-reference,producer,method,evidence:{path,sha256}}. Independence is
an explicit declaration, not something this program can certify. Reference
error is absent in difference-only mode. Renderer assertions never prove FEL,
RPU, target rendering, authoring or production capability.

The output is created exclusively after all validation/measurement succeeds.
It contains the exact manifests, processed frame/evidence bytes, harness code,
metric vectors/licenses and a receipt with their verified SHA256 identities.
No decoding, chroma resampling, automatic alignment, exposure fitting, network
access, subprocess invocation, SSIM, rate comparison or quality badge occurs.
"""
from __future__ import annotations

import argparse
from array import array
from datetime import datetime, timezone
from fractions import Fraction
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import stat
import struct
import sys

if __package__:
    from . import metrics
else:
    import metrics

MAX_MANIFEST_BYTES = 1024 * 1024
MAX_EVIDENCE_BYTES = 1024 * 1024
MAX_SOURCE_BYTES = 8 * 1024 * 1024
MAX_FRAMES = 64
MAX_RASTER_SIDE = 256
MAX_PIXELS_PER_STREAM = 262144
MAX_RUN_BYTES = 32 * 1024 * 1024
CODE_FILES = ('run.py', 'metrics.py', 'metric_vectors.json', 'COLOUR-LICENSE', 'PU21-LICENSE')
SHA = re.compile(r'[0-9a-f]{64}\Z')
RATIONAL = re.compile(r'-?[0-9]{1,18}/[0-9]{1,18}\Z')


class Refusal(ValueError):
    """The supplied inputs cannot support an exact bounded comparison."""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def require(condition, message):
    if not condition:
        raise Refusal(message)


def text_field(value, name):
    require(isinstance(value, str) and 0 < len(value) <= 2048 and value.strip() == value,
            name + ' must be a nonempty bounded string')
    return value


def hash_field(value):
    require(isinstance(value, str) and SHA.fullmatch(value), 'SHA256 must be lowercase 64-digit hex')
    return value


def rational(value, name):
    require(isinstance(value, str) and RATIONAL.fullmatch(value), name + ' must be a bounded n/d rational')
    numerator, denominator = map(int, value.split('/'))
    require(denominator > 0, name + ' denominator must be positive')
    return Fraction(numerator, denominator)


def canonical_fraction(value):
    return f'{value.numerator}/{value.denominator}'


def check_components(path):
    path = Path(os.path.abspath(path))
    require(not any(p.is_symlink() for p in (path, *path.parents)), 'symlinked path components are refused')
    return path


def read_regular(path, limit):
    path = check_components(path)
    # O_NOFOLLOW and fstat avoid following a final symlink or opening a FIFO.
    # Trusted local scratch directories are required; this is not an OS sandbox.
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode), 'input must be a regular file')
    require(0 <= before.st_size <= limit, 'input exceeds bounded byte allowance')
    descriptor = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
    with os.fdopen(descriptor, 'rb') as source:
        opened = os.fstat(source.fileno())
        require(stat.S_ISREG(opened.st_mode) and opened.st_size <= limit,
                'input changed type or exceeded its byte allowance')
        require((before.st_dev, before.st_ino) == (opened.st_dev, opened.st_ino), 'input changed during open')
        data = source.read(limit + 1)
        after = os.fstat(source.fileno())
    require(len(data) <= limit and len(data) == opened.st_size, 'input changed length or exceeded allowance')
    require((opened.st_size, opened.st_mtime_ns, opened.st_ctime_ns) ==
            (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'input changed during read')
    return data


def relative_path(root, value):
    require(isinstance(value, str) and value and len(value) <= 2048 and '\\' not in value,
            'input path must be a bounded relative POSIX path')
    parts = value.split('/')
    require(not Path(value).is_absolute() and all(p not in ('', '.', '..') for p in parts),
            'input path escapes or is not canonical')
    return check_components(root.joinpath(*parts))


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate JSON keys are refused')
        result[key] = value
    return result


def invalid_constant(value):
    raise Refusal('nonfinite JSON values are refused: ' + value)


def finite_json(value):
    if isinstance(value, float):
        require(math.isfinite(value), 'nonfinite JSON numbers are refused')
    elif isinstance(value, dict):
        for child in value.values():
            finite_json(child)
    elif isinstance(value, list):
        for child in value:
            finite_json(child)


def load_manifest(path, role):
    path = check_components(path)
    raw = read_regular(path, MAX_MANIFEST_BYTES)
    try:
        document = json.loads(raw, object_pairs_hook=unique_object, parse_constant=invalid_constant)
        finite_json(document)
    except (json.JSONDecodeError, UnicodeDecodeError, RecursionError) as exc:
        raise Refusal('invalid or excessively nested manifest JSON') from exc
    require(isinstance(document, dict) and type(document.get('schema')) is int and document['schema'] == 1,
            'manifest schema must be 1')
    require(document.get('role') == role, 'manifest role mismatch')
    source = document.get('source')
    renderer = document.get('renderer')
    require(isinstance(source, dict) and isinstance(renderer, dict), 'source and renderer identities are required')
    text_field(source.get('id'), 'source id')
    hash_field(source.get('sha256'))
    for name in ('id', 'revision'):
        text_field(renderer.get(name), 'renderer ' + name)
    hash_field(renderer.get('artifact_sha256'))
    picture = document.get('picture')
    require(isinstance(picture, dict), 'picture contract is required')
    for name in ('width', 'height'):
        require(type(picture.get(name)) is int and 1 <= picture[name] <= MAX_RASTER_SIDE,
                'raster must be bounded 1..256')
    contract = {'pixel_format': 'rgb48le', 'range': 'full', 'primaries': 'bt2020', 'transfer': 'smpte2084'}
    require(all(picture.get(k) == v for k, v in contract.items()), 'only full-range RGB48LE BT2020 PQ is supported')
    require(picture.get('domain') in ('reconstruction', 'mapped'), 'explicit reconstruction or mapped domain is required')
    target = document.get('target')
    require(isinstance(target, dict), 'explicit target policy is required')
    for name in ('id', 'mapping'):
        text_field(target.get(name), 'target ' + name)
    require(target.get('gamut') == 'bt2020' and isinstance(target.get('parameters'), dict), 'target gamut/parameters missing')
    for name in ('peak_nits', 'black_nits'):
        require(type(target.get(name)) in (int, float) and 0 <= target[name] <= 10000,
                'target luminance must be finite and within PQ bounds')
    require(0 <= target['black_nits'] < target['peak_nits'] <= 10000, 'target luminance outside PQ bounds')
    entries = document.get('frames')
    require(isinstance(entries, list) and 1 <= len(entries) <= MAX_FRAMES, 'frame count must be within 1..64')
    pixels = picture['width'] * picture['height']
    require(pixels * len(entries) <= MAX_PIXELS_PER_STREAM, 'stream exceeds tiny-control pixel allowance')
    files = {}
    frames = []
    previous_end = None

    def artifact(record, limit):
        require(isinstance(record, dict), 'artifact identity is required')
        name = record.get('path')
        artifact_path = relative_path(path.parent, name)
        # Keep exact manifest bytes separately so even a frame named manifest.json
        # cannot overwrite the reproduction record.
        data = read_regular(artifact_path, limit)
        require(digest(data) == hash_field(record.get('sha256')), 'artifact SHA256 mismatch')
        if name in files:
            require(files[name] == data, 'artifact mutated between reads')
        files[name] = data
        return data

    for entry in entries:
        require(isinstance(entry, dict), 'frame record must be an object')
        pts = rational(entry.get('pts'), 'PTS')
        duration = rational(entry.get('duration'), 'duration')
        require(duration > 0, 'frame duration must be positive')
        require(previous_end is None or pts >= previous_end, 'duplicate, reordered or overlapping frame interval')
        previous_end = pts + duration
        data = artifact(entry, pixels * 6)
        require(len(data) == pixels * 6, 'raw frame size does not match RGB48LE raster')
        frames.append({'pts': canonical_fraction(pts), 'duration': canonical_fraction(duration), 'data': data})
    source_status = 'declared-unverified'
    if 'path' in source:
        artifact(source, MAX_SOURCE_BYTES)
        source_status = 'hash-verified-bytes; semantic identity declared'
    provenance = document.get('provenance')
    if role == 'independent-reference':
        require(isinstance(provenance, dict) and provenance.get('kind') == 'independent',
                'reference requires explicit independent provenance')
        require(provenance.get('scope') in ('analytic-control', 'picture-reference'),
                'reference provenance scope must be explicit')
        for name in ('producer', 'method'):
            text_field(provenance.get(name), 'reference ' + name)
        artifact(provenance.get('evidence'), MAX_EVIDENCE_BYTES)
    return {'manifest': document, 'manifest_bytes': raw, 'files': files, 'frames': frames,
            'source_verification': source_status}


def validate_pair(first, second):
    a, b = first['manifest'], second['manifest']
    require(a['picture'] == b['picture'], 'picture domain/raster/color contract mismatch')
    require(a['target'] == b['target'], 'target policy mismatch')
    require(a['source']['sha256'] == b['source']['sha256'] and a['source']['id'] == b['source']['id'],
            'comparison source binding mismatch')
    require(len(first['frames']) == len(second['frames']), 'missing frame: frame counts differ')
    for a_frame, b_frame in zip(first['frames'], second['frames']):
        require((a_frame['pts'], a_frame['duration']) == (b_frame['pts'], b_frame['duration']),
                'exact rational frame association mismatch')


def percentile(values, fraction):
    """Nearest-rank empirical quantile; no interpolation, explicitly recorded."""
    return values[max(0, math.ceil(fraction * len(values)) - 1)]


def summarize(errors, squared_sum, count):
    ordered = sorted(errors)
    mse = squared_sum / count
    return {'pixels': count,
            'delta_e_itp': {'mean': math.fsum(errors) / count, 'p95': percentile(ordered, .95),
                            'p99': percentile(ordered, .99), 'max': ordered[-1], 'units': 'Delta E ITP',
                            'direction': 'lower'},
            'pu21_luma': {'mse': mse, 'psnr_db': None if mse == 0 else metrics.pu21_psnr_from_mse(mse),
                          'identical': mse == 0, 'psnr_direction': 'higher', 'zero_mse_psnr': '+infinity'}}


def compare_frames(first, second):
    errors = array('d')
    squared = []
    per_frame = []
    worst = None
    for index, (a, b) in enumerate(zip(first['frames'], second['frames'])):
        frame_errors = array('d')
        frame_squared = []
        luma_a, luma_b = [], []
        at_peak_a = at_peak_b = floor_a = floor_b = 0
        peak = first['manifest']['target']['peak_nits']
        black = first['manifest']['target']['black_nits']
        for ar, br in zip(struct.iter_unpack('<HHH', a['data']), struct.iter_unpack('<HHH', b['data'])):
            value = metrics.compare_rgb48_pixel(ar, br)
            frame_errors.append(value['delta_e_itp'])
            frame_squared.append(value['pu21_luminance_squared_error'])
            ya, yb = value['reference_luminance_nits'], value['candidate_luminance_nits']
            luma_a.append(ya)
            luma_b.append(yb)
            # These are observed extrema/threshold fractions, not a claim that
            # clipping happened; legitimate pictures can reach target peak.
            at_peak_a += ya >= peak
            at_peak_b += yb >= peak
            floor_a += ya <= black
            floor_b += yb <= black
        frame = summarize(frame_errors, math.fsum(frame_squared), len(frame_errors))
        frame.update(index=index, pts=a['pts'], duration=a['duration'])
        frame['luminance_nits'] = {'first': {'min': min(luma_a), 'max': max(luma_a)},
                                   'second': {'min': min(luma_b), 'max': max(luma_b)}}
        frame['threshold_fractions'] = {
            'first_at_or_above_target_peak': at_peak_a / len(frame_errors),
            'second_at_or_above_target_peak': at_peak_b / len(frame_errors),
            'first_at_or_below_target_black': floor_a / len(frame_errors),
            'second_at_or_below_target_black': floor_b / len(frame_errors)}
        if worst is None or frame['delta_e_itp']['mean'] > worst['delta_e_itp']['mean']:
            worst = frame
        per_frame.append(frame)
        errors.extend(frame_errors)
        squared.append(math.fsum(frame_squared))
    result = summarize(errors, math.fsum(squared), len(errors))
    result.update(frames=per_frame, worst_mean_frame={'index': worst['index'], 'pts': worst['pts']},
                  weighting='pixels; frames equally weighted at equal raster, not duration',
                  quantile='nearest rank across pixels', first=first['manifest']['role'], second=second['manifest']['role'])
    return result


def run(candidate, baseline, output, reference=None):
    output = check_components(output)
    require(not output.exists(), 'output exists; overwrite refused')
    require(output.parent.is_dir(), 'output parent must already exist')
    streams = {'candidate': load_manifest(candidate, 'candidate'), 'baseline': load_manifest(baseline, 'baseline')}
    if reference is not None:
        streams['reference'] = load_manifest(reference, 'independent-reference')
    total = sum(len(s['manifest_bytes']) + sum(map(len, s['files'].values())) for s in streams.values())
    require(total <= MAX_RUN_BYTES, 'run exceeds total bounded input bytes')
    validate_pair(streams['candidate'], streams['baseline'])
    if 'reference' in streams:
        validate_pair(streams['candidate'], streams['reference'])
    difference = compare_frames(streams['baseline'], streams['candidate'])
    reference_error = None
    if 'reference' in streams:
        reference_error = {role: compare_frames(streams['reference'], streams[role]) for role in ('candidate', 'baseline')}
    code_root = Path(__file__).resolve().parent
    code = {name: read_regular(code_root / name, MAX_MANIFEST_BYTES) for name in CODE_FILES}
    identities = {}
    for role, stream in streams.items():
        identities[role] = {
            'manifest_sha256': digest(stream['manifest_bytes']),
            'source': {'declared': stream['manifest']['source'], 'verification': stream['source_verification']},
            'renderer': {'declared': stream['manifest']['renderer'], 'verification': 'declared-unverified'},
            'artifacts': {name: {'sha256': digest(data), 'bytes': len(data)} for name, data in stream['files'].items()}}
    frames = streams['candidate']['frames']
    gaps = []
    for before, after in zip(frames, frames[1:]):
        end = Fraction(before['pts']) + Fraction(before['duration'])
        start = Fraction(after['pts'])
        if start > end:
            gaps.append({'start': canonical_fraction(end), 'end': canonical_fraction(start)})
    receipt = {
        'schema': 1, 'kind': 'bounded-offline-DV-quality-association-control',
        'created_utc': datetime.now(timezone.utc).isoformat(),
        'runtime': {'python_version': sys.version, 'implementation': sys.implementation.name,
                    'system': platform.system(), 'machine': platform.machine(), 'byteorder': sys.byteorder},
        'run_identity': digest(json.dumps({r: s['manifest_sha256'] for r, s in identities.items()}, sort_keys=True).encode()
                               + b''.join(code.values())),
        'mode': 'declared-independent-reference' if reference_error is not None else 'difference-only',
        'capability_qualification': None,
        'association': {'supplied_frames': 'exact rational PTS/duration agreement',
                        'source_timeline_completeness': 'unverified',
                        'common_gaps': gaps,
                        'coverage': 'Only supplied intervals; shared omissions and source boundaries are not verified.'},
        'limitations': ['Manifest semantics/renderer/source claims are not independently certified.',
                        'No DV/FEL processing, metadata authoring, production admission or fidelity badge is established.',
                        'Tiny controls only; no held-out corpus, duration weighting, viewing, SSIM or rate qualification.'],
        'identities': identities,
        'reference_provenance': None if reference_error is None else {
            'declared': streams['reference']['manifest']['provenance'],
            'verification': 'evidence bytes hash verified; independence and semantic validity declared'},
        'metric_contract': {
            'colour_pin': '248121e33fcae458e62190ac872f3e16ab2044bf',
            'pu21_pin': '78340c0c4c20908c6bdcf0931dfde763c53a24ad',
            'pq_scale_nits': 10000, 'rgb_luminance_weights': list(metrics.BT2020_LUMINANCE),
            'pu21_variant': 'banding_glare', 'pu21_clamp_nits': [.005, 10000], 'pu21_psnr_peak': 256,
            'exposure_or_camera_fitting': False, 'ictcp_matrices': 'BT.2100 PQ / 4096',
            'chroma_upsampling': 'none; already interleaved RGB', 'ct_half_scaling': 'exactly once'},
        'difference': difference, 'reference_error': reference_error,
        'tool_files': {name: digest(data) for name, data in code.items()},
        'reproduce': ['python3', 'tool/run.py', 'compare', '--candidate', 'inputs/candidate/manifest.json',
                      '--baseline', 'inputs/baseline/manifest.json']
                    + (['--reference', 'inputs/reference/manifest.json'] if reference_error is not None else [])
                    + ['--output', '../new-comparison']}
    # Exclusive mkdir fences overwrite races. Leave any interrupted run visible;
    # never erase data or retry into an existing receipt directory.
    try:
        output.mkdir(mode=0o700)
    except FileExistsError as exc:
        raise Refusal('output exists; overwrite refused') from exc
    for role, stream in streams.items():
        destination = output / 'inputs' / role
        destination.mkdir(parents=True)
        (destination / 'manifest.json').write_bytes(stream['manifest_bytes'])
        # Mirror paths beneath a separate root; reproduction manifests retain
        # exact bytes and resolve against artifact_root via staging below.
        for name, data in stream['files'].items():
            dest = destination / 'artifacts' / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
        # A normalized manifest redirects only file paths to their snapshots.
        staged = json.loads(stream['manifest_bytes'])
        for entry in staged['frames']:
            entry['path'] = 'artifacts/' + entry['path']
        if 'path' in staged['source']:
            staged['source']['path'] = 'artifacts/' + staged['source']['path']
        if role == 'reference':
            staged['provenance']['evidence']['path'] = 'artifacts/' + staged['provenance']['evidence']['path']
        (destination / 'reproduce.json').write_text(json.dumps(staged, indent=2, allow_nan=False) + '\n')
    receipt['reproduce'] = [arg.replace('/manifest.json', '/reproduce.json') for arg in receipt['reproduce']]
    (output / 'tool').mkdir()
    for name, data in code.items():
        (output / 'tool' / name).write_bytes(data)
    (output / 'receipt.json').write_text(json.dumps(receipt, sort_keys=True, indent=2, allow_nan=False) + '\n')
    return receipt


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    compare = commands.add_parser('compare', help='exact, bounded offline frame association and metrics')
    compare.add_argument('--candidate', required=True, type=Path)
    compare.add_argument('--baseline', required=True, type=Path)
    compare.add_argument('--reference', type=Path)
    compare.add_argument('--output', required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        result = run(args.candidate, args.baseline, args.output, args.reference)
    except (Refusal, OSError) as exc:
        print('refused: ' + str(exc), file=sys.stderr)
        return 2
    print(json.dumps({'mode': result['mode'], 'receipt': str(args.output / 'receipt.json'),
                      'frames': len(result['difference']['frames']), 'capability_qualification': None}))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
