"""Distinct analytic frames with exact source/RPU association and rational time."""
from pathlib import Path
import hashlib
import json
ROOT = Path(__file__).resolve().parent

def digest(data):
    return hashlib.sha256(data).hexdigest()

frames = []
for name in ('reconstructed', 'base'):
    src = ROOT / (('zero' if name == 'base' else name) + '16.rgb48le')
    data = src.read_bytes()
    if len(data) != 16 * 16 * 6:
        raise ValueError('incorrect source frame length')
    output = bytearray()
    for f in range(4):
        # Frame0 remains identical to initial static control. Later cyclic
        # spatial shifts preserve the exact seed's known reconstruction.
        frame = b''.join(data[((y//4+f*3)%16*16+(x//4+f)%16)*6:
                             ((y//4+f*3)%16*16+(x//4+f)%16+1)*6]
                         for y in range(64) for x in range(64))
        output.extend(frame)
        if name == 'reconstructed':
            rpu_name = f'adapted-rpu-frame{f}.nal'
            rpu = (ROOT / rpu_name).read_bytes()
            frames.append({'index': f, 'pts': f, 'duration': 1,
                           'source_rgb48le_sha256': digest(frame),
                           'rpu_path': rpu_name, 'rpu_sha256': digest(rpu),
                           'synthetic_l1': {'min_pq': 0, 'max_pq': 4095-f, 'avg_pq': 2048-f}})
    (ROOT / (name + '64.rgb48le')).write_bytes(output)
manifest = {'scope': 'analytic-control', 'width': 64, 'height': 64, 'frame_count': 4,
            'frame_rate': '24/1', 'time_base': '1/24', 'frames': frames,
            'format': 'RGB48LE', 'range': 'full', 'primaries': 'BT2020', 'transfer': 'ST2084',
            'source_transform': 'Original P7 luma1/16+3x/4 applied once before residual; adapted RPU identity',
            'source_method': 'Exact nearest4x replication of cyclically permuted declared CPU analytic reconstruction; no picture reference',
            'frame_sampling': 'source_x=(x//4+frame)%16; source_y=(y//4+3*frame)%16',
            'rpu_pairing': 'Distinct bounded synthetic L1 values by frame; identity mapping/no residual; bframes0,keyint24,scenecut0',
            'metadata_semantics': 'Synthetic identity tags; not image analysis or artistic metadata authoring',
            'timestamp_limit': 'Synthetic manifest PTS only; elementary HEVC has no actual PTS; container/reordered pairing unproven',
            'independent_dv_reference': None, 'p81_conformance': None}
(ROOT / 'source-definition.json').write_text(json.dumps(manifest, indent=2) + '\n')
