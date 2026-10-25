"""Check the GPU yuv420p10 export against a CPU BT.2020 NCL conversion.

The helper's `pq-bilinear-yuv420p10-v1` export converts the reconstructed
BT.2020 PQ R'G'B' picture to limited-range 10-bit Y'CbCr 4:2:0 on the GPU.
This compares it with the same window exported as `pq-bilinear-rgb48-v1` and
converted on the CPU with the BT.2020 non-constant-luminance matrix:

- luma within one 10-bit code at every pixel;
- chroma within one code plus the local spread of the CPU chroma over the
  sample's 4x4 neighbourhood, so the comparison does not depend on the exact
  subsampling kernel but still catches a wrong matrix, swapped Cb/Cr, a wrong
  range or a bit-shift error;
- every code inside the limited range.

usage: compare_yuv420p10_export.py RGB48.nut YUV420P10.nut WIDTH HEIGHT [FRAMES]
Prints one JSON line with the largest differences; exits 1 on a failure.
"""
from array import array
import json
import subprocess
import sys

KR, KB = 0.2627, 0.0593
KG = 1 - KR - KB


def raw(path, pixel_format, frames):
    data = subprocess.check_output(
        ['ffmpeg', '-nostdin', '-v', 'error', '-threads', '1', '-i', str(path),
         '-map', '0:v:0', '-frames:v', str(frames), '-fps_mode', 'passthrough',
         '-pix_fmt', pixel_format, '-f', 'rawvideo', 'pipe:1'], timeout=300)
    values = array('H', data)
    if sys.byteorder != 'little':
        values.byteswap()
    return values


def reference(rgb, width, height, frame):
    """CPU Y'CbCr at full resolution, unquantized chroma, quantized luma."""
    base = frame * width * height * 3
    luma, blue, red = [], [], []
    for pixel in range(width * height):
        r, g, b = (rgb[base + 3 * pixel + c] / 65535 for c in range(3))
        y = KR * r + KG * g + KB * b
        luma.append(round(64 + 876 * y))
        blue.append(512 + 896 * (b - y) / (2 * (1 - KB)))
        red.append(512 + 896 * (r - y) / (2 * (1 - KR)))
    return luma, blue, red


def compare(rgb_path, yuv_path, width, height, frames):
    rgb = raw(rgb_path, 'rgb48le', frames)
    yuv = raw(yuv_path, 'yuv420p10le', frames)
    plane, chroma = width * height, (width // 2) * (height // 2)
    if len(rgb) != frames * plane * 3 or len(yuv) != frames * (plane + 2 * chroma):
        raise SystemExit(f'short decode: {len(rgb)} rgb, {len(yuv)} yuv samples')
    worst = {'luma': 0, 'cb': 0, 'cr': 0}
    for frame in range(frames):
        luma, blue, red = reference(rgb, width, height, frame)
        base = frame * (plane + 2 * chroma)
        for index in range(plane):
            code = yuv[base + index]
            if not 64 <= code <= 940:
                raise SystemExit(f'luma code {code} outside limited range')
            worst['luma'] = max(worst['luma'], abs(code - luma[index]))
        for name, cpu, offset in (('cb', blue, plane), ('cr', red, plane + chroma)):
            for cy in range(height // 2):
                for cx in range(width // 2):
                    code = yuv[base + offset + cy * (width // 2) + cx]
                    if not 64 <= code <= 960:
                        raise SystemExit(f'{name} code {code} outside limited range')
                    window = [cpu[y * width + x]
                              for y in range(max(0, 2 * cy - 1), min(height, 2 * cy + 3))
                              for x in range(max(0, 2 * cx - 1), min(width, 2 * cx + 3))]
                    low, high = min(window), max(window)
                    miss = max(0.0, low - code, code - high)
                    worst[name] = max(worst[name], miss)
    result = {'frames': frames, 'raster': [width, height],
              'max_luma_code_difference': worst['luma'],
              'max_cb_codes_outside_neighbourhood': round(worst['cb'], 3),
              'max_cr_codes_outside_neighbourhood': round(worst['cr'], 3)}
    print(json.dumps(result))
    if worst['luma'] > 1 or worst['cb'] > 1 or worst['cr'] > 1:
        raise SystemExit(1)
    return result


if __name__ == '__main__':
    if len(sys.argv) not in (5, 6):
        raise SystemExit(__doc__)
    compare(sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]),
            int(sys.argv[5]) if len(sys.argv) == 6 else 3)
