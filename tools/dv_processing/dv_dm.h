// Closed-form Dolby Vision display management for display-targeted exports.
// CM v4 for shots whose RPU carries CM v4.0 metadata (Level254 / Level8), CM v2.9 otherwise.
// The equations, their derivation against the licensed reference renderer and the evidence live in
// rog_hdr_dv CM4_CLOSED_FORM.md and CM29_CLOSED_FORM.md; dm_closed_form.h is the
// double-precision CPU reference the shader is checked against (controls/dm_shader_control.c).
//
// Per frame the CPU prepares the shot state (curve anchors, trims interpolated to the target,
// primary-saturation coefficients) from the RPU; the GPU runs three passes over the native
// BT.2020/PQ master texture:
//   1. pointwise mapping -> intensity differences D (what the tone curve removed)
//   2. horizontal Gaussian / derivative-of-Gaussian of D
//   3. vertical pass, detail stage, colour finish and target-gamut clip -> BT.2020/PQ RGB.
// CM v2.9 also evaluates its 65x33x33 output lattice once per frame (dv_dm_body_lattice) for the
// vertices pixels reach most, instead of eight vertex evaluations per pixel; the vertices are the
// same float32 computation, so the result is the same.
#ifndef PLURX_DV_DM_H
#define PLURX_DV_DM_H
#include <math.h>
#include <stdbool.h>
#include <stdio.h>
#include <libplacebo/shaders/custom.h>
#include <libplacebo/dispatch.h>
#include "dm_closed_form.h"

#define DV_DM_METHOD_CM4 "dolby-cm4-closed-form-v1"
#define DV_DM_METHOD_CM29 "dolby-cm29-closed-form-v4"

enum { DV_DM_U = 24 };   // vec4 uniforms
typedef struct {
  int algo;              // 4 or 29
  float u[DV_DM_U][4];
  double weight;         // spatial detail weight (<= -1: detail stage off; controls only)
  const char *trim;      // selection description for the frame report
  int trim_targets;
  dm_cm4_state c4;       // CPU reference states (controls; the shader uses only u[])
  dm_cm29_state c29;
} dv_dm_frame;

// Preset target displays referenced by L8 target_display_index (no L10 needed).
static inline bool dv_dm_preset(unsigned id, double *peak, double *min_nits, enum dm_prim *prim) {
  switch (id) {
    case 1: *peak = 100; *min_nits = 0.005; *prim = DM_BT709; return true;
    case 27: *peak = 600; *min_nits = 0; *prim = DM_P3D65; return true;
    case 28: *peak = 600; *min_nits = 0; *prim = DM_BT2020; return true;
    case 37: *peak = 2000; *min_nits = 0; *prim = DM_P3D65; return true;
    case 38: *peak = 2000; *min_nits = 0; *prim = DM_BT2020; return true;
    case 48: *peak = 1000; *min_nits = 0; *prim = DM_P3D65; return true;
    case 49: *peak = 1000; *min_nits = 0; *prim = DM_BT2020; return true;
    default: return false;
  }
}
static inline enum dm_prim dv_dm_l9_prim(unsigned index) {
  // dovi_tool PREDEFINED_COLORSPACE_PRIMARIES: 0 DCI-P3 D65, 1 BT.709, 2 BT.2020
  return index == 1 ? DM_BT709 : (index == 2 ? DM_BT2020 : DM_P3D65);
}
static inline double dv_dm_pq12(unsigned v) { return v / 4095.0; }
static inline double dv_dm_off(unsigned raw, double centre) { return ((double)raw - centre) / centre; }
// L2/L8 slope/offset/power -> lift/gain/gamma (inverse of dovi_tool's XML parser)
// Nonsensical codes (gain -2, power 0) are held just inside the finite range.
static inline void dv_dm_sop(unsigned slope, unsigned offset, unsigned power, double *L, double *G, double *Ga) {
  double A = slope / 2048.0 + 1, B = (offset - 2048.0) / 2048.0;
  *G = dm_maxd(A + B - 2, -1.99); *L = 2 * B / (*G + 2); *Ga = 2 * (2 / (dm_maxd(power, 1) / 2048.0 + 1) - 1);
}

static inline void dv_dm_put(float u[4], double a, double b, double c, double d) {
  u[0] = (float)a; u[1] = (float)b; u[2] = (float)c; u[3] = (float)d;
}
static inline void dv_dm_rows(float (*u)[4], const double m[3][3]) {
  for (int i = 0; i < 3; i++) dv_dm_put(u[i], m[i][0], m[i][1], m[i][2], 0);
}

// Pack the CPU states into the shader uniforms.
static inline void dv_dm_pack_cm4(dv_dm_frame *f, const dm_cm4_state *s) {
  f->algo = 4;
  dv_dm_put(f->u[0], s->SMin, s->SMid, s->SMax, s->Tmin);
  dv_dm_put(f->u[1], s->Tmax, s->TMin, s->TMid, s->TMax);
  dv_dm_put(f->u[2], s->sMin, s->sMid, s->sMax, s->sgf);
  dv_dm_put(f->u[3], s->tr.lift, s->tr.gain, s->tr.gamma, s->tr.cw);
  dv_dm_put(f->u[4], s->a, s->b, s->SR, s->six);
  dv_dm_put(f->u[5], s->Lmin, s->Lmax, f->weight, 0);
  // six-vector: anchors sorted ascending with their sat/hue values
  int ord[6] = {0, 1, 2, 3, 4, 5};
  for (int i = 1; i < 6; i++) for (int j = i; j > 0 && s->anc[ord[j]] < s->anc[ord[j-1]]; j--) { int t = ord[j]; ord[j] = ord[j-1]; ord[j-1] = t; }
  dv_dm_put(f->u[6], s->anc[ord[0]], s->anc[ord[1]], s->anc[ord[2]], s->anc[ord[3]]);
  dv_dm_put(f->u[7], s->anc[ord[4]], s->anc[ord[5]], 0, 0);
  dv_dm_put(f->u[8], s->tr.sat[ord[0]], s->tr.sat[ord[1]], s->tr.sat[ord[2]], s->tr.sat[ord[3]]);
  dv_dm_put(f->u[9], s->tr.sat[ord[4]], s->tr.sat[ord[5]], s->tr.hue[ord[0]], s->tr.hue[ord[1]]);
  dv_dm_put(f->u[10], s->tr.hue[ord[2]], s->tr.hue[ord[3]], s->tr.hue[ord[4]], s->tr.hue[ord[5]]);
  dv_dm_rows(f->u + 11, s->to_t);
  dv_dm_rows(f->u + 14, s->from_t);
  bool sop_on = s->tr.lift != 0 || s->tr.gain != 0 || s->tr.gamma != 0;
  dv_dm_put(f->u[17], sop_on, s->Tmin > s->SMin, 0, 0);
}
static inline void dv_dm_pack_cm29(dv_dm_frame *f, const dm_cm29_state *s) {
  f->algo = 29;
  dv_dm_put(f->u[0], s->a, s->b, s->c, s->n);
  // u[1].z: stock gamut cube black; resolved output black is packed separately.
  dv_dm_put(f->u[1], s->Tm, s->TM, s->cube_min, s->tg.peak_nits);
  dv_dm_put(f->u[2], s->tr.lift, s->tr.gain, s->tr.gamma, s->tr.cw);
  dv_dm_put(f->u[3], s->tr.sg, f->weight, s->tg.floor_pq, s->tg.prim == DM_BT709);
  dv_dm_rows(f->u + 4, s->rgb2lms);
  dv_dm_rows(f->u + 7, s->lms2rgb);
  dv_dm_put(f->u[10], s->grid_origin, s->native_black, s->native_span, 0);
  dv_dm_put(f->u[19], s->decode_black, s->decode_span, 0, 0);
  dv_dm_rows(f->u + 11, s->to_t);
  dv_dm_rows(f->u + 14, s->from_t);
  bool sop_on = s->tr.lift != 0 || s->tr.gain != 0 || s->tr.gamma != 0;
  dv_dm_put(f->u[17], sop_on, s->tr.cw != 0, 0, 0);
}

// Build the frame state from the RPU for a target of `nits` (PQ container), black `floor_nits`,
// primaries `tprim`; `smast_nits` is the title mastering peak (L6, else 4000). With `cm4_trims` /
// `cm29_trims` false that family's L8 / L2 creative trims are not applied (neutral trims, default
// MS weight): Dolby's untrimmed mapping of L1 (+ L3).
static inline const char *dv_dm_prepare(const DoviVdrDmData *dm, double nits, double floor_nits,
                                        enum dm_prim tprim, double smast_nits, bool cm4_trims, bool cm29_trims,
                                        dv_dm_frame *f) {
  memset(f, 0, sizeof(*f));
  if (!dm || !dm->dm_data.level1) return "missing L1";
  const DoviDmData *d = &dm->dm_data;
  const DoviExtMetadataBlockLevel1 *l1 = d->level1;
  double L1[3] = {dv_dm_pq12(l1->min_pq), dv_dm_pq12(l1->avg_pq), dv_dm_pq12(l1->max_pq)}, L3[3] = {0, 0, 0};
  if (d->level3) {
    L3[0] = dv_dm_off(d->level3->min_pq_offset, 2048); L3[1] = dv_dm_off(d->level3->avg_pq_offset, 2048);
    L3[2] = dv_dm_off(d->level3->max_pq_offset, 2048);
  }
  enum dm_prim sprim = d->level9 ? dv_dm_l9_prim(d->level9->source_primary_index) : DM_P3D65;
  double tmin = floor_nits > 0 ? dm_pq_oetf(floor_nits) : 0, tmax = dm_pq_oetf(nits), smast = dm_pq_oetf(smast_nits);
  bool cm4 = d->level254 != NULL || d->level8.len > 0;
  if (cm4) {
    // effective L1: L1 + L3, anchors held one 12-bit code apart (measured on real titles)
    double q = 1.0 / 4095, e[3] = {L1[0] + L3[0], L1[1] + L3[1], L1[2] + L3[2]};
    e[0] = dm_mind(e[0], e[1] - q); e[2] = dm_maxd(e[2], e[1] + q);
    dm_cm4_trim trims[32]; double tpq[32]; enum dm_prim tpr[32]; int n = 0;
    for (size_t i = 0; cm4_trims && i < d->level8.len && n < 32; i++) {
      const DoviExtMetadataBlockLevel8 *x = d->level8.list[i];
      double pk = 0, mn = 0; enum dm_prim tp = DM_BT2020;
      if (!dv_dm_preset(x->target_display_index, &pk, &mn, &tp)) {
        const DoviExtMetadataBlockLevel10 *t10 = NULL;
        for (size_t j = 0; j < d->level10.len; j++)
          if (d->level10.list[j]->target_display_index == x->target_display_index) t10 = d->level10.list[j];
        if (!t10) continue;   // target display not resolvable without L10: trim not used
        pk = dm_pq_eotf(dv_dm_pq12(t10->target_max_pq)); tp = dv_dm_l9_prim(t10->target_primary_index);
      }
      dm_cm4_trim *t = &trims[n]; memset(t, 0, sizeof(*t));
      dv_dm_sop(x->trim_slope, x->trim_offset, x->trim_power, &t->lift, &t->gain, &t->gamma);
      t->cw = dv_dm_off(x->trim_chroma_weight, 2048); t->sg = dv_dm_off(x->trim_saturation_gain, 2048);
      if (x->length > 10) t->mcb = dv_dm_off(x->target_mid_contrast, 2048);
      if (x->length > 12) t->hc = dv_dm_off(x->clip_trim, 2048);
      if (x->length > 13) {
        const unsigned sv[6] = {x->saturation_vector_field0, x->saturation_vector_field1, x->saturation_vector_field2,
                                x->saturation_vector_field3, x->saturation_vector_field4, x->saturation_vector_field5};
        for (int k = 0; k < 6; k++) t->sat[k] = dv_dm_off(sv[k], 128);
      }
      if (x->length > 19) {
        const unsigned hv[6] = {x->hue_vector_field0, x->hue_vector_field1, x->hue_vector_field2,
                                x->hue_vector_field3, x->hue_vector_field4, x->hue_vector_field5};
        for (int k = 0; k < 6; k++) t->hue[k] = dv_dm_off(hv[k], 128);
      }
      tpr[n] = tp; tpq[n++] = dm_pq_oetf(pk);
    }
    // tone trims linear in PQ; colour trims linear in display size across gamuts (mastering display = L9 gamut, title peak).
    // At or above the mastering peak the trims have faded out: the untrimmed mapping, reported as such.
    bool applied = n > 0 && tmax < smast;
    dm_cm4_trim tr;
    if (applied) tr = dm_cm4_interp_gamut(trims, tpq, tpr, n, tmax, smast, tprim, sprim);
    else memset(&tr, 0, sizeof(tr));
    dm_cm4_state s;
    if (!dm_cm4_prepare(&s, e, tmin, tmax, &tr, tprim, sprim)) return "degenerate CM4 shot";
    f->weight = 0.0;
    f->trim_targets = applied ? n : 0; f->trim = applied ? "l8-interpolated" : "l1-only";
    f->c4 = s;
    dv_dm_pack_cm4(f, &s);
    return NULL;
  }
  // CM v2.9: the L2 trim authored for this target's peak (12-bit PQ match); otherwise each decoded
  // trim field interpolated linearly in target PQ between the bracketing L2 targets, with neutral
  // trims at PQ 0 and at the mastering display (title peak). No usable L2, or a target at or above
  // the mastering peak (even with an L2 authored for it): neutral trim with the default MS weight.
  dm_cm29_trim tr = {0, 0, 0, 0, 0, 0}; unsigned want = (unsigned)floor(tmax * 4095 + 0.5); int found = 0;
  {
    dm_cm29_trim lo_t = tr, hi_t = tr, ex_t = tr; double lo = -1, hi = 2; bool exact = false;
    for (size_t i = 0; cm29_trims && i < d->level2.len; i++) {
      const DoviExtMetadataBlockLevel2 *x = d->level2.list[i];
      dm_cm29_trim t;
      dv_dm_sop(x->trim_slope, x->trim_offset, x->trim_power, &t.lift, &t.gain, &t.gamma);
      t.cw = dv_dm_off(x->trim_chroma_weight, 2048); t.sg = dv_dm_off(x->trim_saturation_gain, 2048);
      t.ms = dv_dm_off((unsigned)(x->ms_weight < 0 ? 2048 : x->ms_weight), 2048);
      double q = dv_dm_pq12(x->target_max_pq);
      if (!exact && (unsigned)x->target_max_pq + 1 >= want && (unsigned)x->target_max_pq <= want + 1) {
        exact = true; ex_t = t;
      }
      if (q < tmax && q > lo) { lo = q; lo_t = t; }
      if (q > tmax && q < hi) { hi = q; hi_t = t; }
    }
    // an L2 target between this display and the mastering peak; without one (and without a lower
    // target) every trim has faded out and the mapping is the untrimmed one
    bool upper = hi <= 1 && hi < smast, lower = lo >= 0;
    if (exact && tmax < smast) { found = 2; tr = ex_t; }
    else if (!exact && tmax < smast && (lower || upper)) {
      if (!upper) { hi = smast; memset(&hi_t, 0, sizeof(hi_t)); }   // neutral at the mastering peak
      if (!lower) { lo = 0; memset(&lo_t, 0, sizeof(lo_t)); }       // neutral at PQ 0
      double w = (hi - tmax) / (hi - lo);                  // weight of the lower trim
      w = dm_mind(dm_maxd(w, 0), 1);
      tr.lift = w * lo_t.lift + (1 - w) * hi_t.lift; tr.gain = w * lo_t.gain + (1 - w) * hi_t.gain;
      tr.gamma = w * lo_t.gamma + (1 - w) * hi_t.gamma; tr.cw = w * lo_t.cw + (1 - w) * hi_t.cw;
      tr.sg = w * lo_t.sg + (1 - w) * hi_t.sg; tr.ms = w * lo_t.ms + (1 - w) * hi_t.ms;
      found = 1;
    }
  }
  dm_cm29_target tg = {nits, floor_nits, tprim, 0.0};
  dm_cm29_state s;
  if (!dm_cm29_prepare(&s, L1, d->level6 ? d->level6->min_display_mastering_luminance / 10000.0 : 0.005,
                       smast_nits, &tg, &tr)) return "degenerate CM2.9 shot";
  f->weight = tr.ms;
  f->trim_targets = found ? (int)d->level2.len : 0; f->trim = found == 2 ? "l2-exact" : (found ? "l2-interpolated" : "l1-only");
  f->c29 = s;
  dv_dm_pack_cm29(f, &s);
  return NULL;
}

// Double-precision CPU reference of the whole frame (controls only): rgba in/out, BT.2020/PQ.
static inline bool dv_dm_cpu_render(const dv_dm_frame *f, const float *in, int w, int h, float *out) {
  size_t n = (size_t)w * h;
  double *a = malloc(n * sizeof(double)), *b = malloc(n * sizeof(double)), *c = malloc(n * sizeof(double)),
         *e = malloc(n * sizeof(double)), *o3 = malloc(3 * n * sizeof(double));
  dm_cm29_pix *px = f->algo == 29 ? malloc(n * sizeof(dm_cm29_pix)) : NULL;
  bool ok = a && b && c && e && o3 && (f->algo == 4 || px);
  for (size_t i = 0; ok && i < n; i++) {
    double rgb[3] = {in[4 * i], in[4 * i + 1], in[4 * i + 2]};
    if (f->algo == 4) { double ic[3]; dm_rgbpq_to_ictcp(rgb, ic); a[i] = ic[0]; dm_cm4_pixel(&f->c4, rgb, o3 + 3 * i); b[i] = o3[3 * i]; }
    else { dm_cm29_pixel_pre(&f->c29, rgb, &px[i]); a[i] = px[i].I; b[i] = px[i].Ib; c[i] = px[i].Io; }
  }
  if (ok && f->weight > -1) {
    ok = dm_detail(a, b, w, h, f->weight, e);
    if (ok) memcpy(b, e, n * sizeof(double));
    if (ok && f->algo == 29 && f->c29.tr.cw != 0) { ok = dm_detail(a, c, w, h, f->weight, e); if (ok) memcpy(c, e, n * sizeof(double)); }
    else if (ok && f->algo == 29) memcpy(c, b, n * sizeof(double));
  }
  for (size_t i = 0; ok && i < n; i++) {
    double rgb[3];
    if (f->algo == 4) { o3[3 * i] = b[i]; dm_cm4_finish(&f->c4, o3 + 3 * i, rgb); }
    else dm_cm29_finish(&f->c29, &px[i], b[i], c[i], rgb);
    for (int k = 0; k < 3; k++) out[4 * i + k] = isnan(rgb[k]) ? 0.0f : (float)rgb[k];   // as the shader
    out[4 * i + 3] = 1;
  }
  free(a); free(b); free(c); free(e); free(o3); free(px);
  return ok;
}

// ------------------------------------------------------------------ GLSL
// The lattice's intensity axis: vertices 0..64 plus one past the top for a clipped Io.
enum { DV_DM_LATTICE_X = 66 };
// Chroma reach the table covers (IPT P/T); vertices beyond it are evaluated per pixel.
#define DV_DM_LATTICE_CHROMA 0.6
#define DV_DM_LATTICE_MAX_R 120
static const char dv_dm_glsl_common[] =
  "#define DV_DM_LATTICE_X 66.0\n"
  "const mat3 R2L_T = mat3(vec3(0.412109375,0.5239257812,0.06396484375), vec3(0.1667480469,0.7204589844,0.1127929688), vec3(0.02416992188,0.07543945312,0.900390625));\n"
  "const mat3 L2R_T = mat3(vec3(3.436606694,-2.506452119,0.06984542432), vec3(-0.7913295556,1.983600452,-0.1922708962), vec3(-0.02594989969,-0.09891371471,1.124863614));\n"
  "const mat3 L2I_T = mat3(vec3(0.5,0.5,0.0), vec3(1.613769531,-3.323486328,1.709716797), vec3(4.378173828,-4.245605469,-0.1325683594));\n"
  "const mat3 I2L_T = mat3(vec3(1.0,0.008609037038,0.111029625), vec3(1.0,-0.008609037038,-0.111029625), vec3(1.0,0.5600313357,-0.320627175));\n"
  "const mat3 IPT_T = mat3(vec3(0.4,0.4,0.2), vec3(4.455,-4.851,0.396), vec3(0.8056,0.3572,-1.1628));\n"
  "const mat3 IPTI_T = mat3(vec3(1.0,0.09756893051,0.2052264332), vec3(1.0,-0.1138764855,0.1332171584), vec3(1.0,0.03261510992,-0.6768871831));\n"
  // ST 2084 in float32: avoid subtracting c1 from pow(e,1/m2), and avoid
  // raising a rounded ratio near 1 to m2. These are the same PQ equations as
  // the double reference, with compensated log1p/expm1 (GLSL has neither).
  // c1-1 = -(c2-c3) = -0.1640625; c2-c3*c1 = 3.22998046875 exactly.
  "float pq_log1p(float x){ float u=1.0+x,d=u-1.0; return d==0.0 ? x : log(u)*x/d; }\n"
  "float pq_expm1(float x){ float u=exp(x),l=log(u); return u==1.0 || l==0.0 ? x : (u-1.0)*x/l; }\n"
  "float pq_eotf_ext(float e){ e=max(e,0.0); float x=log(max(e/7.309559025783966e-7,1.0))/78.84375; float d=0.8359375*pq_expm1(x); return 10000.0*pow(d/max(3.22998046875-18.6875*d,1e-12),1.0/0.1593017578125); }\n"
  "float pq_eotf(float e){ return pq_eotf_ext(clamp(e,0.0,1.0)); }\n"
  "float pq_oetf(float y){ y=clamp(y/10000.0,0.0,1.0); float t=pow(y,0.1593017578125); return exp(78.84375*pq_log1p(-0.1640625*(1.0-t)/(1.0+18.6875*t))); }\n"
  "vec3 pq_eotf3(vec3 e){ return vec3(pq_eotf(e.x),pq_eotf(e.y),pq_eotf(e.z)); }\n"
  "vec3 pq_oetf3(vec3 y){ return vec3(pq_oetf(y.x),pq_oetf(y.y),pq_oetf(y.z)); }\n"
  "vec3 mrow(vec4 r0, vec4 r1, vec4 r2, vec3 v){ return vec3(dot(r0.xyz,v),dot(r1.xyz,v),dot(r2.xyz,v)); }\n"
  "vec3 ictcp(vec3 rgb){ vec3 lms=pq_eotf3(rgb)*R2L_T; return pq_oetf3(lms)*L2I_T; }\n"
  // ---------------------------------------------------------- CM4
  "float herm(float T,float y1,float y2,float s1,float s2,float dx){ float T2=T*T,T3=T2*T; return (2.0*T3-3.0*T2+1.0)*y1+(T3-2.0*T2+T)*dx*s1+(-2.0*T3+3.0*T2)*y2+(T3-T2)*dx*s2; }\n"
  "float c4curve(float x){\n"
  " float SMin=u[0].x,SMid=u[0].y,SMax=u[0].z,TMin=u[1].y,TMid=u[1].z,TMX=u[1].w,sMin=u[2].x,sMid=u[2].y,sMax=u[2].z;\n"
  " if(x<SMin) return TMin+(x-SMin)*sMin;\n"
  " if(x<=SMid) return herm((x-SMin)/(SMid-SMin),TMin,TMid,sMin,sMid,SMid-SMin);\n"
  " if(x<=SMax) return herm((x-SMid)/(SMax-SMid),TMid,TMX,sMid,sMax,SMax-SMid);\n"
  " return TMX+(x-SMax)*sMax; }\n"
  "float c4sop(float v){ if(u[17].x<0.5) return v; float Tmin=u[0].w,Tmax=u[1].x,L=u[3].x,g=u[3].y,G=u[3].z;\n"
  " float n=(v-Tmin)/(Tmax-Tmin); n=max((1.0+g)*n+L*(1.0+g/2.0)*(1.0-n),0.0); n=pow(n,(2.0-G)/(2.0+G)); return n*(Tmax-Tmin)+Tmin; }\n"
  "float satscale(float Io,float Ii){ float a=Io-1.0,b=Ii-1.0; return (a*a*a+1.0)/(b*b*b+1.0); }\n"
  "float c4meso(float I){ float t=clamp(1.0-(I*4095.0-25.0)/875.0,0.0,1.0); return 1.0-t*t; }\n"
  "float anc(int k){ return k<4 ? u[6][k] : u[7][k-4]; }\n"
  "float sixsat(int k){ return k<4 ? u[8][k] : u[9][k-4]; }\n"
  "float sixhue(int k){ return k<2 ? u[9][k+2] : u[10][k-2]; }\n"
  "vec3 c4point(vec3 rgb){\n"
  " vec3 ic=ictcp(rgb); float I=ic.x, S=sqrt(0.25*ic.y*ic.y+ic.z*ic.z);\n"
  " float m=min(S,u[4].z), S3=m*m*m, cw=u[3].w;\n"
  " float Ip=c4sop(c4curve(I));\n"
  " vec3 o; o.x=Ip*(1.0+u[4].x*S3)*(1.0+2.0*cw*S);\n"
  " float ss=satscale(Ip,I); float sI=max(min(1.0,ss),ss*c4meso(I));\n"
  " if(u[17].y>0.5){ float c1=clamp((Ip-u[0].w)/(u[0].w-u[0].x),0.0,1.0); sI*=1.0-(1.0-c1)*(1.0-c1); }\n"
  " float f=sI*u[2].w*(1.0+1.5*cw*S)*(1.0+u[4].y*S3); o.y=ic.y*f; o.z=ic.z*f;\n"
  " if(u[4].w>0.5){\n"
  "  float h=degrees(atan(o.z,o.y/2.0)); if(h<0.0) h+=360.0;\n"
  "  float smh=1.0, r1=0.0;\n"
  "  for(int k=0;k<6;k++){ int j=(k+1)%6; float span=mod(anc(j)-anc(k)+720.0,360.0), dd=mod(h-anc(k)+720.0,360.0);\n"
  "   if(dd>=360.0-1e-4&&span>0.0) dd=0.0;\n"
  "   if(dd<span){ float t=dd/span; smh+=(1.0-t)*sixsat(k)+t*sixsat(j); r1+=(1.0-t)*sixhue(k)+t*sixhue(j); break; } }\n"
  "  r1*=0.5; float r2=1.0-r1*r1/2.0, T=o.y/2.0, P=o.z; o.y=2.0*smh*(T*r2-P*r1); o.z=smh*(T*r1+P*r2); }\n"
  " return o; }\n"
  "vec3 c4finish(vec3 o){\n"
  " vec3 lmsp=o*I2L_T; vec3 lms=vec3(pq_eotf_ext(lmsp.x),pq_eotf_ext(lmsp.y),pq_eotf_ext(lmsp.z));\n"
  " vec3 lin=lms*L2R_T; vec3 lt=clamp(mrow(u[11],u[12],u[13],lin),vec3(u[5].x),vec3(u[5].y));\n"
  " lin=mrow(u[14],u[15],u[16],lt); return pq_oetf3(max(lin,vec3(0.0))); }\n"
  // ---------------------------------------------------------- CM2.9
  "float c29curve(float x){ float xn=pow(max(x,0.0),u[0].w); float v=pow(max((u[0].x+u[0].y*xn)/(1.0+u[0].z*xn),0.0),1.0/3.0); return clamp(v,u[1].x,u[1].y); }\n"
  "float c29sop(float y){ if(u[17].x<0.5) return y; float L=u[2].x,g=u[2].y,G=u[2].z;\n"
  " return clamp(pow(max((1.0+g/2.0)*((1.0-L/2.0)*y+L/2.0),0.0),2.0/(2.0+G)),u[1].x,u[1].y); }\n"
  "vec3 ipt(vec3 rgb){ vec3 lms=max(mrow(u[4],u[5],u[6],pq_eotf3(rgb)),vec3(0.0)); return pq_oetf3(lms)*IPT_T; }\n"
  "vec2 c29point(vec3 p, out float I){ I=p.x; float C=length(p.yz); float Ib=c29sop(c29curve(I));\n"
  " float Io=u[17].y>0.5 ? c29sop(c29curve(I+0.5*u[2].w*C)) : Ib; return vec2(Ib,Io); }\n"
  "vec3 c29vertex(vec3 q){\n"
  " if(q.x<=u[10].x) return vec3(0.0);\n"
  " vec3 lmsp=clamp(q*IPTI_T,0.0,1.0);\n"
  " vec3 lin=mrow(u[7],u[8],u[9],pq_eotf3(lmsp));\n"
  " vec3 lt=clamp(mrow(u[11],u[12],u[13],lin),vec3(u[1].z),vec3(u[1].w));\n"
  " return u[3].w>0.5 ? (pow(lt,vec3(1.0/2.4))-vec3(u[10].y))/u[10].z : pq_oetf3(lt); }\n"
  "// Lattice vertex idx (integer-valued; x 0..64, y/z centred on 0): the per-frame table when idx is\n"
  "// inside it (u[18].z: its chroma offset R, u[18].w: its chroma size), otherwise evaluated here.\n"
  "vec3 c29corner(sampler2D lat, vec3 origin, vec3 step, vec3 idx){\n"
  " float R=u[18].z, nc=u[18].w;\n"
  " if(idx.x>=0.0&&idx.x<DV_DM_LATTICE_X&&idx.y>=-R&&idx.y<nc-R&&idx.z>=-R&&idx.z<nc-R)\n"
  "  return texelFetch(lat,ivec2(int(idx.x)+int(DV_DM_LATTICE_X)*int(idx.y+R),int(idx.z+R)),0).rgb;\n"
  " return c29vertex(origin+idx*step); }\n"
  "vec3 c29finish(sampler2D lat, vec3 p, float Ib, float Io){\n"
  " Ib=max(Ib,0.0); Io=max(Io,0.0); float Ii=max(p.x,1e-6);\n"
  " float sc=clamp((Ib/Ii)*(2.0+Ii)/(2.0+Ib),0.5,2.0)*(1.0+0.5*u[3].x);\n"
  " vec3 q=vec3(Io,p.y*sc,p.z*sc), origin=vec3(u[10].x,0.0,0.0);\n"
  " vec3 step=vec3(u[1].y/64.0,u[1].y/32.0,u[1].y/32.0);\n"
  " vec3 pos=(q-origin)/step, base=floor(pos), f=pos-base, acc=vec3(0.0);\n"
  " for(int bits=0;bits<8;bits++){\n"
  "  vec3 b=vec3(float(bits&1),float((bits>>1)&1),float((bits>>2)&1));\n"
  "  vec3 w=mix(vec3(1.0)-f,f,b);\n"
  "  acc+=c29corner(lat,origin,step,base+b)*(w.x*w.y*w.z); }\n"
  " vec3 lt=u[3].w>0.5 ? pow(acc*u[19].y+vec3(u[19].x),vec3(2.4)) : pq_eotf3(acc);\n"
  " vec3 lin=mrow(u[14],u[15],u[16],lt);\n"
  " return max(pq_oetf3(clamp(lin,0.0,u[1].w)),vec3(u[3].z)); }\n"
  // ---------------------------------------------------------- spatial kernels
  "const float GX[11]=float[11](0.008812229293,0.02714357714,0.0651140566,0.121649073,0.1769983568,0.2005654142,0.1769983568,0.121649073,0.0651140566,0.02714357714,0.008812229293);\n"
  "const float GY[5]=float[5](0.05448868455,0.244201342,0.4026199469,0.244201342,0.05448868455);\n";

// Each method's passes are separate shaders: one shader holding both methods' code spills
// registers on some GPUs (Intel Arc), which slows both.
static const char dv_dm_body_point_cm4[] =   // pass 1: D (x: base/intensity)
  "vec3 rgb=texelFetch(master,ivec2(gl_FragCoord.xy),0).rgb;\n"
  "vec3 ic=ictcp(rgb); vec3 o=c4point(rgb); color=vec4(ic.x-o.x,0.0,0.0,1.0);\n";
static const char dv_dm_body_point_cm29[] =  // pass 1: D (x: base/intensity, y: chroma-weighted)
  "vec3 rgb=texelFetch(master,ivec2(gl_FragCoord.xy),0).rgb;\n"
  "vec3 p=ipt(rgb); float I; vec2 b=c29point(p,I); color=vec4(I-b.x,I-b.y,0.0,1.0);\n";
static const char dv_dm_body_lattice[] = // CM2.9 pass 0: the frame's output lattice
  "ivec2 q=ivec2(gl_FragCoord.xy); int X=int(DV_DM_LATTICE_X), R=int(u[18].z);\n"
  "vec3 idx=vec3(float(q.x%X),float(q.x/X-R),float(q.y-R));\n"
  "vec3 origin=vec3(u[10].x,0.0,0.0), step=vec3(u[1].y/64.0,u[1].y/32.0,u[1].y/32.0);\n"
  "color=vec4(c29vertex(origin+idx*step),1.0);\n";
static const char dv_dm_body_h[] =       // pass 2: horizontal Gaussian and derivative
  "ivec2 q=ivec2(gl_FragCoord.xy); int w=textureSize(dpass,0).x; vec4 acc=vec4(0.0);\n"
  "for(int j=-5;j<=5;j++){ int x=clamp(q.x-j,0,w-1); vec2 d=texelFetch(dpass,ivec2(x,q.y),0).xy; float g=GX[j+5], dg=-float(j)/4.0*g;\n"
  " acc+=vec4(g*d.x,dg*d.x,g*d.y,dg*d.y); }\n"
  "color=acc;\n";
static const char dv_dm_body_final_cm4[] =   // pass 3: vertical, detail, finish
  "ivec2 q=ivec2(gl_FragCoord.xy); vec3 rgb=texelFetch(master,q,0).rgb; float wgt=u[18].y;\n"
  "float B1=0.0,dx1=0.0,dy1=0.0,B2=0.0,dx2=0.0,dy2=0.0;\n"
  "if(wgt>-1.0){ int h=textureSize(hpass,0).y;\n"
  " for(int j=-2;j<=2;j++){ int y=clamp(q.y-j,0,h-1); vec4 t=texelFetch(hpass,ivec2(q.x,y),0); float g=GY[j+2], dg=-float(j)*g;\n"
  "  B1+=g*t.x; dx1+=g*t.y; dy1+=dg*t.x; B2+=g*t.z; dx2+=g*t.w; dy2+=dg*t.z; } }\n"
  " vec3 ic=ictcp(rgb); vec3 o=c4point(rgb);\n"
  " if(wgt>-1.0){ float D=ic.x-o.x; float r=clamp(1.0+wgt-4.0*(4.0*abs(dx1)+abs(dy1)),0.0,1.0); o.x=o.x+r*(D-B1); }\n"
  " vec3 res=c4finish(o); color=vec4(mix(res,vec3(0.0),isnan(res)),1.0);\n"
  ;
static const char dv_dm_body_final_cm29[] =  // pass 3: vertical, detail, finish
  "ivec2 q=ivec2(gl_FragCoord.xy); vec3 rgb=texelFetch(master,q,0).rgb; float wgt=u[18].y;\n"
  "float B1=0.0,dx1=0.0,dy1=0.0,B2=0.0,dx2=0.0,dy2=0.0;\n"
  "if(wgt>-1.0){ int h=textureSize(hpass,0).y;\n"
  " for(int j=-2;j<=2;j++){ int y=clamp(q.y-j,0,h-1); vec4 t=texelFetch(hpass,ivec2(q.x,y),0); float g=GY[j+2], dg=-float(j)*g;\n"
  "  B1+=g*t.x; dx1+=g*t.y; dy1+=dg*t.x; B2+=g*t.z; dx2+=g*t.w; dy2+=dg*t.z; } }\n"
  " vec3 p=ipt(rgb); float I; vec2 b=c29point(p,I); float Ib=b.x, Io=b.y;\n"
  " if(wgt>-1.0){ float r1=clamp(1.0+wgt-4.0*(4.0*abs(dx1)+abs(dy1)),0.0,1.0); Ib=Ib+r1*((I-b.x)-B1);\n"
  "  if(u[17].y>0.5){ float r2=clamp(1.0+wgt-4.0*(4.0*abs(dx2)+abs(dy2)),0.0,1.0); Io=Io+r2*((I-b.y)-B2); } else Io=Ib; }\n"
  " vec3 res=c29finish(lpass,p,Ib,Io); color=vec4(mix(res,vec3(0.0),isnan(res)),1.0);\n"
  ;

static struct { pl_dispatch dispatch; pl_tex dtex, htex, ltex, out; } dv_dm_gpu;

static inline pl_tex dv_dm_tex(pl_gpu gpu, pl_tex *t, int w, int h) {
  if (*t && ((*t)->params.w != w || (*t)->params.h != h)) pl_tex_destroy(gpu, t);
  if (!*t) {
    pl_fmt format = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
        PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_LINEAR);
    if (!format) return NULL;
    *t = pl_tex_create(gpu, pl_tex_params(.w = w, .h = h, .format = format, .sampleable = true, .renderable = true, .blit_src = true));
  }
  return *t;
}
static inline bool dv_dm_pass(pl_shader sh, const char *desc, const char *body, pl_tex in, const char *in_name,
                              pl_tex in2, const char *in2_name, pl_tex in3, const char *in3_name,
                              float (*u)[4], int w, int h) {
  struct pl_shader_desc d[3] = {
    {.desc = {.name = in_name, .type = PL_DESC_SAMPLED_TEX},
     .binding = {.object = in, .address_mode = PL_TEX_ADDRESS_CLAMP, .sample_mode = PL_TEX_SAMPLE_NEAREST}},
    {.desc = {.name = in2_name, .type = PL_DESC_SAMPLED_TEX},
     .binding = {.object = in2, .address_mode = PL_TEX_ADDRESS_CLAMP, .sample_mode = PL_TEX_SAMPLE_NEAREST}},
    {.desc = {.name = in3_name, .type = PL_DESC_SAMPLED_TEX},
     .binding = {.object = in3, .address_mode = PL_TEX_ADDRESS_CLAMP, .sample_mode = PL_TEX_SAMPLE_NEAREST}},
  };
  struct pl_var var = pl_var_vec4("u"); var.dim_a = DV_DM_U;
  struct pl_shader_var v = {.var = var, .data = u, .dynamic = true};
  return pl_shader_custom(sh, &(struct pl_custom_shader){
      .description = desc, .header = dv_dm_glsl_common, .body = body,
      .input = PL_SHADER_SIG_NONE, .output = PL_SHADER_SIG_COLOR,
      .descriptors = d, .num_descriptors = in3 ? 3 : in2 ? 2 : 1, .variables = &v, .num_variables = 1,
      .output_w = w, .output_h = h});
}

// Map the native master texture for the prepared frame. Returns the BT.2020/PQ output texture.
static inline pl_tex dv_dm_render(pl_log log, pl_gpu gpu, pl_tex source, dv_dm_frame *f) {
  int w = source->params.w, h = source->params.h;
  if (!dv_dm_gpu.dispatch) dv_dm_gpu.dispatch = pl_dispatch_create(log, gpu);
  if (!dv_dm_gpu.dispatch || !dv_dm_tex(gpu, &dv_dm_gpu.dtex, w, h) || !dv_dm_tex(gpu, &dv_dm_gpu.htex, w, h) ||
      !dv_dm_tex(gpu, &dv_dm_gpu.out, w, h)) return NULL;
  const char *desc = f->algo == 4 ? DV_DM_METHOD_CM4 : DV_DM_METHOD_CM29;
  pl_shader sh;
  pl_tex lattice = NULL;   // CM v2.9 only
  if (f->algo == 4) {
    dv_dm_put(f->u[18], 4, f->weight, 0, 0);
  } else {
    // The same step the shader computes; R chroma steps each side of zero cover DV_DM_LATTICE_CHROMA.
    float step = f->u[1][1] / 32.0f;
    int r = step > 0 ? (int)ceil(DV_DM_LATTICE_CHROMA / step) : 0;
    if (r > DV_DM_LATTICE_MAX_R) r = DV_DM_LATTICE_MAX_R;
    int nc = 2 * r + 2;
    dv_dm_put(f->u[18], 29, f->weight, r, nc);
    if (!(lattice = dv_dm_tex(gpu, &dv_dm_gpu.ltex, DV_DM_LATTICE_X * nc, nc))) return NULL;
    sh = pl_dispatch_begin(dv_dm_gpu.dispatch);
    if (!dv_dm_pass(sh, desc, dv_dm_body_lattice, source, "master", NULL, "unused", NULL, "unused2", f->u,
                    DV_DM_LATTICE_X * nc, nc) ||
        !pl_dispatch_finish(dv_dm_gpu.dispatch, pl_dispatch_params(.shader = &sh, .target = lattice))) return NULL;
  }
  if (f->weight > -1.0) {
    sh = pl_dispatch_begin(dv_dm_gpu.dispatch);
    if (!dv_dm_pass(sh, desc, f->algo == 4 ? dv_dm_body_point_cm4 : dv_dm_body_point_cm29, source, "master", NULL, "unused", NULL, "unused2", f->u, w, h) ||
        !pl_dispatch_finish(dv_dm_gpu.dispatch, pl_dispatch_params(.shader = &sh, .target = dv_dm_gpu.dtex))) return NULL;
    sh = pl_dispatch_begin(dv_dm_gpu.dispatch);
    if (!dv_dm_pass(sh, desc, dv_dm_body_h, dv_dm_gpu.dtex, "dpass", NULL, "unused", NULL, "unused2", f->u, w, h) ||
        !pl_dispatch_finish(dv_dm_gpu.dispatch, pl_dispatch_params(.shader = &sh, .target = dv_dm_gpu.htex))) return NULL;
  }
  sh = pl_dispatch_begin(dv_dm_gpu.dispatch);
  if (!dv_dm_pass(sh, desc, f->algo == 4 ? dv_dm_body_final_cm4 : dv_dm_body_final_cm29, source, "master", dv_dm_gpu.htex, "hpass", lattice, "lpass", f->u, w, h) ||
      !pl_dispatch_finish(dv_dm_gpu.dispatch, pl_dispatch_params(.shader = &sh, .target = dv_dm_gpu.out))) return NULL;
  return dv_dm_gpu.out;
}
static inline void dv_dm_destroy(pl_gpu gpu) {
  pl_tex_destroy(gpu, &dv_dm_gpu.dtex); pl_tex_destroy(gpu, &dv_dm_gpu.htex); pl_tex_destroy(gpu, &dv_dm_gpu.ltex);
  pl_tex_destroy(gpu, &dv_dm_gpu.out);
  pl_dispatch_destroy(&dv_dm_gpu.dispatch);
}
#endif
