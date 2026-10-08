"""Six distinct 64x64 Main10 native code controls, before encoding."""
from pathlib import Path
import hashlib, json, struct
r = Path(__file__).resolve().parent
rows = []
for layer in ['bl', 'el']:
    with (r / f'{layer}.yuv420p10le').open('wb') as stream:
        for frame in range(6):
            if layer == 'bl':
                y = [400 + 4 * frame + 2 * (x // 4) + yy // 4 for yy in range(64) for x in range(64)]
            else:
                y = [512 + (8 + frame) * (1 if (x // 4 + yy // 4 + frame) % 2 else -1) for yy in range(64) for x in range(64)]
            raw = struct.pack('<6144H', *y + [512] * 2048)
            stream.write(raw)
            rows.append({'layer': layer, 'logical_source_frame': frame, 'source_sha256': hashlib.sha256(raw).hexdigest()})
(r / 'source-definition.json').write_text(json.dumps({'scope': 'synthetic native Main10 controls', 'width': 64, 'height': 64, 'logical_rate': '24/1', 'BL': 'Y=400+4f+2floor(x/4)+floor(y/4);U=V=512', 'EL': 'Y=512±(8+f) checker;U=V=512', 'frames': rows}, indent=2) + '\n')
