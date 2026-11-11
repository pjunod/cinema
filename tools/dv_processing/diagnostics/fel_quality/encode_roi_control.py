"""Offline quantOffsets (ROI side data) control around a retained CLI encode.

The retained baseline recipe's ffmpeg argv is translated, token by token, into
the in-process harness (roi_encode.c): encoder options go to avcodec_open2 as
one dictionary, exactly as the CLI passes them. Unknown argv tokens refuse.

Modes:
  none / zero  equivalence gates E0/E1: the HEVC elementary stream must be
               byte-identical to the baseline's. A mismatch is an invalid
               harness, not a quality result.
  luma         the predeclared source-derived map (see the protocol file).

Writes <prefix>-hdr.mp4, -recipe.json, -encode.log, -map.json into
<output-root>/corrected-encodes, creating each exclusively. No playback,
packaging or deployment change.
"""
from pathlib import Path
import argparse, hashlib, json, subprocess, time

FFMPEG = '/opt/homebrew/bin/ffmpeg'
VF = ('setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc,'
      'format=yuv420p10le,settb=expr=1/24000,setpts=N*1001')
DICT = {'-preset': 'preset', '-profile:v': 'profile', '-x265-params': 'x265-params', '-b:v': 'b', '-maxrate': 'maxrate',
        '-bufsize': 'bufsize', '-bf': 'bf', '-flags': 'flags', '-g': 'g', '-keyint_min': 'keyint_min',
        '-sc_threshold': 'sc_threshold', '-color_primaries': 'color_primaries', '-color_trc': 'color_trc',
        '-colorspace': 'colorspace', '-color_range': 'color_range', '-threads:v': 'threads'}
FIXED = {'-nostdin': None, '-hide_banner': None, '-loglevel': 'verbose', '-filter_threads': '1', '-threads': '1',
         '-map': '0:v:0', '-an': None, '-sn': None, '-vf': VF, '-c:v': 'libx265', '-fps_mode': 'passthrough',
         '-force_key_frames': 'expr:eq(mod(n,48),0)', '-f': 'mp4'}


def sha(p):
    with Path(p).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def translate(argv):
    a = list(argv)
    assert a[:4] == ['nice', '-n', '15', FFMPEG], a[:4]
    a = a[4:]
    out = a.pop()  # output path
    opts, pos, i = {}, {}, 0
    while i < len(a):
        t = a[i]
        if t in FIXED:
            if FIXED[t] is None:
                i += 1
            else:
                assert a[i + 1] == FIXED[t], (t, a[i + 1]); i += 2
        elif t in DICT:
            opts[DICT[t]] = a[i + 1]; i += 2
        elif t == '-r':
            num, den = a[i + 1].split('/'); pos['rate'] = (int(num), int(den)); i += 2
        elif t == '-i':
            pos['input'] = a[i + 1]; i += 2
        elif t == '-enc_time_base':
            assert a[i + 1].startswith('1:'); pos['tb_den'] = int(a[i + 1][2:]); i += 2
        elif t == '-frames:v':
            pos['frames'] = int(a[i + 1]); i += 2
        elif t == '-movflags':
            pos['movflags'] = a[i + 1]; i += 2
        elif t == '-tag:v':
            pos['tag'] = a[i + 1]; i += 2
        else:
            raise SystemExit(f'refused: untranslated baseline argv token {t!r}')
    assert set(pos) == {'rate', 'input', 'tb_den', 'frames', 'movflags', 'tag'}, pos
    return opts, pos, out


def annexb(mp4, dst):
    with dst.open('xb') as f:
        subprocess.run([FFMPEG, '-nostdin', '-hide_banner', '-loglevel', 'error', '-i', str(mp4), '-map', '0:v:0', '-c', 'copy',
                        '-bsf:v', 'hevc_mp4toannexb', '-f', 'hevc', '-'], stdout=f, check=True)
    return sha(dst)


p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--baseline-recipe', required=True, type=Path)
p.add_argument('--baseline-input-root', required=True, type=Path, help='where the baseline input now lives (relocation)')
p.add_argument('--baseline-mp4', required=True, type=Path)
p.add_argument('--harness', required=True, type=Path)
p.add_argument('--output-root', required=True, type=Path)
p.add_argument('--prefix', required=True)
p.add_argument('--mode', choices=['none', 'zero', 'luma', 'lumazm'], required=True)
p.add_argument('--grid', type=int, default=16)
p.add_argument('--threshold-nits', type=float, default=0.0)
p.add_argument('--qp-offset', type=int, default=0)
p.add_argument('--protocol', type=Path)
a = p.parse_args()

base = json.loads(a.baseline_recipe.read_text())
assert base['status'] == 'encoded' and sha(a.baseline_mp4) == base['sha256']
opts, pos, _ = translate(base['argv'])
src = Path(pos['input'])
if not src.exists():
    src = a.baseline_input_root / src.name
assert sha(src) == base['source_sha256'], 'source hash mismatch'
assert pos['rate'] == (24000, 1001) and pos['tb_den'] == 24000 and pos['frames'] == 48
if a.mode in ('luma', 'lumazm'):
    assert a.protocol and a.threshold_nits > 0 and a.qp_offset != 0, 'luma map needs a committed protocol and nonzero settings'
grid = 8 if ':qg-size=8' in opts['x265-params'] else 16
assert a.grid == grid, f'grid must follow libx265.c ({grid} for this qg-size)'

out = (a.output_root / 'corrected-encodes').resolve(); out.mkdir(parents=True, exist_ok=True)
stem = out / a.prefix
dst = Path(f'{stem}-hdr.mp4'); mapj = Path(f'{stem}-map.json'); log = Path(f'{stem}-encode.log')
for f in [dst, mapj, log, Path(f'{stem}-recipe.json')]:
    assert not f.exists(), f'refused: {f} exists'
argv = [str(a.harness), str(src), str(dst), str(mapj), a.mode, str(a.grid), repr(a.threshold_nits), str(a.qp_offset),
        str(pos['frames']), '24000', '1001', str(pos['tb_den']), pos['movflags'], pos['tag']] + [f'{k}={v}' for k, v in opts.items()]
t = time.monotonic()
with log.open('xb') as f:
    r = subprocess.run(['nice', '-n', '15', *argv], stdout=f, stderr=subprocess.STDOUT, timeout=1800)
record = {'variant': 'hdr-off-chroma9', 'fixture': a.prefix.split('-')[0], 'mode': a.mode, 'grid': a.grid,
          'threshold_nits': a.threshold_nits, 'qp_offset': a.qp_offset, 'qg_size': 8 if grid == 8 else 32,
          'cadence': '24000/1001', 'source': str(src), 'source_sha256': base['source_sha256'],
          'baseline_recipe': {'path': str(a.baseline_recipe), 'sha256': sha(a.baseline_recipe)},
          'baseline_mp4_sha256': base['sha256'], 'harness': {'path': str(a.harness), 'sha256': sha(a.harness)},
          'driver': {'path': str(Path(__file__).resolve()), 'sha256': sha(Path(__file__).resolve())},
          'protocol': {'path': str(a.protocol), 'sha256': sha(a.protocol)} if a.protocol else None,
          'encoder_dictionary': opts, 'argv': argv, 'exit_code': r.returncode, 'elapsed_seconds': time.monotonic() - t,
          'scope': 'offline encoder control; no playback/deployment/packaging change'}
import re
cli_unused = sorted(set(re.findall(r'Codec AVOption (\S+) \(', Path(str(a.baseline_recipe).replace('-recipe.json', '-encode.log')).read_text())))
harness_unused = sorted(set(re.findall(r'roi_encode: unused option ([^=\s]+)=', log.read_text())))
record['unused_encoder_options'] = {'cli_baseline': cli_unused, 'harness': harness_unused, 'equal': cli_unused == harness_unused}
record['status'] = 'encoded' if r.returncode == 0 and cli_unused == harness_unused else 'encode_failed'
if record['status'] == 'encoded':
    record.update(bytes=dst.stat().st_size, sha256=sha(dst), average_mbps=dst.stat().st_size * 8 / (48 * 1001 / 24000) / 1e6,
                  map=json.loads(mapj.read_text()), map_sha256=sha(mapj))
    eb = annexb(dst, Path(f'{stem}-annexb.hevc'))
    bb_path = Path(f'{stem}-baseline-annexb.hevc'); bb = annexb(a.baseline_mp4, bb_path)
    record['elementary_stream'] = {'candidate_sha256': eb, 'baseline_sha256': bb, 'identical_to_baseline': eb == bb}
    if a.mode in ('none', 'zero'):
        record['equivalence_gate'] = 'pass' if eb == bb else 'FAIL'
    Path(f'{stem}-annexb.hevc').unlink(); bb_path.unlink()  # hashes retained; bytes reproducible from the mp4s
with Path(f'{stem}-recipe.json').open('x') as f:
    f.write(json.dumps(record, indent=2) + '\n')
print(json.dumps({k: record.get(k) for k in ['status', 'mode', 'sha256', 'average_mbps', 'elementary_stream', 'equivalence_gate']}))
raise SystemExit(0 if record['status'] == 'encoded' else 1)
