// Private qualification instrument: actual RPU + post-reconstruction RGB/PQ.
// Not built by build.sh, not a production output policy or a full trim renderer.
#include <errno.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <libavutil/mem.h>
#include <libavutil/sha.h>
#include <libplacebo/vulkan.h>
static void require(bool ok, const char *why) {
  if (!ok) { fprintf(stderr, "trim candidate refused: %s\n", why); exit(1); }
}
#include "trim_candidate_shader.h"

static void read_exact(const char *path, void *data, size_t size) {
  FILE *f = fopen(path, "rb");
  require(f != NULL, "input open");
  require(fread(data, 1, size, f) == size && fgetc(f) == EOF && !ferror(f),
          "exact bounded input length");
  require(fclose(f) == 0, "input close");
}
static DoviRpuOpaque *read_rpu(const char *path) {
  uint8_t nal[4099];
  FILE *f = fopen(path, "rb");
  require(f != NULL, "RPU open");
  size_t n = fread(nal, 1, sizeof(nal), f);
  require(n >= 3 && n <= 4098 && feof(f) && !ferror(f), "bounded fresh RPU NAL");
  require(fclose(f) == 0, "RPU close");
  DoviRpuOpaque *rpu = dovi_parse_unspec62_nalu(nal, n);
  require(rpu && !dovi_rpu_get_error(rpu), "actual libdovi RPU parse/CRC");
  return rpu;
}
#include "trim_identity.h"
int main(int argc, char **argv) {
  require(argc >= 10 && argc <= 40 && argc % 2 == 0,
      "POLICY FLOOR PEAK WIDTH HEIGHT MODE OUTPUT_DIR RPU RGBA32F [RPU RGBA32F ...] (<=16 frames)");
  require(dv_target_policy(argv[1], argv[2], argv[3]), "explicit target policy");
  unsigned width, height;
  require(dv_target_uint(argv[4], &width) && dv_target_uint(argv[5], &height) &&
      width >= 16 && width <= 4096 && height >= 16 && height <= 2160,
      "bounded native raster");
  float direct[3]; char tail;
  bool public_bridge = !strcmp(argv[6], DV_TRIM_BRIDGE_METHOD);
  require(public_bridge || sscanf(argv[6], "direct-b3=%f,%f,%f%c", &direct[0],
      &direct[1], &direct[2], &tail) == 3, "explicit parameter provenance");
  require(mkdir(argv[7], 0700) == 0, "fresh output directory");
  size_t count = (size_t)width * height * 4;
  float *pixels = malloc(count * sizeof(float));
  require(pixels != NULL, "bounded RGB buffer");
  pl_log log = pl_log_create(PL_API_VER, NULL);
  pl_vulkan vk = pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
  require(vk != NULL, "native Vulkan context");
  pl_gpu gpu = vk->gpu;
  pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
      PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_LINEAR);
  require(fmt != NULL, "source float32 format");
  pl_tex source = pl_tex_create(gpu, pl_tex_params(.w = (int)width, .h = (int)height,
      .format = fmt, .sampleable = true, .host_writable = true));
  require(source != NULL, "source texture");
  struct dv_trim_gpu state = {0};
  for (int arg = 8, frame = 0; arg < argc; arg += 2, frame++) {
    DoviRpuOpaque *rpu = read_rpu(argv[arg]);
    const DoviRpuDataHeader *header = dovi_rpu_get_header(rpu);
    const DoviVdrDmData *dm = dovi_rpu_get_vdr_dm_data(rpu);
    require(header && dm && !header->use_prev_vdr_rpu_flag &&
        header->vdr_dm_metadata_present_flag, "fresh complete per-frame metadata");
    struct dv_trim_selection trim;
    const char *error = dv_trim_select(dm, dv_target.nits, &trim);
    require(error == NULL, error ? error : "trim selection");
    dv_target_metadata(dm);
    float sop[3];
    if (public_bridge) require(dv_trim_public_sop(&trim, sop), "public SOP level/encoding");
    else memcpy(sop, direct, sizeof(sop));
    char key[65];
    dv_trim_identity(&trim, dm->dm_data.level1, sop, width, height, public_bridge, key);
    read_exact(argv[arg+1], pixels, count * sizeof(float));
    for (size_t i = 0; i < count; i++)
      require(isfinite(pixels[i]) && pixels[i] >= 0 && pixels[i] <= 1,
              "finite normalized post-reconstruction PQ input");
    require(pl_tex_upload(gpu, pl_tex_transfer_params(.tex = source, .ptr = pixels)),
            "native master upload");
    pl_tex mapped = dv_target_render(log, gpu, source, frame);
    pl_tex result = dv_trim_render(&state, log, gpu, mapped, sop, dv_target.nits);
    require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = result, .ptr = pixels)),
            "candidate output download");
    char path[8192];
    require(snprintf(path, sizeof(path), "%s/frame-%03d.rgba32f", argv[7], frame) <
            (int)sizeof(path), "bounded output path");
    FILE *f = fopen(path, "wbx");
    require(f && fwrite(pixels, sizeof(float), count, f) == count && fclose(f) == 0,
            "exclusive candidate output");
    printf("{\"kind\":\"trim_candidate_frame\",\"frame\":%d,\"method\":\"%s\","
           "\"acceptance\":\"unaccepted\",\"creative_trims_applied\":false,"
           "\"scope\":\"rgb-sop-only\",\"chroma_compensation_applied\":false,"
           "\"saturation_gain_applied\":false,\"other_trim_fields_applied\":false,"
           "\"l3_applied\":false,\"detail_applied\":false,"
           "\"parameter_source\":\"%s\",\"b3_method\":\"%s\","
           "\"numerical_policy\":\"%s\",\"selected_metadata_level\":%u,"
           "\"identity_sha256\":\"%s\","
           "\"gain\":%.9g,\"offset\":%.9g,\"gamma\":%.9g,\"trim_words\":[",
           frame, DV_TRIM_METHOD, public_bridge ? DV_TRIM_BRIDGE_METHOD : DV_TRIM_DIRECT_METHOD,
           DV_TRIM_B3_METHOD, DV_TRIM_BOUNDARY_METHOD, trim.word[DT_LEVEL], key,
           sop[0], sop[1], sop[2]);
    for (unsigned i = 0; i < DV_TRIM_ID_WORDS; i++)
      printf("%s%u", i ? "," : "", trim.word[i]);
    puts("]}");
    dovi_rpu_free_vdr_dm_data(dm);
    dovi_rpu_free_header(header);
    dovi_rpu_free(rpu);
  }
  dv_trim_gpu_destroy(&state, gpu);
  dv_target_destroy(gpu);
  pl_tex_destroy(gpu, &source);
  pl_vulkan_destroy(&vk); pl_log_destroy(&log); free(pixels);
  return 0;
}
