// Control-only: run the production closed-form display-management shader (dv_dm.h) on a float RGBA
// raster for synthetic RPU metadata, and the double-precision CPU reference on the same input.
// Writes both rasters; the Python controller compares them (and the CPU reference against the
// measured vendor references). Never used by the production helper.
//
// usage: dm_shader_control PARAMS RGBA_F32_IN W H GPU_OUT CPU_OUT
// PARAMS lines (raw RPU units):
//   l1 MIN AVG MAX | l3 MIN AVG MAX | l6 MIN_1E4 MAX | l9 INDEX | l254
//   l2 TARGET_MAX_PQ SLOPE OFFSET POWER CHROMA SAT MS
//   l8 INDEX LENGTH SLOPE OFFSET POWER CHROMA SAT MS MID CLIP S0..S5 H0..H5
//   l10 INDEX TARGET_MAX_PQ TARGET_MIN_PQ PRIMARY_INDEX
//   target NITS FLOOR_NITS PRIM(0 bt2020|1 p3d65|2 bt709) SMAST_NITS
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libdovi/rpu_parser.h>
#include <libplacebo/vulkan.h>
#include "../dv_dm.h"

static void require(bool ok, const char *why) {
  if (!ok) { fprintf(stderr, "dm control refused: %s\n", why); exit(1); }
}

int main(int argc, char **argv) {
  require(argc == 7, "PARAMS RGBA_IN W H GPU_OUT CPU_OUT");
  int w = atoi(argv[3]), h = atoi(argv[4]);
  require(w > 0 && h > 0 && w <= 8192 && h <= 8192, "size");
  static DoviExtMetadataBlockLevel1 l1; static DoviExtMetadataBlockLevel3 l3; static DoviExtMetadataBlockLevel6 l6;
  static DoviExtMetadataBlockLevel9 l9; static DoviExtMetadataBlockLevel254 l254;
  static DoviExtMetadataBlockLevel2 l2[16]; static const DoviExtMetadataBlockLevel2 *l2p[16];
  static DoviExtMetadataBlockLevel8 l8[16]; static const DoviExtMetadataBlockLevel8 *l8p[16];
  static DoviExtMetadataBlockLevel10 l10[16]; static const DoviExtMetadataBlockLevel10 *l10p[16];
  DoviVdrDmData dm; memset(&dm, 0, sizeof(dm));
  int n2 = 0, n8 = 0, n10 = 0; double nits = 1000, floor_nits = 0, smast = 4000; int prim = 1;
  FILE *p = fopen(argv[1], "r"); require(p != NULL, "params open");
  char key[16];
  while (fscanf(p, "%15s", key) == 1) {
    unsigned v[32] = {0};
    if (!strcmp(key, "l1")) { require(fscanf(p, "%u %u %u", &v[0], &v[1], &v[2]) == 3, "l1");
      l1.min_pq = v[0]; l1.avg_pq = v[1]; l1.max_pq = v[2]; dm.dm_data.level1 = &l1; }
    else if (!strcmp(key, "l3")) { require(fscanf(p, "%u %u %u", &v[0], &v[1], &v[2]) == 3, "l3");
      l3.min_pq_offset = v[0]; l3.avg_pq_offset = v[1]; l3.max_pq_offset = v[2]; dm.dm_data.level3 = &l3; }
    else if (!strcmp(key, "l6")) { require(fscanf(p, "%u %u", &v[0], &v[1]) == 2, "l6");
      l6.min_display_mastering_luminance = v[0]; l6.max_display_mastering_luminance = v[1]; dm.dm_data.level6 = &l6; }
    else if (!strcmp(key, "l9")) { require(fscanf(p, "%u", &v[0]) == 1, "l9"); l9.source_primary_index = v[0]; dm.dm_data.level9 = &l9; }
    else if (!strcmp(key, "l254")) { l254.dm_version_index = 2; dm.dm_data.level254 = &l254; }
    else if (!strcmp(key, "l2")) { require(n2 < 16 && fscanf(p, "%u %u %u %u %u %u %u", &v[0], &v[1], &v[2], &v[3], &v[4], &v[5], &v[6]) == 7, "l2");
      DoviExtMetadataBlockLevel2 *x = &l2[n2]; x->target_max_pq = v[0]; x->trim_slope = v[1]; x->trim_offset = v[2];
      x->trim_power = v[3]; x->trim_chroma_weight = v[4]; x->trim_saturation_gain = v[5]; x->ms_weight = (int16_t)v[6];
      l2p[n2] = x; n2++; }
    else if (!strcmp(key, "l8")) { require(n8 < 16, "l8 count");
      for (int i = 0; i < 22; i++) require(fscanf(p, "%u", &v[i]) == 1, "l8");
      DoviExtMetadataBlockLevel8 *x = &l8[n8]; x->target_display_index = v[0]; x->length = v[1];
      x->trim_slope = v[2]; x->trim_offset = v[3]; x->trim_power = v[4]; x->trim_chroma_weight = v[5];
      x->trim_saturation_gain = v[6]; x->ms_weight = v[7]; x->target_mid_contrast = v[8]; x->clip_trim = v[9];
      x->saturation_vector_field0 = v[10]; x->saturation_vector_field1 = v[11]; x->saturation_vector_field2 = v[12];
      x->saturation_vector_field3 = v[13]; x->saturation_vector_field4 = v[14]; x->saturation_vector_field5 = v[15];
      x->hue_vector_field0 = v[16]; x->hue_vector_field1 = v[17]; x->hue_vector_field2 = v[18];
      x->hue_vector_field3 = v[19]; x->hue_vector_field4 = v[20]; x->hue_vector_field5 = v[21];
      l8p[n8] = x; n8++; }
    else if (!strcmp(key, "l10")) { require(n10 < 16 && fscanf(p, "%u %u %u %u", &v[0], &v[1], &v[2], &v[3]) == 4, "l10");
      DoviExtMetadataBlockLevel10 *x = &l10[n10]; x->length = 5; x->target_display_index = v[0]; x->target_max_pq = v[1];
      x->target_min_pq = v[2]; x->target_primary_index = v[3]; l10p[n10] = x; n10++; }
    else if (!strcmp(key, "target")) { require(fscanf(p, "%lf %lf %d %lf", &nits, &floor_nits, &prim, &smast) == 4, "target"); }
    else require(false, "unknown params key");
  }
  fclose(p);
  dm.dm_data.level2.list = (__typeof__(dm.dm_data.level2.list))l2p; dm.dm_data.level2.len = n2;
  dm.dm_data.level8.list = (__typeof__(dm.dm_data.level8.list))l8p; dm.dm_data.level8.len = n8;
  dm.dm_data.level10.list = (__typeof__(dm.dm_data.level10.list))l10p; dm.dm_data.level10.len = n10;

  dv_dm_frame f;
  const char *why = dv_dm_prepare(&dm, nits, floor_nits, (enum dm_prim)prim, smast, true, true, &f);
  require(why == NULL, why ? why : "prepare");
  size_t n = (size_t)w * h;
  float *pix = malloc(n * 16), *gpu_out = malloc(n * 16), *cpu_out = malloc(n * 16);
  require(pix && gpu_out && cpu_out, "memory");
  FILE *in = fopen(argv[2], "rb"); require(in != NULL, "input open");
  require(fread(pix, 16, n, in) == n && fgetc(in) == EOF, "exact input raster"); fclose(in);

  pl_log log = pl_log_create(PL_API_VER, NULL);
  pl_vulkan vk = pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
  require(vk != NULL, "Vulkan context");
  pl_gpu gpu = vk->gpu;
  pl_fmt format = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32, PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_LINEAR | PL_FMT_CAP_RENDERABLE);
  require(format != NULL, "float32 texture format");
  pl_tex source = pl_tex_create(gpu, pl_tex_params(.w = w, .h = h, .format = format, .sampleable = true, .host_writable = true));
  require(source && pl_tex_upload(gpu, pl_tex_transfer_params(.tex = source, .ptr = pix)), "input upload");
  pl_tex out = dv_dm_render(log, gpu, source, &f);
  require(out != NULL, "shader");
  pl_tex rb = pl_tex_create(gpu, pl_tex_params(.w = w, .h = h, .format = format, .host_readable = true, .blit_dst = true));
  require(rb != NULL, "readback texture");
  pl_tex_blit(gpu, pl_tex_blit_params(.src = out, .dst = rb));
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = rb, .ptr = gpu_out)), "download");
  require(dv_dm_cpu_render(&f, pix, w, h, cpu_out), "CPU reference");
  FILE *o1 = fopen(argv[5], "wb"), *o2 = fopen(argv[6], "wb");
  require(o1 && o2 && fwrite(gpu_out, 16, n, o1) == n && fwrite(cpu_out, 16, n, o2) == n && !fclose(o1) && !fclose(o2), "outputs");
  printf("{\"algo\":%d,\"trim\":\"%s\",\"trim_targets\":%d,\"weight\":%.6f}\n", f.algo, f.trim, f.trim_targets, f.weight);
  pl_tex_destroy(gpu, &rb); pl_tex_destroy(gpu, &source); dv_dm_destroy(gpu);
  pl_vulkan_destroy(&vk); pl_log_destroy(&log);
  free(pix); free(gpu_out); free(cpu_out);
  return 0;
}
