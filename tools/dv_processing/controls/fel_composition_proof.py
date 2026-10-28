"""Prove that the reconstructed Profile 8.1 base layer is the FEL-composed picture.

One served window of a Profile 7 FEL source, as the daemon authored it
(`PLURX_DV_WINDOW_DUMP` from the ignored physical test
`actual_processed_hdr_windows_verify_audio_cadence_native_shapes_and_seek`
with `PLURX_DV_WINDOW_DESTINATION=profile81`), is compared with two
alternatives over the same source frames:

- reference: the helper's own FEL master render (`fel`,
  `pq-bilinear-rgb48-v1`, native raster) - the composed picture before any
  encode;
- EL dropped: the plain P7 -> P8.1 conversion. The window's elementary stream
  is converted with `dovi_tool -m 2` (libdovi `To81`, the conversion plurx's
  `dvconvert.rs` applies), remuxed with `mkvmerge`, and rendered by the same
  helper in `base-rpu` mode: the picture a Dolby Vision display reconstructs
  from the converted stream (BL plus the RPU's reshaping, no NLQ residual).
  It is rendered unencoded, so it carries no encode loss the served window has.

Both are compared with the reference in absolute BT.2020 light: PSNR over
every pixel of every frame on PQ-coded R'G'B', and the mean and 99th
percentile Delta E ITP (`tools/dv_quality/metrics.py`, ITU-R BT.2124) on a
fixed pixel lattice. The converted stream's base layer must also be
bit-identical to the source's, so the comparison is frame-aligned.

Two verdicts: `composition_proved` compares the composed and EL-dropped
pictures through the identical pre-encode 4:2:0 export, so composition is
the only difference; `served_window_closer` compares the served (encoded)
window with the EL-dropped picture, so it also measures the delivery encode.

usage: fel_composition_proof.py WORKDIR SOURCE.mkv WINDOW_DIR
  WINDOW_DIR holds window-000.mp4 and window-000.json from the dump.
Environment: PLURX_DV_PROCESSING_DIR (default /usr/lib/plurx/dv-processing),
PLURX_FFMPEG, PLURX_FFPROBE, PLURX_DOVI_TOOL, PLURX_MKVMERGE,
PLURX_DV_DECODE_DEVICE (default /dev/dri/renderD128), PROOF_STRIDE (16).
Prints one JSON document; exits 1 unless both verdicts hold.
"""
from array import array
from fractions import Fraction
import hashlib
import json
import math
import os
import pathlib
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent.parent / 'dv_quality'))
import metrics  # noqa: E402  (BT.2124 ICtCp / Delta E ITP, checked vectors)

BUNDLE = pathlib.Path(os.environ.get('PLURX_DV_PROCESSING_DIR', '/usr/lib/plurx/dv-processing'))
FFMPEG = os.environ.get('PLURX_FFMPEG', 'ffmpeg')
FFPROBE = os.environ.get('PLURX_FFPROBE', 'ffprobe')
DOVI = os.environ.get('PLURX_DOVI_TOOL', 'dovi_tool')
MKVMERGE = os.environ.get('PLURX_MKVMERGE', 'mkvmerge')
DEVICE = os.environ.get('PLURX_DV_DECODE_DEVICE', '/dev/dri/renderD128')
STRIDE = int(os.environ.get('PROOF_STRIDE', '16'))


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, timeout=900, **kwargs)


def helper_env():
    env = dict(os.environ)
    libs = [str(BUNDLE / 'lib')]
    media = pathlib.Path(FFMPEG).parent / 'lib'
    if (media / 'libva.so.2').exists():
        libs.append(str(media))
    env['LD_LIBRARY_PATH'] = ':'.join(libs)
    if pathlib.Path(DEVICE).exists():
        env['PLURX_DV_DECODE_DEVICE'] = DEVICE
    return env


def video_facts(path):
    doc = json.loads(run([FFPROBE, '-v', 'error', '-select_streams', 'v:0', '-show_entries',
                          'stream=index,width,height,time_base,start_pts,r_frame_rate',
                          '-of', 'json', str(path)]).stdout)
    return doc['streams'][0]


def render(work, source, mode, frames, raster, start, end, origin, export='pq-bilinear-rgb48-v1'):
    """Helper window render to a NUT; returns (nut path, decoder report)."""
    out = work / f'render-{mode}-{source.stem}-{export}'
    out.mkdir()
    nut = out / 'picture.nut'
    args = [str(BUNDLE / 'bin' / 'segment_decode_render'), str(source), str(out), '0', str(frames),
            str(raster[0]), str(raster[1]), '0', '0', '0', 'bt2020-pq-master-clip', str(nut),
            f'{start.numerator}/{start.denominator}', f'{end.numerator}/{end.denominator}', '512',
            mode, str(raster[0]), str(raster[1]), export]
    if origin is not None:
        args += ['--held-source-origin', f'{origin.numerator}/{origin.denominator}']
    try:
        result = run(args, env=helper_env())
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"renderer refused ({mode}): {error.stderr.decode(errors='replace')[-2000:]}")
    rows = [json.loads(line) for line in result.stdout.decode().splitlines() if line.startswith('{')]
    decoder = next((row for row in rows if row.get('kind') == 'decoder'), None)
    pictures = [row for row in rows if row.get('kind') in ('frame', 'rendered_frame', 'picture')]
    return nut, decoder, len(pictures), rows


def rgb48_filter(matrix):
    if matrix:
        return f'scale=in_color_matrix={matrix}:in_range=tv:out_range=pc,format=rgb48le'
    return 'format=rgb48le'


def rgb48_frames(path, frames, raster, matrix=None):
    """Yield one decoded frame at a time as 16-bit R'G'B' samples."""
    size = raster[0] * raster[1] * 3 * 2
    process = subprocess.Popen(
        [FFMPEG, '-nostdin', '-v', 'error', '-i', str(path), '-map', '0:v:0',
         '-frames:v', str(frames), '-fps_mode', 'passthrough', '-vf', rgb48_filter(matrix),
         '-f', 'rawvideo', 'pipe:1'], stdout=subprocess.PIPE)
    try:
        for _ in range(frames):
            data = process.stdout.read(size)
            if len(data) != size:
                raise SystemExit(f'{path}: short frame ({len(data)} of {size} bytes)')
            values = array('H', data)
            if sys.byteorder != 'little':
                values.byteswap()
            yield values
    finally:
        process.stdout.close()
        process.wait(timeout=60)


def psnr(reference, candidate, frames, matrix=None, region=None):
    """FFmpeg's PSNR over every pixel of every frame, on 16-bit PQ R'G'B'.

    Both inputs are restamped to their frame index first. The psnr filter
    pairs frames by timestamp, and a 23.976 fps window in a millisecond time
    base (41.708 ms stored as 42) is otherwise paired with the previous
    reference frame two times in three.
    """
    index = 'settb=expr=1,setpts=N'
    crop = ''
    if region:
        x0, y0, x1, y1 = region
        crop = f',crop={x1 - x0}:{y1 - y0}:{x0}:{y0}'
    graph = (f'[0:v]{index},format=gbrp16le{crop}[r];[1:v]{index},{rgb48_filter(matrix)},'
             f'format=gbrp16le{crop}[c];[r][c]psnr')
    stderr = run([FFMPEG, '-nostdin', '-hide_banner', '-i', str(reference), '-i', str(candidate),
                  '-frames:v', str(frames), '-lavfi', graph, '-f', 'null', '-']).stderr.decode()
    line = [line for line in stderr.splitlines() if 'PSNR' in line and 'average:' in line][-1]
    value = line.split('average:')[1].split()[0]
    return math.inf if value == 'inf' else float(value)


def active_region(reference, raster, threshold=1024):
    """The picture inside any letterbox/pillarbox, from the reference's first
    frame: rows and columns holding a sample above `threshold` (16-bit PQ,
    about 0.03 nits). Bars carry no picture, and Delta E ITP near 0 nits is
    so steep that a bar at 0.00001 vs 0.004 nits scores 10 on every pixel."""
    width, height = raster
    frame = next(rgb48_frames(reference, 1, raster))
    rows = [y for y in range(height)
            if max(frame[y * width * 3:(y + 1) * width * 3]) > threshold]
    cols = [x for x in range(0, width) if any(
        max(frame[(y * width + x) * 3:(y * width + x) * 3 + 3]) > threshold
        for y in range(rows[0], rows[-1] + 1, 8))]
    return (cols[0] & ~1, rows[0] & ~1, (cols[-1] + 2) & ~1, (rows[-1] + 2) & ~1)


def bl_digest(path, frames):
    data = run([FFMPEG, '-nostdin', '-v', 'error', '-i', str(path), '-map', '0:v:0',
                '-frames:v', str(frames), '-fps_mode', 'passthrough', '-pix_fmt', 'yuv420p10le',
                '-f', 'rawvideo', 'pipe:1']).stdout
    return hashlib.sha256(data).hexdigest(), len(data)


def summary(deltas):
    deltas.sort()
    return round(sum(deltas) / len(deltas), 4), round(deltas[int(0.99 * (len(deltas) - 1))], 4)


def compare(reference, candidate, frames, raster, matrix=None, region=None):
    """Whole frame, and (with `region`) the active picture inside the bars."""
    width, height = raster
    x0, y0, x1, y1 = region or (0, 0, width, height)
    deltas, active = [], []
    pairs = zip(rgb48_frames(reference, frames, raster), rgb48_frames(candidate, frames, raster, matrix))
    for ref_frame, cand_frame in pairs:
        for y in range(STRIDE // 2, height, STRIDE):
            row = y * width * 3
            for x in range(STRIDE // 2, width, STRIDE):
                at = row + 3 * x
                ref = [metrics.pq_eotf(ref_frame[at + c] / 65535) for c in range(3)]
                cand = [metrics.pq_eotf(cand_frame[at + c] / 65535) for c in range(3)]
                delta = metrics.delta_e_itp(metrics.rgb_nits_to_ictcp(ref),
                                            metrics.rgb_nits_to_ictcp(cand))
                deltas.append(delta)
                if x0 <= x < x1 and y0 <= y < y1:
                    active.append(delta)
    mean, p99 = summary(deltas)
    result = {
        'psnr_db': psnr(reference, candidate, frames, matrix),
        'delta_e_itp_mean': mean,
        'delta_e_itp_p99': p99,
        'delta_e_itp_samples': len(deltas),
    }
    if region:
        mean, p99 = summary(active)
        result['active'] = {'psnr_db': psnr(reference, candidate, frames, matrix, region),
                            'delta_e_itp_mean': mean, 'delta_e_itp_p99': p99,
                            'delta_e_itp_samples': len(active)}
    return result


def main():
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    # The helper enters its output directory, so every path is absolute.
    work, source, window = (pathlib.Path(arg).resolve() for arg in sys.argv[1:])
    work.mkdir(exist_ok=False)
    meta = json.loads((window / 'window-000.json').read_text())
    served = window / 'window-000.mp4'
    numerator, denominator = meta['numerator'], meta['denominator']
    frames = meta['duration_ticks'] // denominator
    facts = video_facts(source)
    raster = (facts['width'], facts['height'])
    tick = Fraction(facts['time_base'])
    origin = Fraction(facts.get('start_pts') or 0) * tick
    start = origin + Fraction(meta['start_ticks'], numerator)
    end = start + Fraction(meta['duration_ticks'], numerator)

    reference_nut, reference_decoder, _, _ = render(work, source, 'fel', frames, raster, start, end, origin)

    # The EL-dropped conversion of exactly these source pictures.
    hevc = work / 'window.hevc'
    run([FFMPEG, '-nostdin', '-v', 'error', '-ss', f'{float(start - origin):.6f}', '-i', str(source),
         '-map', '0:v:0', '-frames:v', str(frames + 16), '-c', 'copy',
         '-bsf:v', 'hevc_mp4toannexb', '-f', 'hevc', str(hevc)])
    converted_hevc = work / 'converted.hevc'
    run([DOVI, '-m', '2', 'convert', '--discard', '-i', str(hevc), '-o', str(converted_hevc)])
    rate = Fraction(facts['r_frame_rate'])
    converted = work / 'converted.mkv'
    run([MKVMERGE, '-q', '-o', str(converted), '--default-duration',
         f'0:{rate.numerator}/{rate.denominator}p', str(converted_hevc)])
    conv_facts = video_facts(converted)
    conv_origin = Fraction(conv_facts.get('start_pts') or 0) * Fraction(conv_facts['time_base'])
    window_seconds = Fraction(meta['duration_ticks'], numerator)
    dropped_nut, dropped_decoder, _, _ = render(work, converted, 'base-rpu', frames, raster,
                                                conv_origin, conv_origin + window_seconds,
                                                conv_origin)
    # The same two pictures through production's pre-encode 4:2:0 export, so
    # chroma subsampling and the Y'CbCr -> R'G'B' step are identical for both
    # and the difference left is composition alone.
    yuv = 'pq-bilinear-yuv420p10-v1'
    composed_yuv, _, _, _ = render(work, source, 'fel', frames, raster, start, end, origin, yuv)
    dropped_yuv, _, _, _ = render(work, converted, 'base-rpu', frames, raster, conv_origin,
                                  conv_origin + window_seconds, conv_origin, yuv)

    # Frame alignment: the conversion keeps the base layer bit-identical.
    source_cut = work / 'source-window.mkv'
    run([FFMPEG, '-nostdin', '-v', 'error', '-ss', f'{float(start - origin):.6f}', '-i', str(source),
         '-map', '0:v:0', '-frames:v', str(frames + 16), '-c', 'copy', str(source_cut)])
    bl_source = bl_digest(source_cut, frames)
    bl_converted = bl_digest(converted, frames)

    region = active_region(reference_nut, raster)
    composed = compare(reference_nut, served, frames, raster, 'bt2020', region)
    el_dropped = compare(reference_nut, dropped_nut, frames, raster, None, region)
    composed_420 = compare(reference_nut, composed_yuv, frames, raster, 'bt2020', region)
    el_dropped_420 = compare(reference_nut, dropped_yuv, frames, raster, 'bt2020', region)
    result = {
        'source': {'raster': raster, 'frames': frames, 'start': str(start), 'end': str(end)},
        'served_window': {'bytes': served.stat().st_size, 'renderer_decode': meta.get('renderer_decode'),
                          'decoder': meta.get('decoder'), 'graph_cpu_threads': meta.get('graph_cpu_threads')},
        'reference_decoder': reference_decoder,
        'el_dropped_decoder': dropped_decoder,
        'bl_alignment': {'source_sha256': bl_source[0], 'converted_sha256': bl_converted[0],
                         'identical': bl_source == bl_converted},
        'composed_served_vs_fel_reference': composed,
        'el_dropped_conversion_vs_fel_reference': el_dropped,
        'pre_encode_420': {'composed_vs_fel_reference': composed_420,
                           'el_dropped_vs_fel_reference': el_dropped_420},
        'stride': STRIDE,
        'active_region': region,
    }
    # Composition: the same pictures through the same 4:2:0 path, the only
    # difference being whether the enhancement layer was composed.
    composition = (result['bl_alignment']['identical']
                   and composed_420['psnr_db'] > el_dropped_420['psnr_db']
                   and composed_420['delta_e_itp_mean'] < el_dropped_420['delta_e_itp_mean']
                   and composed_420['delta_e_itp_p99'] < el_dropped_420['delta_e_itp_p99'])
    # Served: after the delivery encode, against the conversion's untouched
    # base layer (which the fallback actually serves).
    served = (result['bl_alignment']['identical']
              and composed['psnr_db'] > el_dropped['psnr_db']
              and composed['delta_e_itp_mean'] < el_dropped['delta_e_itp_mean'])
    result['composition_proved'] = composition
    result['served_window_closer'] = served
    print(json.dumps(result, indent=1))
    sys.exit(0 if composition and served else 1)


if __name__ == '__main__':
    main()
