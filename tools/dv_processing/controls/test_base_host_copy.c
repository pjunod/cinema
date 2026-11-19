// Control-only: the base renderer's plane staging on a host copy of a decoded
// surface. A surface the Vulkan device cannot import is transferred to host
// memory as P010 (two planes, code << 6); the staging step once required
// planar low-aligned input and aborted the window on it. This drives the
// production staging function with dv_hw_host_copy forced, on both layouts.
// Needs a Vulkan device (a software one is enough); no decoder, no VA-API.
//
// Build with the helper's own flags plus -Wno-unused-function, against the
// dependency prefix: -I<prefix>/include, the static FFmpeg flags from
// <prefix>/ffmpeg/lib/pkgconfig, libplacebo's from its own .pc, and
// <prefix>/lib/libdovi.a.
#include "../fel_renderer.h"
#include "../base_dv_renderer.h"

static AVFrame *picture(enum AVPixelFormat format, int width, int height) {
  AVFrame *frame = av_frame_alloc();
  require(frame != NULL, "control frame");
  frame->format = format;
  frame->width = width;
  frame->height = height;
  require(av_frame_get_buffer(frame, 0) == 0, "control frame planes");
  // Every 10-bit code appears; P010 stores it high-aligned, planar low-aligned.
  int shift = format == AV_PIX_FMT_P010LE ? 6 : 0;
  int planes = format == AV_PIX_FMT_P010LE ? 2 : 3;
  for (int p = 0; p < planes; p++) {
    int rows = p ? height / 2 : height;
    int samples = p ? (planes == 2 ? width : width / 2) : width;
    for (int y = 0; y < rows; y++) {
      uint16_t *row = (uint16_t *)(frame->data[p] + (size_t)y * frame->linesize[p]);
      for (int x = 0; x < samples; x++)
        row[x] = (uint16_t)(((x + 37 * y + 211 * p) & 1023) << shift);
    }
  }
  return frame;
}

static void stage(enum AVPixelFormat format, int planes, bool keeps_textures) {
  pl_gpu gpu = base_context.vk->gpu;
  AVFrame *frame = picture(format, base_context.width, base_context.height);
  struct pl_frame source = {0};
  require(pl_map_avframe_ex(gpu, &source,
              pl_avframe_params(.frame = frame, .tex = base_context.textures)),
          "control frame upload");
  require(source.num_planes == planes, "control plane layout");
  pl_tex before[4] = {0};
  for (int p = 0; p < planes; p++)
    before[p] = source.planes[p].texture;
  int entered = source.repr.bits.bit_shift;
  base_restage_software_planes(gpu, frame, &source);
  require(source.repr.bits.sample_depth == 16 && source.repr.bits.color_depth == 10 &&
              source.repr.bits.bit_shift == 6,
          "staged base planes are code << 6 in UNORM16");
  require(source.num_planes == planes, "staging keeps the plane layout");
  for (int p = 0; p < planes; p++)
    require(source.planes[p].texture == before[p], "staging keeps the uploaded textures");
  // P010 arrives already shifted and is not rewritten; planar arrives low-aligned.
  require(entered == (keeps_textures ? 6 : 0), "upload representation");
  pl_unmap_avframe(gpu, &source);
  av_frame_free(&frame);
}

int main(void) {
  base_gpu_init(64, 64);
  // The import failure the renderer remembers for the rest of a window.
  dv_hw_host_copy = true;
  stage(AV_PIX_FMT_P010LE, 2, true);
  puts("test_base_host_copy_p010_planes_are_accepted_unchanged: PASS");
  stage(AV_PIX_FMT_YUV420P10LE, 3, false);
  puts("test_base_host_copy_planar_planes_are_restaged: PASS");
  puts("test_base_host_copy: PASS");
  return 0;
}
