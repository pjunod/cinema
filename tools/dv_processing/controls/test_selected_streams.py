"""Manual selected-stream equivalence with unrelated audio, text and video.

Supply generated locked controls, two reviewed helpers built from otherwise
identical source, their shared library directory and fresh scratch. This is a
real demux/decode/render check, not production or display qualification.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


def test_discard_keeps_selected_native_pictures_and_multistream_eof(
        controls, baseline, optimized, library, scratch):
    controls, scratch = Path(controls), Path(scratch)
    scratch.mkdir()
    subtitle = scratch / 'unrelated.srt'
    subtitle.write_text('1\n00:00:00,000 --> 00:00:00,500\nunrelated\n')
    source = scratch / 'multistream.mkv'
    # Absolute HEVC index3, following audio, subtitle and another video. Audio
    # deliberately extends beyond the six-picture selected video.
    subprocess.run([
        'ffmpeg', '-nostdin', '-v', 'error', '-copyts', '-threads', '1',
        '-i', str(controls / 'p7-omit-fel/source.mkv'),
        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=0.5',
        '-f', 'lavfi', '-i', 'color=c=green:s=64x64:r=24:d=0.5',
        '-f', 'srt', '-i', str(subtitle),
        '-map', '1:a', '-map', '3:s', '-map', '2:v', '-map', '0:v',
        '-af', 'asetpts=PTS+1024', '-c:a', 'aac', '-threads:a', '1', '-c:s', 'srt',
        '-c:v:0', 'libx264', '-threads:v:0', '1', '-bf:v:0', '0',
        '-c:v:1', 'copy', '-avoid_negative_ts', 'disabled', str(source),
    ], check=True, timeout=30)
    probe = json.loads(subprocess.check_output([
        'ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(source)
    ], timeout=30))
    assert [stream['codec_type'] for stream in probe['streams']] == ['audio', 'subtitle', 'video', 'video']
    assert probe['streams'][3]['codec_name'] == 'hevc'
    env = os.environ | {'PLURX_DV_FRAME_HASHES': '1', 'LP_NUM_THREADS': '1',
                        'LD_LIBRARY_PATH': str(library)}
    results = []
    for start, end, frames in [('0/1', '83/1000', 2), ('83/1000', '167/1000', 2),
                               ('125/1000', '208/1000', 2), ('208/1000', '249/1000', 1)]:
        observations = []
        for name, helper in [('baseline', baseline), ('optimized', optimized)]:
            directory = scratch / f'{name}-{len(results)}'
            directory.mkdir()
            process = subprocess.run([
                str(helper), str(source), str(directory), '3', '64', '64', '64',
                '0', '0', '0', 'bt2020-pq-master-clip', str(directory / 'out.nut'),
                start, end, '512', 'fel', '32', '32', 'pq-bilinear-rgb48-v1',
            ], env=env, capture_output=True, timeout=30)
            (directory / 'stdout').write_bytes(process.stdout)
            (directory / 'stderr').write_bytes(process.stderr)
            assert process.returncode == 0, (name, start, process.stderr)
            events = [json.loads(line) for line in process.stdout.splitlines()]
            pairs = [event for event in events if event['kind'] == 'accepted_source_pair']
            rendered = [event for event in events if event['kind'] == 'rendered_frame']
            assert len(pairs) == len(rendered) == frames
            compared = [event for event in events if event['kind'] in
                        ['coded_window_input', 'accepted_source_pair', 'rendered_frame',
                         'window_complete', 'segment_complete']]
            observations.append(compared)
        assert observations[0] == observations[1], ('selected packets/pictures/EOF changed', start)
        results.append({'start': start, 'end': end, 'frames': frames, 'same_native_output': True})
    # Packet/native/RPU/RGB equality also catches any repeated first picture.
    metadata = {'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
                'helpers': {name: hashlib.sha256(Path(helper).read_bytes()).hexdigest()
                            for name, helper in [('baseline', baseline), ('optimized', optimized)]},
                'library_sha256': hashlib.sha256((Path(library) / 'libplacebo.so.374').read_bytes()).hexdigest(),
                'selected_absolute_index': 3, 'original_inventory': 4, 'windows': results}
    (scratch / 'results.json').write_text(json.dumps(metadata, indent=2) + '\n')
    return metadata


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['controls', 'baseline', 'optimized', 'library', 'scratch']:
        parser.add_argument(name, type=Path)
    print(json.dumps(test_discard_keeps_selected_native_pictures_and_multistream_eof(**vars(parser.parse_args()))))
