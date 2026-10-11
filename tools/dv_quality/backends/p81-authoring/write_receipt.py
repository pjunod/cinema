"""Retain bounded scope, reproducible commands and immutable artifact hashes."""
from pathlib import Path
import hashlib
import json
import platform
ROOT = Path(__file__).resolve().parent

def read(name):
    return json.loads((ROOT/name).read_text())

receipt = {
 'schema': 'm0-p81-authored-base-analytic-v1',
 'result': 'diagnostic-control-pass',
 'scope': 'Synthetic identity-reshape P7 reconstruction -> authored Main10 base + adapted RPUs',
 'runtime': {'requested_container_platform': 'linux/arm64',
             'observed_uname': (ROOT/'encode.log').read_text().splitlines()[0],
             'replay_client_system': platform.system(), 'replay_client_architecture': platform.machine(),
             'execution_mode': None, 'rustc': '1.97.1', 'ffmpeg': '5.1.9-0+deb12u1',
             'x265': '3.5+1-f0c1022b6', 'image': (ROOT/'replay-image-id.txt').read_text().strip(),
             'cpus': 2, 'memory_bytes': 2147483648},
 'replay': {'entry_point': 'bash replay.sh in a fresh bundle copy without libdovi-source/',
            'public_source': 'quietvoid/dovi_tool@83e1fdad6dcd5995556235946e7c5c0f9010d5a1',
            'archive_sha256': '57d13f03d04a7a1b3d7dcb139b976dc3748c745c1720d4b6e44b61e5ef67b69b',
            'lock': 'resolved-Cargo.lock; approved CPU experiment workspace/CAPI/dev graph resolution, not pristine upstream --locked proof',
            'live_dependencies': 'Debian apt repositories and package transport are live, not a fully pinned OS snapshot; versions retained',
            'cross_render': 'Retained separate parsed-RPU/libplacebo GPU outputs checked by check_cross_render.py; authoring replay does not rerender GPU outputs',
            'gpu_replay_dependency': 'Separate parsed-RPU GPU bundle from sibling; libplacebo0d043c7f6f79cd3687c023454bdacbe615e4d96f API374'},
 'portable_import': {
     'cli': 'python3 import_cross_render.py --source GPU_REPLAY_ROOT --destination NEW_VALIDATION_ROOT/cross-render',
     'validation_root_inputs': ['check_cross_render.py', 'source-definition.json', 'analytic-hdr10-reference.rgb48le', 'decoded.yuv420p10le'],
     'validation_command': 'python3 NEW_VALIDATION_ROOT/check_cross_render.py',
     'destination': 'Must be absent and outside GPU source tree; no bundle-root default or in-place overwrite',
     'copied_inventory': 33, 'focused_controls': 10,
     'observed_refresh': 'Corrected sibling GPU replay imported separately; all16 RGB output hashes identical to historical snapshots'},
 'synthetic_static_hdr10_metadata': {
     'mastering_max_nits': 1000, 'mastering_min_nits': 0.005,
     'mastering_chromaticity_50000_units': {'green': [8500,39850], 'blue': [6550,2300], 'red': [35400,14600], 'white': [15635,16450]},
     'max_cll': 1000, 'max_fall': 400,
     'provenance': 'Explicit synthetic constants for this fixture; not measured content light, mastering verification, or image analysis'},
 'gpu_output_status': 'Retained historical sibling GPU outputs with exact input/RPU/tag/output hash binding; authoring replay does not freshly render GPU',
 'authoring': read('authoring-check-receipt.json'),
 'association': read('rpu-association-receipt.json'),
 'injection': read('injection-receipt.json'),
 'cross_render': read('cross-render-check-receipt.json'),
 'negative_controls': {'missing_rpu': 'reject', 'extra_rpu': 'reject', 'swapped_rpu': 'reject',
                       'swapped_decoded_frames': 'reject', 'swapped_source_frames': 'reject',
                       'truncated_decode': 'reject', 'existing_rpu': 'reject', 'missing_aud': 'reject',
                       'close_gpu_pixel_mutation': 'reject', 'scalar_substituted_gpu_output': 'reject',
                       'swapped_gpu_picture_outputs': 'reject', 'duplicate_gpu_stage': 'reject',
                       'missing_gpu_stage': 'reject', 'extra_gpu_row': 'reject',
                       'wrong_gpu_output_hash': 'reject', 'wrong_gpu_delta': 'reject'},
 'failed_attempts': {'native_rpu_encoder_option': 'x2653.5 does not recognize dolby-vision-rpu',
                     'lossless': 'Encoder selected Rext despite requested Main10; passing candidate uses QP0 Main10',
                     'default_chroma_location': 'Default sampling did not match closed-form point fixture; explicit centered chroma fixed it'},
 'unmet': ['Nonidentity-reshape P7 reconstructed-base authoring control',
           'Independent Dolby-enabled picture reference or certified profile conformance',
           'Actual container DV configuration/PTS and decoder B-frame reorder association',
           'Encoded original P7 BL/EL reconstruction/timestamp association',
           'Creative L2/L8 trim authoring or rendering; nonstandard matrix source support',
           'Metadata semantic adaptation beyond bounded synthetic tags',
           'Matched-rate commercial picture quality/performance/device validation'],
 'independent_dv_picture_reference': None, 'p81_conformance': None,
 'reference_fidelity_improvement': None, 'real_time_performance': None,
}
text = (ROOT/'encode.log').read_text()
if 'ffmpeg version 5.1.9-0+deb12u1' not in text or 'HEVC encoder version 3.5+1-f0c1022b6' not in text:
    raise ValueError('observed tool versions changed')
(ROOT/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')

excluded = {'libdovi-source', 'libdovi', 'portable-evidence', '__pycache__'}
ledger = {}
for artifact in ROOT.rglob('*'):
    relative = artifact.relative_to(ROOT)
    if not artifact.is_file() or any(part in excluded for part in relative.parts):
        continue
    if artifact.name in ('SHA256SUMS.json', 'generate_frame_rpus', 'adapted-rpu-four.bin'):
        continue
    ledger[str(relative)] = hashlib.sha256(artifact.read_bytes()).hexdigest()
(ROOT/'SHA256SUMS.json').write_text(json.dumps(ledger, indent=2)+'\n')
