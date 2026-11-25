// SPDX-License-Identifier: Apache-2.0
// Runs the GPU composition stage (fel_compose.h) on one frame of a layer
// capture and writes the composed 4:2:0 picture exactly as
// diagnostics/fel_quality/fel_reference_compose does: 16-bit little-endian
// planes holding hdr_bit_depth codes. run_fel_compose_parity.py compares the
// two outputs code for code.
//
// Usage: fel_compose_control PARAMS BL.yuv EL.yuv FRAME WIDTH HEIGHT
//                            EL_WIDTH EL_HEIGHT OUT.raw [interleaved]
//
// interleaved uploads each layer's Cb and Cr as one RG16 texture, the layout
// of a decoder surface the helper maps in place (P010), so the stage reads
// two components from one texture as it does in production.
//
// PARAMS is the reference's frc1 text (fel_reference_measure.params_text);
// this program reads it into the stage's own parameter struct. BL.yuv and
// EL.yuv are 10-bit 4:2:0 planar little-endian captures, frame after frame.
#define _POSIX_C_SOURCE 200809L
#include <inttypes.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <libplacebo/vulkan.h>
#include "../fel_compose.h"
#include "fel_compose_io.h"

// libplacebo reports some failures (a rejected pass) only in its log and
// still returns; any error there fails the control.
static int log_errors;
static void log_cb(void *priv, enum pl_log_level level, const char *msg) {
  (void)priv;
  if (level <= PL_LOG_ERR) log_errors++;
  if (level <= PL_LOG_WARN) fprintf(stderr, "libplacebo: %s\n", msg);
}

// Uploads one 4:2:0 picture as UNORM16 planes holding code << (16 - depth):
// three R16 planes, or R16 luma and one RG16 chroma plane.
static void upload(pl_gpu gpu, const uint16_t *d, int w, int h, int depth, bool interleaved,
                   struct fel_compose_layer *layer) {
  pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_UNORM, 1, 16, 16, PL_FMT_CAP_SAMPLEABLE);
  pl_fmt fmt2 = pl_find_fmt(gpu, PL_FMT_UNORM, 2, 16, 16, PL_FMT_CAP_SAMPLEABLE);
  if (!fmt || !fmt2) fail("UNORM16 plane format");
  for (size_t i = 0; i < (size_t)w * h * 3 / 2; i++)
    if (d[i] >> depth) fail("sample outside the declared bit depth");
  if (interleaved) {
    size_t ny = (size_t)w * h, nc = ny / 4;
    uint16_t *y = malloc(ny * 2), *uv = malloc(nc * 4);
    if (!y || !uv) fail("out of memory");
    for (size_t i = 0; i < ny; i++) y[i] = (uint16_t)(d[i] << (16 - depth));
    for (size_t i = 0; i < nc; i++) {
      uv[2 * i] = (uint16_t)(d[ny + i] << (16 - depth));
      uv[2 * i + 1] = (uint16_t)(d[ny + nc + i] << (16 - depth));
    }
    layer->tex[0] = pl_tex_create(gpu, pl_tex_params(.w = w, .h = h, .format = fmt,
                                                     .sampleable = true, .host_writable = true));
    layer->tex[1] = layer->tex[2] = pl_tex_create(gpu, pl_tex_params(.w = w / 2, .h = h / 2, .format = fmt2,
                                                                     .sampleable = true, .host_writable = true));
    if (!layer->tex[0] || !layer->tex[1] ||
        !pl_tex_upload(gpu, pl_tex_transfer_params(.tex = layer->tex[0], .ptr = y)) ||
        !pl_tex_upload(gpu, pl_tex_transfer_params(.tex = layer->tex[1], .ptr = uv)))
      fail("plane upload");
    layer->channel[0] = 0;
    layer->channel[1] = 0;
    layer->channel[2] = 1;
    free(y);
    free(uv);
    return;
  }
  size_t offset = 0;
  for (int c = 0; c < 3; c++) {
    int pw = c ? w / 2 : w, ph = c ? h / 2 : h;
    size_t n = (size_t)pw * ph;
    uint16_t *shifted = malloc(n * sizeof(uint16_t));
    if (!shifted) fail("out of memory");
    for (size_t i = 0; i < n; i++)
      shifted[i] = (uint16_t)(d[offset + i] << (16 - depth));
    layer->tex[c] = pl_tex_create(gpu, pl_tex_params(.w = pw, .h = ph, .format = fmt,
                                                     .sampleable = true, .host_writable = true));
    if (!layer->tex[c] || !pl_tex_upload(gpu, pl_tex_transfer_params(.tex = layer->tex[c], .ptr = shifted)))
      fail("plane upload");
    layer->channel[c] = 0;
    free(shifted);
    offset += n;
  }
}

int main(int argc, char **argv) {
  if (argc != 10 && !(argc == 11 && !strcmp(argv[10], "interleaved")))
    fail("usage: PARAMS BL.yuv EL.yuv FRAME WIDTH HEIGHT EL_WIDTH EL_HEIGHT OUT.raw [interleaved]");
  bool interleaved = argc == 11;
  struct fel_compose_params p;
  read_params(argv[1], &p);
  int frame = atoi(argv[4]), w = atoi(argv[5]), h = atoi(argv[6]), ew = atoi(argv[7]), eh = atoi(argv[8]);
  if (w <= 0 || h <= 0 || ew <= 0 || eh <= 0) fail("dimensions");
  uint16_t *bl = read_frame(argv[2], frame, w, h), *el = read_frame(argv[3], frame, ew, eh);
  pl_log log = pl_log_create(PL_API_VER, pl_log_params(.log_cb = log_cb, .log_level = PL_LOG_WARN));
  pl_vulkan vk = pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
  if (!vk) fail("Vulkan device");
  pl_gpu gpu = vk->gpu;
  struct fel_compose_layer bll, ell;
  upload(gpu, bl, w, h, p.bl_bit_depth, interleaved, &bll);
  upload(gpu, el, ew, eh, p.el_bit_depth, interleaved, &ell);
  pl_tex luma, chroma;
  // FEL_COMPOSE_TIMING=N composes N more times and reports each pass's GPU
  // time per frame (the stage's share of the 4K frame-time acceptance).
  const char *repeat = getenv("FEL_COMPOSE_TIMING");
  int timing = repeat ? atoi(repeat) : 0;
  fel_compose_gpu.timing = timing > 0;
  const char *why = fel_compose_render(log, gpu, &p, &bll, &ell, w, h, ew, eh, &luma, &chroma);
  for (int i = 0; i < timing && !why; i++)
    why = fel_compose_render(log, gpu, &p, &bll, &ell, w, h, ew, eh, &luma, &chroma);
  if (timing > 0) {
    pl_gpu_finish(gpu);
    for (int k = 0; k < 4; k++)
      for (uint64_t ns; (ns = pl_timer_query(gpu, fel_compose_gpu.timer[k])); )
        fel_compose_gpu.timing_ns[k] += ns;
    printf("{\"kind\":\"fel_compose_timing\",\"frames\":%d,\"v_luma_us\":%.1f,\"v_chroma_us\":%.1f,"
           "\"luma_us\":%.1f,\"chroma_us\":%.1f}\n", timing,
           fel_compose_gpu.timing_ns[0] / 1e3 / timing, fel_compose_gpu.timing_ns[1] / 1e3 / timing,
           fel_compose_gpu.timing_ns[2] / 1e3 / timing, fel_compose_gpu.timing_ns[3] / 1e3 / timing);
  }
  if (why) fail(why);
  if (log_errors) fail("libplacebo reported errors while composing");
  size_t ny = (size_t)w * h, nc = ny / 4;
  uint16_t *y = malloc(ny * 2), *cbcr = malloc(nc * 4), *out = malloc((ny + 2 * nc) * 2);
  if (!y || !cbcr || !out) fail("out of memory");
  if (!pl_tex_download(gpu, pl_tex_transfer_params(.tex = luma, .ptr = y)) ||
      !pl_tex_download(gpu, pl_tex_transfer_params(.tex = chroma, .ptr = cbcr)))
    fail("composed plane download (host-readable UNORM16 R/RG)");
  int shift = 16 - p.hdr_bit_depth;
  for (size_t i = 0; i < ny; i++) out[i] = y[i] >> shift;
  for (size_t i = 0; i < nc; i++) {
    out[ny + i] = cbcr[2 * i] >> shift;
    out[ny + nc + i] = cbcr[2 * i + 1] >> shift;
  }
  FILE *f = fopen(argv[9], "wb");
  if (!f || fwrite(out, 2, ny + 2 * nc, f) != ny + 2 * nc || fclose(f)) fail("output write");
  printf("{\"kind\":\"fel_compose_control\",\"method\":\"%s\",\"width\":%d,\"height\":%d,"
         "\"el_width\":%d,\"el_height\":%d,\"interleaved\":%s,\"gpu_glsl\":%d}\n",
         FEL_COMPOSE_METHOD, w, h, ew, eh, interleaved ? "true" : "false", gpu->glsl.version);
  fel_compose_destroy(gpu);
  for (int c = interleaved ? 1 : 2; c >= 0; c--) {
    pl_tex_destroy(gpu, &bll.tex[c]);
    pl_tex_destroy(gpu, &ell.tex[c]);
  }
  pl_vulkan_destroy(&vk);
  pl_log_destroy(&log);
  free(bl); free(el); free(y); free(cbcr); free(out);
  return 0;
}
