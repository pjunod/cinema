// Control-only L2 parameter bridge and B.3 RGB equation (9), after SOP.
// L8 chroma/saturation has no established bridge here and remains unapplied.
#ifndef PLURX_TRIM_L2_CHROMA_H
#define PLURX_TRIM_L2_CHROMA_H
#include "../trim_selection.h"
#define DV_TRIM_L2_CHROMA_BRIDGE "etsi-ts103572-1.3.1-l2-cw-sg-v1"
#define DV_TRIM_L2_CHROMA_METHOD "st2094-10-2021-b3-eq9-post-sop-bt2020-v1"
#define DV_TRIM_L2_CHROMA_BOUNDARY "continuous-black-no-bucket-clip-pq-ceiling-10000-v1"
// BT.2020-2 table 4 constant-luminance coefficients, applied to linear RGB.
// This candidate's explicit target/output encoding is BT.2020/D65, not the
// illustrative cinema-projector primaries in SMPTE's equation (9) example.
static const float dv_trim_bt2020_luma[3] = {0.2627f, 0.6780f, 0.0593f};
struct dv_trim_chroma {
  float value[2]; // ChromaCompensationWeight, SaturationGain
  bool applied;  // Public L2 coverage, not a claim of creative-trim acceptance.
};
static inline bool dv_trim_l2_chroma(const struct dv_trim_selection *trim,
                                    bool public_bridge, struct dv_trim_chroma *out) {
  *out = (struct dv_trim_chroma){0};
  if (!public_bridge || trim->word[DT_LEVEL] != 2) return true;
  if (trim->word[DT_VERSION] != 2 || !dv_trim_common_valid(trim)) return false;
  // ETSI TS 103 572 v1.3.1 equations (7)/(8), tied to ST2094-10 6.3.1/2.
  // This centered /4096 scale is L2-only; the SOP L8 multiplier is unrelated.
  out->value[0] = ((int)trim->word[DT_CHROMA] - 2048) / 4096.0f;
  out->value[1] = ((int)trim->word[DT_SATURATION] - 2048) / 4096.0f;
  out->applied = true;
  return true;
}
static inline bool dv_trim_chroma_valid(const struct dv_trim_chroma *chroma) {
  return isfinite(chroma->value[0]) && chroma->value[0] >= -0.5f && chroma->value[0] < 0.5f &&
      isfinite(chroma->value[1]) && chroma->value[1] >= -0.5f && chroma->value[1] < 0.5f &&
      (chroma->applied || (chroma->value[0] == 0 && chroma->value[1] == 0));
}
// Stable algebraic form of B.3 eq(9) for nonnegative post-SOP components.
// Positive luma weights bound F_i/Y. 1+SG >= .5 gives the continuous black
// extension G_i=0, without 0*infinity for a zero component and negative SG.
// The formula has no bucket clamp; the shader's final PQ encoding clips only
// to its representable 0..10000 cd/m² range (explicit named boundary policy).
static inline void dv_trim_b3_chroma(const double f[3],
                                     const struct dv_trim_chroma *chroma, double g[3]) {
  if (chroma->value[0] == 0 && chroma->value[1] == 0) {
    memcpy(g, f, 3 * sizeof(*g)); return;
  }
  double y = 0;
  for (unsigned i = 0; i < 3; i++) y += dv_trim_bt2020_luma[i] * f[i];
  for (unsigned i = 0; i < 3; i++)
    g[i] = y > 0 ? (1.0 + chroma->value[0]) * y *
        pow(f[i] / y, 1.0 + chroma->value[1]) : 0;
}
#endif
