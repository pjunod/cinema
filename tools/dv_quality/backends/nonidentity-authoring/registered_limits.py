"""Immutable analytic limits; chronology is declared, not a dated attestation."""
from pathlib import Path
import hashlib
import json
import math

EXPECTED_SHA256 = 'd7d71c1aad9f1278e567a38589dc96bf1cc8b0f800b40c98a95d801c16821e9f'
INTEGER_LIMITS = ('cpu_scalar_max_rgb_code_error','encoder_max_yuv_code_error',
                  'forward_inverse_ncl_max_code_error','gpu_vs_scalar_full_matrix_max_rgb_code_error',
                  'deliberately_repeated_mapping_vs_once_min_max_rgb_code_separation')


def reject_constant(value):
    raise ValueError('nonfinite JSON constant: '+value)


def finite_float(value):
    result = float(value)
    if not math.isfinite(result):
        raise ValueError('nonfinite JSON float')
    return result


def read_json(path):
    return json.loads(Path(path).read_text(),parse_constant=reject_constant,parse_float=finite_float)


def read_limits(root):
    path = Path(root)/'preregistered-controls.json'
    if hashlib.sha256(path.read_bytes()).hexdigest() != EXPECTED_SHA256:
        raise ValueError('immutable registered limits digest differs')
    data = read_json(path)
    for field in INTEGER_LIMITS:
        if type(data[field]) is not int or data[field] < 0:
            raise ValueError('registered integer limit has invalid type/value')
    value = data['gpu_vs_scalar_full_matrix_max_pq_error']
    if type(value) is not float or not math.isfinite(value) or value <= 0:
        raise ValueError('registered PQ limit has invalid type/value')
    return data
