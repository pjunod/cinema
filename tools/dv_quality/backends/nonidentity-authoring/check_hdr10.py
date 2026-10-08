"""Independent BT2020 NCL integer analytic check for declared synthetic controls."""
from pathlib import Path
import hashlib
import json
import math
import struct
from registered_limits import read_limits, read_json
import inject_rpu
ROOT = Path(__file__).resolve().parent
W = H = 64
FRAMES = 4

def words(path, count):
    data = path.read_bytes()
    if len(data) != count * 2:
        raise ValueError(f'incorrect artifact length: {path.name}')
    return struct.unpack('<' + str(count) + 'H', data)

def verify():
    seed = (ROOT / 'reconstructed16-reference.rgb48le').read_bytes()
    if seed != (ROOT / 'reconstructed16.rgb48le').read_bytes():
        raise ValueError('reconstructed source differs from independent analytic seed')
    expanded = b''.join(seed[((y//4+f*3)%16*16+(x//4+f)%16)*6:
                                 ((y//4+f*3)%16*16+(x//4+f)%16+1)*6]
                        for f in range(FRAMES) for y in range(H) for x in range(W))
    if expanded != (ROOT / 'reconstructed64.rgb48le').read_bytes():
        raise ValueError('source expansion/timing differs from definition')
    rgb = words(ROOT / 'reconstructed64.rgb48le', W * H * FRAMES * 3)
    raw = words(ROOT / 'reconstructed64.yuv420p10le', W * H * FRAMES * 3 // 2)
    decoded = words(ROOT / 'decoded.yuv420p10le', len(raw))
    out = words(ROOT / 'decoded-hdr10.rgb48le', len(rgb))
    baseline = words(ROOT / 'baseline-decoded.yuv420p10le', len(raw))
    if decoded != baseline:
        raise ValueError('RPU insertion altered ordinary HDR10 decode')
    registration = read_limits(ROOT)
    quant_error = 0
    model_error = 0
    expected_rgb = []
    expected_yuv = []
    kr, kg, kb = 0.2627, 0.6780, 0.0593
    for frame in range(FRAMES):
        planar = raw[frame * W * H * 3 // 2:(frame + 1) * W * H * 3 // 2]
        expected_planes = [[0] * (W * H), [0] * (W * H // 4), [0] * (W * H // 4)]
        for y in range(H):
            for x in range(W):
                idx = frame * W * H + y * W + x
                r, g, b = (v / 65535 for v in rgb[idx * 3:idx * 3 + 3])
                yp = kr * r + kg * g + kb * b
                cb, cr = (b - yp) / (2 * (1 - kb)), (r - yp) / (2 * (1 - kr))
                expected_planes[0][y * W + x] = round(64 + 876 * yp)
                ci = (y // 2) * (W // 2) + x // 2
                expected_planes[1][ci] = round(512 + 896 * cb)
                expected_planes[2][ci] = round(512 + 896 * cr)
                yf = (decoded[frame * W * H * 3 // 2 + y * W + x] - 64) / 876
                uf = (decoded[frame * W * H * 3 // 2 + W * H + ci] - 512) / 896
                vf = (decoded[frame * W * H * 3 // 2 + W * H * 5 // 4 + ci] - 512) / 896
                q = (yf + 2 * (1 - kr) * vf,
                     yf - 2 * kb * (1 - kb) / kg * uf - 2 * kr * (1 - kr) / kg * vf,
                     yf + 2 * (1 - kb) * uf)
                reference = [round(max(0, min(1, v)) * 65535) for v in q]
                expected_rgb.extend(reference)
                model_error = max(model_error, *(abs(a-b) for a,b in zip(reference, out[idx*3:idx*3+3])))
        expected_yuv.extend(sum(expected_planes, []))
    quant_error = max(abs(a-b) for a,b in zip(expected_yuv, raw))
    compression_error = max(abs(a-b) for a,b in zip(raw, decoded))
    total_rgb_error = max(abs(a-b) for a,b in zip(rgb, out))
    # Bound: +/-1 code model/rounding allowance in YUV, plus observed QP0
    # compression allowance of two codes, amplified by the worst NCL inverse (blue channel).
    bound = math.ceil(65535 * (1 + registration['encoder_max_yuv_code_error']) * (1 / 876 + 1.8814 / 896)) + 2
    if quant_error > 1 or compression_error > registration['encoder_max_yuv_code_error'] or model_error > registration['forward_inverse_ncl_max_code_error'] or total_rgb_error > bound:
        raise ValueError(f'analytic bound failed: {quant_error=} {model_error=} {total_rgb_error=} {bound=}')
    (ROOT / 'analytic-hdr10-reference.rgb48le').write_bytes(struct.pack('<' + str(len(expected_rgb)) + 'H', *expected_rgb))
    receipt = {'scope': 'analytic-control; standard matrix, no trims',
               'declared_max_compression_yuv_code_error': registration['encoder_max_yuv_code_error'],
               'chroma_sample_location': 'center; explicit point sampling',
               'profile': json.loads((ROOT/'probe.json').read_text())['streams'][0]['profile'],
               'source_to_yuv_max_code_error_vs_bt2020_ncl': quant_error,
               'compression_yuv_max_code_error': compression_error,
               'hdr10_decode_vs_analytic_inverse_ncl_max_rgb_code_error': model_error,
               'hdr10_decode_vs_reconstructed_max_rgb_code_error': total_rgb_error,
               'registered_arithmetic_rgb_bound_codes': bound,
               'bound_selection_timing': 'Registered before CPU reconstruction, encode and GPU render; analytic mechanism control only, no picture quality qualification',
               'bound_derivation': registration['encoded_hdr10_vs_reconstructed_rgb_bound_derivation'],
               'hdr10_decode_unchanged_after_rpu_insertion': True,
               'p81_conformance': None, 'independent_dv_picture_reference': None,
               'dv_enabled_render': None, 'result': 'pass'}
    probe = read_json(ROOT/'probe.json')
    stream = probe['streams'][0]
    expected = {'profile':'Main 10','width':64,'height':64,'pix_fmt':'yuv420p10le',
                'color_range':'tv','color_space':'bt2020nc','color_primaries':'bt2020',
                'color_transfer':'smpte2084','chroma_location':'center'}
    if any(stream.get(k) != v for k,v in expected.items()) or len(probe['frames']) != 4:
        raise ValueError('actual HDR10 tags/dimensions/frame count differ')
    for frame in probe['frames']:
        if any(frame.get(k) != v for k,v in expected.items() if k != 'profile'):
            raise ValueError('actual decoded HDR10 frame tags differ')
    receipt['verified_hdr10_tags'] = expected
    receipt['verified_frame_count'] = 4
    if receipt['profile'] != 'Main 10':
        raise ValueError('actual encoder output not Main10')
    (ROOT/'authoring-check-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt

if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
