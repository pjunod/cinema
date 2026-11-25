// SPDX-License-Identifier: Apache-2.0
// Runs the independent composer reference (fel_reference_composer.h) on one
// frame of a layer capture (bl.yuv / el.yuv: 10-bit 4:2:0 planar,
// little-endian, frame after frame) and writes the composed 4:2:0 picture as
// 16-bit little-endian planes holding hdr_bit_depth codes.
//
// Usage: fel_reference_compose PARAMS BL.yuv EL.yuv FRAME WIDTH HEIGHT
//                              EL_WIDTH EL_HEIGHT OUT.raw [UNROUNDED.raw]
//
// UNROUNDED.raw, when given, receives v + r for every sample as 32-bit
// little-endian integers: the 16-bit-domain sum before clause 5.4.3.3 rounds
// it to hdr_bit_depth.
//
// PARAMS is whitespace-separated text written by fel_reference_measure.py:
//   frc1 DENOM BL_DEPTH EL_DEPTH HDR_DEPTH DISABLE_RESIDUAL
//   then for each component: NUM_PIVOTS PIVOT... , then per piece
//   IDC POLY_ORDER C0 C1 C2 MMR_ORDER MMR_CONSTANT and 21 MMR coefficients,
//   then NLQ_OFFSET SLOPE THRESHOLD HDR_IN_MAX. Coefficients are fp values.
#define _POSIX_C_SOURCE 200809L
#include "../../fel_reference_composer.h"
#include <inttypes.h>
#include <stdio.h>

static void fail(const char *why) {
  fprintf(stderr, "fel_reference_compose: %s\n", why);
  exit(2);
}

static int64_t next(FILE *f) {
  int64_t v;
  if (fscanf(f, "%" SCNd64, &v) != 1)
    fail("malformed parameters");
  return v;
}

static void read_params(const char *path, struct frc_params *p) {
  FILE *f = fopen(path, "r");
  char magic[8];
  if (!f || fscanf(f, "%7s", magic) != 1 || strcmp(magic, "frc1"))
    fail("parameters are not frc1 text");
  memset(p, 0, sizeof(*p));
  p->coefficient_log2_denom = (int)next(f);
  p->bl_bit_depth = (int)next(f);
  p->el_bit_depth = (int)next(f);
  p->hdr_bit_depth = (int)next(f);
  p->disable_residual = next(f) != 0;
  for (int c = 0; c < 3; c++) {
    struct frc_component_mapping *m = &p->map[c];
    m->num_pivots = (int)next(f);
    if (m->num_pivots < 2 || m->num_pivots > FRC_MAX_PIVOTS)
      fail("pivot count");
    for (int i = 0; i < m->num_pivots; i++)
      m->pivot_value[i] = (int32_t)next(f);
    for (int k = 0; k < m->num_pivots - 1; k++) {
      struct frc_piece *piece = &m->piece[k];
      piece->mapping_idc = (int)next(f);
      piece->poly_order = (int)next(f);
      for (int i = 0; i < 3; i++)
        piece->poly_coef[i] = next(f);
      piece->mmr_order = (int)next(f);
      piece->mmr_constant = next(f);
      for (int i = 0; i < 3; i++)
        for (int j = 0; j < 7; j++)
          piece->mmr_coef[i][j] = next(f);
    }
    p->nlq[c].offset = (int32_t)next(f);
    p->nlq[c].slope = next(f);
    p->nlq[c].threshold = next(f);
    p->nlq[c].hdr_in_max = next(f);
  }
  fclose(f);
}

static uint16_t *read_frame(const char *path, int frame, int w, int h) {
  size_t count = (size_t)w * h * 3 / 2;
  uint16_t *data = malloc(count * sizeof(uint16_t));
  FILE *f = fopen(path, "rb");
  if (!data || !f || fseeko(f, (off_t)frame * (off_t)(count * 2), SEEK_SET) ||
      fread(data, 2, count, f) != count)
    fail("short layer capture");
  fclose(f);
  return data;
}

static struct frc_picture picture(const uint16_t *d, int w, int h) {
  struct frc_picture pic = {{{d, w, h, w},
                             {d + (size_t)w * h, w / 2, h / 2, w / 2},
                             {d + (size_t)w * h * 5 / 4, w / 2, h / 2, w / 2}}};
  return pic;
}

int main(int argc, char **argv) {
  if (argc != 10 && argc != 11)
    fail("usage: PARAMS BL.yuv EL.yuv FRAME WIDTH HEIGHT EL_WIDTH EL_HEIGHT OUT.raw [UNROUNDED.raw]");
  struct frc_params p;
  read_params(argv[1], &p);
  int frame = atoi(argv[4]), w = atoi(argv[5]), h = atoi(argv[6]);
  int ew = atoi(argv[7]), eh = atoi(argv[8]);
  uint16_t *bl = read_frame(argv[2], frame, w, h);
  uint16_t *el = read_frame(argv[3], frame, ew, eh);
  struct frc_picture blp = picture(bl, w, h), elp = picture(el, ew, eh);
  size_t count = (size_t)w * h * 3 / 2;
  uint16_t *out = malloc(count * sizeof(uint16_t));
  if (!out)
    fail("out of memory");
  int32_t *sum = argc == 11 ? malloc(count * sizeof(int32_t)) : NULL;
  if (argc == 11 && !sum)
    fail("out of memory");
  struct frc_output o = {{out, out + (size_t)w * h, out + (size_t)w * h * 5 / 4},
                         {w, w / 2, w / 2},
                         {sum, sum ? sum + (size_t)w * h : NULL,
                          sum ? sum + (size_t)w * h * 5 / 4 : NULL}};
  const char *why = frc_compose(&p, &blp, &elp, &o);
  if (why)
    fail(why);
  FILE *f = fopen(argv[9], "wb");
  if (!f || fwrite(out, 2, count, f) != count || fclose(f))
    fail("output write");
  if (sum) {
    f = fopen(argv[10], "wb");
    if (!f || fwrite(sum, 4, count, f) != count || fclose(f))
      fail("unrounded output write");
    free(sum);
  }
  free(bl);
  free(el);
  free(out);
  return 0;
}
