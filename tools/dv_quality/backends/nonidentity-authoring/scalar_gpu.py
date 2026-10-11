"""Scalar PQ/matrix/affine predictions evaluated separately from GPU outputs.
The transform convention is pinned libplacebo0d043c7 API374; this independent
arithmetic implementation is not an independently approved Dolby decoder.
"""
from pathlib import Path
import hashlib
import json
import math
import struct
from registered_limits import read_limits, read_json
from verify_gpu_inputs import verify_input
ROOT = Path(__file__).resolve().parent
HPE_INVERSE = ((3.06441879,-2.16597676,.10155818),
               (-.65612108,1.78554118,-.12943749),
               (.01736321,-.04725154,1.03004253))
FRAME_BYTES = 64 * 64 * 3


def eotf(code):
    p = max(code, 0) ** (32/2523)
    return (max(p-3424/4096, 0)/(2413/128-2392/128*p)) ** (16384/2610)


def oetf(light):
    p = max(light, 0) ** (2610/16384)
    return ((3424/4096+2413/128*p)/(1+2392/128*p)) ** (2523/32)


def predicted(decoded, parsed):
    dm = parsed['vdr_dm_data']
    nonlinear = [[dm[f'ycc_to_rgb_coef{3*i+j}']/8192 for j in range(3)] for i in range(3)]
    linear = [[dm[f'rgb_to_lms_coef{3*i+j}']/16384 for j in range(3)] for i in range(3)]
    matrix = [[sum(HPE_INVERSE[i][k]*linear[k][j] for k in range(3)) for j in range(3)] for i in range(3)]
    offsets = [dm[f'ycc_to_rgb_offset{i}']/2**28*1024/1023 for i in range(3)]
    curves = parsed['rpu_data_mapping']['curves']
    coeffs = [[a+b/2**23 for a,b in zip(c['poly_coef_int'][0],c['poly_coef'][0])] for c in curves]
    output = []
    for y in range(64):
        for x in range(64):
            ci = y//2*32+x//2
            v = [decoded[y*64+x]/1023, decoded[4096+ci]/1023, decoded[5120+ci]/1023]
            mapped = [coeffs[c][0]+coeffs[c][1]*v[c] for c in range(3)]
            nonlinear_rgb = [sum(nonlinear[i][j]*(mapped[j]-offsets[j]) for j in range(3)) for i in range(3)]
            light = [eotf(value) for value in nonlinear_rgb]
            output.extend(oetf(sum(matrix[i][j]*light[j] for j in range(3))) for i in range(3))
    return output


def code_values(values):
    return [round(max(0,min(1,v))*65535) for v in values]


def verify(root=ROOT):
    registration = read_limits(root)
    data = (root/'decoded.yuv420p10le').read_bytes()
    if len(data) != 4*FRAME_BYTES:
        raise ValueError('decoded sequence length changed')
    results = []
    separation = []
    for f in range(4):
        frame_data = data[f*FRAME_BYTES:(f+1)*FRAME_BYTES]
        decoded = struct.unpack('<6144H', frame_data)
        vectors = {}
        for control,prefix in [('correct','adapted'),('wrong','wrong')]:
            input_evidence = verify_input(root,control,prefix,f)
            parsed = read_json(root/f'{prefix}-rpu-frame{f}.json')
            consumed = root/'gpu'/control/f'frame-{f}'
            if (consumed/'decoded.yuv420p10le').read_bytes() != data:
                raise ValueError('actual GPU decoded-input bytes differ')
            if (consumed/'consumed-rpu.nal').read_bytes() != (root/f'{prefix}-rpu-frame{f}.nal').read_bytes():
                raise ValueError('actual GPU RPU input bytes differ')
            if not parsed['header']['disable_residual_flag']:
                raise ValueError('P8 control unexpectedly enables residual')
            values = predicted(decoded,parsed)
            if not all(math.isfinite(v) for v in values):
                raise ValueError('nonfinite scalar prediction')
            expected = code_values(values)
            (root/f'{control}-scalar-frame{f}.rgb48le').write_bytes(struct.pack('<12288H',*expected))
            vectors[control] = expected
            for stage in ('reconstruction','rendered'):
                output = root/'gpu'/control/f'frame-{f}'/'outputs'/f'zero-{stage}'
                raw = output.with_suffix('.rgba32f').read_bytes()
                rgb = output.with_suffix('.rgb48le').read_bytes()
                if len(raw) != 65536 or len(rgb) != 24576:
                    raise ValueError('GPU output length changed')
                floats = struct.unpack('<16384f',raw)
                actual = [floats[4*i+c] for i in range(4096) for c in range(3)]
                if not all(math.isfinite(v) for v in floats):
                    raise ValueError('nonfinite GPU output')
                pq_error = max(abs(a-b) for a,b in zip(actual,values))
                actual_codes = struct.unpack('<12288H',rgb)
                code_error = max(abs(a-b) for a,b in zip(actual_codes,expected))
                if pq_error > registration['gpu_vs_scalar_full_matrix_max_pq_error'] or code_error > registration['gpu_vs_scalar_full_matrix_max_rgb_code_error']:
                    raise ValueError('GPU/scalar registered arithmetic limit failed')
                results.append({**input_evidence,'frame':f,'control':control,'stage':stage,'max_pq_error':pq_error,
                                'max_rgb_code_error':code_error,'output_sha256':hashlib.sha256(rgb).hexdigest(),
                                'rpu_sha256':hashlib.sha256((root/f'{prefix}-rpu-frame{f}.nal').read_bytes()).hexdigest(),
                                'decoded_frame_sha256':hashlib.sha256(frame_data).hexdigest()})
        actual_correct = struct.unpack('<12288H',(root/'gpu/correct'/f'frame-{f}/outputs/zero-rendered.rgb48le').read_bytes())
        actual_wrong = struct.unpack('<12288H',(root/'gpu/wrong'/f'frame-{f}/outputs/zero-rendered.rgb48le').read_bytes())
        max_delta = max(abs(a-b) for a,b in zip(actual_correct,actual_wrong))
        wrong_vs_once = max(abs(a-b) for a,b in zip(actual_wrong,vectors['correct']))
        if max_delta < registration['deliberately_repeated_mapping_vs_once_min_max_rgb_code_separation'] or wrong_vs_once <= registration['gpu_vs_scalar_full_matrix_max_rgb_code_error']:
            raise ValueError('deliberate repeated-affine negative was not discriminated')
        separation.append({'frame':f,'wrong_vs_correct_max_rgb_code_delta':max_delta,
                           'wrong_vs_once_scalar_max_rgb_code_error':wrong_vs_once,
                           'once_scalar_control_rejects_wrong_output':True})
    receipt = {'scope':'Declared affine-luma synthetic control; full-matrix independent arithmetic, no independent DV picture reference',
               'registered_limits_sha256':hashlib.sha256((root/'preregistered-controls.json').read_bytes()).hexdigest(),
               'result':'analytic-control-pass','results':results,'repeated_mapping_negative':separation,
               'p81_conformance':None,'reference_fidelity_improvement':None}
    (root/'gpu-check-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
    return receipt

if __name__ == '__main__':
    print(json.dumps(verify(),indent=2))
