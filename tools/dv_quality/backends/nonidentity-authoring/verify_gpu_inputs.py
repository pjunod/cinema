"""Bind actual consumed bytes and parsed curve/frame event to the declared case."""
from fractions import Fraction
import json
from registered_limits import read_json, reject_constant, finite_float


def verify_input(root, control, prefix, frame):
    consumed = root/'gpu'/control/f'frame-{frame}'
    if (consumed/'decoded.yuv420p10le').read_bytes() != (root/'decoded.yuv420p10le').read_bytes():
        raise ValueError('actual consumed decoded bytes differ')
    if (consumed/'consumed-rpu.nal').read_bytes() != (root/f'{prefix}-rpu-frame{frame}.nal').read_bytes():
        raise ValueError('actual consumed RPU bytes differ')
    parsed = read_json(root/f'{prefix}-rpu-frame{frame}.json')
    definition = read_json(root/'source-definition.json')
    probe = [json.loads(line,parse_constant=reject_constant,parse_float=finite_float)
             for line in (consumed/'probe.jsonl').read_text().splitlines()]
    def one(kind):
        rows = [row for row in probe if row.get('kind') == kind]
        if len(rows) != 1:
            raise ValueError('actual GPU event count differs: '+kind)
        return rows[0]
    curve = parsed['rpu_data_mapping']['curves'][0]
    expected_curve = {'constant_integer':curve['poly_coef_int'][0][0],
                      'constant_fraction':curve['poly_coef'][0][0],
                      'slope_integer':curve['poly_coef_int'][0][1],
                      'slope_fraction':curve['poly_coef'][0][1],'denominator':8388608}
    def strict(actual, expected):
        return all(type(actual.get(key)) is type(value) and actual.get(key) == value for key,value in expected.items())
    actual_curve = one('parsed_luma_curve')
    if not strict(actual_curve,expected_curve):
        raise ValueError('actual parsed luma curve differs')
    expected_tag = definition['frames'][frame]['synthetic_l1']
    tag = one('parsed_l1')
    if not strict(tag,expected_tag):
        raise ValueError('actual parsed L1 tag differs')
    event = one('frame')
    expected = {'case':'zero','el_bound':False,'nlq_active':False,'rpu_parsed':True,
                'direct_dispatch_ok':True,'render_ok':True,'render_errors':0,'qualified_fel':False}
    if not strict(event,expected) or type(event.get('pts')) is not str or type(event.get('duration')) is not str or Fraction(event['pts']) != Fraction(frame,24) or Fraction(event['duration']) != Fraction(1,24):
        raise ValueError('actual GPU frame event differs')
    info = one('parsed_rpu')
    expected_info = {'profile':8,'el_type':'none','parser_error':False,'bl_depth':10,
                     'el_depth':10,'vdr_depth':12,'mapping_segments':1,'nlq_method':'disabled',
                     'creative_l2_count':0,'creative_l8_count':0,'creative_trims_applied':False}
    if not strict(info,expected_info):
        raise ValueError('actual GPU parsed header/admission differs')
    return {'consumed_bytes_verified':True,'actual_parsed_luma_curve':expected_curve,
            'actual_frame_event':event}
