"""Qualify a finished qg-size spatial-control encode before any quality scoring.

Derived from the private provisional qualifier executed for earlier controls
(archive tooling/qualify_spatial_control.py). Changes, and nothing else:

* the task root is this file's grandparent (a relocated APFS clone of the
  archive) instead of the wiped /private/tmp working root;
* the requested qg-size is an argument rather than a constant;
* two receipts the provisional tool lacked are now required before scoring:
  - the exact x265 ``AQ: mode / str / qg-size / cu-tree`` log tuple;
  - the effective QP-group size derived from the coded SPS/PPS syntax of both
    the encoded and the authored stream:
    Log2MinCuQpDeltaSize = CtbLog2SizeY - diff_cu_qp_delta_depth (H.265 7.4.3.3).

A failed assertion is an invalid test, not a negative quality result.
No live playback, packaging or deployment change.
"""
from pathlib import Path
import argparse, hashlib, json, re, subprocess, sys

ROOT = Path(__file__).resolve().parents[1]
OLD = Path('/Users/pjunod/.local/state/plurx-fel-quality/20261026-measurement-calibration')
FFMPEG = '/opt/homebrew/bin/ffmpeg'
FFPROBE = '/opt/homebrew/bin/ffprobe'
VARIANTS = ['hdr-off', 'psyoff', 'no-sao', 'chroma-fine', 'hdr-off-chroma6', 'hdr-off-chroma9']

ap = argparse.ArgumentParser()
ap.add_argument('fixture', choices=['michael1800', 'michael5400'])
ap.add_argument('variant', choices=VARIANTS)
ap.add_argument('prefix')
ap.add_argument('--qg-size', type=int, required=True)
ap.add_argument('--aq-strength', default='1.0')
ap.add_argument('--rpu-list', type=Path, help='derived ordered RPU list (same order and hashes as the sealed list, relocated paths)')
a = ap.parse_args()
fixture, variant, prefix = a.fixture, a.variant, a.prefix
assert prefix.startswith(fixture + '-')
base = ROOT / 'corrected-encodes' / prefix


def sha(p):
    with Path(p).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def trace(src, dst):
    cmd = [FFMPEG, '-nostdin', '-hide_banner', '-loglevel', 'verbose', '-i', str(src), '-map', '0:v:0',
           '-c:v', 'copy', '-bsf:v', 'trace_headers', '-f', 'null', '-']
    with dst.open('xb') as f:
        subprocess.run(cmd, stdout=f, stderr=subprocess.STDOUT, check=True)
    return cmd, dst.read_text(errors='replace').splitlines()


def values(lines, key):
    return [int(x.rsplit('=', 1)[1]) for x in lines if re.search(r'\b' + key + r'\b', x) and '=' in x]


def qp_group(lines, label):
    keys = ['log2_min_luma_coding_block_size_minus3', 'log2_diff_max_min_luma_coding_block_size',
            'cu_qp_delta_enabled_flag', 'diff_cu_qp_delta_depth']
    got = {}
    for k in keys:
        v = values(lines, k)
        assert v and len(set(v)) == 1, (label, k, v)
        got[k] = {'value': v[0], 'occurrences': len(v)}
    assert got['cu_qp_delta_enabled_flag']['value'] == 1, (label, 'cu_qp_delta disabled')
    ctb_log2 = got['log2_min_luma_coding_block_size_minus3']['value'] + 3 + got['log2_diff_max_min_luma_coding_block_size']['value']
    qg_log2 = ctb_log2 - got['diff_cu_qp_delta_depth']['value']
    return {'syntax': got, 'CtbLog2SizeY': ctb_log2, 'CtbSizeY': 1 << ctb_log2,
            'Log2MinCuQpDeltaSize': qg_log2, 'effective_qp_group_size': 1 << qg_log2,
            'formula': 'Log2MinCuQpDeltaSize = CtbLog2SizeY - diff_cu_qp_delta_depth (H.265 7.4.3.3)'}


recipe = json.loads(Path(str(base) + '-recipe.json').read_text())
assert recipe['status'] == 'encoded' and recipe['qg_size'] == a.qg_size and recipe['variant'] == variant
input_mp4 = Path(str(base) + '-hdr.mp4'); output = Path(str(base) + '-rust-p81.mp4')
assert sha(input_mp4) == recipe['sha256']
log = Path(str(base) + '-encode.log').read_text()
assert '48 frames decoded' in log and '48 frames encoded' in log and 'aq-mode=2' in ' '.join(recipe['argv'])
assert f':qg-size={a.qg_size}' in ' '.join(recipe['argv'])

# Receipt 1: exact AQ tuple reported by the encoder itself.
aq = re.findall(r'AQ: mode / str / qg-size / cu-tree\s*:\s*(\d+) / ([0-9.]+) / (\d+) / (\d+)', log)
assert len(aq) == 1, ('AQ log line count', aq)
aq_tuple = {'aq_mode': int(aq[0][0]), 'aq_strength': aq[0][1], 'qg_size': int(aq[0][2]), 'cu_tree': int(aq[0][3])}
assert aq_tuple == {'aq_mode': 2, 'aq_strength': a.aq_strength, 'qg_size': a.qg_size, 'cu_tree': 1}, aq_tuple

name = 'michael-' + fixture.removeprefix('michael'); sealed_rpulist = OLD / 'calibrated-quality' / name / 'ordered-source-rpus.json'
rpulist = sealed_rpulist
if a.rpu_list:
    # The sealed list names wiped /private/tmp paths; a derived list must keep order and hashes exactly.
    rpulist = a.rpu_list.resolve(); assert rpulist.is_relative_to(ROOT)
    sealed = json.loads(sealed_rpulist.read_text()); derived = json.loads(rpulist.read_text())
    assert [x['sha256'] for x in sealed] == [x['sha256'] for x in derived] and len(derived) == 48
    assert all(sha(x['path']) == x['sha256'] for x in derived)
req = Path(str(base) + '-author-request.json'); prep = Path(str(base) + '-prepare-result.json'); res = Path(str(base) + '-author-result.json')
args = [sys.executable, str(OLD / 'author/prepare_request.py'), '--input-mp4', str(input_mp4), '--expected-input-sha256', recipe['sha256'],
        '--output-mp4', str(output), '--rpu-list', str(rpulist), '--source-level', '6', '--request-json', str(req), '--ffprobe', FFPROBE,
        '--timescale', '24000', '--start', '0', '--frame-ticks', '1001', '--raster', '3840', '2160']
with prep.open('x') as f:
    subprocess.run(args, stdout=f, stderr=subprocess.STDOUT, check=True)
with res.open('x') as f:
    subprocess.run([str(OLD / 'author/fel-rust-author-request'), str(req)], stdout=f, stderr=subprocess.STDOUT, check=True)
payload = Path(str(base) + '-picture-payload.json')
subprocess.run([sys.executable, str(OLD / 'tooling/audit_authored_picture_payload.py'), str(input_mp4), str(output), str(payload)], check=True)
audit = json.loads(payload.read_text()); events = json.loads((OLD / 'calibrated-quality' / name / 'F/events.json').read_text())
accepted = [x for x in events if x['kind'] == 'accepted_source_base']
assert len(accepted) == 48 and [x['frame'] for x in accepted] == list(range(48))
assert [x['RPU'][0] for x in audit['output']['frames']] == [x['rpu_sha256'] for x in accepted]

raw = Path(str(base) + '-native-frames0-24.yuv420p10le'); assert not raw.exists()
argv = ['nice', '-n', '15', FFMPEG, '-nostdin', '-hide_banner', '-loglevel', 'verbose', '-threads', '1', '-i', str(output), '-map', '0:v:0',
        '-vf', "select=eq(n\\,0)+eq(n\\,24)", '-frames:v', '2', '-fps_mode', 'passthrough', '-c:v', 'rawvideo', '-pix_fmt', 'yuv420p10le',
        '-threads:v', '1', '-f', 'rawvideo', str(raw)]
with Path(str(base) + '-native-decode.log').open('xb') as f:
    subprocess.run(argv, stdout=f, stderr=subprocess.STDOUT, check=True)
assert 'auto_scale' not in Path(str(base) + '-native-decode.log').read_text()
b = raw.read_bytes(); framebytes = 3840 * 2160 * 3; assert len(b) == 2 * framebytes

trace_cmd, tl = trace(input_mp4, Path(str(base) + '-headers.log'))
colour_keys = ['general_level_idc', 'general_tier_flag', 'chroma_sample_loc_type_top_field', 'chroma_sample_loc_type_bottom_field',
               'colour_primaries', 'transfer_characteristics', 'matrix_coefficients', 'video_full_range_flag']
lines = [x for x in tl if any(y in x for y in colour_keys)]
expected = {'general_level_idc': 153, 'general_tier_flag': 1, 'chroma_sample_loc_type_top_field': 2, 'chroma_sample_loc_type_bottom_field': 2,
            'colour_primaries': 9, 'transfer_characteristics': 16, 'matrix_coefficients': 9, 'video_full_range_flag': 0}
for key, val in expected.items():
    vals = values(lines, key); assert vals and all(x == val for x in vals), (key, vals)

# Receipt 2: effective QP-group size from actual coded syntax, encoded and authored.
_, tl_out = trace(output, Path(str(base) + '-authored-headers.log'))
qg_encoded = qp_group(tl, 'encoded'); qg_authored = qp_group(tl_out, 'authored')
assert qg_encoded['effective_qp_group_size'] == qg_authored['effective_qp_group_size'] == a.qg_size, (qg_encoded, qg_authored)

probe = json.loads(subprocess.check_output([FFPROBE, '-v', 'error', '-select_streams', 'v:0', '-show_packets', '-show_entries', 'packet=size', '-of', 'json', str(output)]))
assert len(probe['packets']) == 48
data = input_mp4.read_bytes(); actual = {}
for key in ['psy-rd', 'psy-rdoq', 'aq-mode', 'rdoq-level', 'qg-size']:
    vals = [float(x) for x in re.findall((rb'(?<![-\w])' + key.encode() + rb'=([0-9.]+)'), data)]
    assert vals, (key, 'missing encoder info'); assert len(set(vals)) == 1, (key, vals); actual[key] = vals[0]
assert bool(re.search(rb'\shdr10-opt\s', data)) != bool(re.search(rb'\sno-hdr10-opt\s', data))
assert bool(re.search(rb'\ssao\s', data)) != bool(re.search(rb'\sno-sao\s', data))
actual['hdr10-opt'] = bool(re.search(rb'\shdr10-opt\s', data)); actual['sao'] = bool(re.search(rb'\ssao\s', data)); actual['hdr10'] = bool(re.search(rb'\shdr10\s', data))
assert actual['hdr10'] and actual['aq-mode'] == 2
assert actual['hdr10-opt'] == (variant not in ['hdr-off', 'hdr-off-chroma6', 'hdr-off-chroma9'])
assert actual['sao'] == (variant != 'no-sao')
for key in ['cbqpoffs', 'crqpoffs']:
    vals = [int(x) for x in re.findall((rb'(?<![-\w])' + key.encode() + rb'=(-?[0-9]+)'), data)]
    assert vals and len(set(vals)) == 1; actual[key] = vals[0]
    assert actual[key] == ({'chroma-fine': -6, 'hdr-off-chroma6': -6, 'hdr-off-chroma9': -9}.get(variant, 0))
assert actual['psy-rd'] == (0 if variant == 'psyoff' else 2) and actual['psy-rdoq'] == 1 and actual['rdoq-level'] == 2 and actual['qg-size'] == a.qg_size

out = {'scope': 'Mac software decoder raw accepted ordinals0/24, actual frozen Rust author, no worker/native-device/display or full-window renderer claim.',
       'fixture': fixture, 'variant': variant, 'prefix': prefix, 'input_sha256': sha(input_mp4), 'authored_mp4': str(output), 'authored_sha256': sha(output),
       'authored_bytes': output.stat().st_size,
       'authored_video_packet_mbps': sum(int(x['size']) for x in probe['packets']) * 8 / (48 * 1001 / 24000) / 1e6,
       'author_request_sha256': sha(req), 'payload_audit_sha256': sha(payload), 'all48_RPUs_equal_actual_F': True, 'raw_file': str(raw), 'raw_sha256': sha(raw),
       'raw_bytes': len(b),
       'native_frames': [{'source_ordinal': i, 'pts': f'{i*1001}/24000', 'duration': '1001/24000', 'bytes': framebytes,
                          'sha256': hashlib.sha256(b[j * framebytes:(j + 1) * framebytes]).hexdigest()} for j, i in enumerate([0, 24])],
       'decoder_command': argv, 'no_auto_scaler_in_verbose_log': True, 'encoder_info_effective_settings': actual, 'header_checks': expected,
       'header_lines': lines, 'encoder_input_frames': 48, 'encoder_output_frames': 48,
       'aq_log_tuple_receipt': {'encode_log_sha256': sha(Path(str(base) + '-encode.log')), 'tuple': aq_tuple},
       'effective_qp_group_receipt': {'encoded': qg_encoded, 'authored': qg_authored, 'trace_command_template': trace_cmd[:-1],
                                      'encoded_trace_sha256': sha(Path(str(base) + '-headers.log')),
                                      'authored_trace_sha256': sha(Path(str(base) + '-authored-headers.log'))},
       'rpu_list': {'sealed': {'path': str(sealed_rpulist), 'sha256': sha(sealed_rpulist)}, 'used': {'path': str(rpulist), 'sha256': sha(rpulist)},
                    'derived_paths_only': rpulist != sealed_rpulist},
       'qualifier': {'path': str(Path(__file__).resolve()), 'sha256': sha(Path(__file__).resolve())}}
out['coarse_DV6_window_average_rate_check'] = out['authored_video_packet_mbps'] <= 130
out['rate_check_limit'] = 'Conservative130Mbps window-average guard only; NOTfullDV/HEVCHRD/CPBconformance'
with Path(str(base) + '-qualification.json').open('x') as f:
    f.write(json.dumps(out, indent=2) + '\n')
print(json.dumps({k: out[k] for k in ['fixture', 'authored_sha256', 'authored_bytes', 'authored_video_packet_mbps', 'raw_sha256']}
                 | {'aq': aq_tuple, 'qg_encoded': qg_encoded['effective_qp_group_size'], 'qg_authored': qg_authored['effective_qp_group_size']}))
