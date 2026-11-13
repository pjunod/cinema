/* Research-only CM v2.9 lattice probe generator.
 * Usage: probe PEAK_NITS MIN_NITS FRAME SEED OUT_YUV OUT_COORDS [LIFT [SATURATION_GAIN]]
 * Flat 11x5 tiles retain the original pointwise probe layout. Float raster I/O
 * is native IEEE-754 little-endian, matching the f32 input format on LE hosts.
 */
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../dm_closed_form.h"

#define WIDTH 3840
#define HEIGHT 2160
#define TOP 42
#define TILE_W 11
#define TILE_H 5

static dm_cm29_state s;
static double sI, sC;
static uint64_t rs = UINT64_C(88172645463325252);

static double rnd(void) {
  rs ^= rs << 13; rs ^= rs >> 7; rs ^= rs << 17;
  return (rs >> 11) / 9007199254740992.0;
}

/* Input colour whose pointwise lattice coordinate is (u0,u1,u2) steps. */
static int colour(double u0, double u1, double u2, float rgb[3]) {
  double Ib = u0 * sI, lo = 0, hi = 1;
  if (dm_cm29_tc(&s, 0) > Ib || dm_cm29_tc(&s, 1) < Ib) return 0;
  /* Include the top endpoint: a clipped tone curve may first reach TM at I < 1. */
  for (int k = 0; k < 60; k++) {
    double m = 0.5 * (lo + hi);
    if (dm_cm29_tc(&s, m) < Ib) lo = m; else hi = m;
  }
  double I = 0.5 * (lo + hi);
  if (I < 1e-4) return 0;
  double sc = dm_clampd((Ib / I) * (2 + I) / (2 + Ib), .5, 2) * (1 + .5 * s.tr.sg);
  double q[3] = {I, u1 * sC / sc, u2 * sC / sc}, lmsp[3], lin[3], o[3];
  dm_mat_vec(s.ipt2lms, q, lmsp);
  for (int k = 0; k < 3; k++) {
    if (lmsp[k] < 0 || lmsp[k] > 1) return 0;
    lin[k] = dm_pq_eotf(lmsp[k]);
  }
  dm_mat_vec(s.lms2rgb, lin, o);
  for (int k = 0; k < 3; k++) {
    if (o[k] < 0 || o[k] > 10000) return 0;
    rgb[k] = (float)dm_pq_oetf(o[k]);
  }
  return 1;
}

static int parse_int(const char *text, int *out) {
  char *end; long value = strtol(text, &end, 10);
  if (!*text || *end || value < 0 || value > 0x7fffffffL) return 0;
  *out = (int)value; return 1;
}

int main(int argc, char **argv) {
  if (argc != 7 && argc != 8 && argc != 9) {
    fprintf(stderr, "usage: %s PEAK_NITS MIN_NITS FRAME SEED OUT_YUV OUT_COORDS [LIFT [SATURATION_GAIN]]\n", argv[0]);
    return 2;
  }
  char *peak_end, *min_end, *seed_end;
  double peak = strtod(argv[1], &peak_end), min_nits = strtod(argv[2], &min_end);
  int frame;
  unsigned long long seed = strtoull(argv[4], &seed_end, 10);
  if (!*argv[1] || *peak_end || !*argv[2] || *min_end || !isfinite(peak) || !isfinite(min_nits) ||
      peak <= 0 || min_nits < 0 || min_nits >= peak || !parse_int(argv[3], &frame) ||
      !*argv[4] || argv[4][0] == '-' || *seed_end) return 2;
  uint16_t endian = 1;
  if (*(const uint8_t *)&endian != 1) {
    fprintf(stderr, "probe raster output requires a little-endian host\n"); return 2;
  }

  const double L1[3] = {0.000488400488, 0.5001221, 0.902808303};
  const dm_cm29_target target = {peak, min_nits, DM_P3D65, 0.0};
  dm_cm29_trim trim = {0};
  if (argc > 7) {
    char *end; trim.lift = strtod(argv[7], &end);
    if (!*argv[7] || *end || !isfinite(trim.lift) || trim.lift < -1 || trim.lift > 1) return 2;
  }
  if (argc > 8) {
    char *end; trim.sg = strtod(argv[8], &end);
    if (!*argv[8] || *end || !isfinite(trim.sg) || trim.sg < -1 || trim.sg > 1) return 2;
  }
  if (!dm_cm29_prepare(&s, L1, 0.005, 4000.0, &target, &trim)) {
    fprintf(stderr, "invalid target configuration\n"); return 3;
  }
  sI = s.TM / 64.0; sC = s.TM / 32.0;
  rs ^= (uint64_t)seed * UINT64_C(0x9E3779B97F4A7C15);

  const int cols = WIDTH / TILE_W;
  const int rows = (HEIGHT - 2 * TOP) / TILE_H;
  const long per = (long)cols * rows, first = (long)frame * per, last = first + per;
  const size_t pixels = (size_t)WIDTH * HEIGHT;
  float *img = malloc(pixels * 3 * sizeof(float));
  if (!img) { fprintf(stderr, "out of memory\n"); return 4; }
  for (size_t i = 0; i < pixels * 3; i++) img[i] = 0.0151f;
  FILE *co = fopen(argv[6], "w");
  if (!co) { perror(argv[6]); free(img); return 4; }
  long n = 0;
#define EMIT do { \
    if (n >= first && n < last) { \
      long tile = n - first; int tx = (int)(tile % cols) * TILE_W; \
      int ty = TOP + (int)(tile / cols) * TILE_H; \
      for (int yy = 0; yy < TILE_H; yy++) for (int xx = 0; xx < TILE_W; xx++) \
        memcpy(img + 3 * ((size_t)(ty + yy) * WIDTH + tx + xx), c, 3 * sizeof(float)); \
      if (fprintf(co, "%d %d\n", tx + 5, ty + 2) < 0) { fclose(co); free(img); return 5; } \
    } \
    n++; \
  } while (0)
  float c[3];
  for (int i = 2; i <= 64; i++)
    for (int j = -16; j <= 16; j++)
      for (int k = -16; k <= 16; k++)
        if (colour(i, j, k, c)) EMIT;
  long vertex_samples = n;
  for (int i = 1; i < 64; i++) for (int j = -16; j < 16; j++) for (int k = -16; k < 16; k++) {
    int ok = 0;
    for (int b = 0; b < 8; b++)
      ok += (i + (b & 1) >= 2) && colour(i + (b & 1), j + ((b >> 1) & 1), k + ((b >> 2) & 1), c);
    if (i > 1 && (ok == 8 || ok == 0)) continue;
    for (int t = 0; t < (i == 1 ? 60 : 24); t++)
      if (colour(i + rnd(), j + rnd(), k + rnd(), c)) EMIT;
  }
  if (fclose(co)) { free(img); return 5; }
  FILE *f = fopen(argv[5], "wb");
  if (!f) { perror(argv[5]); free(img); return 4; }
  size_t written = fwrite(img, 3 * sizeof(float), pixels, f);
  int close_status = fclose(f);
  int ok = written == pixels && close_status == 0;
  free(img);
  if (!ok) { fprintf(stderr, "failed writing probe raster\n"); return 5; }
  fprintf(stderr, "vertex samples %ld, in-cell samples %ld, tiles per frame %ld, frames needed %ld\n",
          vertex_samples, n - vertex_samples, per, (n + per - 1) / per);
  return 0;
}
