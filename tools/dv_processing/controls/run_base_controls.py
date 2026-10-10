"""Focused real held-window controls for the public base-only DOVI mapper.

Run after locked generator normal+base modes and fixture-muxer/helper builds.
Reuses controls/bl.mkv; profile5 alters VUI only, never encodes new pictures.
This test proves operations/timing/refusal wiring, not visual DV certification.
Previously retained binary64 controls exceeded the declared4RGB48-code bound:
P5=6, P8 affine/piecewise=27 (MMR1=3,MMR3=4). That failure remains a separate
numeric diagnostic; it is not converted into a passing regression threshold.
"""
from pathlib import Path
from fractions import Fraction
import hashlib
import json
import os
import subprocess
import shutil
import tempfile

FAMILIES = ('p5', 'p5-mmr1', 'p5-mmr3', 'p8-affine', 'p8-piecewise')
TIMES = ('42/1000', '83/1000', '125/1000')
COMPLETION = {'window_complete', 'segment_complete', 'base_processing_complete'}



def test_base_profile_nut_outputs(root=Path('/work')):
    """Validate actual decoded payload/timestamps, not only renderer stdout."""
    root = Path(root)
    pixels = {}
    for case in (*FAMILIES, 'p7-omit-fel', 'base-diagnostics'):
        directory = root / case / 'output'
        nut = directory / 'out.nut'
        probe = json.loads(subprocess.check_output(
            ['ffprobe', '-v', 'error', '-select_streams', 'v:0',
             '-show_streams', '-show_frames', '-of', 'json', str(nut)],
            timeout=30))
        assert len(probe['streams']) == 1
        stream = probe['streams'][0]
        assert stream['codec_name'] == 'rawvideo'
        assert stream['width'] == stream['height'] == 64
        assert stream['pix_fmt'] == 'rgb48le'
        frames = probe['frames']
        assert len(frames) == 3
        observed_pts = [int(frame['pts']) * Fraction(stream['time_base']) for frame in frames]
        assert observed_pts == [Fraction(time) for time in TIMES]
        assert all(frame['width'] == frame['height'] == 64 and
                   frame['pix_fmt'] == 'rgb48le' for frame in frames)
        raw = subprocess.check_output(
            ['ffmpeg', '-nostdin', '-v', 'error', '-threads', '1', '-i', str(nut),
             '-map', '0:v:0', '-fps_mode', 'passthrough', '-pix_fmt', 'rgb48le',
             '-f', 'rawvideo', 'pipe:1'], timeout=30)
        assert len(raw) == 3 * 64 * 64 * 6
        events = [json.loads(line) for line in (directory / 'stdout').read_text().splitlines()]
        rendered = [event for event in events if event['kind'] == 'rendered_frame']
        assert len(rendered) == 3
        for index, frame in enumerate(rendered):
            if 'rgb_sha256' in frame:
                chunk = raw[index * 24576:(index + 1) * 24576]
                assert hashlib.sha256(chunk).hexdigest() == frame['rgb_sha256']
        pixels[case] = raw
        (directory / 'nut-probe.json').write_text(json.dumps(probe, indent=2) + '\n')
    assert pixels['base-diagnostics'] == pixels['p8-affine']
    return {case: hashlib.sha256(raw).hexdigest() for case, raw in pixels.items()}



def test_base_profile_nut_refusals(root=Path('/work')):
    """Missing NUT, damaged header and altered pixel payload cannot pass."""
    root = Path(root)
    for mutation in ('missing', 'header', 'pixels'):
        with tempfile.TemporaryDirectory(prefix='nut-refusal-', dir=root) as temporary:
            copy = Path(temporary)
            for case in (*FAMILIES, 'p7-omit-fel', 'base-diagnostics'):
                directory = copy / case / 'output'
                directory.mkdir(parents=True)
                for name in ('stdout', 'out.nut'):
                    shutil.copyfile(root / case / 'output' / name, directory / name)
            if mutation == 'missing':
                (copy / 'p5/output/out.nut').unlink()
            elif mutation == 'header':
                (copy / 'p5/output/out.nut').write_bytes(b'corrupt-NUT-header')
            else:
                nut = copy / 'base-diagnostics/output/out.nut'
                raw = subprocess.check_output(
                    ['ffmpeg', '-nostdin', '-v', 'error', '-threads', '1', '-i', str(nut),
                     '-pix_fmt', 'rgb48le', '-f', 'rawvideo', 'pipe:1'], timeout=30)
                data = bytearray(nut.read_bytes())
                offset = data.find(raw[:64])
                assert offset >= 0, 'known raw pixel payload must be present in NUT'
                data[offset] ^= 1
                nut.write_bytes(data)
            try:
                test_base_profile_nut_outputs(copy)
            except AssertionError:
                pass
            except subprocess.CalledProcessError as failure:
                assert failure.returncode > 0, 'signal/crash is not controlled checker refusal'
            else:
                raise AssertionError(f'{mutation} NUT unexpectedly passed')
    return ('missing', 'header', 'pixels')


def test_base_profile_windows(root=Path('/work')):
    root = Path(root)
    arch = os.environ.get('DV_TEST_ARCH_LIB', 'aarch64-linux-gnu')
    icd = os.environ.get('DV_TEST_VULKAN_ICD',
                         '/usr/share/vulkan/icd.d/lvp_icd.aarch64.json')
    env = os.environ | {
        'LD_LIBRARY_PATH': str(root / f'prefix/lib/{arch}'),
        'VK_ICD_FILENAMES': icd,
        'XDG_RUNTIME_DIR': str(root / 'runtime'),
        'PLURX_DV_FRAME_HASHES': '0',
    }
    (root / 'runtime').mkdir(exist_ok=True)
    (root / 'runtime').chmod(0o700)
    results = []

    def mux(name, rpus, profile=None, mode='normal', bl=None):
        directory = root / name
        directory.mkdir(exist_ok=True)
        source = directory / 'source.mkv'
        assert not source.exists(), 'fresh control scratch required'
        args = [str(root / 'make_movie'), str(bl or root / 'bl.mkv'),
                str(root / 'el.mkv' if profile is None else bl or root / 'bl.mkv'),
                str(rpus), str(source), mode]
        if profile is not None:
            args.append(f'base{profile}')
        p = subprocess.run(args, capture_output=True, timeout=30)
        (directory / 'mux.stdout').write_bytes(p.stdout)
        (directory / 'mux.stderr').write_bytes(p.stderr)
        assert p.returncode == 0, (name, p.returncode, p.stderr)
        return source

    def render(name, source, reason=None, cap='64', el=('0', '0'),
               diagnostic=False, pre_gpu=False):
        directory = root / name / 'output'
        directory.mkdir(parents=True)
        with source.open('rb') as held, (directory / 'out.nut').open('xb') as nut:
            args = [str(root / 'bin/segment_decode_render'),
                    f'/proc/self/fd/{held.fileno()}', '.', '0', cap,
                    '64', '64', *el, '0', 'bt2020-pq-master-clip',
                    f'/proc/self/fd/{nut.fileno()}', '42/1000', '167/1000',
                    '512', 'base-rpu']
            p = subprocess.run(args, cwd=directory,
                               env=env | {'PLURX_DV_FRAME_HASHES': '1' if diagnostic else '0'},
                               pass_fds=(held.fileno(), nut.fileno()),
                               capture_output=True, timeout=60)
        (directory / 'stdout').write_bytes(p.stdout)
        (directory / 'stderr').write_bytes(p.stderr)
        (directory / 'exit.json').write_text(json.dumps({'exit': p.returncode}) + '\n')
        events = [json.loads(line) for line in p.stdout.splitlines()]
        if reason:
            assert p.returncode == 1 and p.stderr.decode().strip() == reason, (name, p.returncode, p.stderr)
            assert not any(e['kind'] in COMPLETION for e in events), name
            if pre_gpu:
                assert not any(e['kind'] in ('gpu_context_created', 'gpu_runtime', 'rendered_frame') for e in events)
            results.append({'case': name, 'controlled_exit': 1, 'reason': reason})
            return events
        assert p.returncode == 0, (name, p.returncode, p.stderr)
        rendered = [e for e in events if e['kind'] == 'rendered_frame']
        accepted = [e for e in events if e['kind'] == 'accepted_source_base']
        assert len(rendered) == len(accepted) == 3
        assert [e['pts'] for e in rendered] == list(TIMES)
        assert [e['pts'] for e in accepted] == list(TIMES)
        for index, (frame, source_frame) in enumerate(zip(rendered, accepted)):
            assert type(frame['frame']) is int and frame['frame'] == index
            assert frame['duration'] == source_frame['duration'] == '41/1000'
            assert frame['el_bound'] is False and frame['nlq_active'] is False
            assert frame['fel_contributed'] is False and frame['creative_trims_applied'] is False
            assert frame['production_qualified'] is False
            assert frame['render_errors'] == 0
            assert ('rgb_sha256' in frame) is diagnostic
            assert ('bl_sha256' in source_frame) is diagnostic
            assert 'el_sha256' not in source_frame
            expected = {'RepresentationNormalization', 'RpuColorConversion', 'TargetMapping'}
            if frame['polynomial_segments']:
                expected.add('PolynomialReshape')
            if frame['mmr_segments']:
                expected.add('MmrReshape')
            assert len(frame['applied_operations']) == len(expected)
            assert set(frame['applied_operations']) == expected
        for kind in ('gpu_runtime', 'gpu_context_created', 'window_complete',
                     'segment_complete', 'base_processing_complete'):
            assert sum(e['kind'] == kind for e in events) == 1
        window = next(e for e in events if e['kind'] == 'window_complete')
        assert window['frames'] == 3 and window['preroll_pairs'] == 1
        assert window['boundary_observed'] is True and window['boundary_pts_ticks'] == 167
        # EOF can also be observed while flushing a reordered decoder.
        assert window['requested_start'] == '42/1000' and window['requested_end'] == '167/1000'
        completion = next(e for e in events if e['kind'] == 'base_processing_complete')
        assert completion['frames'] == 3 and completion['decoded_layers'] == 1
        assert completion['fel_contributed'] is False
        assert (directory / 'timing.tsv').read_text().splitlines() == [f'{pts}\t41/1000' for pts in TIMES]
        results.append({'case': name, 'exit': 0, 'frames': 3, 'decoded_layers': 1,
                        'fel_contributed': False, 'diagnostic_hashes': diagnostic})
        return events

    # Reuse encoded sample NALs; only VUI/range metadata changes for the P5 fixture.
    p5_bl = root / 'bl-p5.mkv'
    subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-n', '-i', str(root / 'bl.mkv'),
                    '-map', '0:v:0', '-c:v', 'copy', '-bsf:v',
                    'hevc_metadata=video_full_range_flag=1:matrix_coefficients=2',
                    str(p5_bl)], check=True, timeout=30)
    for family in FAMILIES:
        profile = 5 if family.startswith('p5') else 8
        source = mux(family, root / family / 'rpus', profile,
                     bl=p5_bl if profile == 5 else root / 'bl.mkv')
        events = render(family, source)
        accepted = [e for e in events if e['kind'] == 'accepted_source_base']
        rendered = [e for e in events if e['kind'] == 'rendered_frame']
        for source_index, (a, f) in enumerate(zip(accepted, rendered), start=1):
            assert a['profile'] == f['profile'] == profile
            nal = root / family / f'rpus/p7-frame{source_index}.nal'
            assert a['rpu_sha256'] == hashlib.sha256(nal.read_bytes()).hexdigest()
            assert (root / family / f'output/rpus/frame-{source_index-1:03}.nal').read_bytes() == nal.read_bytes()
            assert f['mmr_segments'] == (2 if 'mmr' in family else 0)
            assert f['polynomial_segments'] == (1 if 'mmr' in family else 4 if family == 'p8-piecewise' else 3)
    p7 = mux('p7-omit-fel', root / 'valid/rpus')
    render('p7-omit-fel', p7)
    eotf = mux('p8-eotf', root / 'p8-eotf/rpus', 8)
    render('p8-eotf', eotf, 'Probe failed: unsupported base reconstructed signal', pre_gpu=True)
    missing = mux('base-missing-rpu', root / 'p8-affine/rpus', 8, mode='missing-rpu')
    render('base-missing-rpu', missing, 'Segment refused: one required source picture and fresh RPU per coded AU')
    wrong = mux('base-profile-mismatch', root / 'p5/rpus', 8, bl=p5_bl)
    render('base-profile-mismatch', wrong, 'Probe failed: decoded base RPU profile agrees with container', pre_gpu=True)
    render('base-no-dovi', root / 'bl.mkv', 'Segment refused: declared source Dolby Vision configuration', pre_gpu=True)
    render('base-cap', root / 'p8-affine/source.mkv', 'Segment refused: coded window frame cap exhausted', cap='2')
    render('base-declared-el', root / 'p8-affine/source.mkv', 'Segment refused: base-only mode has no declared EL', el=('64', '64'), pre_gpu=True)
    render('base-diagnostics', root / 'p8-affine/source.mkv', diagnostic=True)
    test_base_profile_nut_outputs(root)
    test_base_profile_nut_refusals(root)
    (root / 'base-results.json').write_text(json.dumps(results, indent=2) + '\n')
    return results


if __name__ == '__main__':
    if len(os.sys.argv) == 3 and os.sys.argv[2] == 'verify-nut':
        test_base_profile_nut_outputs(Path(os.sys.argv[1]))
        test_base_profile_nut_refusals(Path(os.sys.argv[1]))
    else:
        test_base_profile_windows(Path(os.sys.argv[1]) if len(os.sys.argv) == 2 else Path('/work'))
    print('base-profile-window-controls-pass')
