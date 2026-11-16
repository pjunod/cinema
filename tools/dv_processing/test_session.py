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

With --pcm-join each window carries the daemon's own interval (from 200 ms
before its encoder's rebase point) and the soundtrack PCM the encoder's
lattice filter produces from the carried pipe is compared with what it
produces from the source read as main read it (-noaccurate_seek -ss), for
every window and across each join. With --session-only --growth N the
helper runs N windows and its descriptor count and RSS must stay flat.

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
            # As the daemon does: the channel count from its own probe.
            channels = subprocess.check_output([
                options.ffprobe, '-v', 'error', '-select_streams', f'a:{audio}',
                '-show_entries', 'stream=channels', '-of', 'csv=p=0', str(options.source)],
                text=True).strip()
            args += ['--audio-ordinal', str(audio), '--audio-channels', channels]
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
            # NUT, or Matroska for a codec NUT cannot tag; ffprobe and ffmpeg
            # probe which.
            audio_reader = Drained(audio_read, (directory / 'audio.carried').open('wb'))
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


AUDIO_RATE = 48000
AAC_FRAME = 1024
LOSSLESS = ('truehd', 'flac', 'mlp', 'alac') 


def lattice(start):
    """The encoder's soundtrack clock for a window starting at film time
    `start` (plurx-core AudioClock, no sync offset): its rebase point and the
    sample at which its kept audio begins, both in 48 kHz samples."""
    anchor = int(max(float(start) - 2.0, 0.0) * AUDIO_RATE) // AAC_FRAME * AAC_FRAME
    seek = max(anchor - 2 * AUDIO_RATE, 0) // AAC_FRAME * AAC_FRAME
    return seek, anchor


def daemon_interval(start, end):
    """dv_runtime::window_soundtrack with a zero origin and no offset."""
    seek, _ = lattice(start)
    return Fraction(seek, AUDIO_RATE) - Fraction(1, 5), end + Fraction(1, 2)


def lattice_pcm(options, inputs, ordinal, start, samples):
    """What the encoder's lattice filter makes of a soundtrack input: f64
    PCM from the window's kept start, `samples` frames long."""
    seek, anchor = lattice(start)
    relative = anchor - seek
    graph = (f'asetpts=PTS-{seek / AUDIO_RATE:.9f}/TB,aresample=48000:async=1:first_pts=0,'
             f'atrim=start_sample={relative},asetpts=PTS-({relative}),'
             f'aresample=48000:async=1:first_pts=0,atrim=end_sample={samples}')
    return subprocess.check_output(
        [options.ffmpeg, '-v', 'error', '-copyts', *inputs, '-map', f'0:a:{ordinal}', '-af', graph,
         '-c:a', 'pcm_f64le', '-f', 'f64le', '-'])


def pcm_difference(a, b, channels):
    """(exact, lag, max abs difference) of two f64 PCM byte strings."""
    from array import array
    x, y = array('d'), array('d')
    x.frombytes(a[:len(a) // 8 * 8])
    y.frombytes(b[:len(b) // 8 * 8])
    n = min(len(x), len(y)) // channels * channels
    if n == 0:
        return False, None, float('inf')
    if x[:n] == y[:n]:
        return True, 0, 0.0
    # The lag that best aligns a 0.1 s excerpt of the first channel.
    probe = range(AUDIO_RATE // 10, AUDIO_RATE // 5)
    best = min(range(-64, 65), key=lambda lag: sum(
        (x[i * channels] - y[(i + lag) * channels]) ** 2 for i in probe))
    return False, best, max(abs(p - q) for p, q in zip(x[:n], y[:n]))


def check_pcm_join(options, windows, directories, codec):
    """The PCM the encoder's lattice filter makes from the carried pipe,
    against what it made on main from the source (-noaccurate_seek -ss at the
    rebase point, which starts the decode at the preceding video keyframe).

    The carried PCM must equal main's bit for bit where main's own PCM does
    not depend on where its decode starts. Where it does (AC-3's dither, the
    AAC decoder's state), the carried PCM may differ from main's by no more
    than main's own decode started at another keyframe. The one licensed
    difference is one main already had: on a millisecond clock, a codec whose
    frames do not fall on whole milliseconds (TrueHD, DTS) is aligned by the
    rounding of its first decoded frame, so main's own alignment moved with
    the keyframe its decode started at. There the carried PCM may be offset
    from main's, within the resampler's 1 ms. Each join is checked the same
    way, on both paths."""
    channels = int(subprocess.check_output([
        options.ffprobe, '-v', 'error', '-select_streams', f'a:{options.audio}',
        '-show_entries', 'stream=channels', '-of', 'csv=p=0', str(options.source)], text=True))
    samples = AUDIO_RATE * 5 // 2

    def main_pcm(start, back=0.0):
        seek, _ = lattice(start)
        return lattice_pcm(options, ['-noaccurate_seek', '-ss', f'{seek / AUDIO_RATE - back:.9f}',
                                     '-i', str(options.source)], options.audio, start, samples)

    probe = json.loads(subprocess.check_output([
        options.ffprobe, '-v', 'error', '-select_streams', f'a:{options.audio}',
        '-read_intervals', f'{float(windows[0][0]) - 3}%{float(windows[-1][1])}',
        '-show_entries', 'packet=pts:stream=time_base', '-of', 'json', str(options.source)]))
    millisecond = probe['streams'][0]['time_base'] == '1/1000'
    pts = sorted(int(p['pts']) for p in probe['packets'])
    off_lattice = millisecond and len({b - a for a, b in zip(pts, pts[1:])}) > 1

    def matches(row, licensed, spread=0.0):
        if row['exact']:
            return True
        if row['lag'] == 0:
            return row['max_abs'] <= spread
        return licensed and abs(row['lag']) < AUDIO_RATE // 1000

    rows, carried, mains, allowed = [], [], [], []
    for (start, _), directory in zip(windows, directories):
        main = main_pcm(start)
        pipe = lattice_pcm(options, ['-i', str(directory / 'audio.carried')], 0, start, samples)
        assert len(pipe) == len(main) == samples * channels * 8, (start, len(pipe), len(main))
        # Main's own spread: its decode started at other keyframes.
        drifts = [pcm_difference(main, main_pcm(start, back), channels) for back in (1.0, 3.0, 5.0, 8.0)]
        drift = {'exact': all(d[0] for d in drifts),
                 'lag': next((d[1] for d in drifts if d[1] != 0), 0),
                 'max_abs': max(d[2] for d in drifts if d[1] == 0) if any(d[1] == 0 for d in drifts) else 0.0}
        exact, lag, error = pcm_difference(main, pipe, channels)
        row = {'start': float(start), 'exact': exact, 'lag': lag, 'max_abs': error,
               'main_from_other_keyframes': drift}
        rows.append(row)
        licensed = off_lattice or not matches(drift, False, float('inf'))
        spread = 0.0 if drift['exact'] else max(1e-2, 2 * drift['max_abs'])
        assert matches(row, licensed, spread), row
        allowed.append((licensed, spread))
        carried.append(pipe)
        mains.append(main)
    for k in range(len(windows) - 1):
        shift = lattice(windows[k + 1][0])[1] - lattice(windows[k][0])[1]
        joined = {}
        for name, pcm in (('carried', carried), ('main', mains)):
            tail = pcm[k][shift * channels * 8:]
            exact, lag, error = pcm_difference(tail, pcm[k + 1][:len(tail)], channels)
            joined[name] = {'exact': exact, 'lag': lag, 'max_abs': error}
        rows.append({'join': k, **joined})
        # The carried join is as continuous as main's.
        main_join = joined['main']
        licensed = (allowed[k][0] or allowed[k + 1][0]
                    or not matches(main_join, False, float('inf')))
        spread = max(allowed[k][1], allowed[k + 1][1],
                     0.0 if main_join['exact'] else 2 * main_join['max_abs'])
        assert matches(joined['carried'], licensed, spread), rows[-1]
    return rows


def helper_footprint(pid):
    fds = len(os.listdir(f'/proc/{pid}/fd'))
    rss = next(int(line.split()[1]) for line in Path(f'/proc/{pid}/status').read_text().splitlines()
               if line.startswith('VmRSS:'))
    return fds, rss


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
    parser.add_argument('--pcm-join', action='store_true',
                        help="carry the daemon's interval and compare the lattice PCM")
    parser.add_argument('--session-only', action='store_true',
                        help='no single-shot comparison (long sessions)')
    parser.add_argument('--growth', type=int, default=0,
                        help='require flat fd count and RSS from this window on')
    options = parser.parse_args()
    assert not options.pcm_join or options.audio is not None
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
        if options.session_only:
            break
        result, elapsed = single_shot(options, source, start, end, frames, options.output / f'single-{i}')
        singles.append(result)
        results['single'].append(round(elapsed, 3))
    codec = None
    if options.audio is not None:
        codec = subprocess.check_output([
            options.ffprobe, '-v', 'error', '-select_streams', f'a:{options.audio}',
            '-show_entries', 'stream=codec_name', '-of', 'csv=p=0', str(options.source)],
            text=True).strip()
    session = Session(options, options.output, options.audio)
    footprints = []
    for i, (start, end) in enumerate(windows):
        audio = None
        if options.pcm_join:
            audio = daemon_interval(start, end)
        elif options.audio is not None:
            # The encoder's soundtrack preroll begins about four seconds
            # before the window; its tail reaches half a second past it.
            audio = (start - 4, end + Fraction(1, 2))
        directory = options.output / f'session-{i}'
        reply, nut, elapsed = session.window(start, end, frames, directory, audio)
        assert reply == b'done', (i, reply, (options.output / 'session.stderr').read_text())
        results['session'].append(round(elapsed, 3))
        if options.session_only:
            footprints.append(helper_footprint(session.process.pid))
            if i >= 2:
                # Keep only what the growth check reads.
                subprocess.run(['rm', '-rf', str(options.output / f'session-{i - 2}')], check=True)
            continue
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
            if codec not in LOSSLESS:
                # The carried packets are the source's, byte for byte.
                expected = source_audio_packets(options, *audio)
                actual = window_audio_packets(options, directory / 'audio.carried')
                assert actual == expected, (i, len(actual), len(expected), actual[:2], expected[:2])
                continue
            # ffprobe's parsers reframe TrueHD and FLAC packets, so they are
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
            actual = decoded_audio(options, directory / 'audio.carried', 0, *kept)
            # TrueHD frames are 40 samples (0.83 ms) apart, so the frame a
            # millisecond timestamp names is found by its samples.
            first = next(j for j, (pts, digest) in enumerate(expected)
                         if abs(pts - actual[0][0]) < Fraction(1, 1000) and digest == actual[0][1])
            expected = expected[first:first + len(actual)]
            assert len(actual) == len(expected) > 0, (i, len(actual), len(expected))
            for (want_pts, want), (have_pts, have) in zip(expected, actual):
                assert have == want and abs(have_pts - want_pts) < Fraction(1, 1000), (
                    i, want_pts, have_pts)
    if options.pcm_join:
        results['pcm'] = check_pcm_join(
            options, windows, [options.output / f'session-{i}' for i in range(len(windows))],
            codec)
    if options.session_only:
        assert session.close() == 0
        results['session'] = {'windows': len(windows),
                              'mean': round(sum(results['session'][1:]) / (len(windows) - 1), 3),
                              'max': max(results['session'][1:])}
        if options.growth:
            settled, last = footprints[options.growth - 1], footprints[-1]
            results['footprint'] = {'at': options.growth, 'settled': settled, 'last': last}
            assert last[0] == settled[0], ('descriptor growth', settled, last)
            assert last[1] - settled[1] < 64 * 1024, ('RSS growth (KiB)', settled, last)
        print(json.dumps(results))
        return
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
