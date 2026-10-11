#!/usr/bin/env python3
"""Create tiny authored PQ-code controls; no Dolby Vision decoder/reference.

python3 tools/dv_quality/example.py --output NEWDIR
python3 tools/dv_quality/run.py compare --candidate NEWDIR/candidate/manifest.json \\
    --baseline NEWDIR/baseline/manifest.json --output RECEIPTDIR
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    output = args.output.absolute()
    if any(p.is_symlink() for p in (output, *output.parents)) or not output.parent.is_dir():
        parser.error('output requires a regular existing parent without symlinks')
    try:
        output.mkdir(mode=0o700)
    except FileExistsError:
        parser.error('output exists; overwrite refused')
    definition = b'Authored neutral PQ RGB48LE grid: base 32768, candidate +0,+1,+2,+3; two frames at 24Hz.'
    for role in ('baseline', 'candidate'):
        directory = output / role
        directory.mkdir()
        (directory / 'source.txt').write_bytes(definition)
        frames = []
        for index in range(2):
            raw = b''.join(struct.pack('<HHH', *((32768 + (n if role == 'candidate' else 0),) * 3)) for n in range(4))
            name = f'frame-{index}.rgb'
            (directory / name).write_bytes(raw)
            frames.append({'path': name, 'sha256': hashlib.sha256(raw).hexdigest(),
                           'pts': f'{index}/24', 'duration': '1/24'})
        manifest = {
            'schema': 1, 'role': role,
            'source': {'id': 'authored-PQ-code-grid', 'sha256': hashlib.sha256(definition).hexdigest(), 'path': 'source.txt'},
            'renderer': {'id': 'example.py authored code values', 'revision': 'v1',
                         'artifact_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()},
            'picture': {'width': 4, 'height': 1, 'pixel_format': 'rgb48le', 'range': 'full',
                        'primaries': 'bt2020', 'transfer': 'smpte2084', 'domain': 'reconstruction'},
            'target': {'id': 'identity-PQ-10000', 'peak_nits': 10000, 'black_nits': .005,
                       'gamut': 'bt2020', 'mapping': 'none', 'parameters': {}},
            'frames': frames}
        (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(json.dumps({'fixture': str(output), 'scope': 'authored mathematical control; no DV/FEL evidence'}))


if __name__ == '__main__':
    main()
