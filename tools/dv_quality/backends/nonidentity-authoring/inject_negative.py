"""Encode a deliberate repeated-affine metadata negative on the same base."""
from pathlib import Path
import hashlib
import inject_rpu
ROOT = Path(__file__).resolve().parent
rpus = [(ROOT/f'wrong-rpu-frame{frame}.nal').read_bytes() for frame in range(4)]
result = inject_rpu.inject((ROOT/'hdr10-base.hevc').read_bytes(),rpus,
                          [hashlib.sha256(r).hexdigest() for r in rpus])
(ROOT/'candidate-wrong-repeated-affine.hevc').write_bytes(result)
