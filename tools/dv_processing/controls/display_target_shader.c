// Control-only native 64x64 RGB/PQ texture -> actual production shader/export.
// Inputs are authored by the independent Python controller, never a vendor LUT.
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libdovi/rpu_parser.h>
#include <libplacebo/vulkan.h>
#include "../yuv420p10.h"
static void require(bool ok, const char *why) {
    if (!ok) { fprintf(stderr, "target control refused: %s\n", why); exit(1); }
}
#include "../display_target.h"
int main(int argc, char **argv) {
    require(argc == 8, "POLICY FLOOR PEAK MAX_PQ AVG_PQ RGB_FLOAT_INPUT YUV_OUTPUT");
    require(dv_target_policy(argv[1], argv[2], argv[3]), "policy");
    unsigned max, avg;
    require(dv_target_uint(argv[4], &max) && max <= 4095 &&
            dv_target_uint(argv[5], &avg) && avg <= max, "L1");
    DoviExtMetadataBlockLevel1 l1 = {.min_pq = 0, .max_pq = max, .avg_pq = avg};
    DoviVdrDmData dm = {.dm_data = {.level1 = &l1}};
    dv_target_metadata(&dm);
    float pixels[64 * 64 * 4];
    FILE *input = fopen(argv[6], "rb");
    require(input != NULL, "input open");
    require(fread(pixels, sizeof(pixels), 1, input) == 1 && fgetc(input) == EOF &&
            !ferror(input), "exact input raster");
    require(fclose(input) == 0, "input close");
    pl_log log = pl_log_create(PL_API_VER, NULL);
    pl_vulkan vk = pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
    require(vk != NULL, "Vulkan context");
    pl_gpu gpu = vk->gpu;
    pl_fmt format = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
        PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_LINEAR);
    require(format != NULL, "float32 texture format");
    pl_tex source = pl_tex_create(gpu, pl_tex_params(.w = 64, .h = 64,
        .format = format, .sampleable = true, .host_writable = true));
    require(source && pl_tex_upload(gpu, pl_tex_transfer_params(.tex = source,
        .ptr = pixels)), "input upload");
    pl_tex target = dv_target_render(log, gpu, source, 0);
    require(dv_yuv10_init(log, gpu, 64, 64) && dv_yuv10_export(NULL, gpu, target),
            "production YUV export");
    dv_yuv10_wait();
    FILE *output = fopen(argv[7], "wbx");
    require(output != NULL, "exclusive output open");
    require(fwrite(dv_yuv10_context.frame, dv_yuv10_frame_bytes(), 1, output) == 1 &&
            fclose(output) == 0, "output write");
    // The optional UNORM source optimization is not used by this float-input control.
    (void)dv_yuv10_source_texture;
    dv_yuv10_close(gpu);
    dv_target_destroy(gpu);
    pl_tex_destroy(gpu, &source);
    pl_vulkan_destroy(&vk);
    pl_log_destroy(&log);
    return 0;
}
