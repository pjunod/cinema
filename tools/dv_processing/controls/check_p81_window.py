"""Check a served reconstructed Profile 8.1 window against its Profile 7 source.

- the configuration record is Profile 8, BL compatibility 1, no EL, carried in
  a `dvvC` box (Profile 8 uses `dvvC`, never the P7 `dvcC`);
- every access unit carries exactly one RPU (UNSPEC62 NAL), counted from the
  packets themselves and from `dovi_tool extract-rpu`;
- per presented frame, the L1, L2 and L8 display-management blocks equal the
  source RPU's (L3, L5, L6 and L9-L11 are reported too). Frames are paired by
  presentation rank, not decode order, because the encoder reorders.

usage: check_p81_window.py WORKDIR WINDOW.mp4 SOURCE.mkv FRAMES
Environment: PLURX_FFPROBE, PLURX_FFMPEG, PLURX_DOVI_TOOL.
Prints one JSON document; exits 1 on any failed check.
"""
import json
import os
import pathlib
import subprocess
import sys

FFPROBE = os.environ.get('PLURX_FFPROBE', 'ffprobe')
FFMPEG = os.environ.get('PLURX_FFMPEG', 'ffmpeg')
DOVI = os.environ.get('PLURX_DOVI_TOOL', 'dovi_tool')
LEVELS = ('Level1', 'Level2', 'Level8')
REPORTED = ('Level3', 'Level5', 'Level6', 'Level9', 'Level10', 'Level11')


def run(args):
    return subprocess.run(args, check=True, capture_output=True, timeout=600).stdout


def packets(path):
    doc = json.loads(run([FFPROBE, '-v', 'error', '-select_streams', 'v:0', '-show_entries',
                          'packet=pts,flags', '-of', 'json', str(path)]))
    return [int(packet['pts']) for packet in doc['packets']]


def rpus(work, path, name):
    annexb = work / f'{name}.hevc'
    run([FFMPEG, '-nostdin', '-v', 'error', '-i', str(path), '-map', '0:v:0', '-c', 'copy',
         '-bsf:v', 'hevc_mp4toannexb', '-f', 'hevc', str(annexb)])
    binary = work / f'{name}.rpu.bin'
    run([DOVI, 'extract-rpu', '-i', str(annexb), '-o', str(binary)])
    exported = work / f'{name}.rpu.json'
    run([DOVI, 'export', '-i', str(binary), '-d', f'all={exported}'])
    return json.loads(exported.read_text())


def rpu_nals_per_access_unit(work, path):
    """UNSPEC62 NAL count in each packet, one Annex B file per packet."""
    directory = work / 'access-units'
    directory.mkdir()
    run([FFMPEG, '-nostdin', '-v', 'error', '-i', str(path), '-map', '0:v:0', '-c', 'copy',
         '-bsf:v', 'hevc_mp4toannexb', '-f', 'image2', str(directory / 'au-%04d.hevc')])
    counts = []
    for unit in sorted(directory.iterdir()):
        data, count, i = unit.read_bytes(), 0, 0
        while True:
            i = data.find(b'\x00\x00\x01', i)
            if i < 0:
                break
            count += ((data[i + 3] >> 1) & 0x3f) == 62
            i += 3
        counts.append(count)
    return counts


def blocks(rpu):
    found = {}
    dm = rpu.get('vdr_dm_data') or {}
    for key in ('cmv29_metadata', 'cmv40_metadata'):
        for block in (dm.get(key) or {}).get('ext_metadata_blocks', []):
            for level, body in block.items():
                found.setdefault(level, []).append(body)
    return found


def main():
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    work, window, source = (pathlib.Path(arg) for arg in sys.argv[1:4])
    frames = int(sys.argv[4])
    work.mkdir(exist_ok=False)
    stream = json.loads(run([FFPROBE, '-v', 'error', '-select_streams', 'v:0', '-show_entries',
                             'stream=codec_tag_string:stream_side_data', '-of', 'json',
                             str(window)]))['streams'][0]
    config = next(side for side in stream.get('side_data_list', [])
                  if side.get('side_data_type') == 'DOVI configuration record')
    data = window.read_bytes()
    out_rpus = rpus(work, window, 'served')
    out_aus = rpu_nals_per_access_unit(work, window)
    out_pts = packets(window)
    src_rpus = rpus(work, source, 'source')
    src_pts = packets(source)
    # Presentation rank -> decode index, for the first FRAMES presented pictures.
    rank = lambda pts: [index for _, index in sorted((p, i) for i, p in enumerate(pts))][:frames]
    out_order, src_order = rank(out_pts), rank(src_pts)
    mismatches, compared = [], 0
    reported_equal = {level: 0 for level in REPORTED}
    for frame, (o, s) in enumerate(zip(out_order, src_order)):
        out_blocks, src_blocks = blocks(out_rpus[o]), blocks(src_rpus[s])
        for level in LEVELS:
            compared += 1
            if out_blocks.get(level) != src_blocks.get(level):
                mismatches.append({'frame': frame, 'level': level,
                                   'served': out_blocks.get(level), 'source': src_blocks.get(level)})
        for level in REPORTED:
            reported_equal[level] += out_blocks.get(level) == src_blocks.get(level)
    present = {level: sum(level in blocks(out_rpus[o]) for o in out_order) for level in LEVELS}
    result = {
        'configuration': {key: config.get(key) for key in (
            'dv_profile', 'dv_level', 'rpu_present_flag', 'el_present_flag', 'bl_present_flag',
            'dv_bl_signal_compatibility_id')},
        'sample_entry': stream.get('codec_tag_string'),
        'dvvC_box': b'dvvC' in data, 'dvcC_box': b'dvcC' in data,
        'access_units': len(out_pts), 'annexb_access_units': len(out_aus),
        'rpu_nals_per_access_unit': sorted(set(out_aus)), 'extracted_rpus': len(out_rpus),
        'served_rpu_profile': sorted({rpu['dovi_profile'] for rpu in out_rpus}),
        'source_rpu_profile': sorted({rpu['dovi_profile'] for rpu in src_rpus}),
        'served_el_type': sorted({str(rpu.get('el_type')) for rpu in out_rpus}),
        'levels_present_in_served_frames': present,
        'l1_l2_l8_compared': compared, 'l1_l2_l8_mismatches': mismatches[:8],
        'l1_l2_l8_mismatch_count': len(mismatches),
        'other_levels_equal_frames': reported_equal,
        'frames': frames,
    }
    ok = (config.get('dv_profile') == 8 and config.get('dv_bl_signal_compatibility_id') == 1
          and config.get('el_present_flag') == 0 and result['dvvC_box'] and not result['dvcC_box']
          and len(out_pts) == frames == len(out_rpus) and set(out_aus) == {1}
          and len(out_aus) == frames and not mismatches and all(present[l] == frames for l in ('Level1',)))
    result['ok'] = ok
    print(json.dumps(result, indent=1))
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    main()
