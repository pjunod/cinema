"""Session-mode controls for segment_decode_render on a real Dolby Vision title.

A session renders consecutive windows in one helper process. These controls
check, against the helper's own single-shot windows on the same source:

* each session window's NUT output, RPUs, timing rows and coded trace are
  byte-identical to the single-shot window's;
* only the first window decodes keyframe preroll; later windows report zero;
* the soundtrack written for each window is exactly the source's packets
  for that window's interval, unchanged;
* a window that does not start where the previous one ended is refused, and
  closing the control socket ends the session cleanly.

Run inside the media runtime (it needs the helper's Vulkan and VA-API
runtime), for example:

  python3 test_session.py --helper ./segment_decode_render --source movie.mkv \
      --mode base-rpu --raster 3840x2160 --start 612 --windows 3 --output /scratch/dir
"""
import argparse
import hashlib
import json
import os
import socket
import subprocess
import threading
import time
from fractions import Fraction
from pathlib import Path


def drain(fd, sink):
    digest = hashlib.sha256()
    size = 0
    with os.fdopen(fd, 'rb') as pipe:
        while True:
            chunk = pipe.read(1 << 20)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
            if sink is not None:
                sink.write(chunk)
    return digest.hexdigest(), size


class Drained(threading.Thread):
    def __init__(self, fd, sink=None):
        super().__init__(daemon=True)
        self.fd, self.sink, self.result = fd, sink, None

    def run(self):
        self.result = drain(self.fd, self.sink)


def fraction_text(value):
    value = Fraction(value)
    return f'{value.numerator}/{value.denominator}'


def argv(options, frames, out='-', nut='-', start='-', end='-'):
    width, height = options.raster
    return [str(options.helper), '/proc/self/fd/3', out, str(options.video_index),
            str(frames), str(width), str(height), '0', '0', '0',
            'bt2020-pq-master-clip', nut, start, end, '512', options.mode,
            str(width), str(height), 'pq-bilinear-yuv420p10-v1']


def evidence(directory):
    directory = Path(directory)
    rpus = sorted((directory / 'rpus').glob('*.nal'))
    events = [json.loads(line) for line in (directory / 'renderer.jsonl').read_text().splitlines()]
    return {
        'timing': (directory / 'timing.tsv').read_text(),
        'rpus': [hashlib.sha256(path.read_bytes()).hexdigest() for path in rpus],
        'coded': sorted((e['pts_ticks'], e['packet_sha256'], e['rpu_sha256'])
                        for e in events if e['kind'] == 'coded_window_input'),
        'complete': next(e for e in events if e['kind'] == 'window_complete'),
        'kinds': sorted({e['kind'] for e in events}),
        'stages': [line.split('\t')[0] for line in (directory / 'stages.tsv').read_text().splitlines()],
    }


def single_shot(options, source, start, end, frames, directory):
    directory.mkdir()
    read, write = os.pipe()
    reader = Drained(read)
    reader.start()
    with (directory / 'renderer.jsonl').open('wb') as out, (directory / 'stderr').open('wb') as err:
        started = time.monotonic()
        code = subprocess.run(argv(options, frames, str(directory), f'/proc/self/fd/{write}',
                                   fraction_text(start), fraction_text(end)),
                              stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                              pass_fds=(3, write), timeout=60).returncode
        elapsed = time.monotonic() - started
    os.close(write)
    reader.join(60)
    assert code == 0, (start, (directory / 'stderr').read_text())
    return reader.result, elapsed


class Session:
    def __init__(self, options, directory, audio):
        self.parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        args = argv(options, 64) + ['--session', str(child.fileno())]
        if audio is not None:
            args += ['--audio-ordinal', str(audio)]
        self.stderr = (directory / 'session.stderr').open('wb')
        self.process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                        stderr=self.stderr, pass_fds=(3, child.fileno()))
        child.close()

    def window(self, start, end, frames, directory, audio=None):
        directory.mkdir(mode=0o700)
        held = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        read, write = os.pipe()
        fds = [write, held]
        command = f'window {fraction_text(start)} {fraction_text(end)} {frames} 512'
        audio_reader = None
        if audio is not None:
            audio_read, audio_write = os.pipe()
            fds.append(audio_write)
            command += f' {fraction_text(audio[0])} {fraction_text(audio[1])}'
            audio_reader = Drained(audio_read, (directory / 'audio.mka').open('wb'))
            audio_reader.start()
        reader = Drained(read)
        reader.start()
        started = time.monotonic()
        socket.send_fds(self.parent, [command.encode()], fds)
        for fd in fds:
            os.close(fd)
        reply = self.parent.recv(64)
        elapsed = time.monotonic() - started
        reader.join(60)
        if audio_reader:
            audio_reader.join(60)
            audio_reader.sink.close()
        return reply, reader.result, elapsed

    def close(self):
        self.parent.close()
        code = self.process.wait(60)
        self.stderr.close()
        return code


def source_audio_packets(options, start, end):
    """The source's selected soundtrack packets that overlap [start, end)."""
    probe = subprocess.check_output([
        options.ffprobe, '-v', 'error', '-select_streams', f'a:{options.audio}',
        '-read_intervals', f'{float(start) - 12}%{float(end) + 4}', '-show_packets',
        '-show_data_hash', 'sha256', '-show_entries', 'packet=pts,duration,data_hash',
        '-of', 'json', str(options.source)])
    stream = json.loads(subprocess.check_output([
        options.ffprobe, '-v', 'error', '-select_streams', f'a:{options.audio}',
        '-show_entries', 'stream=time_base', '-of', 'json', str(options.source)]))
    base = Fraction(stream['streams'][0]['time_base'])
    rows = []
    for packet in json.loads(probe)['packets']:
        pts, duration = int(packet['pts']), int(packet.get('duration', 0))
        if (pts + duration) * base > start and pts * base < end:
            rows.append((pts * base, packet['data_hash']))
    return sorted(set(rows))


def window_audio_packets(options, path):
    probe = json.loads(subprocess.check_output([
        options.ffprobe, '-v', 'error', '-show_packets', '-show_data_hash', 'sha256',
        '-show_entries', 'packet=pts,data_hash:stream=time_base', '-of', 'json', str(path)]))
    base = Fraction(probe['streams'][0]['time_base'])
    return sorted((int(p['pts']) * base, p['data_hash']) for p in probe['packets'])


def decoded_audio(options, path, ordinal, start, end, seek=None):
    """Decoded soundtrack frames (pts, md5) with pts in [start, end)."""
    args = [options.ffmpeg, '-v', 'error', '-copyts']
    if seek is not None:
        args += ['-ss', str(float(seek))]
    # pcm_f64le holds float and 32-bit decodes exactly; a 16-bit sink would
    # dither, and dither state depends on where the decode began.
    args += ['-to', str(float(end) + 1), '-i', str(path), '-map', f'0:a:{ordinal}',
             '-c:a', 'pcm_f64le', '-f', 'framemd5', '-']
    base, rows = None, []
    for line in subprocess.check_output(args, text=True).splitlines():
        if line.startswith('#tb 0:'):
            base = Fraction(line.split(':', 1)[1].strip())
        if line.startswith('#') or not line.strip():
            continue
        fields = [field.strip() for field in line.split(',')]
        rows.append((int(fields[2]) * base, fields[5]))
    return [(pts, digest) for pts, digest in rows if start <= pts < end]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mode', choices=('base-rpu', 'fel'), required=True)
    parser.add_argument('--raster', default='3840x2160')
    parser.add_argument('--video-index', type=int, default=0)
    parser.add_argument('--fps', default='24/1')
    parser.add_argument('--start', type=Fraction, required=True)
    parser.add_argument('--seconds', type=Fraction, default=Fraction(2))
    parser.add_argument('--windows', type=int, default=3)
    parser.add_argument('--audio', type=int, help='soundtrack ordinal to carry')
    parser.add_argument('--ffprobe', default='/usr/lib/jellyfin-ffmpeg/ffprobe')
    parser.add_argument('--ffmpeg', default='/usr/lib/jellyfin-ffmpeg/ffmpeg')
    options = parser.parse_args()
    options.raster = tuple(int(v) for v in options.raster.split('x'))
    options.output.mkdir(mode=0o700)
    fps = Fraction(options.fps)
    frames = int(options.seconds * fps)
    windows = [(options.start + i * options.seconds, options.start + (i + 1) * options.seconds)
               for i in range(options.windows)]
    source = os.open(options.source, os.O_RDONLY)
    os.dup2(source, 3)
    results = {'single': [], 'session': []}
    singles = []
    for i, (start, end) in enumerate(windows):
        result, elapsed = single_shot(options, source, start, end, frames, options.output / f'single-{i}')
        singles.append(result)
        results['single'].append(round(elapsed, 3))
    session = Session(options, options.output, options.audio)
    for i, (start, end) in enumerate(windows):
        audio = None
        if options.audio is not None:
            # The encoder's soundtrack preroll begins about four seconds
            # before the window; its tail reaches half a second past it.
            audio = (start - 4, end + Fraction(1, 2))
        directory = options.output / f'session-{i}'
        reply, nut, elapsed = session.window(start, end, frames, directory, audio)
        assert reply == b'done', (i, reply, (options.output / 'session.stderr').read_text())
        results['session'].append(round(elapsed, 3))
        single = evidence(options.output / f'single-{i}')
        warm = evidence(directory)
        assert nut == singles[i], (i, nut, singles[i])
        for field in ('timing', 'rpus', 'coded'):
            assert warm[field] == single[field], (i, field)
        assert warm['kinds'] == single['kinds'], (i, warm['kinds'], single['kinds'])
        if i == 0:
            assert warm['complete']['preroll_pairs'] == single['complete']['preroll_pairs'] or audio
            assert 'seeked' in warm['stages'], warm['stages']
        else:
            assert warm['complete']['preroll_pairs'] == 0, warm['complete']
            assert 'seeked' not in warm['stages'] and warm['stages'][0] == 'started', warm['stages']
        for key in ('frames', 'boundary_pts_ticks', 'first_emitted_pts_ticks', 'last_emitted_end_ticks'):
            assert warm['complete'][key] == single['complete'][key], (i, key)
        if audio is not None:
            codec = subprocess.check_output([
                options.ffprobe, '-v', 'error', '-select_streams', f'a:{options.audio}',
                '-show_entries', 'stream=codec_name', '-of', 'csv=p=0', str(options.source)],
                text=True).strip()
            if codec != 'truehd':
                # The carried packets are the source's, byte for byte.
                expected = source_audio_packets(options, *audio)
                actual = window_audio_packets(options, directory / 'audio.mka')
                assert actual == expected, (i, len(actual), len(expected), actual[:2], expected[:2])
                continue
            # ffprobe's MLP parser reframes TrueHD packets, so they are
            # compared through the decoder, which resynchronises on TrueHD's
            # major sync and is lossless: what the encoder keeps (the window
            # and its two seconds of AAC preroll) decodes to exactly the
            # source's samples. Matroska stores millisecond timestamps, so a
            # carried frame's timestamp may differ from the source decode's
            # sample-exact one by under a millisecond: below the encoder's
            # aresample async=1 compensation threshold (1 ms), so it never
            # moves a sample.
            kept = (audio[0] + 1, audio[1] - Fraction(1, 2))
            margin = Fraction(1, 100)
            expected = decoded_audio(options, options.source, options.audio,
                                     kept[0] - margin, kept[1] + margin, seek=audio[0] - 4)
            actual = decoded_audio(options, directory / 'audio.mka', 0, *kept)
            # TrueHD frames are 40 samples (0.83 ms) apart, so the frame a
            # millisecond timestamp names is found by its samples.
            first = next(j for j, (pts, digest) in enumerate(expected)
                         if abs(pts - actual[0][0]) < Fraction(1, 1000) and digest == actual[0][1])
            expected = expected[first:first + len(actual)]
            assert len(actual) == len(expected) > 0, (i, len(actual), len(expected))
            for (want_pts, want), (have_pts, have) in zip(expected, actual):
                assert have == want and abs(have_pts - want_pts) < Fraction(1, 1000), (
                    i, want_pts, have_pts)
    # A window that skips ahead is refused, and the session ends with it.
    start, end = windows[-1][1] + options.seconds, windows[-1][1] + 2 * options.seconds
    audio = (start - 4, end + Fraction(1, 2)) if options.audio is not None else None
    reply, _, _ = session.window(start, end, frames, options.output / 'session-gap', audio)
    assert reply == b'', reply
    code = session.close()
    assert code == 1, code
    assert 'contiguous session window' in (options.output / 'session.stderr').read_text()
    # Closing the control socket ends an idle session cleanly.
    session = Session(options, options.output / 'session-gap', options.audio)
    assert session.close() == 0
    print(json.dumps(results))


if __name__ == '__main__':
    main()
