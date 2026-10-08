"""Verify parsed synthetic tags, exact AU association and rational durations."""
from pathlib import Path
from fractions import Fraction
import hashlib
import json
import inject_rpu
ROOT = Path(__file__).resolve().parent

def verify():
    definition = json.loads((ROOT/'source-definition.json').read_text())
    probe = json.loads((ROOT/'probe.json').read_text())
    if len(probe['frames']) != 4:
        raise ValueError('decoded frame count changed')
    time_base = Fraction(probe['streams'][0]['time_base'])
    parsed_tags = []
    units = []
    for nal in inject_rpu.nals((ROOT/'candidate.hevc').read_bytes()):
        kind = (nal[0] >> 1) & 63
        if kind == 35:
            units.append([])
        if not units:
            raise ValueError('NAL before AUD')
        units[-1].append(nal)
    if len(units) != 4:
        raise ValueError('access unit count changed')
    source = (ROOT/'reconstructed64.rgb48le').read_bytes()
    for frame, (association, au, picture) in enumerate(zip(definition['frames'], units, probe['frames'])):
        parsed = json.loads((ROOT/f'adapted-rpu-frame{frame}.json').read_text())
        blocks = parsed['vdr_dm_data']['cmv29_metadata']['ext_metadata_blocks']
        tags = [b['Level1'] for b in blocks if 'Level1' in b]
        if tags != [association['synthetic_l1']]:
            raise ValueError('parsed L1 identity differs from expected source association')
        rpus = [n for n in au if ((n[0] >> 1) & 63) == 62]
        if len(rpus) != 1 or hashlib.sha256(rpus[0]).hexdigest() != association['rpu_sha256']:
            raise ValueError('RPU access unit identity/order differs')
        if hashlib.sha256(source[frame*64*64*6:(frame+1)*64*64*6]).hexdigest() != association['source_rgb48le_sha256']:
            raise ValueError('source frame identity/order differs')
        if Fraction(picture['pkt_duration']) * time_base != Fraction(1,24):
            raise ValueError('actual coded frame duration differs from24/1')
        if parsed['header']['disable_residual_flag'] is not True:
            raise ValueError('adapted RPU unexpectedly enables residual')
        if not any(d['side_data_type'] == 'Dolby Vision Metadata' for d in picture['side_data_list']):
            raise ValueError('FFmpeg did not parse RPU side data')
        parsed_tags.append(tags[0])
    result = {'result': 'pass', 'scope': 'synthetic no-reorder elementary control',
              'parsed_l1_tags': parsed_tags, 'frame_count': 4,
              'distinct_source_hashes': len({f['source_rgb48le_sha256'] for f in definition['frames']}),
              'distinct_rpu_hashes': len({f['rpu_sha256'] for f in definition['frames']}),
              'coded_duration': '1/24', 'manifest_time_base': '1/24',
              'actual_container_pts': None, 'p81_conformance': None}
    if result['distinct_source_hashes'] != 4 or result['distinct_rpu_hashes'] != 4:
        raise ValueError('temporal identity controls are not distinct')
    (ROOT/'rpu-association-receipt.json').write_text(json.dumps(result, indent=2) + '\n')
    return result

if __name__ == '__main__':
    print(json.dumps(verify(), indent=2))
