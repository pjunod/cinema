"""Check exact nonidentity source/metadata recipe and paired output NALs."""
from pathlib import Path
import hashlib
import json
import inject_rpu
from verify_gpu_inputs import verify_input
from registered_limits import read_limits, read_json
ROOT = Path(__file__).resolve().parent

def verify(root=ROOT):
    read_limits(root)
    source = json.loads((root/'p7-affine-fel.json').read_text())
    p7_curve = source['rpu_data_mapping']['curves'][0]
    if p7_curve['poly_coef_int'] != [[0,0]] or p7_curve['poly_coef'] != [[524288,6291456]]:
        raise ValueError('original affine curve differs')
    if source['header']['disable_residual_flag'] or source['dovi_profile'] != 7:
        raise ValueError('original P7 FEL role differs')
    definition = json.loads((root/'source-definition.json').read_text())
    sequences = [('correct','adapted','candidate.hevc'), ('wrong','wrong','candidate-wrong-repeated-affine.hevc')]
    rows = []
    for control,prefix,path in sequences:
        units = []
        for nal in inject_rpu.nals((root/path).read_bytes()):
            if ((nal[0] >> 1) & 63) == 35:
                units.append([])
            if not units:
                raise ValueError('NAL before AUD')
            units[-1].append(nal)
        if len(units) != 4:
            raise ValueError('access unit count differs')
        for frame,unit in enumerate(units):
            data = (root/f'{prefix}-rpu-frame{frame}.nal').read_bytes()
            rpus = [n for n in unit if ((n[0] >> 1) & 63) == 62]
            if rpus != [data]:
                raise ValueError('actual RPU access-unit identity/order differs')
            parsed = json.loads((root/f'{prefix}-rpu-frame{frame}.json').read_text())
            curve = parsed['rpu_data_mapping']['curves'][0]
            expected = ([[0,1]], [[0,0]]) if control == 'correct' else ([[0,0]], [[524288,6291456]])
            if (curve['poly_coef_int'],curve['poly_coef']) != expected or not parsed['header']['disable_residual_flag']:
                raise ValueError('adaptation/negative recipe differs')
            verify_input(root,control,prefix,frame)
            probe = [json.loads(line) for line in (root/'gpu'/control/f'frame-{frame}/probe.jsonl').read_text().splitlines()]
            l1 = [row for row in probe if row.get('kind') == 'parsed_l1']
            expected_tag = definition['frames'][frame]['synthetic_l1']
            if len(l1) != 1 or {key:l1[0][key] for key in expected_tag} != expected_tag:
                raise ValueError('actual GPU parsed-tag identity differs')
            rows.append({'control':control,'frame':frame,'rpu_sha256':hashlib.sha256(data).hexdigest(),
                         'luma_curve': 'identity' if control=='correct' else '1/16+3x/4',
                         'residual_disabled':True})
    receipt = {'scope':'Synthetic exact recipes/AUD association; no P8.1 container/device conformance',
               'result':'analytic-control-pass','results':rows,'actual_container_pts':None}
    (root/'fixture-association-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
    return receipt

if __name__ == '__main__':
    print(json.dumps(verify(),indent=2))
