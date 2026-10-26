/* dm_closed_form.h - closed-form Dolby Vision display mapping (CM v4 and CM v2.9), measured against
 * the licensed reference renderer (cm_offline 5.6.4) with synthetic patches; see rog_hdr_dv
 * CM4_CLOSED_FORM.md and CM29_CLOSED_FORM.md for the derivation and the evidence.
 *
 * Reference (CPU, double precision) implementation. All signal values are PQ in [0,1] unless noted;
 * colours are BT.2020 RGB. Per-shot state is prepared once (dm_cm4_prepare / dm_cm29_prepare) and
 * then applied per pixel. The spatial detail stage works on a whole intensity plane (dm_detail).
 *
 * Usage, CM4:   dm_cm4_trim t = dm_cm4_interp_gamut(trims, tpq, tprims, n, tmax_pq, smast_pq, rprim, mprim);
 *               dm_cm4_state s; dm_cm4_prepare(&s, l1, tmin_pq, tmax_pq, &t, DM_P3D65, DM_P3D65);
 *               dm_cm4_pixel(&s, rgb_in, ictcp_pre);          (per pixel)
 *               dm_detail(I_in, I_pre, w, h, 0.0, I_out);      (plane; CM4 uses w = 0)
 *               ictcp_pre[0] = I_out[i]; dm_cm4_finish(&s, ictcp_pre, rgb_out);
 * Usage, CM2.9: dm_cm29_state s; dm_cm29_prepare(&s, l1, smin_nits, spk_nits, &target, &l2);
 *               dm_cm29_pixel_pre(&s, rgb_in, &pix)           (per pixel: IPT and pointwise intensities)
 *               dm_detail(I, Ib, ..., l2.ms, Ib_out) (and the same for Io when cw != 0)
 *               dm_cm29_finish(&s, &pix, Ib_out, Io_out, rgb_out);
 */
#ifndef DM_CLOSED_FORM_H
#define DM_CLOSED_FORM_H
#include <math.h>
#define DM_PI 3.14159265358979323846
#include <string.h>
#include <stdlib.h>

#ifndef DM_STATIC
#define DM_STATIC static
#endif

enum dm_prim { DM_BT2020 = 0, DM_P3D65 = 1, DM_BT709 = 2 };

/* ------------------------------------------------------------------ basics */
#define DM_PQ_M1 0.1593017578125
#define DM_PQ_M2 78.84375
#define DM_PQ_C1 0.8359375
#define DM_PQ_C2 18.8515625
#define DM_PQ_C3 18.6875

DM_STATIC double dm_clampd(double v, double lo, double hi) { return v < lo ? lo : (v > hi ? hi : v); }
DM_STATIC double dm_mind(double a, double b) { return a < b ? a : b; }
DM_STATIC double dm_maxd(double a, double b) { return a > b ? a : b; }

/* PQ EOTF, signal -> nits. Input clamped to [0,1] (as the measurement tools do). */
DM_STATIC double dm_pq_eotf(double e) {
  e = dm_clampd(e, 0.0, 1.0);
  double p = pow(e, 1.0 / DM_PQ_M2);
  return 10000.0 * pow(dm_maxd(p - DM_PQ_C1, 0.0) / (DM_PQ_C2 - DM_PQ_C3 * p), 1.0 / DM_PQ_M1);
}
/* PQ EOTF without the upper clamp (analytic continuation; CM4 output path). */
DM_STATIC double dm_pq_eotf_ext(double e) {
  e = dm_maxd(e, 0.0);
  double p = pow(e, 1.0 / DM_PQ_M2);
  double den = DM_PQ_C2 - DM_PQ_C3 * p; if (den < 1e-12) den = 1e-12;
  return 10000.0 * pow(dm_maxd(p - DM_PQ_C1, 0.0) / den, 1.0 / DM_PQ_M1);
}
/* PQ inverse EOTF, nits -> signal, clamped to [0, 10000] nits. */
DM_STATIC double dm_pq_oetf(double y) {
  y = dm_clampd(y / 10000.0, 0.0, 1.0);
  double p = pow(y, DM_PQ_M1);
  return pow((DM_PQ_C1 + DM_PQ_C2 * p) / (1.0 + DM_PQ_C3 * p), DM_PQ_M2);
}

DM_STATIC void dm_mat_vec(const double m[3][3], const double v[3], double o[3]) {
  double a = m[0][0]*v[0] + m[0][1]*v[1] + m[0][2]*v[2];
  double b = m[1][0]*v[0] + m[1][1]*v[1] + m[1][2]*v[2];
  double c = m[2][0]*v[0] + m[2][1]*v[1] + m[2][2]*v[2];
  o[0] = a; o[1] = b; o[2] = c;
}
DM_STATIC void dm_mat_mul(const double a[3][3], const double b[3][3], double o[3][3]) {
  double t[3][3];
  for (int i = 0; i < 3; i++) for (int j = 0; j < 3; j++)
    t[i][j] = a[i][0]*b[0][j] + a[i][1]*b[1][j] + a[i][2]*b[2][j];
  memcpy(o, t, sizeof(t));
}
DM_STATIC int dm_mat_inv(const double m[3][3], double o[3][3]) {
  double d = m[0][0]*(m[1][1]*m[2][2]-m[1][2]*m[2][1]) - m[0][1]*(m[1][0]*m[2][2]-m[1][2]*m[2][0])
           + m[0][2]*(m[1][0]*m[2][1]-m[1][1]*m[2][0]);
  if (fabs(d) < 1e-300) return 0;
  double t[3][3];
  t[0][0] = (m[1][1]*m[2][2]-m[1][2]*m[2][1])/d; t[0][1] = (m[0][2]*m[2][1]-m[0][1]*m[2][2])/d;
  t[0][2] = (m[0][1]*m[1][2]-m[0][2]*m[1][1])/d; t[1][0] = (m[1][2]*m[2][0]-m[1][0]*m[2][2])/d;
  t[1][1] = (m[0][0]*m[2][2]-m[0][2]*m[2][0])/d; t[1][2] = (m[0][2]*m[1][0]-m[0][0]*m[1][2])/d;
  t[2][0] = (m[1][0]*m[2][1]-m[1][1]*m[2][0])/d; t[2][1] = (m[0][1]*m[2][0]-m[0][0]*m[2][1])/d;
  t[2][2] = (m[0][0]*m[1][1]-m[0][1]*m[1][0])/d;
  memcpy(o, t, sizeof(t)); return 1;
}

/* RGB -> XYZ for a primaries set with D65 white (0.3127, 0.3290). */
DM_STATIC void dm_rgb2xyz(enum dm_prim p, double o[3][3]) {
  static const double P[3][3][2] = {
    {{0.708, 0.292}, {0.170, 0.797}, {0.131, 0.046}},
    {{0.680, 0.320}, {0.265, 0.690}, {0.150, 0.060}},
    {{0.640, 0.330}, {0.300, 0.600}, {0.150, 0.060}}};
  double m[3][3], mi[3][3], s[3];
  for (int k = 0; k < 3; k++) {
    double x = P[p][k][0], y = P[p][k][1];
    m[0][k] = x / y; m[1][k] = 1.0; m[2][k] = (1.0 - x - y) / y;
  }
  const double W[3] = {0.3127 / 0.3290, 1.0, (1.0 - 0.3127 - 0.3290) / 0.3290};
  dm_mat_inv(m, mi); dm_mat_vec(mi, W, s);
  for (int i = 0; i < 3; i++) for (int k = 0; k < 3; k++) o[i][k] = m[i][k] * s[k];
}
/* Linear RGB conversion matrix src -> dst. */
DM_STATIC void dm_conv(enum dm_prim src, enum dm_prim dst, double o[3][3]) {
  double a[3][3], b[3][3], bi[3][3];
  dm_rgb2xyz(src, a); dm_rgb2xyz(dst, b); dm_mat_inv(b, bi); dm_mat_mul(bi, a, o);
}

/* ICtCp (BT.2100). */
static const double DM_RGB2LMS[3][3] = {
  {1688/4096.0, 2146/4096.0, 262/4096.0}, {683/4096.0, 2951/4096.0, 462/4096.0}, {99/4096.0, 309/4096.0, 3688/4096.0}};
static const double DM_LMS2ICTCP[3][3] = {
  {2048/4096.0, 2048/4096.0, 0.0}, {6610/4096.0, -13613/4096.0, 7003/4096.0}, {17933/4096.0, -17390/4096.0, -543/4096.0}};

/* PQ BT.2020 RGB -> ICtCp (as the measurement tools: LMS clamped to [0, 10000] nits). */
DM_STATIC void dm_rgbpq_to_ictcp(const double rgb[3], double o[3]) {
  double lin[3] = {dm_pq_eotf(rgb[0]), dm_pq_eotf(rgb[1]), dm_pq_eotf(rgb[2])}, lms[3];
  dm_mat_vec(DM_RGB2LMS, lin, lms);
  for (int i = 0; i < 3; i++) lms[i] = dm_pq_oetf(lms[i]);
  dm_mat_vec(DM_LMS2ICTCP, lms, o);
}
/* Linear BT.2020 (nits) straight to LMS, keeping slightly negative components: I and ITP chroma. */
DM_STATIC void dm_itp_lin(const double lin[3], double *I, double *S) {
  double lms[3], v[3];
  dm_mat_vec(DM_RGB2LMS, lin, lms);
  for (int i = 0; i < 3; i++) lms[i] = dm_pq_oetf(dm_maxd(lms[i], 0.0));
  dm_mat_vec(DM_LMS2ICTCP, lms, v);
  *I = v[0]; *S = sqrt(0.25 * v[1] * v[1] + v[2] * v[2]);
}

/* ------------------------------------------------------------------ spatial detail stage */
/* Separable Gaussian sigma_x = 2 (taps -5..5), sigma_y = 1 (taps -2..2), edge replication.
 *   D = I_in - I_pw; B = G*D; a = 4 (sx^2 |Dx*D| + sy^2 |Dy*D|) with derivative-of-Gaussian kernels;
 *   r = clip(1 + w - a, 0, 1); out = I_pw + r (D - B).  w <= -1: out = I_pw. */
#define DM_DET_RX 5
#define DM_DET_RY 2
DM_STATIC void dm_detail_kernels(double gx[2*DM_DET_RX+1], double dgx[2*DM_DET_RX+1],
                                 double gy[2*DM_DET_RY+1], double dgy[2*DM_DET_RY+1]) {
  double sx = 2.0, sy = 1.0, s;
  s = 0; for (int j = -DM_DET_RX; j <= DM_DET_RX; j++) s += (gx[j+DM_DET_RX] = exp(-j*j/(2*sx*sx)));
  for (int j = -DM_DET_RX; j <= DM_DET_RX; j++) { gx[j+DM_DET_RX] /= s; dgx[j+DM_DET_RX] = -j/(sx*sx)*gx[j+DM_DET_RX]; }
  s = 0; for (int j = -DM_DET_RY; j <= DM_DET_RY; j++) s += (gy[j+DM_DET_RY] = exp(-j*j/(2*sy*sy)));
  for (int j = -DM_DET_RY; j <= DM_DET_RY; j++) { gy[j+DM_DET_RY] /= s; dgy[j+DM_DET_RY] = -j/(sy*sy)*gy[j+DM_DET_RY]; }
}
/* correlate (scipy.ndimage.convolve1d convention: out[i] = sum_j k[j] * in[i - (j - r)]... see note) */
/* scipy convolve1d(a, k) computes out[i] = sum_j k[j] a[i + r - j] (true convolution, kernel flipped).
 * The Gaussian is symmetric; the derivative kernel is odd, so the sign flips relative to correlation;
 * only |.| of the derivative responses is used, so the convention does not matter. */
DM_STATIC int dm_detail(const double *Iin, const double *Ipw, int w, int h, double weight, double *out) {
  size_t n = (size_t)w * h;
  if (weight <= -1.0) { memcpy(out, Ipw, n * sizeof(double)); return 1; }
  double gx[2*DM_DET_RX+1], dgx[2*DM_DET_RX+1], gy[2*DM_DET_RY+1], dgy[2*DM_DET_RY+1];
  dm_detail_kernels(gx, dgx, gy, dgy);
  double *D = malloc(n * sizeof(double)), *tg = malloc(n * sizeof(double)), *td = malloc(n * sizeof(double));
  if (!D || !tg || !td) { free(D); free(tg); free(td); return 0; }
  for (size_t i = 0; i < n; i++) D[i] = Iin[i] - Ipw[i];
  /* horizontal pass: tg = gx*D, td = dgx*D */
  for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
    double a = 0, b = 0;
    for (int j = -DM_DET_RX; j <= DM_DET_RX; j++) {
      int xx = x - j; xx = xx < 0 ? 0 : (xx >= w ? w - 1 : xx);
      double v = D[(size_t)y * w + xx];
      a += gx[j+DM_DET_RX] * v; b += dgx[j+DM_DET_RX] * v;
    }
    tg[(size_t)y * w + x] = a; td[(size_t)y * w + x] = b;
  }
  /* vertical pass: B = gy*tg, Dx = gy*td, Dy = dgy*tg */
  for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
    double B = 0, dx = 0, dy = 0;
    for (int j = -DM_DET_RY; j <= DM_DET_RY; j++) {
      int yy = y - j; yy = yy < 0 ? 0 : (yy >= h ? h - 1 : yy);
      size_t k = (size_t)yy * w + x;
      B += gy[j+DM_DET_RY] * tg[k]; dx += gy[j+DM_DET_RY] * td[k]; dy += dgy[j+DM_DET_RY] * tg[k];
    }
    size_t i = (size_t)y * w + x;
    double a = 4.0 * (4.0 * fabs(dx) + 1.0 * fabs(dy));
    double r = dm_clampd(1.0 + weight - a, 0.0, 1.0);
    out[i] = Ipw[i] + r * (D[i] - B);
  }
  free(D); free(tg); free(td);
  return 1;
}

/* ================================================================== CM v4 */
typedef struct {
  double lift, gain, gamma, cw, sg, mcb, hc, sat[6], hue[6];
} dm_cm4_trim;

/* Trim interpolation across target displays (L8 TIDs). trims[i] holds the trim for a target
 * of max PQ tpq[i] (any order). Tone trims (SOP, mcb, hc) are linear in target-max PQ, with SOP
 * interpolated in (gain, offset = lift(1+gain/2), power = (2-gamma)/(2+gamma)); colour trims
 * (cw, sg, six-vector) are linear in F(PQ) = |0.9669 - PQ|^2.027. Held below the lowest trim
 * target, faded to zero at the source mastering peak above the highest. */
#define DM_FC 0.9669
#define DM_FG 2.027
DM_STATIC double dm_fcol(double x) { return pow(fabs(DM_FC - x), DM_FG); }
DM_STATIC dm_cm4_trim dm_cm4_mix(const dm_cm4_trim *a, const dm_cm4_trim *b, double t, double tc) {
  dm_cm4_trim o;
  o.gain = a->gain * (1 - t) + b->gain * t;
  o.mcb = a->mcb * (1 - t) + b->mcb * t;
  o.hc = a->hc * (1 - t) + b->hc * t;
  o.cw = a->cw * (1 - tc) + b->cw * tc;
  o.sg = a->sg * (1 - tc) + b->sg * tc;
  for (int k = 0; k < 6; k++) { o.sat[k] = a->sat[k]*(1-tc) + b->sat[k]*tc; o.hue[k] = a->hue[k]*(1-tc) + b->hue[k]*tc; }
  double Oa = a->lift * (1 + a->gain / 2), Ob = b->lift * (1 + b->gain / 2);
  double Pa = (2 - a->gamma) / (2 + a->gamma), Pb = (2 - b->gamma) / (2 + b->gamma);
  double O = Oa * (1 - t) + Ob * t, P = Pa * (1 - t) + Pb * t;
  o.lift = O / (1 + o.gain / 2); o.gamma = 2 * (1 - P) / (1 + P);
  return o;
}
DM_STATIC dm_cm4_trim dm_cm4_interp(const dm_cm4_trim *trims, const double *tpq, int n, double tmax, double smast) {
  dm_cm4_trim zero; memset(&zero, 0, sizeof(zero));
  if (n <= 0) return zero;
  int ord[64]; if (n > 64) n = 64;
  for (int i = 0; i < n; i++) ord[i] = i;
  for (int i = 1; i < n; i++) for (int j = i; j > 0 && tpq[ord[j]] < tpq[ord[j-1]]; j--) { int t = ord[j]; ord[j] = ord[j-1]; ord[j-1] = t; }
  if (tmax <= tpq[ord[0]]) return trims[ord[0]];
  for (int i = 0; i + 1 < n; i++) {
    double x0 = tpq[ord[i]], x1 = tpq[ord[i+1]];
    if (x0 <= tmax && tmax <= x1) {
      if (x1 - x0 <= 0) return trims[ord[i+1]];
      double t = (tmax - x0) / (x1 - x0), tc = (dm_fcol(tmax) - dm_fcol(x0)) / (dm_fcol(x1) - dm_fcol(x0));
      return dm_cm4_mix(&trims[ord[i]], &trims[ord[i+1]], t, tc);
    }
  }
  double x0 = tpq[ord[n-1]];
  if (smast <= x0) return trims[ord[n-1]];
  if (tmax >= smast) return zero;
  double t = (tmax - x0) / (smast - x0), tc = (dm_fcol(tmax) - dm_fcol(x0)) / (dm_fcol(smast) - dm_fcol(x0));
  return dm_cm4_mix(&trims[ord[n-1]], &zero, t, tc);
}

/* Colour trims across gamuts. Every display (gamut G, peak PQ) has a size
 * V = K_G + A_G*|c_G - PQ|^2.027 (units: V(BT.709, 100 nits) = 1, V(P3, 4000 nits) = 0; BT.2020 is
 * the F coordinate above up to an affine map). Colour trims (cw, sg, six-vector) are piecewise
 * linear in V through the trim targets, held for displays smaller than the smallest target, and
 * fade to zero at the mastering display (its gamut and peak). Tone trims are unchanged. */
DM_STATIC double dm_display_size(enum dm_prim g, double pq) {
  static const double c[3] = {0.966900, 0.981273, 0.960873}, A[3] = {3.439551, 3.239306, 2.869631},
                      K[3] = {-0.193660, -0.018673, 0.424750};   /* DM_BT2020, DM_P3D65, DM_BT709 */
  return K[g] + A[g] * pow(fabs(c[g] - pq), DM_FG);
}
DM_STATIC void dm_cm4_colour_mix(dm_cm4_trim *o, const dm_cm4_trim *a, const dm_cm4_trim *b, double u) {
  o->cw = a->cw * (1 - u) + b->cw * u; o->sg = a->sg * (1 - u) + b->sg * u;
  for (int k = 0; k < 6; k++) { o->sat[k] = a->sat[k] * (1 - u) + b->sat[k] * u; o->hue[k] = a->hue[k] * (1 - u) + b->hue[k] * u; }
}
DM_STATIC dm_cm4_trim dm_cm4_interp_gamut(const dm_cm4_trim *trims, const double *tpq, const enum dm_prim *tprim,
                                          int n, double tmax, double smast, enum dm_prim rprim, enum dm_prim mprim) {
  dm_cm4_trim out = dm_cm4_interp(trims, tpq, n, tmax, smast), zero;
  memset(&zero, 0, sizeof(zero));
  if (n <= 0) return out;
  if (n > 64) n = 64;
  double V[64], VR = dm_display_size(rprim, tmax), VS = dm_display_size(mprim, smast);
  int ord[64];
  for (int i = 0; i < n; i++) { ord[i] = i; V[i] = dm_display_size(tprim[i], tpq[i]); }
  for (int i = 1; i < n; i++) for (int j = i; j > 0 && V[ord[j]] > V[ord[j-1]]; j--) { int t = ord[j]; ord[j] = ord[j-1]; ord[j-1] = t; }
  if (V[ord[0]] <= VS) { dm_cm4_colour_mix(&out, &trims[ord[0]], &trims[ord[0]], 0); return out; }  /* no target smaller than the master: hold */
  int m = 0; while (m < n && V[ord[m]] > VS) m++;                 /* nodes ord[0..m-1], then the master */
  if (VR >= V[ord[0]]) { dm_cm4_colour_mix(&out, &trims[ord[0]], &trims[ord[0]], 0); return out; }
  if (VR <= VS) { dm_cm4_colour_mix(&out, &zero, &zero, 0); return out; }
  for (int i = 0; i < m; i++) {
    double v0 = V[ord[i]], v1 = i + 1 < m ? V[ord[i+1]] : VS;
    const dm_cm4_trim *b = i + 1 < m ? &trims[ord[i+1]] : &zero;
    if (v1 <= VR && VR <= v0) { dm_cm4_colour_mix(&out, &trims[ord[i]], b, v0 > v1 ? (v0 - VR) / (v0 - v1) : 1); return out; }
  }
  return out;
}

typedef struct {
  double SMin, SMid, SMax, Tmin, Tmax;       /* curve inputs (SMax after highlight clipping) */
  double TMin, TMid, TMax, sMin, sMid, sMax;  /* main curve */
  dm_cm4_trim tr;
  double a, b, SR, sgf;                      /* primary-saturation coefficients */
  double anc[6];                             /* six-vector anchor hues (deg, ITP) */
  int six;
  double to_t[3][3], from_t[3][3], lms2rgb[3][3], ictcp2lms[3][3], Lmin, Lmax;
} dm_cm4_state;

DM_STATIC double dm_herm(double T, double y1, double y2, double s1, double s2, double dx) {
  double T2 = T * T, T3 = T2 * T;
  return (2*T3 - 3*T2 + 1)*y1 + (T3 - 2*T2 + T)*dx*s1 + (-2*T3 + 3*T2)*y2 + (T3 - T2)*dx*s2;
}
/* first-pass Hermite C0 (mid anchor) */
DM_STATIC double dm_cm4_c0(double x, double SMin, double SMax, double Tmin, double Tmax) {
  double k0 = Tmin + 0.1, yL = Tmin + 0.02, yE = Tmax - 0.1, s0 = dm_mind(1.0, 2 * (Tmax - Tmin));
  if (x <= k0) {
    double d = k0 - SMin; if (d <= 0) return yL;
    double r = (k0 - yL) / d, sL = dm_mind(3 * r, r * r);
    return dm_herm((x - SMin) / d, yL, k0, sL, s0, d);
  }
  double d = SMax - k0; if (d <= 0) return yE;
  double r = (yE - k0) / d, sE = dm_mind(dm_mind(3 * r, 1.0), r * r);
  return dm_herm((x - k0) / d, k0, yE, s0, sE, d);
}
DM_STATIC double dm_cm4_tmid(double SMin, double SMid, double SMax, double Tmin, double Tmax) {
  double E = dm_maxd(0.0, SMax - Tmax), A = dm_maxd(0.0, Tmin - SMin);
  double c = dm_cm4_c0(SMid, SMin, SMax, Tmin, Tmax);
  if (SMid <= Tmin + 0.1) return SMid + dm_mind(dm_maxd(c - SMid, 0.0), A);
  return SMid - dm_mind(dm_maxd(SMid - c, 0.0), E);
}
/* unclamped main curve */
DM_STATIC double dm_cm4_curve(const dm_cm4_state *s, double x) {
  if (x < s->SMin) return s->TMin + (x - s->SMin) * s->sMin;
  if (x <= s->SMid) return dm_herm((x - s->SMin) / (s->SMid - s->SMin), s->TMin, s->TMid, s->sMin, s->sMid, s->SMid - s->SMin);
  if (x <= s->SMax) return dm_herm((x - s->SMid) / (s->SMax - s->SMid), s->TMid, s->TMax, s->sMid, s->sMax, s->SMax - s->SMid);
  return s->TMax + (x - s->SMax) * s->sMax;
}
/* SOP trims in target-normalised PQ */
DM_STATIC double dm_cm4_sop(const dm_cm4_state *s, double v) {
  const dm_cm4_trim *t = &s->tr;
  if (t->lift == 0 && t->gain == 0 && t->gamma == 0) return v;
  double n = (v - s->Tmin) / (s->Tmax - s->Tmin);
  n = dm_maxd((1 + t->gain) * n + t->lift * (1 + t->gain / 2) * (1 - n), 0.0);
  n = pow(n, (2 - t->gamma) / (2 + t->gamma));
  return n * (s->Tmax - s->Tmin) + s->Tmin;
}
DM_STATIC double dm_cm4_C(const dm_cm4_state *s, double v) { return dm_cm4_sop(s, dm_cm4_curve(s, v)); }
DM_STATIC double dm_satscale(double Io, double Ii) { return (pow(Io - 1, 3) + 1) / (pow(Ii - 1, 3) + 1); }
DM_STATIC double dm_c4meso(double I) { double c3 = (I * 4095 - 25) / (900 - 25); double t = dm_clampd(1 - c3, 0, 1); return 1 - t * t; }

/* l1 = (min, avg, max) PQ including L3 offsets; tmin/tmax target PQ; tprim/sprim target/source primaries */
DM_STATIC int dm_cm4_prepare(dm_cm4_state *s, const double l1[3], double tmin, double tmax,
                             const dm_cm4_trim *trim, enum dm_prim tprim, enum dm_prim sprim) {
  memset(s, 0, sizeof(*s));
  if (trim) s->tr = *trim;
  s->Tmin = tmin; s->Tmax = tmax;
  /* anchors held one 12-bit code apart inside [0,1] (as measured on real titles), also after the
   * highlight-clipping shift, so black, flat and clipped frames stay mappable */
  const double q = 1.0 / 4095;
  s->SMid = dm_mind(dm_maxd(l1[1], q), 1 - q);
  s->SMin = dm_mind(l1[0], s->SMid - q);
  s->SMax = dm_maxd(dm_mind(l1[2] + s->tr.hc / 2, 1.0), s->SMid + q);
  if (!(s->SMin < s->SMid && s->SMid < s->SMax && tmin < tmax)) return 0;
  double TM = dm_cm4_tmid(s->SMin, s->SMid, s->SMax, tmin, tmax);
  s->TMid = TM;
  s->TMax = dm_mind(TM + s->SMax - s->SMid, tmax);
  s->TMin = dm_maxd(TM - s->SMid + s->SMin, tmin);
  double mms = 3 * (TM - s->TMin) / (s->SMid - s->SMin), mxs = 3 * (s->TMax - TM) / (s->SMax - s->SMid);
  double rl = (TM - s->TMin) / (s->SMid - s->SMin), rh = (s->TMax - TM) / (s->SMax - s->SMid);
  s->sMin = dm_mind(mms, rl * rl);
  s->sMax = dm_mind(dm_mind(mxs, 1.0), rh * rh);
  s->sMid = dm_mind(dm_mind(mms, mxs), 1 - s->SMid + TM) + 0.5 * s->tr.mcb;
  /* red-primary terms: source red at SMax in source primaries, target red at Tmax in target primaries */
  double cs[3][3], ct[3][3], lin[3], IR, SR, ITR, STR;
  dm_conv(sprim, DM_BT2020, cs); dm_conv(tprim, DM_BT2020, ct);
  { double e[3] = {dm_pq_eotf(s->SMax), 0, 0}; dm_mat_vec(cs, e, lin); }
  if (sprim == DM_BT2020) { double rgb[3] = {s->SMax, 0, 0}, v[3]; dm_rgbpq_to_ictcp(rgb, v); IR = v[0]; SR = sqrt(0.25*v[1]*v[1] + v[2]*v[2]); }
  else dm_itp_lin(lin, &IR, &SR);
  if (tprim == DM_BT2020) { double rgb[3] = {s->TMax, 0, 0}, v[3]; dm_rgbpq_to_ictcp(rgb, v); ITR = v[0]; STR = sqrt(0.25*v[1]*v[1] + v[2]*v[2]); }
  else { double e[3] = {dm_pq_eotf(tmax), 0, 0}; dm_mat_vec(ct, e, lin); dm_itp_lin(lin, &ITR, &STR); }
  double IcR = dm_cm4_C(s, IR);
  s->sgf = 1 + s->tr.sg / 2;
  s->SR = SR;
  s->a = dm_mind(0.0, (ITR / IcR - 1) / (SR * SR * SR));
  s->b = dm_mind(0.0, (STR / (SR * dm_satscale(IcR, IR) * s->sgf) - 1) / (SR * SR * SR));
  /* six-vector anchors: hues of BT.2020 R, Y, G, C, B, M at PQ 1 */
  static const double PR[6][3] = {{1,0,0},{1,1,0},{0,1,0},{0,1,1},{0,0,1},{1,0,1}};
  for (int k = 0; k < 6; k++) {
    double v[3]; dm_rgbpq_to_ictcp(PR[k], v);
    double hdeg = atan2(v[2], v[1] / 2) * 180.0 / DM_PI; if (hdeg < 0) hdeg += 360; s->anc[k] = hdeg;
  }
  for (int k = 0; k < 6; k++) if (s->tr.sat[k] != 0 || s->tr.hue[k] != 0) s->six = 1;
  /* output */
  dm_conv(DM_BT2020, tprim, s->to_t); dm_conv(tprim, DM_BT2020, s->from_t);
  dm_mat_inv(DM_RGB2LMS, s->lms2rgb); dm_mat_inv(DM_LMS2ICTCP, s->ictcp2lms);
  s->Lmin = dm_pq_eotf(tmin); s->Lmax = dm_pq_eotf(tmax);
  return 1;
}

/* six-vector weights: linear between angularly adjacent anchors */
DM_STATIC void dm_six_weights(const double anc[6], double h, double W[6]) {
  int ord[6] = {0,1,2,3,4,5};
  for (int i = 1; i < 6; i++) for (int j = i; j > 0 && anc[ord[j]] < anc[ord[j-1]]; j--) { int t = ord[j]; ord[j] = ord[j-1]; ord[j-1] = t; }
  for (int k = 0; k < 6; k++) W[k] = 0;
  /* circular segments between angularly adjacent anchors; robust at the 360-degree seam */
  for (int k = 0; k < 6; k++) {
    int i = ord[k], j = ord[(k + 1) % 6];
    double span = fmod(anc[j] - anc[i] + 720.0, 360.0), d = fmod(h - anc[i] + 720.0, 360.0);
    if (d < span || (d >= 360.0 - 1e-9 && span > 0)) {
      if (d >= 360.0 - 1e-9) d = 0;
      double t = d / span; W[i] = 1 - t; W[j] = t; return;
    }
  }
}

/* per pixel: PQ BT.2020 RGB -> pre-clip ICtCp */
DM_STATIC void dm_cm4_pixel(const dm_cm4_state *s, const double rgb[3], double o[3]) {
  double ic[3]; dm_rgbpq_to_ictcp(rgb, ic);
  double I = ic[0], S = sqrt(0.25 * ic[1] * ic[1] + ic[2] * ic[2]);
  double S3 = pow(dm_mind(S, s->SR), 3);
  double Ip = dm_cm4_C(s, I);
  o[0] = Ip * (1 + s->a * S3) * (1 + 2 * s->tr.cw * S);
  double ss = dm_satscale(Ip, I);
  double sI = dm_maxd(dm_mind(1.0, ss), ss * dm_c4meso(I));
  if (s->Tmin > s->SMin) { double c1 = dm_clampd((Ip - s->Tmin) / (s->Tmin - s->SMin), 0, 1); sI *= 1 - (1 - c1) * (1 - c1); }
  double f = sI * s->sgf * (1 + 1.5 * s->tr.cw * S) * (1 + s->b * S3);
  o[1] = ic[1] * f; o[2] = ic[2] * f;
  if (s->six) {
    double h = atan2(o[2], o[1] / 2) * 180.0 / DM_PI; if (h < 0) h += 360;
    double W[6], smh = 1, r1 = 0; dm_six_weights(s->anc, h, W);
    for (int k = 0; k < 6; k++) { smh += W[k] * s->tr.sat[k]; r1 += W[k] * s->tr.hue[k]; }
    r1 *= 0.5; double r2 = 1 - r1 * r1 / 2, T = o[1] / 2, P = o[2];
    o[1] = 2 * smh * (T * r2 - P * r1); o[2] = smh * (T * r1 + P * r2);
  }
}
/* pre-clip ICtCp (I possibly replaced by the detail stage) -> PQ BT.2020 RGB, clipped in target primaries */
DM_STATIC void dm_cm4_finish(const dm_cm4_state *s, const double o[3], double rgb[3]) {
  double lmsp[3], lms[3], lin[3], lt[3];
  dm_mat_vec(s->ictcp2lms, o, lmsp);
  for (int i = 0; i < 3; i++) lms[i] = dm_pq_eotf_ext(lmsp[i]);
  dm_mat_vec(s->lms2rgb, lms, lin);
  dm_mat_vec(s->to_t, lin, lt);
  for (int i = 0; i < 3; i++) lt[i] = dm_clampd(lt[i], s->Lmin, s->Lmax);
  dm_mat_vec(s->from_t, lt, lin);
  for (int i = 0; i < 3; i++) rgb[i] = dm_pq_oetf(dm_maxd(lin[i], 0.0));
}

/* ================================================================== CM v2.9 */
typedef struct { double lift, gain, gamma, cw, sg, ms; } dm_cm29_trim;   /* L2 Trim fields 3..8 */
typedef struct { double peak_nits, min_nits; enum dm_prim prim; double floor_pq; } dm_cm29_target;

typedef struct {
  double a, b, c, n, Tm, TM, xs0;           /* rational curve */
  dm_cm29_trim tr; dm_cm29_target tg;
  double rgb2lms[3][3], lms2rgb[3][3], ipt2lms[3][3], to_t[3][3], from_t[3][3];
  double native_black, native_span, decode_black, decode_span;
  double grid_origin, cube_min;             /* stock gamut cube and resolved output black */
} dm_cm29_state;

typedef struct { double ipt[3], C, I, Ib, Io; } dm_cm29_pix;

static const double DM_M2020_XYZ[3][3] = {{0.636958, 0.144617, 0.168881}, {0.262700, 0.677998, 0.059302}, {0.0, 0.028073, 1.060985}};
static const double DM_HPE[3][3] = {{0.4002, 0.7076, -0.0808}, {-0.2263, 1.1653, 0.0457}, {0.0, 0.0, 0.9182}};
static const double DM_IPT[3][3] = {{0.4, 0.4, 0.2}, {4.4550, -4.8510, 0.3960}, {0.8056, 0.3572, -1.1628}};
DM_STATIC double dm_pq_nits(double n) { return n > 0 ? dm_pq_oetf(n) : 0.0; }
DM_STATIC int dm_solve3(double A[3][3], const double b[3], double x[3]) {
  double Ai[3][3]; if (!dm_mat_inv(A, Ai)) return 0; dm_mat_vec(Ai, b, x); return 1;
}
DM_STATIC int dm_cm29_prepare(dm_cm29_state *s, const double l1[3], double smin, double spk,
                              const dm_cm29_target *tg, const dm_cm29_trim *trim) {
  memset(s, 0, sizeof(*s));
  if (trim) s->tr = *trim;
  s->tg = *tg;
  /* black and flat frames: crush and clip held one 12-bit code apart inside [0,1] */
  const double q = 1.0 / 4095;
  double crush = dm_mind(dm_maxd(l1[0], 0.0), 1 - q), mid = l1[1], clip = dm_maxd(dm_mind(l1[2], 1.0), crush + q);
  double Sm = dm_pq_nits(smin), SM = dm_pq_nits(spk), Tm = dm_pq_nits(tg->min_nits), TM = dm_pq_nits(tg->peak_nits);
  if (!(clip > crush && SM > Sm && TM > Tm)) return 0;
  double ratio = dm_mind(1.0, sqrt((TM + Tm) / (SM + Sm)));
  double n = 3 * sqrt(1 / ratio);
  double coef[3];
  /* Some anchor sets (very dark shots with a few codes between crush and clip, and some bright
   * shots with a high minimum; about 1-2 % of an L1 grid) fit a rational with a pole or a downturn
   * inside [0,1]. Such a fit is rejected and the shot's clip widened (doubling its span) until the
   * curve is increasing and pole-free on [0,1]; fits accepted first time are unchanged. */
  for (int tries = 0;; tries++) {
    double m = dm_mind(dm_maxd(mid, crush + 0.25 * (clip - crush)), crush + 0.75 * (clip - crush));
    double key = 2 * (m - Sm) / (SM - Sm), shift = m * (1 - ratio) * key;
    double xs[3] = {dm_maxd(crush, 1e-9), m, clip};
    double ts[3] = {dm_maxd(crush - shift, Tm), m - shift, dm_mind(clip - shift, TM)};
    double A[3][3], w[3];
    for (int i = 0; i < 3; i++) { w[i] = ts[i] * ts[i] * ts[i]; double xn = pow(xs[i], n); A[i][0] = 1; A[i][1] = xn; A[i][2] = -xn * w[i]; }
    int ok = dm_solve3(A, w, coef);
    /* (a + b x^n)/(1 + c x^n) increases iff b - a c > 0, and has no pole on [0,1] iff 1 + c > 0 */
    if (ok && coef[1] - coef[0] * coef[2] > 0 && 1 + coef[2] > 0) { s->xs0 = xs[0]; break; }
    if (clip >= 1 || tries >= 24) {
      /* nothing left to widen (a frame at the top of the range): the two-anchor curve through
       * black and clip, y^3 = a + b x^n, increasing (or flat) and pole-free */
      double x0 = pow(xs[0], n), x2 = pow(xs[2], n), y0 = w[0], y2 = dm_maxd(w[2], w[0]);
      coef[2] = 0; coef[1] = x2 > x0 ? (y2 - y0) / (x2 - x0) : 0; coef[0] = y0 - coef[1] * x0;
      s->xs0 = xs[0]; break;
    }
    clip = dm_mind(1.0, crush + 2 * (clip - crush));
  }
  s->a = coef[0]; s->b = coef[1]; s->c = coef[2]; s->n = n; s->Tm = Tm; s->TM = TM;
  double xyz2lms[3][3]; memcpy(xyz2lms, DM_HPE, sizeof(xyz2lms));
  dm_mat_mul(xyz2lms, DM_M2020_XYZ, s->rgb2lms); dm_mat_inv(s->rgb2lms, s->lms2rgb); dm_mat_inv(DM_IPT, s->ipt2lms);
  dm_conv(DM_BT2020, tg->prim, s->to_t); dm_conv(tg->prim, DM_BT2020, s->from_t);
  /* The stock 100-nit BT.1886 gamut cube is defined at 0.005 nits. A resolved
   * display black changes the tone curve and output decoding, not those vertices. */
  s->cube_min = tg->prim == DM_BT709 ? 0.005 : 0;
  s->native_black = tg->prim == DM_BT709 ? pow(s->cube_min, 1.0 / 2.4) : 0;
  s->native_span = tg->prim == DM_BT709 ? pow(tg->peak_nits, 1.0 / 2.4) - s->native_black : 1;
  s->decode_black = tg->prim == DM_BT709 ? pow(tg->min_nits, 1.0 / 2.4) : 0;
  s->decode_span = tg->prim == DM_BT709 ? pow(tg->peak_nits, 1.0 / 2.4) - s->decode_black : 1;
  s->grid_origin = dm_pq_nits(s->cube_min);
  if (!(s->native_span > 0 && s->decode_span > 0)) return 0;
  return 1;
}
DM_STATIC double dm_cm29_curve(const dm_cm29_state *s, double x) {
  double xn = pow(dm_maxd(x, 0.0), s->n);
  double v = cbrt(dm_maxd((s->a + s->b * xn) / (1 + s->c * xn), 0.0));
  return dm_clampd(v, s->Tm, s->TM);
}
DM_STATIC double dm_cm29_sop(const dm_cm29_state *s, double y) {
  const dm_cm29_trim *t = &s->tr;
  if (t->lift == 0 && t->gain == 0 && t->gamma == 0) return y;
  double v = pow(dm_maxd((1 + t->gain / 2) * ((1 - t->lift / 2) * y + t->lift / 2), 0.0), 2 / (2 + t->gamma));
  return dm_clampd(v, s->Tm, s->TM);
}
DM_STATIC double dm_cm29_tc(const dm_cm29_state *s, double v) { return dm_cm29_sop(s, dm_cm29_curve(s, v)); }
/* per pixel, before the spatial stage: IPT, chroma, pointwise base (Ib) and chroma-weighted (Io) intensities */
DM_STATIC void dm_cm29_pixel_pre(const dm_cm29_state *s, const double rgb[3], dm_cm29_pix *p) {
  double lin[3] = {dm_pq_eotf(rgb[0]), dm_pq_eotf(rgb[1]), dm_pq_eotf(rgb[2])}, lms[3];
  dm_mat_vec(s->rgb2lms, lin, lms);
  for (int i = 0; i < 3; i++) lms[i] = dm_pq_oetf(dm_maxd(lms[i], 0.0));
  dm_mat_vec(DM_IPT, lms, p->ipt);
  p->I = p->ipt[0]; p->C = hypot(p->ipt[1], p->ipt[2]);
  p->Ib = dm_cm29_tc(s, p->I);
  p->Io = s->tr.cw != 0 ? dm_cm29_tc(s, p->I + 0.5 * s->tr.cw * p->C) : p->Ib;
}
/* The gamut operation is trilinear in target-peak-normalized IPT. Evaluate only its
 * eight analytic corners: inverse IPT, PQ EOTF, target matrix/cube, native encoding.
 * No measured output tables are required. The zero-intensity plane is black even
 * at virtual (nonphysical) chroma corners. BT.1886 uses the stock cube black as origin. */
DM_STATIC void dm_cm29_grid_vertex(const dm_cm29_state *s, const double q[3], double out[3]) {
  if (q[0] <= s->grid_origin) {
    for (int k = 0; k < 3; k++) out[k] = 0;
    return;
  }
  double lmsp[3], lms[3], rgb[3], target[3];
  dm_mat_vec(s->ipt2lms, q, lmsp);
  for (int k = 0; k < 3; k++) lms[k] = dm_pq_eotf(dm_clampd(lmsp[k], 0, 1));
  dm_mat_vec(s->lms2rgb, lms, rgb);
  dm_mat_vec(s->to_t, rgb, target);
  const double lo = s->cube_min;
  for (int k = 0; k < 3; k++) {
    double v = dm_clampd(target[k], lo, s->tg.peak_nits);
    out[k] = s->tg.prim == DM_BT709
      ? (pow(v, 1.0 / 2.4) - s->native_black) / s->native_span : dm_pq_oetf(v);
  }
}
DM_STATIC void dm_cm29_finish(const dm_cm29_state *s, const dm_cm29_pix *p, double Ib, double Io, double rgb[3]) {
  Ib = dm_maxd(Ib, 0); Io = dm_maxd(Io, 0);
  const double Ii = dm_maxd(p->I, 1e-6);
  /* Colorfulness compensation, US20160005349A1 eqs. 11-14 (k1=.5, k2=1).
   * Bound the complete scale before the creative saturation trim. */
  const double sc = dm_clampd((Ib / Ii) * (2 + Ii) / (2 + Ib), .5, 2) * (1 + .5 * s->tr.sg);
  const double q[3] = {Io, p->ipt[1] * sc, p->ipt[2] * sc};
  const double step[3] = {s->TM / 64, s->TM / 32, s->TM / 32};
  double i[3], f[3], acc[3] = {0, 0, 0};
  for (int k = 0; k < 3; k++) {
    double u = (q[k] - (k == 0 ? s->grid_origin : 0)) / step[k];
    i[k] = floor(u); f[k] = u - i[k];
  }
  for (int bits = 0; bits < 8; bits++) {
    double node[3], v[3], w = 1;
    for (int k = 0; k < 3; k++) {
      const int bit = (bits >> k) & 1;
      node[k] = (i[k] + bit) * step[k] + (k == 0 ? s->grid_origin : 0);
      w *= bit ? f[k] : 1 - f[k];
    }
    dm_cm29_grid_vertex(s, node, v);
    for (int k = 0; k < 3; k++) acc[k] += w * v[k];
  }
  double target[3], lin[3];
  for (int k = 0; k < 3; k++) target[k] = s->tg.prim == DM_BT709
    ? pow(acc[k] * s->decode_span + s->decode_black, 2.4) : dm_pq_eotf(acc[k]);
  dm_mat_vec(s->from_t, target, lin);
  for (int k = 0; k < 3; k++) rgb[k] = dm_maxd(dm_pq_oetf(dm_clampd(lin[k], 0, s->tg.peak_nits)), s->tg.floor_pq);
}

#endif
