// Authored engineering regressions. Run only after independent complete review.
// Synthetic structs/pixels establish selection/cache mechanics, never DV optics.
#include <assert.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libplacebo/vulkan.h>
static void require(bool ok, const char *why) {
  if (!ok) { fprintf(stderr, "trim regression refused: %s\n", why); exit(1); }
}
#include "trim_candidate_shader.h"
#include "trim_identity.h"
static DoviExtMetadataBlockLevel2 neutral_l2(unsigned target) {
  return (DoviExtMetadataBlockLevel2){.target_max_pq = (uint16_t)target,
    .trim_slope = 2048, .trim_offset = 2048, .trim_power = 2048,
    .trim_chroma_weight = 2048, .trim_saturation_gain = 2048, .ms_weight = 2048};
}
static void test_public_level_sop_and_boundaries(void) {
  // Same encoded values must yield distinct actual L2/L8 parameters. Expected
  // fractions are exact binary values from the published rescaling, not a fit.
  const unsigned raw[] = {0, 1, 2047, 2048, 2049, 4095};
  const float gain2[] = {0.5f, 0.500244140625f, 0.999755859375f, 1,
                         1.000244140625f, 1.499755859375f};
  const float gain8[] = {0, 0.00048828125f, 0.99951171875f, 1,
                         1.00048828125f, 1.99951171875f};
  struct dv_trim_selection trim; dv_trim_neutral(&trim);
  float sop[3];
  assert(dv_trim_public_sop(&trim, sop) && sop[0] == 1 && sop[1] == 0 && sop[2] == 1);
  for (unsigned i = 0; i < sizeof(raw) / sizeof(*raw); i++) {
    trim.word[DT_SLOPE] = trim.word[DT_OFFSET] = trim.word[DT_POWER] = raw[i];
    trim.word[DT_LEVEL] = 2;
    assert(dv_trim_public_sop(&trim, sop));
    assert(sop[0] == gain2[i] && sop[1] == gain2[i] - 1 && sop[2] == gain2[i]);
    assert(dv_trim_b3_parameters_valid(sop, 1000));
    trim.word[DT_LEVEL] = 8;
    assert(dv_trim_public_sop(&trim, sop));
    assert(sop[0] == gain8[i] && sop[1] == gain8[i] - 1 && sop[2] == gain8[i]);
    assert(dv_trim_b3_parameters_valid(sop, 1000));
  }
  trim.word[DT_LEVEL] = 3; assert(!dv_trim_public_sop(&trim, sop));
  trim.word[DT_LEVEL] = 0; assert(!dv_trim_public_sop(&trim, sop)); // nonneutral without provenance
  trim.word[DT_LEVEL] = 8; trim.word[DT_POWER] = 4096;
  assert(!dv_trim_public_sop(&trim, sop));
  const float invalid[][3] = {{2, 0, 1}, {1, 1, 1}, {1, 0, 2},
    {-0.0001f, 0, 1}, {1, -1.0001f, 1}, {1, 0, -0.0001f},
    {NAN, 0, 1}, {1, INFINITY, 1}, {1, 0, NAN}};
  for (unsigned i = 0; i < sizeof(invalid) / sizeof(*invalid); i++)
    assert(!dv_trim_b3_parameters_valid(invalid[i], 1000));
  const float fractional[] = {1.75f, -0.75f, 0.25f};
  assert(dv_trim_b3_parameters_valid(fractional, 1000)); // no old L2-only envelope
  assert(!dv_trim_b3_parameters_valid(fractional, NAN));
  assert(!dv_trim_b3_parameters_valid(fractional, 0));
  assert(dv_trim_b3_component(0, 1000, 0, -1, 0) == 1000); // explicit 0^0 policy
  assert(dv_trim_b3_component(0, 1000, 0, -1, 1.0/2048) == 0);
  assert(dv_trim_b3_component(1000, 1000, 1.75, -0.75, 0.25) == 1000);
}
static void test_selection_and_identity(void) {
  DoviExtMetadataBlockLevel1 l1 = {.min_pq = 0, .max_pq = 4095, .avg_pq = 2048};
  unsigned bucket = dv_trim_bucket_pq(1000);
  DoviExtMetadataBlockLevel2 low = neutral_l2(bucket - 1),
    exact = neutral_l2(bucket), high = neutral_l2(bucket + 1);
  const DoviExtMetadataBlockLevel2 *list[] = {&high, &low, &exact};
  DoviVdrDmData dm = {.dm_data = {.level1 = &l1,
    .level2 = {.list = list, .len = 3}}};
  struct dv_trim_selection selected, original;
  assert(!dv_trim_select(&dm, 1000, &selected));
  assert(selected.word[DT_TARGET_PQ] == bucket && selected.word[DT_LEVEL] == 2);
  assert(dv_trim_sop_neutral(&selected));
  original = selected;
  list[0] = &exact; list[2] = &high;
  assert(!dv_trim_select(&dm, 1000, &selected));
  assert(!memcmp(&selected, &original, sizeof(selected))); // list order irrelevant
  dm.dm_data.level2.len = 1; list[0] = &high;
  assert(!dv_trim_select(&dm, 1000, &selected));
  assert(!selected.word[DT_LEVEL] && dv_trim_sop_neutral(&selected)); // no below
  dm.dm_data.level2.len = 2; list[0] = list[1] = &exact;
  assert(dv_trim_select(&dm, 1000, &selected)); // duplicate target refuses
  dm.dm_data.level2.len = 1;
  DoviExtMetadataBlockLevel8 l8 = {.length = 10, .target_display_index = 20,
    .trim_slope = 2100, .trim_offset = 2048, .trim_power = 2048,
    .trim_chroma_weight = 2048, .trim_saturation_gain = 2048, .ms_weight = 2048};
  DoviExtMetadataBlockLevel10 l10 = {.length = 5, .target_display_index = 20,
    .target_max_pq = (uint16_t)bucket, .target_min_pq = 1, .target_primary_index = 2};
  const DoviExtMetadataBlockLevel8 *l8s[] = {&l8};
  const DoviExtMetadataBlockLevel10 *l10s[] = {&l10};
  dm.dm_data.level8.list = l8s; dm.dm_data.level8.len = 1;
  assert(dv_trim_select(&dm, 1000, &selected)); // never guess preset/index target
  dm.dm_data.level10.list = l10s; dm.dm_data.level10.len = 1;
  assert(!dv_trim_select(&dm, 1000, &selected));
  assert(selected.word[DT_LEVEL] == 8 && selected.word[DT_SLOPE] == 2100);
  l10.target_max_pq++;
  assert(!dv_trim_select(&dm, 1000, &selected) && selected.word[DT_LEVEL] == 2);
  l10.target_max_pq--;
  assert(!dv_trim_select(&dm, 1000, &selected));
  float sop[3]; assert(dv_trim_public_sop(&selected, sop));
  char before[65], after[65];
  dv_target.nits = 1000; dv_target.min_1e4 = 50; dv_target.title_peak = 4000;
  dv_trim_identity(&selected, &l1, sop, 64, 64, true, before);
  dv_trim_identity(&selected, &l1, sop, 64, 64, true, after);
  assert(!strcmp(before, after)); // stable shot
  for (unsigned i = 0; i < DV_TRIM_ID_WORDS; i++) {
    struct dv_trim_selection changed = selected; changed.word[i]++;
    dv_trim_identity(&changed, &l1, sop, 64, 64, true, after);
    assert(strcmp(before, after)); // every selected actual field, even unapplied
  }
  dv_trim_identity(&selected, &l1, sop, 64, 64, false, after);
  assert(strcmp(before, after)); // public bridge versus direct parameter source
  dv_trim_identity(&selected, &l1, sop, 65, 64, true, after);
  assert(strcmp(before, after));
  dv_trim_identity(&selected, &l1, sop, 64, 65, true, after);
  assert(strcmp(before, after));
  l1.avg_pq++; dv_trim_identity(&selected, &l1, sop, 64, 64, true, after);
  assert(strcmp(before, after)); l1.avg_pq--;
  dv_target.nits++; dv_trim_identity(&selected, &l1, sop, 64, 64, true, after);
  assert(strcmp(before, after)); dv_target.nits--;
  sop[0] += 0.125f;
  dv_trim_identity(&selected, &l1, sop, 64, 64, true, after);
  assert(strcmp(before, after)); // actual direct uniform changes identity
  assert(dv_trim_b3_component(400, 1000, 1, 0, 1) == 400);
  assert(dv_trim_b3_component(400, 1000, 1.25, 0, 1) == 500);
  assert(dv_trim_b3_component(400, 1000, 1, -0.5, 1) == 0);
  assert(dv_trim_b3_component(1000, 1000, 1.5, 0.5, 1.5) == 1000);
}
static void test_only_trim_changes_native_output(void) {
  float pixels[64 * 64 * 4], a[64 * 64 * 4], b[64 * 64 * 4], again[64 * 64 * 4];
  for (unsigned i = 0; i < 64 * 64; i++) {
    pixels[4*i] = 0.4f; pixels[4*i+1] = 0.5f;
    pixels[4*i+2] = 0.6f; pixels[4*i+3] = 1;
  }
  pl_log log = pl_log_create(PL_API_VER, NULL);
  pl_vulkan vk = pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
  require(vk != NULL, "Vulkan"); pl_gpu gpu = vk->gpu;
  pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32, PL_FMT_CAP_SAMPLEABLE);
  require(fmt != NULL, "float format");
  pl_tex source = pl_tex_create(gpu, pl_tex_params(.w = 64, .h = 64,
      .format = fmt, .host_writable = true, .sampleable = true));
  require(source && pl_tex_upload(gpu, pl_tex_transfer_params(.tex = source,
      .ptr = pixels)), "constant master upload");
  struct dv_trim_selection trim; dv_trim_neutral(&trim);
  float sop[3]; assert(dv_trim_public_sop(&trim, sop));
  struct dv_trim_gpu state = {0};
  pl_tex output = dv_trim_render(&state, log, gpu, source, sop, 1000);
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output, .ptr = a)), "A read");
  assert(!memcmp(pixels, a, sizeof(a))); // EXACT neutral, no PQ round trip
  trim.word[DT_LEVEL] = 2; trim.word[DT_SLOPE] = 2560; assert(dv_trim_public_sop(&trim, sop));
  output = dv_trim_render(&state, log, gpu, source, sop, 1000);
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output, .ptr = a)), "nonneutral A read");
  trim.word[DT_LEVEL] = 8; assert(dv_trim_public_sop(&trim, sop));
  output = dv_trim_render(&state, log, gpu, source, sop, 1000);
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output, .ptr = b)), "B read");
  assert(memcmp(a, b, sizeof(a))); // same shader body; only actual trim changed
  trim.word[DT_LEVEL] = 2; trim.word[DT_SLOPE] = 2560; assert(dv_trim_public_sop(&trim, sop));
  output = dv_trim_render(&state, log, gpu, source, sop, 1000);
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output, .ptr = again)), "A repeat read");
  assert(!memcmp(a, again, sizeof(a))); // no stale B curve on returning to A
  for (unsigned i = 0; i < 64 * 64; i++) {
    assert(b[4*i] > a[4*i] && b[4*i+1] > a[4*i+1] && b[4*i+2] > a[4*i+2]);
    assert(isfinite(b[4*i]) && b[4*i] <= 1);
  }
  // Exercise the formerly rejected L8 envelope and its zero-power boundary in
  // the same dispatch cache. The boundary branch must avoid GLSL pow(0,0).
  trim.word[DT_LEVEL] = 8;
  trim.word[DT_SLOPE] = trim.word[DT_OFFSET] = trim.word[DT_POWER] = 0;
  assert(dv_trim_public_sop(&trim, sop));
  output = dv_trim_render(&state, log, gpu, source, sop, 1000);
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output, .ptr = b)), "zero power read");
  for (unsigned i = 0; i < 64 * 64; i++)
    for (unsigned c = 0; c < 3; c++)
      assert(isfinite(b[4*i+c]) && fabs(dv_target_eotf(b[4*i+c]) - 1000) < 0.1);
  trim.word[DT_POWER] = 1; assert(dv_trim_public_sop(&trim, sop));
  output = dv_trim_render(&state, log, gpu, source, sop, 1000);
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output, .ptr = b)), "positive fractional power read");
  for (unsigned i = 0; i < 64 * 64; i++)
    for (unsigned c = 0; c < 3; c++)
      assert(isfinite(b[4*i+c]) && dv_target_eotf(b[4*i+c]) < 0.0001);
  dv_trim_gpu_destroy(&state, gpu); pl_tex_destroy(gpu, &source);
  pl_vulkan_destroy(&vk); pl_log_destroy(&log);
}
int main(void) {
  test_public_level_sop_and_boundaries(); test_selection_and_identity();
  test_only_trim_changes_native_output();
  // This control intentionally tests SOP alone; the included L1 path is covered
  // by separate existing target controls and the source-bound packet instrument.
  (void)dv_target_policy; (void)dv_target_policy_name; (void)dv_target_metadata;
  (void)dv_target_render; (void)dv_target_destroy;
  puts("synthetic trim selection/identity/SOP cache controls passed; no optics acceptance");
  return 0;
}
