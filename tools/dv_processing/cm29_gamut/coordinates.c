/* Research-only native coordinate extraction for flat CM v2.9 probe tiles.
 * Usage: coordinates PEAK_NITS MIN_NITS SRC_YUV COORDS_TXT OUT_F64 [LIFT [SATURATION_GAIN]]
 * Input raster is little-endian BT.2020/PQ f32 RGB; output coordinates are
 * explicitly serialized as little-endian IEEE-754 binary64.
 */
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../dm_closed_form.h"

#define WIDTH 3840
#define HEIGHT 2160

_Static_assert(sizeof(float) == 4, "requires IEEE-754 binary32 float");
_Static_assert(sizeof(double) == 8, "requires IEEE-754 binary64 double");

static float read_f32le(const uint8_t b[4]) {
  uint32_t bits = (uint32_t)b[0] | ((uint32_t)b[1] << 8) |
                  ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24);
  float value; memcpy(&value, &bits, sizeof(value)); return value;
}

static int write_f64le(FILE *out, double value) {
  uint64_t bits; memcpy(&bits, &value, sizeof(bits));
  uint8_t b[8];
  for (int i = 0; i < 8; i++) b[i] = (uint8_t)(bits >> (8 * i));
  return fwrite(b, sizeof(b), 1, out) == 1;
}

int main(int argc, char **argv) {
  if (argc != 6 && argc != 7 && argc != 8) {
    fprintf(stderr, "usage: %s PEAK_NITS MIN_NITS SRC_YUV COORDS_TXT OUT_F64 [LIFT [SATURATION_GAIN]]\n", argv[0]);
    return 2;
  }
  char *end_peak, *end_min;
  double peak = strtod(argv[1], &end_peak), min_nits = strtod(argv[2], &end_min);
  if (!*argv[1] || *end_peak || !*argv[2] || *end_min || !isfinite(peak) ||
      !isfinite(min_nits) || peak <= 0 || min_nits < 0 || min_nits >= peak) return 2;
  const double L1[3] = {0.000488400488, 0.5001221, 0.902808303};
  const dm_cm29_target target = {peak, min_nits, DM_P3D65, 0.0};
  dm_cm29_trim trim = {0}; dm_cm29_state state;
  if (argc > 6) {
    char *end; trim.lift = strtod(argv[6], &end);
    if (!*argv[6] || *end || !isfinite(trim.lift) || trim.lift < -1 || trim.lift > 1) return 2;
  }
  if (argc > 7) {
    char *end; trim.sg = strtod(argv[7], &end);
    if (!*argv[7] || *end || !isfinite(trim.sg) || trim.sg < -1 || trim.sg > 1) return 2;
  }
  if (!dm_cm29_prepare(&state, L1, 0.005, 4000.0, &target, &trim)) return 3;

  FILE *in = fopen(argv[3], "rb"), *coords = fopen(argv[4], "r"), *out = fopen(argv[5], "wb");
  if (!in || !coords || !out) {
    if (in) fclose(in); if (coords) fclose(coords); if (out) fclose(out);
    fprintf(stderr, "failed opening input/output\n"); return 4;
  }
  int x, y, status = 0, scanned;
  while ((scanned = fscanf(coords, "%d %d", &x, &y)) == 2) {
    if (x < 0 || x >= WIDTH || y < 0 || y >= HEIGHT) { status = 5; break; }
    uint8_t bytes[12];
    long offset = ((long)y * WIDTH + x) * (long)sizeof(bytes);
    if (fseek(in, offset, SEEK_SET) || fread(bytes, sizeof(bytes), 1, in) != 1) { status = 5; break; }
    double rgb[3] = {read_f32le(bytes), read_f32le(bytes + 4), read_f32le(bytes + 8)};
    dm_cm29_pix pixel;
    dm_cm29_pixel_pre(&state, rgb, &pixel);
    double Ib = dm_clampd(pixel.Ib, 0, state.TM), Ii = dm_maxd(pixel.I, 1e-6);
    double sc = dm_clampd((Ib / Ii) * (2 + Ii) / (2 + Ib), .5, 2) * (1 + .5 * state.tr.sg);
    double u[3] = {Ib * 64 / state.TM,
                   pixel.ipt[1] * sc * 32 / state.TM,
                   pixel.ipt[2] * sc * 32 / state.TM};
    if (!write_f64le(out, u[0]) || !write_f64le(out, u[1]) || !write_f64le(out, u[2])) {
      status = 6; break;
    }
  }
  if (ferror(coords) || (scanned != EOF && scanned != 2)) status = 5;
  int close_in = fclose(in), close_coords = fclose(coords), close_out = fclose(out);
  if (close_in || close_coords || close_out) status = 7;
  return status;
}
