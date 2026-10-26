"""Post-review real helper controls: complete title scan, per-frame target,
raw-L1 response, exact overlap hashes, and actual Rust-policy master elision.
Phase 2 (closed-form display management): each frame trace names the
display-management family and the raw L1 it used; pixel accuracy of the
equations is checked by test_dv_dm and run_dm_parity.py, not here.

Requires explicitly compiled source-bound binaries (native Linux, no emulation).
No root/device actions; the operator supplies a task-owned output directory and
Vulkan/dependency environment. All commands and return codes are retained.
"""
import argparse
from array import array
from fractions import Fraction
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys



def oetf(nits):
    m1, m2, c1, c2, c3 = 2610 / 16384, 2523 / 32, 3424 / 4096, 2413 / 128, 2392 / 128
    p = (nits / 10000) ** m1
    return ((c1 + c2 * p) / (1 + c3 * p)) ** m2


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(args):
    root = args.output.resolve()
    root.mkdir()
    commands = []

    def command(argv, name, allow_failure=False, env=None):
        argv = list(map(str, argv))
        completed = subprocess.run(argv, capture_output=True, timeout=180, env=env)
        (root / f'{name}.stdout').write_bytes(completed.stdout)
        (root / f'{name}.stderr').write_bytes(completed.stderr)
        commands.append({'argv': argv, 'exit': completed.returncode, 'name': name})
        if not allow_failure and completed.returncode:
            raise RuntimeError(f'{name}: exit {completed.returncode}')
        return completed

    command([args.fixtures, root / 'tags'], 'generate')
    # Generate genuinely encoded 18-frame inputs; no vendor or real-title data.
    # Same immutable elementary pixels for every metadata case.
    plane = array('H', [64 + round(x * 876 / 63) for y in range(64) for x in range(64)]
                  + [512] * 2048)
    if sys.byteorder != 'little':
        plane.byteswap()
    (root / 'ramp.yuv').write_bytes(plane.tobytes() * 18)
    for layer in ('bl', 'el'):
        command([args.ffmpeg, '-nostdin', '-v', 'error', '-threads', '1', '-f', 'rawvideo',
                 '-pixel_format', 'yuv420p10le', '-video_size', '64x64', '-framerate', '24',
                 '-i', root / 'ramp.yuv', '-frames:v', '18', '-c:v', 'libx265',
                 '-x265-params', 'pools=none:frame-threads=1:bframes=0:keyint=18:log-level=error',
                 root / f'{layer}.mkv'], f'encode-{layer}')

    def policy(scan, nits, name):
        result = command([args.policy, scan, nits], name)
        return json.loads(result.stdout)

    def render(source, profile, choice, start, end, name, rgb=False):
        output = root / name
        output.mkdir()
        argv = [args.helper, source, output, '0', '64', '64', '64',
                '64' if profile == 7 else '0', '64' if profile == 7 else '0', '0',
                choice['output_policy'], output / 'out.nut', start, end, '512',
                'fel' if profile == 7 else 'base-rpu', '64', '64',
                'pq-bilinear-rgb48-v1' if rgb else 'pq-bilinear-yuv420p10-v1']
        if choice.get('endpoints') is not None:
            argv.extend(choice['endpoints'])
        result = command(argv, name, env=os.environ | {'PLURX_DV_FRAME_HASHES': '1'})
        events = [json.loads(line) for line in result.stdout.splitlines()]
        completed = [e for e in events if e['kind'] == 'segment_complete']
        assert len(completed) == 1 and completed[0]['output_policy'] == choice['output_policy']
        rows = [e for e in events if e['kind'] == 'rendered_frame']
        assert rows and len(rows) == completed[0]['frames']
        raw = command([args.ffmpeg, '-nostdin', '-v', 'error', '-threads', '1', '-i',
                       output / 'out.nut', '-map', '0:v:0', '-fps_mode', 'passthrough',
                       '-pix_fmt', 'rgb48le' if rgb else 'yuv420p10le', '-f', 'rawvideo', 'pipe:1'],
                      f'{name}-decode').stdout
        size = 24576 if rgb else 12288
        assert len(raw) == len(rows) * size
        frames = {}
        for i, row in enumerate(rows):
            payload = raw[i * size:(i + 1) * size]
            assert sha_bytes(payload) == row['rgb_sha256' if rgb else 'yuv10_sha256']
            pts = Fraction(row['pts'])
            assert pts not in frames
            frames[pts] = payload
        return frames, events

    master = {'output_policy': 'bt2020-pq-master-clip', 'endpoints': None}
    results = []
    for profile in (7, 8):
        sources, scans = {}, {}
        for family in ('high', 'alternate', 'low', 'missing', 'missing-rpu', 'conflict', 'corrupt'):
            case = f'p{profile}-{family}'
            source = root / f'{case}.mkv'
            argv = [args.mux, root / 'bl.mkv', root / 'el.mkv', root / 'tags' / case,
                    source, 'missing-rpu' if family == 'missing-rpu' else 'normal']
            if profile == 8:
                argv.append('base8')
            command(argv, f'{case}-mux')
            sources[family] = source
            scan_result = command([args.scanner, source, '0'], f'{case}-scan', allow_failure=family == 'corrupt')
            if scan_result.returncode:
                assert family == 'corrupt'
                results.append({'case': case, 'scanner_refused': True})
                continue
            scan = json.loads(scan_result.stdout)
            (root / f'{case}-scan.json').write_text(json.dumps(scan))
            scans[family] = root / f'{case}-scan.json'
            assert scan['complete'] and scan['decoded_frames'] == 18
            assert sum(scan[k] for k in ('valid_l1_frames', 'missing_l1_frames', 'invalid_l1_frames')) == 18
            selected = policy(scans[family], 1000, f'{case}-policy')
            if family in ('missing', 'missing-rpu', 'corrupt'):
                assert scan['valid_l1_frames'] < 18, case
                assert selected['output_policy'] == master['output_policy'] and selected['decline']
            else:
                assert scan['valid_l1_frames'] == 18
                assert scan['max_l1_pq'] == (1000 if family == 'low' else 4095)
                assert selected['output_policy'] == (master['output_policy'] if family == 'low'
                                                     else 'bt2020-pq-target-1000')
                if family == 'conflict':
                    assert scan['mastering_luminance']['state'] == 'inconsistent'
                    assert selected['endpoints'] == ['50', '4000']
            results.append({'case': case, 'scan': scan, 'policy': selected, 'source_sha256': sha(source)})

        # Truncated titles must not become complete just because the demuxer
        # uses EOF for a shortened container. Preserve its original declaration.
        damaged = root / f'p{profile}-truncated.mkv'
        source_bytes = sources['high'].read_bytes()
        damaged.write_bytes(source_bytes[:len(source_bytes) // 2])
        truncated = command([args.scanner, damaged, '0'], f'p{profile}-truncated-scan', allow_failure=True)
        assert truncated.returncode != 0 and not truncated.stdout.strip()
        results.append({'profile': profile, 'truncation_exit': truncated.returncode})
        master_rgb = {}
        for family in ('high', 'alternate'):
            master_rgb[family], _ = render(sources[family], profile, master, '0/1', '1/4',
                                           f'p{profile}-{family}-master-rgb', rgb=True)
        assert master_rgb['high'] == master_rgb['alternate'], 'L1 change altered master reconstruction'
        for nits in (1000, 1600):
            targeted = {}
            for family in ('high', 'alternate'):
                choice = policy(scans[family], nits, f'p{profile}-{family}-{nits}-policy')
                frames, events = render(sources[family], profile, choice, '0/1', '1/4',
                                        f'p{profile}-{family}-{nits}')
                targeted[family] = frames
                l1 = json.loads((root / 'tags' / f'p{profile}-{family}' / 'l1.json').read_text())
                trace = {e['frame']: e for e in events if e['kind'] == 'display_target_frame'}
                assert len(trace) == len(frames)
                for index, (pts, raw) in enumerate(frames.items()):
                    source_index = round(pts * 24)
                    metadata = l1[source_index]
                    assert (trace[index]['max_pq'], trace[index]['avg_pq']) == (metadata['max_pq'], metadata['avg_pq'])
                    assert trace[index]['method'] == 'dolby-closed-form-cm4-cm29-v5'
                    # served output applies each family's creative trims (DV_TARGET_TRIMS_CM4/_CM29)
                    curve = trace[index]['curve']
                    assert curve in ('cm4-l1-only', 'cm4-l8-interpolated', 'cm29-l1-only',
                                     'cm29-l2-exact', 'cm29-l2-interpolated'), trace[index]
                    if curve.startswith('cm4-'):
                        assert trace[index]['detail_weight'] == 0, trace[index]
                        assert (trace[index]['trim_targets'] == 0) == (curve == 'cm4-l1-only'), trace[index]
                    actual = words(raw)
                    assert max(actual[:4096]) <= 64 + 876 * oetf(nits) + 1
                    results.append({'profile': profile, 'family': family, 'nits': nits,
                                    'pts': str(pts), 'curve': trace[index]['curve'], 'samples': len(actual)})
                if family == 'high':
                    overlap, _ = render(sources[family], profile, choice, '1/8', '3/8',
                                         f'p{profile}-overlap-{nits}')
                    shared = frames.keys() & overlap.keys()
                    assert len(shared) >= 3
                    assert all(frames[t] == overlap[t] for t in shared), 'window dependence'
                    results.append({'profile': profile, 'nits': nits,
                                    'overlap_yuv10_sha256': {str(t): sha_bytes(frames[t]) for t in sorted(shared)}})
            assert targeted['high'] != targeted['alternate'], 'ignored per-frame L1'
        elided = policy(scans['low'], 1000, f'p{profile}-elision-policy')
        assert elided['output_policy'] == master['output_policy'] and elided['endpoints'] is None
        a, _ = render(sources['low'], profile, elided, '0/1', '1/4', f'p{profile}-elided')
        b, _ = render(sources['low'], profile, master, '0/1', '1/4', f'p{profile}-master')
        assert a == b and a, 'false master-byte elision'
        results.append({'profile': profile, 'elided_yuv10_sha256': {str(t): sha_bytes(a[t]) for t in a}})
    assert results and commands
    receipt = {'binaries': {k: sha(getattr(args, k)) for k in ('helper', 'scanner', 'policy', 'fixtures', 'mux', 'ffmpeg')},
               'controller_sha256': sha(__file__), 'commands': commands, 'results': results,
               'scope': 'synthetic full-helper/scan controls; no real-title or physical-display claim'}
    (root / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


def words(data):
    result = array('H', data)
    if sys.byteorder != 'little':
        result.byteswap()
    return result


def sha_bytes(data):
    return hashlib.sha256(data).hexdigest()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('helper', 'scanner', 'policy', 'fixtures', 'mux', 'ffmpeg', 'output'):
        parser.add_argument('--' + name, required=True, type=Path)
    args = parser.parse_args()
    for name in ('helper', 'scanner', 'policy', 'fixtures', 'mux', 'ffmpeg'):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    receipt = run(args)
    print(json.dumps({'commands': len(receipt['commands']), 'results': len(receipt['results'])}))
