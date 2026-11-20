// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_CODED_AU_H
#define PLURX_CODED_AU_H

// Four-byte hvcC access-unit identity used by the renderer and the scanner's
// planned M7 census. The caller owns transport bounds and selects the PTS.
struct coded_picture {
  unsigned bl, el, rpus;
  const uint8_t *rpu;
  size_t rpu_size;
};

static inline void hash_bytes(const uint8_t *bytes, size_t size, char hex[65]) {
  struct AVSHA *sha = av_sha_alloc();
  uint8_t out[32];
  need(sha != NULL && av_sha_init(sha, 256) == 0, "SHA256");
  av_sha_update(sha, bytes, size);
  av_sha_final(sha, out);
  av_free(sha);
  for (int i = 0; i < 32; i++)
    snprintf(hex + 2 * i, 3, "%02x", out[i]);
}
static inline void finish_coded_vcl(struct AVSHA *sha, char hex[65]) {
  uint8_t digest[32];
  av_sha_final(sha, digest);
  av_free(sha);
  for (int i = 0; i < 32; i++)
    snprintf(hex + 2 * i, 3, "%02x", digest[i]);
}

static inline struct coded_picture inspect_coded_picture(
    const AVPacket *packet, struct AVSHA *base_sha, bool forbid_el) {
  struct coded_picture picture = {0};
  size_t at = 0;
  while (at < (size_t)packet->size) {
    need((size_t)packet->size - at >= 4, "coded NAL length prefix");
    const uint8_t *p = packet->data + at;
    uint32_t n = (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 |
                 (uint32_t)p[2] << 8 | p[3];
    at += 4;
    need(n >= 2 && n <= (size_t)packet->size - at, "coded NAL bounds");
    p = packet->data + at;
    int type = (p[0] >> 1) & 63;
    need(((p[0] & 1) << 5 | (p[1] >> 3)) == 0 && (p[1] & 7) != 0,
         "coded layer/temporal header");
    if (type < 32) {
      need(n >= 3, "base slice header");
      picture.bl += (p[2] & 128) != 0;
      // The live P8 copy preserves these base-layer VCL units verbatim.
      // Include each original four-byte length so boundaries are unambiguous.
      if (base_sha)
        av_sha_update(base_sha, p - 4, (size_t)n + 4);
    }
    if (type == 63) {
      need(!forbid_el, "M7 candidate has no enhancement-layer NAL units");
      need(n >= 4, "enhancement wrapper");
      int inner = (p[2] >> 1) & 63;
      need(((p[2] & 1) << 5 | (p[3] >> 3)) == 0 && (p[3] & 7) != 0,
           "enhancement layer/temporal header");
      if (inner < 32) {
        need(n >= 5, "enhancement slice header");
        picture.el += (p[4] & 128) != 0;
      }
    }
    if (type == 62) {
      picture.rpus++;
      picture.rpu = p;
      picture.rpu_size = n;
    }
    at += n;
  }
  return picture;
}

#endif
