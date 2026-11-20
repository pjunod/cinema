"""Exercise exclusive window metadata boundaries with actual decoded pictures.

Run inside the existing control image after generating valid/invalid-area RPUs
and compiling make_movie and the current renderer. All outputs use a new child
directory so earlier control evidence stays intact.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


TIMESTAMPS = 'setts=pts=N/(24*TB):dts=N/(24*TB):duration=1/(24*TB)'


def control_ffmpeg(binary, *args):
    subprocess.run([binary, '-nostdin', '-v', 'error', '-threads', '1',
                    *map(str, args)], check=True, timeout=60)


def author_gops(root, work, ffmpeg, cases):
    for layer, color in [('bl', 'gray'), ('el', 'black')]:
        ffmpeg('-f', 'lavfi', '-i', f'color=c={color}:s=64x64:r=24',
               '-frames:v', '18', '-pix_fmt', 'yuv420p10le', '-c:v', 'libx265',
               '-x265-params',
               'pools=none:frame-threads=1:bframes=0:keyint=18:log-level=error',
               work / f'{layer}.mkv')
    for case in cases:
        subprocess.run([str(root / 'make_movie'), str(work / 'bl.mkv'),
                        str(work / 'el.mkv'), str(root / case / 'rpus'),
                        str(work / f'{case}.mkv'), 'normal'],
                       check=True, timeout=30, capture_output=True)


def author_valid_source(root, work, ffmpeg_binary, ffprobe, mkvmerge):
    work.mkdir()

    def ffmpeg(*args):
        control_ffmpeg(ffmpeg_binary, *args)

    author_gops(root, work, ffmpeg, ['valid'])
    # Append complete GOPs without losing the Matroska Profile 7 mapping.
    # The clip retains an exclusive boundary after the last 48-picture window.
    source = work / 'source.mkv'
    gop = work / 'valid.mkv'
    repeats = 49
    subprocess.run([mkvmerge, '--quiet', '-o', str(source),
                    str(gop), *[f'+{gop}'] * (repeats - 1)],
                   check=True, timeout=60)
    probe = json.loads(subprocess.check_output(
        [ffprobe, '-v', 'error', '-select_streams', 'v:0', '-count_packets',
         '-show_streams', '-of', 'json', source], timeout=30))
    stream, = probe['streams']
    config = [record for record in stream.get('side_data_list', [])
              if record.get('side_data_type') == 'DOVI configuration record']
    assert len(config) == 1 and config[0]['dv_profile'] == 7, config
    assert all(config[0][flag] == 1 for flag in (
        'bl_present_flag', 'el_present_flag', 'rpu_present_flag')), config
    assert int(stream['nb_read_packets']) == 18 * repeats, stream['nb_read_packets']


def test_exclusive_window_metadata_boundary():
    root = Path('/work')
    work = root / 'exclusive-boundary'
    work.mkdir()
    env = os.environ | {
        'LD_LIBRARY_PATH': '/work/prefix/lib/aarch64-linux-gnu',
        'VK_ICD_FILENAMES': '/usr/share/vulkan/icd.d/lvp_icd.aarch64.json',
        'XDG_RUNTIME_DIR': '/work/runtime',
    }
    (root / 'runtime').mkdir(exist_ok=True)
    (root / 'runtime').chmod(0o700)

    def ffmpeg(*args):
        control_ffmpeg('ffmpeg', *args)

    author_gops(root, work, ffmpeg, ['valid', 'invalid-area'])
    ffmpeg('-i', work / 'valid.mkv', '-map', '0:v:0', '-c:v', 'copy',
           '-bsf:v', TIMESTAMPS, work / 'valid18.mkv')
    (work / 'repeat.txt').write_text('file valid18.mkv\n' * 3)
    ffmpeg('-f', 'concat', '-safe', '0', '-i', work / 'repeat.txt',
           '-map', '0:v:0', '-frames:v', '48', '-c:v', 'copy',
           '-bsf:v', TIMESTAMPS, work / 'first48.mkv')
    ffmpeg('-i', work / 'invalid-area.mkv', '-map', '0:v:0', '-c:v', 'copy',
           '-bsf:v', TIMESTAMPS, work / 'last18.mkv')
    (work / 'concat.txt').write_text('file first48.mkv\nfile last18.mkv\n')
    source = work / 'source.mkv'
    ffmpeg('-f', 'concat', '-safe', '0', '-i', work / 'concat.txt',
           '-map', '0:v:0', '-c:v', 'copy', source)
    results = []
    for base_only in [False, True]:
        for later in [False, True]:
            output = work / f'{"base" if base_only else "fel"}-{int(later)}'
            output.mkdir()
            with source.open('rb') as src, (output / 'out.nut').open('xb') as out:
                command = [str(root / 'bin/segment_decode_render'),
                           f'/proc/self/fd/{src.fileno()}', '.', '0', '64', '64',
                           '64', '64', '64', '0', 'bt2020-pq-master-clip',
                           f'/proc/self/fd/{out.fileno()}',
                           '2/1' if later else '0/1',
                           '11/4' if later else '2/1', '512']
                if base_only:
                    command[7:9] = ['0', '0']
                    command.append('base-rpu')
                completed = subprocess.run(command, cwd=output, env=env,
                                           pass_fds=(src.fileno(), out.fileno()),
                                           capture_output=True, timeout=60)
            (output / 'stdout').write_bytes(completed.stdout)
            (output / 'stderr').write_bytes(completed.stderr)
            events = [json.loads(line) for line in completed.stdout.splitlines()]
            if later:
                assert completed.returncode == 1, completed.stderr.decode()
                reason = (b'invalid base active area' if base_only
                          else b'invalid active area metadata')
                assert reason in completed.stderr, completed.stderr.decode()
                assert not any(e['kind'] == 'window_complete' for e in events)
            else:
                assert completed.returncode == 0, completed.stderr.decode()
                receipt = next(e for e in events if e['kind'] == 'window_complete')
                assert receipt['frames'] == 48 and receipt['boundary_observed']
                assert receipt['boundary_pts_ticks'] == 2000
                decoded = subprocess.check_output([
                    'ffmpeg', '-nostdin', '-v', 'error', '-threads', '1', '-i',
                    str(output / 'out.nut'), '-pix_fmt', 'rgb48le', '-f',
                    'rawvideo', 'pipe:1'], timeout=30)
                assert len(decoded) == 48 * 64 * 64 * 6
            results.append({'base_only': base_only, 'later_window': later,
                            'exit': completed.returncode})
    (work / 'receipt.json').write_text(json.dumps({
        'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
        'cases': results,
    }, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--author-valid-only', type=Path, metavar='FRESH_DIRECTORY')
    parser.add_argument('--controls', type=Path, default=Path('/work'),
                        help='existing make_movie and valid/rpus output directory')
    parser.add_argument('--ffmpeg', default='ffmpeg')
    parser.add_argument('--ffprobe', default='ffprobe')
    parser.add_argument('--mkvmerge', default='mkvmerge')
    args = parser.parse_args()
    if args.author_valid_only is None:
        test_exclusive_window_metadata_boundary()
    else:
        author_valid_source(args.controls, args.author_valid_only,
                            args.ffmpeg, args.ffprobe, args.mkvmerge)
