// CPU-only real parser/manual mapper contract, using locked authored RPU controls.
// No device creation, rendering, HEVC association or normative pixel claim.
#include <assert.h>
#ifdef NDEBUG
#error "test_raw_rpu relies on assert; build without NDEBUG"
#endif
#include "fel_renderer.h"

int main(int argc, char **argv) {
  if (argc != 2) {
    fprintf(stderr, "usage: test_raw_rpu AUTHORED_RPU_NAL\n");
    return 2;
  }
  FILE *file = fopen(argv[1], "rb");
  assert(file);
  uint8_t bytes[65537];
  size_t size = fread(bytes, 1, sizeof(bytes), file);
  assert(!ferror(file) && feof(file) && size > 0 && size <= 65536);
  assert(fclose(file) == 0);
  struct pl_dovi_metadata mapped;
  map_fel_rpu(NULL, bytes, size, &mapped, false, 64, 64);
  // Constants authored by controls/generator/src/main.rs, not inferred from
  // the result or copied through a second libplacebo mapping call.
  assert(mapped.nlq_active);
  const int16_t nonlinear[3][3] = {
      {9574, 0, 13802}, {9574, -1540, -5348}, {9574, 17610, 0}};
  const int16_t linear[3][3] = {
      {7222, 8771, 390}, {2654, 12430, 1300}, {0, 422, 15962}};
  const float offsets[3] = {1.0f / 16, 0.5f, 0.5f};
  for (int c = 0; c < 3; c++) {
    assert(mapped.comp[c].num_pivots == 2);
    assert(mapped.comp[c].pivots[0] == 0);
    assert(mapped.comp[c].pivots[1] == 1023.0f / 1024);
    assert(mapped.comp[c].poly_coeffs[0][0] == 0);
    assert(mapped.comp[c].poly_coeffs[0][1] == 1);
    assert(mapped.comp[c].poly_coeffs[0][2] == 0);
    assert(mapped.nlq[c].offset == 512);
    assert(mapped.nlq[c].deadzone_slope == 1.0f / 4096);
    assert(mapped.nlq[c].deadzone_threshold == 0);
    assert(mapped.nlq[c].hdr_in_max == 0.125f);
    assert(mapped.nonlinear_offset[c] == offsets[c]);
    for (int k = 0; k < 3; k++) {
      assert(mapped.nonlinear.m[c][k] == nonlinear[c][k] / 8192.0f);
      assert(mapped.linear.m[c][k] == linear[c][k] / 16384.0f);
    }
  }
  puts("raw-RPU API387 manual projection passed (CPU only)");
  return 0;
}
