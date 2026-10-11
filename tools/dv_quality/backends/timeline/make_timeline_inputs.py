"""Author bounded picture identities and declared timelines, before encoding."""
import hashlib
import json
from pathlib import Path
import struct
import sys


def generate(root):
    root = Path(root)
    frames = []
    for layer in ('bl', 'el'):
        pictures = []
        for frame in range(6):
            if layer == 'bl':
                y = [400 + 4 * frame + 2 * (x // 4) + row // 4
                     for row in range(64) for x in range(64)]
            else:
                y = [512 + (8 + frame) * (1 if (x // 4 + row // 4 + frame) % 2 else -1)
                     for row in range(64) for x in range(64)]
            raw = struct.pack('<6144H', *(y + [512] * 2048))
            pictures.append(raw)
            frames.append({'layer': layer, 'source_picture': frame,
                           'sha256': hashlib.sha256(raw).hexdigest()})
        (root / f'{layer}.yuv420p10le').write_bytes(b''.join(pictures))
        for segment in (0, 1):
            (root / f'{layer}-segment{segment}.yuv420p10le').write_bytes(
                b''.join(pictures[3 * segment:3 * segment + 3]))
    definition = {
        'scope': 'authored synthetic native-picture/timeline control',
        'width': 64, 'height': 64, 'pixel_format': 'yuv420p10le',
        'frames': frames, 'vfr_pts_ms': [0, 40, 110, 140, 230, 300],
        'segment_pts_ms': [0, 40, 110], 'terminal_interval_ms': 41, 'container_default_interval_ms': 40,
        'interval_policy': 'adjacent actual display PTS gaps; terminal41ms synthetic',
        'seek_target': '200/1000',
        'seek_display_policy': 'validate preroll, select displayPTS>=target; no product seek semantics claim',
    }
    (root / 'source-definition.json').write_text(json.dumps(definition, indent=2, allow_nan=False) + '\n')


if __name__ == '__main__':
    generate(sys.argv[1])
