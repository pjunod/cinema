"""Bind analytic controls and limitations without implying picture qualification."""
from pathlib import Path
import hashlib
import json
import platform
from registered_limits import read_limits
from verify_refusals import verify as verify_refusals
ROOT = Path(__file__).resolve().parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(name):
    return json.loads((ROOT/name).read_text())


read_limits(ROOT)
admission = verify_refusals(ROOT)
registration_hash = digest(ROOT/'preregistered-controls.json')
if (ROOT/'registered-limits.sha256').read_text().strip() != registration_hash:
    raise ValueError('registered limits changed after replay started')
runtime = read('runtime-inputs.json')
receipt = {
    'schema':'m0-p81-nonidentity-affine-v1',
    'result':'analytic-control-pass',
    'scope':'Declared synthetic affine-luma P7/FEL reconstruction and repeated-curve discrimination; no picture/profile qualification',
    'registration':{'sha256':registration_hash,
                    'timing':'Declared original chronology from session command order: registration written before first CPU/encode/GPU measurements; no independently dated/signed witness. Fresh replay hash only checks within-replay stability',
                    'controls':read('preregistered-controls.json')},
    'recipe':{'original_p7':'luma1/16+3x/4, chromaidentity, residualrr/1024',
              'once_vdr12_luma':'256+3*BL_Y+4*rr; exact integer for these samples',
              'correct_adapted_p81':'To81 then remove_mapping; identity curve/residual disabled',
              'deliberately_wrong_p81':'Residual disabled; original luma affine curve restored onto already reconstructed encoded base'},
    'cpu':read('cpu-check-receipt.json'),
    'hdr10':read('authoring-check-receipt.json'),
    'gpu':read('gpu-check-receipt.json'),
    'association':read('fixture-association-receipt.json'),
    'unsupported_matrix':{'exit_code':admission['unsupported_matrix'],'output_refused':True},
    'unsupported_trims':{'exit_code':admission['creative_trims'],'output_refused':True,'fixture':'L2 default neutral trim block; all L2 presence unsupported'},
    'runtime':{**runtime,'cpu_platform':'linux/amd64','gpu_platform':'linux/arm64',
               'client_system':platform.system(),'client_architecture':platform.machine(),
               'execution_mode':None,'rustc':'1.97.1','cpus':2,'memory_bytes':2147483648},
    'replay':{'command':'bash replay.sh NEW_SCRATCH CPU_REPLAY_ROOT PARSED_GPU_REPLAY_ROOT',
              'dependencies':'Prepared pinned CPU source/staticlib and GPU parser/libplacebo prefix from previous source-only replay bundles',
              'dependency_verification':'Exact scoped source/staticlib/header/sharedlibrary/link inventory enforced before and after copying. Matching prerequisite receipt schema/toolchain/pin/library/image fields checked; entire prerequisite receipt hashes recorded but not compared with historical lock receipt hashes. Actual Docker image/platform checked; retained dependencies, rebuilt probes/fixtures',
              'public_pins':{'DoViBaker':'ffba39830b694ddca0bf5f73dcf1b462713bb7f4',
                             'libdovi':'83e1fdad6dcd5995556235946e7c5c0f9010d5a1',
                             'libplacebo':'0d043c7f6f79cd3687c023454bdacbe615e4d96f'},
              'lock':'resolved-Cargo.lock; recorded workspace/CAPI/dev graph resolution, not pristine upstream --locked claim',
              'scope':'New scratch only; previous bundle source/dependencies read-only; no production service/install'},
    'synthetic_static_metadata':{'max_cll':1000,'max_fall':400,'mastering_max_nits':1000,'mastering_min_nits':.005,
                                 'measured':False,'note':'Synthetic constants/tags, no mastering or content analysis claim'},
    'limitations':['Only declared single affine luma curve, identity chroma and supplied fixed standard matrix coefficients',
                   'Original CPU processor omits rgb_to_lms; source domain restriction remains mandatory',
                   'Creative trims, MMR, other depths, metadata reuse and general authoring unsupported here',
                   'Detecting deliberate repeated affine shaping does not certify general metadata adaptation',
                   'P8 GPU receives paired decoded YUV/explicit RPU and frame index; no demux/container DV configuration proof',
                   'Elementary stream timing uses synthetic manifest24/1; original encoded P7 BL/EL pairing separate'],
    'independent_dv_picture_reference':None,'p81_conformance':None,
    'reference_fidelity_improvement':None,'real_time_performance':None,
}
if 'rustc 1.97.1' not in (ROOT/'generate.log').read_text():
    raise ValueError('actual generator toolchain differs')
(ROOT/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
