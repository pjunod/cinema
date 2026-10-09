"""Exercise exclusive window metadata boundaries with actual decoded pictures.

Run inside the existing control image after generating valid/invalid-area RPUs
and compiling make_movie and the current renderer. All outputs use a new child
directory so earlier control evidence stays intact.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess


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
        subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-threads', '1',
                        *map(str, args)], check=True, timeout=60)

    for layer, color in [('bl', 'gray'), ('el', 'black')]:
        ffmpeg('-f', 'lavfi', '-i', f'color=c={color}:s=64x64:r=24',
               '-frames:v', '18', '-pix_fmt', 'yuv420p10le', '-c:v', 'libx265',
               '-x265-params',
               'pools=none:frame-threads=1:bframes=0:keyint=18:log-level=error',
               work / f'{layer}.mkv')
    for case in ['valid', 'invalid-area']:
        subprocess.run([str(root / 'make_movie'), str(work / 'bl.mkv'),
                        str(work / 'el.mkv'), str(root / case / 'rpus'),
                        str(work / f'{case}.mkv'), 'normal'],
                       check=True, timeout=30, capture_output=True)
    timestamps = 'setts=pts=N/(24*TB):dts=N/(24*TB):duration=1/(24*TB)'
    ffmpeg('-i', work / 'valid.mkv', '-map', '0:v:0', '-c:v', 'copy',
           '-bsf:v', timestamps, work / 'valid18.mkv')
    (work / 'repeat.txt').write_text('file valid18.mkv\n' * 3)
    ffmpeg('-f', 'concat', '-safe', '0', '-i', work / 'repeat.txt',
           '-map', '0:v:0', '-frames:v', '48', '-c:v', 'copy',
           '-bsf:v', timestamps, work / 'first48.mkv')
    ffmpeg('-i', work / 'invalid-area.mkv', '-map', '0:v:0', '-c:v', 'copy',
           '-bsf:v', timestamps, work / 'last18.mkv')
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
    test_exclusive_window_metadata_boundary()
