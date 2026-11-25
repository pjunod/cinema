// SPDX-License-Identifier: Apache-2.0
// The composition controls' inputs: the reference's frc1 parameter text read
// into the stage's own parameter struct, and one frame of a layer capture.
// Shared by fel_compose_control.c (GPU) and the shader's CPU port
// (fel_compose_cpu_port.py), so both read a case identically.
#ifndef PLURX_FEL_COMPOSE_IO_H
#define PLURX_FEL_COMPOSE_IO_H
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

#ifndef FEL_COMPOSE_IO_NAME
#define FEL_COMPOSE_IO_NAME "fel_compose_control"
#endif

static void fail(const char *why) {
  fprintf(stderr, FEL_COMPOSE_IO_NAME ": %s\n", why);
  exit(2);
}

static int64_t next(FILE *f) {
  int64_t v;
  if (fscanf(f, "%" SCNd64, &v) != 1) fail("malformed parameters");
  return v;
}

static void read_params(const char *path, struct fel_compose_params *p) {
  FILE *f = fopen(path, "r");
  char magic[8];
  if (!f || fscanf(f, "%7s", magic) != 1 || strcmp(magic, "frc1")) fail("parameters are not frc1 text");
  memset(p, 0, sizeof(*p));
  p->coefficient_log2_denom = (int)next(f);
  p->bl_bit_depth = (int)next(f);
  p->el_bit_depth = (int)next(f);
  p->hdr_bit_depth = (int)next(f);
  p->disable_residual = next(f) != 0;
  for (int c = 0; c < 3; c++) {
    int n = (int)next(f);
    if (n < 2 || n > FEL_COMPOSE_PIVOTS) fail("pivot count beyond the stage's eight pieces");
    p->map[c].num_pivots = n;
    for (int i = 0; i < n; i++) p->map[c].pivot[i] = (int)next(f);
    for (int k = 0; k < n - 1; k++) {
      int idc = (int)next(f), poly_order = (int)next(f);
      int64_t poly[3] = {next(f), next(f), next(f)};
      int mmr_order = (int)next(f);
      int64_t constant = next(f), mmr[21];
      for (int i = 0; i < 21; i++) mmr[i] = next(f);
      p->map[c].mapping_idc[k] = idc;
      if (idc == 0) {
        p->map[c].order[k] = poly_order;
        memcpy(p->map[c].coef[k], poly, sizeof(poly));
      } else {
        p->map[c].order[k] = mmr_order;
        p->map[c].coef[k][0] = constant;
        memcpy(&p->map[c].coef[k][1], mmr, sizeof(mmr));
      }
    }
    p->nlq[c].offset = (int)next(f);
    p->nlq[c].slope = (uint64_t)next(f);
    p->nlq[c].threshold = (uint64_t)next(f);
    p->nlq[c].hdr_in_max = (uint64_t)next(f);
  }
  fclose(f);
}

static uint16_t *read_frame(const char *path, int frame, int w, int h) {
  size_t count = (size_t)w * h * 3 / 2;
  uint16_t *data = (uint16_t *)malloc(count * sizeof(uint16_t));
  FILE *f = fopen(path, "rb");
  if (!data || !f || fseeko(f, (off_t)frame * (off_t)(count * 2), SEEK_SET) ||
      fread(data, 2, count, f) != count)
    fail("short layer capture");
  fclose(f);
  return data;
}

#endif
