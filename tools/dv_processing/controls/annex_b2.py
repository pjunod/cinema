"""Independent ST 2094-10:2021 Annex B.2 reference for targeted-export controls.

Source: https://pub.smpte.org/latest/st2094-10/st2094-10-2021.pdf, pp11-13,
equations (2)-(7). The oracle solves the three anchor equations by Gaussian
elimination, rather than copying the C coefficient expression or invoking
libplacebo. PQ transfer follows ST2084. No library-produced LUT is an input.

The title-floor substitution, no-expansion branch and degenerate-anchor linear
extension are Cinema policy, not claims about the informative SMPTE example.
"""
import math
from dataclasses import dataclass
from decimal import Decimal, localcontext

BUCKETS = (400, 600, 1000, 1600, 2000, 4000)


def eotf(code):
    """Normalized PQ -> absolute luminance (cd/m²)."""
    p = max(0.0, min(1.0, code)) ** (32 / 2523)
    return 10000 * (max(p - 3424 / 4096, 0) / (2413 / 128 - 2392 / 128 * p)) ** (16384 / 2610)


def oetf(nits):
    """Absolute luminance -> normalized PQ."""
    p = (max(0.0, min(10000.0, nits)) / 10000) ** (2610 / 16384)
    return ((3424 / 4096 + 2413 / 128 * p) / (1 + 2392 / 128 * p)) ** (2523 / 32)


def solve_anchors(xs, ys):
    """Solve c1 + c2*x - c3*x*y = y with scaled partial pivoting.

    This is the defining equation, not the implementation's closed-form
    determinant. Reject a singular fit rather than inventing coefficients.
    """
    with localcontext() as context:
        context.prec = 60
        return _solve_decimal(xs, ys)


def _solve_decimal(xs, ys):
    anchors = [(Decimal(str(x)), Decimal(str(y))) for x, y in zip(xs, ys, strict=True)]
    rows = [[Decimal(1), x, -x * y, y] for x, y in anchors]
    scales = [max(abs(v) for v in row[:3]) for row in rows]
    for col in range(3):
        pivot = max(range(col, 3), key=lambda i: abs(rows[i][col]) / scales[i])
        rows[col], rows[pivot] = rows[pivot], rows[col]
        scales[col], scales[pivot] = scales[pivot], scales[col]
        divisor = rows[col][col]
        if divisor == 0:
            raise ValueError("singular Annex B.2 anchors")
        rows[col] = [v / divisor for v in rows[col]]
        for row in range(3):
            if row != col:
                factor = rows[row][col]
                rows[row] = [a - factor * b for a, b in zip(rows[row], rows[col], strict=True)]
    return tuple(float(row[3]) for row in rows)


@dataclass(frozen=True)
class Curve:
    floor: float
    peak: float
    target: float
    coefficients: tuple
    mode: str

    def nits(self, value):
        # Equal source/target floors: preserve true black below that floor.
        if value <= self.floor:
            return max(0.0, value)
        if self.mode == "no-expansion":
            return min(value, self.target)
        if self.mode == "degenerate-anchor-linear":
            return min(self.target, self.floor + (value - self.floor) *
                       (self.target - self.floor) / (self.peak - self.floor))
        c1, c2, c3 = self.coefficients
        value = min(value, self.peak)
        return min(self.target, max(self.floor, (c1 + c2 * value) / (1 + c3 * value)))

    def pq(self, value):
        if eotf(value) <= self.floor:
            return max(0.0, value)
        if self.mode == "no-expansion":
            return min(value, oetf(self.target))
        return oetf(self.nits(eotf(value)))


def from_l1(max_pq, avg_pq, target, title_min_1e4=50, title_peak=4000, *, sdr=False):
    if (target not in BUCKETS and not (sdr and target == 100)) or type(target) is not int:
        raise ValueError("unsupported target bucket")
    if not (type(max_pq) is int and type(avg_pq) is int and 0 <= avg_pq <= max_pq <= 4095):
        raise ValueError("invalid L1; whole-session master fallback required")
    floor = title_min_1e4 / 10000
    if not (0 <= title_min_1e4 <= 10000 and 0 < title_peak <= 10000 and floor < target):
        raise ValueError("invalid title mastering endpoint")
    peak = eotf(max_pq / 4095) if max_pq else title_peak
    if peak <= target:
        return Curve(floor, peak, target, (), "no-expansion")
    average = eotf(avg_pq / 4095)
    adaptation = min(0.8 * target, math.sqrt(average * math.sqrt(floor * target)))
    mode = "degenerate-anchor-linear"
    coefficients = ()
    if floor < average < peak and floor < adaptation < target:
        try:
            coefficients = solve_anchors((floor, average, peak), (floor, adaptation, target))
            c1, c2, c3 = coefficients
            # A positive derivative and no pole over the interval prove
            # monotonicity; c3 >= 0 is sufficient but not necessary.
            if (all(math.isfinite(c) for c in coefficients)
                    and (1 + c3 * floor) * (1 + c3 * peak) > 0
                    and c2 - c1 * c3 > 0):
                mode = "annex-b2-rgb-v1"
        except ValueError:
            pass
    return Curve(floor, peak, target, coefficients, mode)
