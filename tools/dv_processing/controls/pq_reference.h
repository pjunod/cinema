// Double-precision ST 2084 for the table-driven CM4 transfer helpers (dv_dm.h).
//
// pq_table_gen.c fits the helper tables against these functions, and pq_helper_exhaustive.py
// uses them to check every float32 input. Each is written so that no step cancels: near black
// the EOTF works from e/e0 - 1 and near its pole from e/ep - 1, both through log1p, so the
// relative error stays below 2^-43 on every input the helpers take (checked against the
// 60-digit Decimal reference in pq_helper_port.py). Callers treat 2^-36 as the bound and
// resolve every input whose value lies that close to a float32 rounding midpoint exactly.
#ifndef PLURX_PQ_REFERENCE_H
#define PLURX_PQ_REFERENCE_H
#include <math.h>

#define PQR_M1 0.1593017578125
#define PQR_M2 78.84375
#define PQR_C1 0.8359375
#define PQR_C2 18.8515625
#define PQR_C3 18.6875
// e0 = c1^m2 (zero light) and ep = (c2/c3)^m2 (the extended EOTF's pole), as double-double:
// hi is the double nearest, lo the double nearest the remainder (60-digit Decimal values).
#define PQR_E0_HI 7.309559025783966e-07
#define PQR_E0_LO -1.6048335859135375e-23
#define PQR_EP_HI 1.9920600818564904
#define PQR_EP_LO 7.544158618609786e-17
// Relative error the callers assume for every value below.
#define PQR_BOUND 1.4551915228366852e-11   // 2^-36

// log(e / (hi + lo)) for e > 0 without cancellation when e is close to hi + lo.
static inline double pqr_log_ratio(double e, double hi, double lo) {
  double d = (e - hi) - lo;             // e - hi is exact when e is within a factor 2 of hi
  double r = d / (hi + lo);
  if (fabs(r) < 0.5) return log1p(r);
  return log(e) - log(hi + lo);
}

// Extended EOTF in nits for a PQ code e >= 0; 0 at or below e0, +inf at or past ep.
static inline double pqr_eotf(double e) {
  if (!(e > 0)) return 0;
  double l0 = pqr_log_ratio(e, PQR_E0_HI, PQR_E0_LO);   // log(e/e0)
  if (l0 <= 0) return 0;
  double lp = pqr_log_ratio(e, PQR_EP_HI, PQR_EP_LO);   // log(e/ep)
  if (lp >= 0) return INFINITY;
  // p = e^(1/m2); numerator p - c1 = c1 expm1(log(e/e0)/m2);
  // denominator c2 - c3 p = -c2 expm1(log(e/ep)/m2), since ep^(1/m2) = c2/c3.
  double numerator = PQR_C1 * expm1(l0 / PQR_M2);
  double denominator = -PQR_C2 * expm1(lp / PQR_M2);
  return 10000 * exp((log(numerator) - log(denominator)) / PQR_M1);
}

// Inverse EOTF (PQ code) for light y in nits, 0 <= y <= 10000.
static inline double pqr_oetf(double y) {
  if (!(y > 0)) return exp(PQR_M2 * log(PQR_C1));
  if (y >= 10000) return 1;
  double z = exp(PQR_M1 * log(y / 10000));
  // log((c1 + c2 z) / (1 + c3 z)), each logarithm of a value above c1 (no cancellation).
  double l = log(PQR_C1 + PQR_C2 * z) - log1p(PQR_C3 * z);
  return exp(PQR_M2 * l);
}
#endif
