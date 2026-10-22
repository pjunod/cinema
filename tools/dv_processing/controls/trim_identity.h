#ifndef PLURX_TRIM_IDENTITY_H
#define PLURX_TRIM_IDENTITY_H
#include <libavutil/mem.h>
#include <libavutil/sha.h>
// SHA256 over explicit little-endian scalar words; never C struct bytes.
// Identity covers the complete selected trim, all target/L1 parameters and
// the actual float32 SOP uniforms even in direct-parameter control mode.
static inline void dv_trim_identity(const struct dv_trim_selection *trim,
    const DoviExtMetadataBlockLevel1 *l1, const float sop[3], unsigned width,
    unsigned height, bool inferred, char hex[65]) {
  uint32_t word[DV_TRIM_ID_WORDS + 12];
  memcpy(word, trim->word, sizeof(trim->word));
  uint32_t *p = word + DV_TRIM_ID_WORDS;
  *p++ = dv_target.nits; *p++ = dv_target.min_1e4; *p++ = dv_target.title_peak;
  *p++ = l1->min_pq; *p++ = l1->max_pq; *p++ = l1->avg_pq;
  *p++ = width; *p++ = height; *p++ = inferred;
  for (unsigned i = 0; i < 3; i++) memcpy(p++, &sop[i], sizeof(uint32_t));
  uint8_t bytes[sizeof(word)];
  for (size_t i = 0; i < sizeof(word) / sizeof(*word); i++)
    for (unsigned j = 0; j < 4; j++) bytes[4*i+j] = (uint8_t)(word[i] >> (8*j));
  struct AVSHA *sha = av_sha_alloc();
  uint8_t digest[32];
  require(sha != NULL && av_sha_init(sha, 256) == 0, "identity SHA256");
  av_sha_update(sha, (const uint8_t *)DV_TRIM_METHOD, strlen(DV_TRIM_METHOD));
  av_sha_update(sha, (const uint8_t *)DV_TARGET_METHOD, strlen(DV_TARGET_METHOD));
  av_sha_update(sha, bytes, sizeof(bytes));
  av_sha_final(sha, digest);
  av_free(sha);
  for (unsigned i = 0; i < 32; i++) snprintf(hex + 2*i, 3, "%02x", digest[i]);
}
#endif
