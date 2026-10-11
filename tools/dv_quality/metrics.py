"""Scalar HDR metrics for absolute BT.2020 light; no exposure fitting.

Equations checked against colour-science/colour at
248121e33fcae458e62190ac872f3e16ab2044bf and PU21 at
78340c0c4c20908c6bdcf0931dfde763c53a24ad. See accompanying BSD licenses.
This numerical module does not establish that input pictures are references.
"""
import math

PQ_M1 = 2610 / 16384
PQ_M2 = 2523 / 32
PQ_C1 = 3424 / 4096
PQ_C2 = 2413 / 128
PQ_C3 = 2392 / 128
PU21_BANDING_GLARE = (.353487901, .3734658629, 8.277049286e-05, .9062562627,
                     .09150303166, .9099517204, 596.3148142)
PU21_PEAK = 256.0
BT2020_LUMINANCE = (.2627, .6780, .0593)
RGB_TO_LMS = ((1688, 2146, 262), (683, 2951, 462), (99, 309, 3688))
LMS_P_TO_ICTCP = ((2048, 2048, 0), (6610, -13613, 7003), (17933, -17390, -543))


def _finite(value):
    value = float(value)
    if not math.isfinite(value):
        raise ValueError('Metric input must be finite')
    return value


def pq_eotf(value):
    """Normalized full-range PQ code [0,1] -> absolute cd/m2 [0,10000]."""
    value = _finite(value)
    if not 0 <= value <= 1:
        raise ValueError('PQ code must be in [0,1]')
    p = value ** (1 / PQ_M2)
    return 10000 * (max(p - PQ_C1, 0) / (PQ_C2 - PQ_C3 * p)) ** (1 / PQ_M1)


def pq_inverse_eotf(value):
    """Absolute cd/m2 -> PQ; 10000 is the PQ scale, not display peak."""
    value = _finite(value)
    if not 0 <= value <= 10000:
        raise ValueError('Absolute luminance must be in [0,10000]')
    p = (value / 10000) ** PQ_M1
    return ((PQ_C1 + PQ_C2 * p) / (1 + PQ_C3 * p)) ** PQ_M2


def bt2020_luminance(rgb):
    if len(rgb) != 3:
        raise ValueError('RGB must have three components')
    return sum(_finite(v) * w for v, w in zip(rgb, BT2020_LUMINANCE))


def _matrix(matrix, vector):
    return tuple(sum(c * v for c, v in zip(row, vector)) / 4096 for row in matrix)


def rgb_nits_to_ictcp(rgb):
    if len(rgb) != 3:
        raise ValueError('RGB must have three components')
    rgb = tuple(_finite(v) for v in rgb)
    if any(v < 0 or v > 10000 for v in rgb):
        raise ValueError('Absolute BT.2020 components must be in [0,10000]')
    return _matrix(LMS_P_TO_ICTCP,
                   tuple(pq_inverse_eotf(v) for v in _matrix(RGB_TO_LMS, rgb)))


def delta_e_itp(ictcp_reference, ictcp_candidate):
    """Inputs are ICtCp; Ct is half-scaled exactly once per BT.2124."""
    if len(ictcp_reference) != 3 or len(ictcp_candidate) != 3:
        raise ValueError('ICtCp must have three components')
    d = tuple(_finite(b) - _finite(a) for a, b in zip(ictcp_reference, ictcp_candidate))
    return 720 * math.sqrt(d[0]**2 + (d[1] / 2)**2 + d[2]**2)


def pu21_encode(value):
    """Authors' banding_glare equation; absolute input, clamp .005..10000."""
    value = min(max(_finite(value), .005), 10000)
    p = PU21_BANDING_GLARE
    y = value ** p[3]
    return max(p[6] * (((p[0] + p[1] * y) / (1 + p[2] * y)) ** p[4] - p[5]), 0)


def pu21_psnr_from_mse(mse):
    mse = _finite(mse)
    if mse < 0:
        raise ValueError('MSE must be nonnegative')
    return math.inf if mse == 0 else 10 * math.log10(PU21_PEAK**2 / mse)


def compare_rgb48_pixel(reference, candidate):
    """RGB48LE samples supplied as unsigned 16-bit triples; full-range PQ."""
    if len(reference) != 3 or len(candidate) != 3:
        raise ValueError('RGB samples must have three components')
    if any(not isinstance(v, int) or not 0 <= v <= 65535 for v in (*reference, *candidate)):
        raise ValueError('RGB48 samples must be integer uint16 values')
    r = tuple(pq_eotf(v / 65535) for v in reference)
    c = tuple(pq_eotf(v / 65535) for v in candidate)
    r_y = bt2020_luminance(r)
    c_y = bt2020_luminance(c)
    return {
        'delta_e_itp': delta_e_itp(rgb_nits_to_ictcp(r), rgb_nits_to_ictcp(c)),
        'pu21_rgb_squared_error': sum((pu21_encode(b) - pu21_encode(a))**2
                                     for a, b in zip(r, c)) / 3,
        'pu21_luminance_squared_error': (pu21_encode(c_y) - pu21_encode(r_y))**2,
        'reference_luminance_nits': r_y,
        'candidate_luminance_nits': c_y,
    }
