"""Post-review selected-video endpoint controls; no GPU or real-title mutation.

Consume retained 18-picture P7/P8 high fixtures and their passing scan JSON,
not a newly rerun baseline. Supply old/new scanners, exact tools and the operator's
source/build/dependency/environment receipt. Every command, true exit and log hash
is retained in fresh durable scratch, including partial failures.
"""
import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import subprocess
import time


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def tag_time(value):
    hours, minutes, seconds = value.split(':')
    return int(hours) * 3600 + int(minutes) * 60 + Fraction(seconds)


def format_tag(value):
    ns = value * 1_000_000_000
    assert ns.denominator == 1 and ns >= 0
    seconds, nanos = divmod(int(ns), 1_000_000_000)
    hours, seconds = divmod(seconds, 3600)
    minutes, seconds = divmod(seconds, 60)
    return f'{hours:02}:{minutes:02}:{seconds:02}.{nanos:09}'


def test_selected_video_duration_tag_uses_endpoint(args):
    root = args.output.resolve()
    root.mkdir(exist_ok=False)
    result = {'status': 'running', 'started_at': time.time(), 'commands': [], 'cases': [],
              'retained_cases': [],
              'inputs': {}, 'truncation_inputs': {},
              'threshold_ticks': 1, 'expected_scanner_cases': 20}
    prior, prior_commands, prior_cases = {}, {}, {}
    prior_root = None
    audio_packets = {}

    def command(argv, name):
        argv = list(map(str, argv))
        row = {'name': name, 'argv': argv, 'started_at': time.time()}
        result['commands'].append(row)
        stdout, stderr = b'', b''
        try:
            process = subprocess.run(argv, capture_output=True, timeout=60)
        except subprocess.TimeoutExpired as error:
            row.update({'exit': None, 'timeout': True})
            stdout, stderr = error.stdout or b'', error.stderr or b''
            raise
        except OSError as error:
            row.update({'exit': None, 'error': str(error)})
            stdout, stderr = b'', str(error).encode()
            raise
        else:
            row['exit'] = process.returncode
            stdout, stderr = process.stdout, process.stderr
            return process
        finally:
            row['ended_at'] = time.time()
            for label, payload in [('stdout', stdout), ('stderr', stderr)]:
                path = root / f'{name}.{label}'
                path.write_bytes(payload)
                row[f'{label}_sha256'] = sha(path)

    def checked(argv, name):
        process = command(argv, name)
        assert process.returncode == 0, (name, process.returncode, process.stderr)
        return process

    def probe(argv, name):
        # Read hash-bound successful preparation, never rerun it on continuation.
        if name in prior_commands:
            row = prior_commands[name]
            relocated = [value.replace(str(prior_root) + '/', str(args.resume_from) + '/')
                         for value in row['argv']]
            assert relocated == list(map(str, argv)) and row['exit'] == 0
            return json.loads((args.resume_from / f'{name}.stdout').read_bytes())
        return json.loads(checked(argv, name).stdout)

    def inspect(path, name, count):
        data = probe([args.ffprobe, '-v', 'error', '-show_streams', '-show_packets',
                           '-show_data_hash', 'sha256', '-of', 'json', path], name)
        streams = data['streams']
        assert [s['codec_type'] for s in streams] == ['audio', 'video']
        video, audio = streams[1], streams[0]
        assert video['index'] == 1 and video['codec_name'] == 'hevc'
        assert video['width'] == video['height'] == 64
        assert int(video['start_pts']) > 0 and 'duration' not in video
        assert int(video.get('nb_frames', 0)) <= 0, 'exercise tag-only completeness'
        packets = [p for p in data['packets'] if p['stream_index'] == 1]
        assert len(packets) == count
        tick = Fraction(video['time_base'])
        origin = min(int(p['pts']) for p in packets)
        endpoint = max(int(p['pts']) + int(p['duration']) for p in packets)
        assert origin == int(video['start_pts']) and all(int(p['duration']) > 0 for p in packets)
        assert tag_time(video['tags']['DURATION']) == endpoint * tick
        assert tag_time(audio['tags']['DURATION']) > endpoint * tick
        assert all(p['data_hash'].startswith('SHA256:') for p in packets)
        audio_tick = Fraction(audio['time_base'])
        audio_packets[name] = [(p['data_hash'], int(p['pts']) * audio_tick,
                                int(p['duration']) * audio_tick)
                               for p in data['packets'] if p['stream_index'] == 0]
        assert audio_packets[name]
        return video, packets, origin, endpoint, tick

    def scan(path, name, expected, reason=None, scanner=None):
        if name in prior_cases:
            case, row = prior_cases[name], prior_commands[name]
            assert case['expected_refusal'] == reason and sha(path) == case['source_sha256']
            assert row['argv'] == [str(scanner or args.scanner), str(prior_root / path.name), '1']
            process = subprocess.CompletedProcess(row['argv'], row['exit'],
                (args.resume_from / f'{name}.stdout').read_bytes(),
                (args.resume_from / f'{name}.stderr').read_bytes())
        else:
            process = command([scanner or args.scanner, path, '1'], name)
        if reason:
            assert process.returncode == 1 and not process.stdout.strip(), (name, process.returncode)
            assert f'DV title scan refused: {reason}\n'.encode() == process.stderr, (name, process.stderr)
        else:
            assert process.returncode == 0, (name, process.returncode, process.stderr)
            assert json.loads(process.stdout) == expected, (name, process.stdout)
        observation = {'name': name, 'exit': process.returncode,
                       'source_sha256': sha(path), 'expected_refusal': reason}
        if name in prior_cases:
            assert observation == prior_cases[name]
            result['retained_cases'].append(observation)
        else:
            result['cases'].append(observation)

    try:
        result['provenance'] = {'path': str(args.provenance), 'sha256': sha(args.provenance),
                                'receipt': json.loads(args.provenance.read_text())}
        result['binaries'] = {name: {'path': str(getattr(args, name)),
                                    'sha256': sha(getattr(args, name))}
                              for name in ['scanner', 'baseline', 'ffmpeg', 'ffprobe']}
        assert result['binaries']['scanner']['sha256'] != result['binaries']['baseline']['sha256']
        if args.resume_from:
            previous = args.resume_from / 'RESULT.json'
            assert sha(previous) == args.resume_sha256, 'pinned partial receipt changed'
            prior = json.loads(previous.read_text())
            assert prior['status'] == 'fail' and prior['threshold_ticks'] == 1
            assert prior['expected_scanner_cases'] == 20 and not prior.get('retained_cases')
            assert prior['binaries'] == result['binaries'], 'actual tools changed'
            result['resume'] = {'path': str(args.resume_from), 'sha256': sha(previous)}
            prior_commands = {row['name']: row for row in prior['commands']}
            prior_cases = {row['name']: row for row in prior['cases']}
            assert len(prior_commands) == len(prior['commands'])
            assert len(prior_cases) == len(prior['cases']) and 0 < len(prior_cases) < 20
            for row in prior_commands.values():
                assert row['ended_at'] >= row['started_at']
                for label in ('stdout', 'stderr'):
                    assert sha(args.resume_from / (row['name'] + '.' + label)) == row[label + '_sha256']
            prior_root = Path(prior_commands['p7-remux']['argv'][-1]).parent
        for profile in (7, 8):
            case = f'p{profile}'
            source = args.controls / f'{case}-high.mkv'
            retained = args.controls / f'{case}-high-scan.json'
            expected = json.loads(retained.read_text())
            assert expected['schema'] == 1 and expected['complete'] is True
            assert expected['decoded_frames'] == expected['valid_l1_frames'] == 18
            assert expected['missing_l1_frames'] == expected['invalid_l1_frames'] == 0
            assert expected['max_l1_pq'] == 4095
            result['inputs'][case] = {'source_sha256': sha(source), 'scan_sha256': sha(retained)}
            shifted = root / f'{case}-positive-origin.mkv'
            if f'{case}-endpoint' in prior_cases:
                shifted = args.resume_from / shifted.name
                assert sha(shifted) == prior_cases[f'{case}-endpoint']['source_sha256']
                assert all(result['inputs'][case][key] == prior['inputs'][case][key]
                           for key in ('source_sha256', 'scan_sha256'))
            else:
                # Offset only synthetic encoded video; unrelated audio ends later.
                checked([args.ffmpeg, '-nostdin', '-v', 'error', '-copyts', '-itsoffset', '0.125',
                         '-i', source, '-f', 'lavfi', '-i',
                         'sine=frequency=440:sample_rate=48000:duration=2',
                         '-map', '1:a', '-map', '0:v:0', '-c:a', 'pcm_s16le', '-c:v', 'copy',
                         '-threads', '1', '-avoid_negative_ts', 'disabled', shifted], f'{case}-remux')
            video, packets, origin, endpoint, tick = inspect(shifted, f'{case}-probe', 18)
            original = probe([args.ffprobe, '-v', 'error', '-select_streams', 'v:0',
                '-show_packets', '-show_data_hash', 'sha256', '-of', 'json', source],
                f'{case}-original-packets')['packets']
            assert [p['data_hash'] for p in original] == [p['data_hash'] for p in packets]
            # ffprobe source timing is read separately, never guessed from nominal rate.
            source_streams = probe([args.ffprobe, '-v', 'error', '-select_streams', 'v:0',
                '-show_streams', '-of', 'json', source], f'{case}-original-stream')['streams']
            assert len(source_streams) == 1
            source_tick = Fraction(source_streams[0]['time_base'])
            for old, new in zip(original, packets, strict=True):
                assert (int(old['pts']) - int(original[0]['pts'])) * source_tick == (int(new['pts']) - int(packets[0]['pts'])) * tick
                assert int(old['duration']) * source_tick == int(new['duration']) * tick
            result['inputs'][case].update({'origin': origin, 'endpoint': endpoint,
                                          'time_base': str(tick), 'selected_index': 1})
            scan(shifted, f'{case}-endpoint', expected)
            scan(shifted, f'{case}-old-sensitivity', expected,
                 'complete selected video duration tag', args.baseline)
            raw = shifted.read_bytes()
            tag = video['tags']['DURATION'].encode()
            assert raw.count(tag) == 1, 'unique selected tag value'
            at = raw.index(tag)
            key = raw.rfind(b'DURATION', 0, at)
            assert key >= 0 and at - key < 32, 'selected tag key association'
            variants = [
                ('plus-one', format_tag((endpoint + 1) * tick), None),
                ('minus-one', format_tag((endpoint - 1) * tick), None),
                ('span-not-endpoint', format_tag((endpoint - origin) * tick), 'complete selected video duration tag'),
                ('plus-two', format_tag((endpoint + 2) * tick), 'complete selected video duration tag'),
                ('minus-two', format_tag((endpoint - 2) * tick), 'complete selected video duration tag'),
                ('malformed', 'x' + tag.decode()[1:], 'selected video duration tag'),
                ('missing', None, 'independent complete selected-video endpoint unavailable'),
            ]
            for name, replacement, reason in variants:
                path = root / f'{case}-{name}.mkv'
                if f'{case}-{name}' in prior_cases:
                    scan(args.resume_from / path.name, f'{case}-{name}', expected, reason)
                    continue
                if replacement is None:
                    payload = raw[:key] + b'XURATION' + raw[key + 8:]
                else:
                    assert len(replacement.encode()) == len(tag), 'unchanged EBML lengths'
                    payload = raw[:at] + replacement.encode() + raw[at + len(tag):]
                path.write_bytes(payload)
                scan(path, f'{case}-{name}', expected, reason)
            truncated = root / f'{case}-truncated.mkv'
            prefix = root / f'{case}-prefix17.mkv'
            # A time cutoff is not a coded-packet count, especially with an offset.
            # Copy exactly the measured first17 AUs; mux full unrelated audio in a
            # separate step so the video limit cannot shorten that control stream.
            checked([args.ffmpeg, '-nostdin', '-v', 'error', '-copyts', '-i', shifted,
                     '-map', '0:v:0', '-c:v', 'copy', '-frames:v', '17',
                     '-avoid_negative_ts', 'disabled', prefix], f'{case}-prefix17')
            checked([args.ffmpeg, '-nostdin', '-v', 'error', '-copyts', '-i', shifted,
                     '-i', prefix, '-map', '0:a', '-map', '1:v:0', '-c', 'copy',
                     '-avoid_negative_ts', 'disabled', truncated], f'{case}-truncate17')
            short_video, short_packets, short_origin, short_end, short_tick = inspect(
                truncated, f'{case}-truncated17-probe', 17)
            assert short_origin * short_tick == origin * tick and short_end * short_tick < endpoint * tick
            assert [p['data_hash'] for p in short_packets] == [p['data_hash'] for p in packets[:17]]
            for full, short in zip(packets[:17], short_packets, strict=True):
                assert int(full['pts']) * tick == int(short['pts']) * short_tick
                assert int(full['duration']) * tick == int(short['duration']) * short_tick
            assert audio_packets[f'{case}-probe'] == audio_packets[f'{case}-truncated17-probe']
            short_raw, short_tag = truncated.read_bytes(), short_video['tags']['DURATION'].encode()
            assert short_raw.count(short_tag) == 1 and len(short_tag) == len(tag)
            # Preserve the full selected-video declaration after dropping a coded AU.
            truncated.write_bytes(short_raw.replace(short_tag, tag, 1))
            result['truncation_inputs'][case] = {'packets': 17, 'payload_and_timing_match': True,
                'full_audio_packets': len(audio_packets[f'{case}-probe']),
                'observed_endpoint': short_end, 'time_base': str(short_tick),
                'observed_short_tag': short_tag.decode(), 'preserved_full_tag': tag.decode(),
                'source_sha256': sha(truncated)}
            scan(truncated, f'{case}-truncated', expected, 'complete selected video duration tag')
        assert len(result['cases']) + len(result['retained_cases']) == result['expected_scanner_cases']
        result['status'] = 'pass'
        return result
    except BaseException as error:
        result.update({'status': 'fail', 'error': repr(error)})
        raise
    finally:
        result['ended_at'] = time.time()
        (root / 'RESULT.json').write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['controls', 'scanner', 'baseline', 'ffmpeg', 'ffprobe', 'provenance', 'output']:
        parser.add_argument(f'--{name}', required=True, type=lambda value: Path(value).resolve())
    parser.add_argument('--resume-from', type=lambda value: Path(value).resolve())
    parser.add_argument('--resume-sha256')
    args = parser.parse_args()
    if bool(args.resume_from) != bool(args.resume_sha256):
        parser.error('--resume-from and --resume-sha256 must be supplied together')
    print(json.dumps(test_selected_video_duration_tag_uses_endpoint(args)))
