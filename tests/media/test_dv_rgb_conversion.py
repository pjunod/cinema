"""Execute the shared HDR measurement inverse against scalar H.273 patches."""
from array import array
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
FILTER = ROOT / 'tools/dv_quality/bt2020_pq_rgb48.filter'


def constant_patch(rgb, bits, subsampled, width=8, height=8):
    """Independently author legal BT.2020 NCL codes, then invert those codes.

    Constant patches eliminate chroma reconstruction as a confound. The
    expected result includes the authored YUV quantization, not the initial
    floating RGB value, so the allowed two-code error is conversion error.
    """
    red, green, blue = rgb
    y = .2627 * red + .6780 * green + .0593 * blue
    factor = 1 << (bits - 10)
    y0, c0 = 64 * factor, 512 * factor
    ys, cs = 876 * factor, 896 * factor
    codes = (math.floor(y0 + ys * y + .5),
             math.floor(c0 + cs * (blue - y) / 1.8814 + .5),
             math.floor(c0 + cs * (red - y) / 1.4746 + .5))
    y, cb, cr = ((codes[0] - y0) / ys, (codes[1] - c0) / cs,
                 (codes[2] - c0) / cs)
    red, blue = y + 1.4746 * cr, y + 1.8814 * cb
    green = (y - .2627 * red - .0593 * blue) / .6780
    expected = tuple(round(min(1, max(0, c)) * 65535) for c in (red, green, blue))
    sizes = (width * height,) + ((width * height // 4,) * 2 if subsampled
                                else (width * height,) * 2)
    data = array('H', (code for code, size in zip(codes, sizes) for _ in range(size)))
    if sys.byteorder != 'little':
        data.byteswap()
    return data.tobytes(), expected


class DvRgbConversionTests(unittest.TestCase):
    def test_absolute_pq_rgb_inverse_matches_independent_h273_patches(self):
        ffmpeg = os.environ.get('PLURX_FFMPEG')
        self.assertTrue(ffmpeg, 'Select the supported executable explicitly with PLURX_FFMPEG')
        colours = [(0, 0, 0), (1, 1, 1), (.02, .02, .02), (.2, .2, .2),
                   (.5, .5, .5), (.7, .4, .3), (.4, .7, .3), (.4, .3, .7),
                   (.8, .6, .4), (.6, .4, .8)]
        filter_graph = FILTER.read_text().strip()
        base = [ffmpeg, '-hide_banner', '-nostdin', '-v', 'error',
                '-filter_threads', '1', '-threads', '1']
        with tempfile.TemporaryDirectory() as directory:
            for bits in (10, 16):
                for subsampled in (False, True):
                    pixel_format = f'yuv{420 if subsampled else 444}p{bits}le'
                    with self.subTest(pixel_format=pixel_format):
                        patches = [constant_patch(rgb, bits, subsampled) for rgb in colours]
                        movie = Path(directory) / f'{pixel_format}.nut'
                        authored = subprocess.run(base + [
                            '-f', 'rawvideo', '-pixel_format', pixel_format, '-video_size', '8x8',
                            '-framerate', '24', '-color_range', 'tv', '-color_primaries', 'bt2020',
                            '-color_trc', 'smpte2084', '-colorspace', 'bt2020nc',
                            '-i', 'pipe:0', '-c:v', 'rawvideo',
                            '-color_range', 'tv', '-color_primaries', 'bt2020',
                            '-color_trc', 'smpte2084', '-colorspace', 'bt2020nc',
                            '-chroma_sample_location', 'left', '-f', 'nut', str(movie)],
                            input=b''.join(data for data, _ in patches), capture_output=True, timeout=30)
                        self.assertEqual(0, authored.returncode, authored.stderr.decode())
                        raw = subprocess.run(base + [
                            '-i', str(movie), '-map', '0:v:0', '-c:v', 'rawvideo',
                            '-pix_fmt', pixel_format, '-f', 'rawvideo', 'pipe:1'],
                            capture_output=True, timeout=30)
                        self.assertEqual(0, raw.returncode, raw.stderr.decode())
                        self.assertEqual(b''.join(data for data, _ in patches), raw.stdout,
                                         'fixture authoring must not change the scalar YUV codes')
                        decoded = subprocess.run(base + [
                            '-i', str(movie), '-map', '0:v:0', '-frames:v', str(len(colours)),
                            '-fps_mode', 'passthrough', '-vf', filter_graph,
                            '-f', 'rawvideo', '-pix_fmt', 'rgb48le', 'pipe:1'],
                            capture_output=True, timeout=30)
                        self.assertEqual(0, decoded.returncode, decoded.stderr.decode())
                        actual = array('H', decoded.stdout)
                        if sys.byteorder != 'little':
                            actual.byteswap()
                        expected = [code for _, pixel in patches for _ in range(64) for code in pixel]
                        self.assertEqual(len(expected), len(actual), 'short or extra decoded frames')
                        worst = max(abs(a - b) for a, b in zip(actual, expected))
                        self.assertLessEqual(worst, 2, f'{pixel_format}: {worst} RGB16 codes')



if __name__ == '__main__':
    unittest.main()
