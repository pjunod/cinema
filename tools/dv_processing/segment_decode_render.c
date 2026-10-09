#define _POSIX_C_SOURCE 200809L
// Finite timestamped source segment -> persistent native-plane FEL renderer.
// No fixture-definition hashes, test variants, production route or publication.
#include "fel_renderer.h"
#include "base_dv_renderer.h"
#include "nut_timing.h"
#include <errno.h>
#include <fcntl.h>
#include <libavcodec/avcodec.h>
#include <libavcodec/bsf.h>
#include <libavformat/avformat.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/opt.h>
#include <libavutil/sha.h>
#include <limits.h>
#include <sys/stat.h>
#include <unistd.h>

#define PAIR_WINDOW 16
static bool base_only;
static int source_profile;
static int expected_w[2], expected_h[2], frame_cap, emitted;
static bool debug_frames, initialized, window_mode, window_done, first_pair;
static bool source_eof, discover_el;
static unsigned preroll, paired, preroll_cap;
struct rational {
  int64_t num;
  int den;
};
static struct rational window_start, window_end;
static int64_t first_emitted_pts = AV_NOPTS_VALUE,
               last_emitted_end = AV_NOPTS_VALUE, boundary_pts = AV_NOPTS_VALUE;
struct budget_io {
  int fd;
  uint64_t bytes;
  bool exhausted;
};
static struct budget_io movie_io;
static AVIOContext *movie_avio;
#ifndef WINDOW_READ_LIMIT
#define WINDOW_READ_LIMIT (128ULL * 1024 * 1024)
#endif

static FILE *rgb_output, *timing_output;
static AVFormatContext *nut_output;
static AVStream *nut_stream;
static int64_t output_pts, output_duration, nut_tick_scale;
static int64_t last_pts = AV_NOPTS_VALUE;
static AVRational source_time_base;
struct coded_input {
  int64_t pts, duration;
  char packet_sha[65], rpu_sha[65];
  bool emitted;
};
static struct coded_input coded_window[64];
static unsigned coded_count;

struct layer {
  AVBSFContext *split;
  AVCodecContext *decoder;
  AVFrame *queue[PAIR_WINDOW];
  int count, index;
};
static struct layer layers[2];
static void need(int ok, const char *reason) {
  if (!ok) {
    fprintf(stderr, "Segment refused: %s\n", reason);
    exit(1);
  }
}
static struct rational rational(const char *text) {
  char *end;
  errno = 0;
  long long n = strtoll(text, &end, 10);
  need(!errno && end != text && *end == '/' && n >= 0,
       "nonnegative exact window rational");
  const char *den = end + 1;
  errno = 0;
  long d = strtol(den, &end, 10);
  need(!errno && end != den && *end == 0 && d > 0 && d <= INT_MAX,
       "bounded window denominator");
  return (struct rational){n, (int)d};
}
static int compare_window(int64_t pts, struct rational value) {
  return av_compare_ts(pts, source_time_base, value.num,
                       (AVRational){1, value.den});
}
static int movie_read(void *opaque, uint8_t *buffer, int size) {
  struct budget_io *io = opaque;
  if (io->bytes >= WINDOW_READ_LIMIT) {
    io->exhausted = true;
    return AVERROR(ENOSPC);
  }
  size_t available = (size_t)(WINDOW_READ_LIMIT - io->bytes);
  if ((size_t)size > available)
    size = (int)available;
  ssize_t n;
  do {
    n = read(io->fd, buffer, (size_t)size);
  } while (n < 0 && errno == EINTR);
  if (n < 0)
    return AVERROR(errno);
  if (n == 0)
    return AVERROR_EOF;
  io->bytes += (uint64_t)n;
  return (int)n;
}
static int64_t movie_seek(void *opaque, int64_t offset, int whence) {
  struct budget_io *io = opaque;
  if (whence & AVSEEK_SIZE) {
    struct stat st;
    return fstat(io->fd, &st) == 0 ? st.st_size : AVERROR(errno);
  }
  off_t result = lseek(io->fd, (off_t)offset, whence & ~AVSEEK_FORCE);
  return result < 0 ? AVERROR(errno) : (int64_t)result;
}
static void open_movie_io(AVFormatContext *input, const char *source) {
  movie_io.fd = open(source, O_RDONLY | O_CLOEXEC);
  struct stat st;
  need(movie_io.fd >= 0 && fstat(movie_io.fd, &st) == 0 && S_ISREG(st.st_mode),
       "held seekable regular movie source");
  uint8_t *buffer = av_malloc(65536);
  need(buffer != NULL, "bounded movie IO buffer");
  movie_avio = avio_alloc_context(buffer, 65536, 0, &movie_io, movie_read, NULL,
                                  movie_seek);
  need(movie_avio != NULL, "movie IO context");
  input->pb = movie_avio;
  input->flags |= AVFMT_FLAG_CUSTOM_IO;
}
static void drop_pair(void) {
  for (int k = 0; k < (base_only ? 1 : 2); k++) {
    av_frame_free(&layers[k].queue[0]);
    layers[k].count--;
    memmove(layers[k].queue, layers[k].queue + 1,
            (size_t)layers[k].count * sizeof(AVFrame *));
  }
}
// The packed buffer remains owned by gpu_render for the duration of this
// synchronous write. A blocked pipe provides backpressure without a frame
// queue.
static void write_rgb(const uint8_t *rgb, size_t bytes, void *unused) {
  (void)unused;
  if (!nut_output) {
    need(fwrite(rgb, 1, bytes, rgb_output) == bytes, "RGB frame write");
    return;
  }
  need(bytes <= INT_MAX && output_pts >= 0 && output_duration > 0,
       "bounded nonnegative NUT presentation interval");
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "NUT packet allocation");
  packet->data = (uint8_t *)rgb;
  packet->size = (int)bytes;
  packet->stream_index = nut_stream->index;
  need(nut_scale_interval(output_pts, output_duration, nut_tick_scale,
                          &packet->pts, &packet->duration),
       "NUT timestamp interval overflow");
  packet->dts = packet->pts;
  packet->flags = AV_PKT_FLAG_KEY;
  // av_write_frame does not take packet ownership. There is a single stream,
  // so interleaving would only introduce an unnecessary buffering boundary.
  int result = av_write_frame(nut_output, packet);
  packet->data = NULL;
  packet->size = 0;
  av_packet_free(&packet);
  need(result >= 0, "streamed NUT packet write");
  avio_flush(nut_output->pb);
  need(nut_output->pb->error >= 0, "streamed NUT pipe flush");
}
static void open_nut(const char *path) {
  need(avformat_alloc_output_context2(&nut_output, NULL, "nut", path) >= 0 &&
           nut_output,
       "NUT output context");
  nut_stream = avformat_new_stream(nut_output, NULL);
  need(nut_stream != NULL, "NUT video stream");
  nut_stream->time_base = source_time_base;
  AVCodecParameters *p = nut_stream->codecpar;
  p->codec_type = AVMEDIA_TYPE_VIDEO;
  p->codec_id = AV_CODEC_ID_RAWVIDEO;
  p->format = AV_PIX_FMT_RGB48LE;
  p->codec_tag = avcodec_pix_fmt_to_codec_tag(AV_PIX_FMT_RGB48LE);
  need(p->codec_tag != 0, "explicit RGB48 NUT tag");
  p->width = expected_w[0];
  p->height = expected_h[0];
  p->bits_per_coded_sample = 48;
  p->color_primaries = AVCOL_PRI_BT2020;
  p->color_trc = AVCOL_TRC_SMPTE2084;
  p->color_space = AVCOL_SPC_RGB;
  p->color_range = AVCOL_RANGE_JPEG;
  need(avio_open(&nut_output->pb, path, AVIO_FLAG_WRITE) >= 0,
       "NUT output open");
  need(avformat_write_header(nut_output, NULL) >= 0, "NUT output header");
  // Refuse a muxer time-base change that would round a source tick.
  need(
      source_time_base.num > 0 && source_time_base.den > 0 &&
          nut_stream->time_base.num > 0 && nut_stream->time_base.den > 0 &&
          ((int64_t)source_time_base.num * nut_stream->time_base.den) %
                  ((int64_t)source_time_base.den * nut_stream->time_base.num) ==
              0,
      "NUT must preserve exact source time ticks");
  nut_tick_scale = ((int64_t)source_time_base.num * nut_stream->time_base.den) /
                   ((int64_t)source_time_base.den * nut_stream->time_base.num);
}
static int number(const char *s, int low, int high) {
  char *end = NULL;
  errno = 0;
  long n = strtol(s, &end, 10);
  need(!errno && end != s && *end == 0 && n >= low && n <= high,
       "bounded numeric argument");
  return (int)n;
}
static enum pl_chroma_location chroma(enum AVChromaLocation value) {
  switch (value) {
  case AVCHROMA_LOC_LEFT:
    return PL_CHROMA_LEFT;
  case AVCHROMA_LOC_CENTER:
    return PL_CHROMA_CENTER;
  case AVCHROMA_LOC_TOPLEFT:
    return PL_CHROMA_TOP_LEFT;
  default:
    need(0, "unsupported or undeclared native chroma location");
    return PL_CHROMA_LEFT;
  }
}
static uint16_t *pack(const AVFrame *frame) {
  size_t count = (size_t)frame->width * frame->height * 3 / 2;
  uint16_t *out = malloc(count * 2);
  need(out != NULL, "native pair allocation");
  size_t at = 0;
  for (int c = 0; c < 3; c++) {
    int w = c ? frame->width / 2 : frame->width,
        h = c ? frame->height / 2 : frame->height;
    for (int y = 0; y < h; y++) {
      need(frame->linesize[c] >= w * 2, "native line stride");
      memcpy(out + at, frame->data[c] + (size_t)y * frame->linesize[c],
             (size_t)w * 2);
      at += (size_t)w;
    }
  }
  return out;
}
static void hash_bytes(const uint8_t *bytes, size_t size, char hex[65]) {
  struct AVSHA *sha = av_sha_alloc();
  uint8_t out[32];
  need(sha != NULL && av_sha_init(sha, 256) == 0, "SHA256");
  av_sha_update(sha, bytes, size);
  av_sha_final(sha, out);
  av_free(sha);
  for (int i = 0; i < 32; i++)
    snprintf(hex + 2 * i, 3, "%02x", out[i]);
}
// Four-byte hvcC packet framing, one first slice per BL/EL picture and one
// fresh RPU. This independent coded-stage trace predates either decoder or GPU.
static void inspect_coded(AVPacket *packet) {
  need(packet->pts != AV_NOPTS_VALUE && packet->duration > 0,
       "coded AU needs original PTS/positive duration");
  int bl = 0, el = 0, rpu = 0;
  size_t at = 0;
  const uint8_t *rpu_bytes = NULL;
  size_t rpu_len = 0;
  while (at < (size_t)packet->size) {
    need((size_t)packet->size - at >= 4, "coded NAL length prefix");
    const uint8_t *p = packet->data + at;
    uint32_t n = (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 |
                 (uint32_t)p[2] << 8 | p[3];
    at += 4;
    need(n >= 2 && n <= (size_t)packet->size - at, "coded NAL bounds");
    p = packet->data + at;
    int type = (p[0] >> 1) & 63;
    need(((p[0] & 1) << 5 | (p[1] >> 3)) == 0 && (p[1] & 7) != 0,
         "coded layer/temporal header");
    if (type < 32) {
      need(n >= 3, "base slice header");
      bl += (p[2] & 128) != 0;
    }
    if (type == 63) {
      need(n >= 4, "enhancement wrapper");
      int inner = (p[2] >> 1) & 63;
      need(((p[2] & 1) << 5 | (p[3] >> 3)) == 0 && (p[3] & 7) != 0,
           "enhancement layer/temporal header");
      if (inner < 32) {
        need(n >= 5, "enhancement slice header");
        el += (p[4] & 128) != 0;
      }
    }
    if (type == 62) {
      rpu++;
      rpu_bytes = p;
      rpu_len = n;
    }
    at += n;
  }
  need(bl == 1 && rpu == 1 &&
           (base_only ? (source_profile == 7 ? el <= 1 : el == 0) : el == 1),
       "one required source picture and fresh RPU per coded AU");
  if (compare_window(packet->pts, window_start) >= 0 &&
      compare_window(packet->pts, window_end) < 0) {
    need(coded_count < (unsigned)frame_cap && coded_count < 64,
         "coded window frame cap exhausted");
    for (unsigned i = 0; i < coded_count; i++)
      need(coded_window[i].pts != packet->pts, "duplicate coded window PTS");
    struct coded_input *input = &coded_window[coded_count++];
    input->pts = packet->pts;
    input->duration = packet->duration;
    hash_bytes(packet->data, packet->size, input->packet_sha);
    hash_bytes(rpu_bytes, rpu_len, input->rpu_sha);
    printf("{\"kind\":\"coded_window_input\",\"pts_ticks\":%lld,\"duration_"
           "ticks\":%lld,\"packet_sha256\":\"%s\",\"rpu_sha256\":\"%s\"}\n",
           (long long)input->pts, (long long)input->duration, input->packet_sha,
           input->rpu_sha);
  }
}
static void render_pairs(void) {
  while (layers[0].count && (base_only || layers[1].count)) {
    AVFrame *bl = layers[0].queue[0],
            *el = base_only ? bl : layers[1].queue[0];
    need(bl->pts == el->pts && bl->duration == el->duration && bl->duration > 0,
         "unmatched or ambiguous BL/EL PTS/duration");
    need(last_pts == AV_NOPTS_VALUE || bl->pts > last_pts,
         "duplicate/out-of-order paired frame");
    AVFrameSideData *raw =
        av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_RPU_BUFFER);
    need(raw && raw->size > 0 && raw->size < 4096,
         "fresh decoder-attached RPU required; cached metadata insufficient");
    uint8_t nalu[4098] = {0x7c, 0x01};
    memcpy(nalu + 2, raw->data, raw->size);
    struct base_mapping base_mapped;
    if (base_only)
      base_metadata(bl, nalu, raw->size + 2, source_profile, &base_mapped);
    if (window_mode) {
      need(++paired <= preroll_cap + frame_cap + 1,
           "bounded window decoded pair count");
      if (!first_pair) {
        need((bl->flags & AV_FRAME_FLAG_KEY) &&
                 (base_only || (el->flags & AV_FRAME_FLAG_KEY)) &&
                 compare_window(bl->pts, window_start) <= 0,
             "independent BL/EL random-access preroll required before window");
        first_pair = true;
      }
      if (compare_window(bl->pts, window_start) < 0 ||
          compare_window(bl->pts, window_end) >= 0) {
        struct pl_dovi_metadata guarded;
        if (!base_only)
          map_parsed_rpu(nalu, raw->size + 2, &guarded, false,
                         bl->width, bl->height);
        last_pts = bl->pts;
        if (compare_window(bl->pts, window_start) < 0)
          need(++preroll <= preroll_cap, "bounded preroll exhausted");
        else {
          boundary_pts = bl->pts;
          window_done = true;
        }
        drop_pair();
        if (window_done)
          return;
        continue;
      }
    }
    need(emitted < frame_cap, "emitted window frame cap exhausted");
    bool diagnostic_hashes = debug_frames || dv_frame_hashes();
    uint16_t *native[2] = {base_only && !diagnostic_hashes ? NULL : pack(bl),
                           base_only ? NULL : pack(el)};
    if (!initialized) {
      if (base_only)
        base_gpu_init(expected_w[0], expected_h[0]);
      else
        gpu_init(expected_w[0], expected_h[0], expected_w[1], expected_h[1]);
      initialized = true;
    }
    char pts[64], duration[64], bl_hash[65], el_hash[65], rpu_hash[65];
    need(source_time_base.num > 0 && source_time_base.den > 0 &&
             bl->pts <= INT64_MAX / source_time_base.num &&
             bl->pts >= INT64_MIN / source_time_base.num &&
             bl->duration <= INT64_MAX / source_time_base.num,
         "rational time overflow");
    snprintf(pts, sizeof(pts), "%lld/%d",
             (long long)(bl->pts * source_time_base.num), source_time_base.den);
    snprintf(duration, sizeof(duration), "%lld/%d",
             (long long)(bl->duration * source_time_base.num),
             source_time_base.den);
    if (diagnostic_hashes) {
      hash_bytes((uint8_t *)native[0], (size_t)bl->width * bl->height * 3, bl_hash);
      if (!base_only)
        hash_bytes((uint8_t *)native[1], (size_t)el->width * el->height * 3, el_hash);
    }
    hash_bytes(nalu, raw->size + 2, rpu_hash);
    if (window_mode) {
      struct coded_input *input = NULL;
      for (unsigned i = 0; i < coded_count; i++)
        if (coded_window[i].pts == bl->pts)
          input = &coded_window[i];
      need(input && !input->emitted && input->duration == bl->duration &&
               strcmp(input->rpu_sha, rpu_hash) == 0,
           "decoded frame/RPU/duration must match independent coded input");
      input->emitted = true;
    }

    printf("{\"kind\":\"%s\",\"frame\":%d,\"pts\":\"%s\","
           "\"duration\":\"%s\"", base_only ? "accepted_source_base" : "accepted_source_pair",
           emitted, pts, duration);
    if (diagnostic_hashes) {
      printf(",\"bl_sha256\":\"%s\"", bl_hash);
      if (!base_only) printf(",\"el_sha256\":\"%s\"", el_hash);
    }
    printf(",\"rpu_sha256\":\"%s\",\"bl_width\":%d,\"bl_height\":%d",
           rpu_hash, bl->width, bl->height);
    if (base_only)
      printf(",\"profile\":%d,\"fel_contributed\":false}\n", source_profile);
    else
      printf(",\"el_width\":%d,\"el_height\":%d}\n", el->width, el->height);
    output_pts = bl->pts;
    output_duration = bl->duration;
    if (base_only)
      base_gpu_render(bl, &base_mapped, emitted, pts, duration, write_rgb, diagnostic_hashes);
    else
      gpu_render(native, nalu, raw->size + 2, emitted, pts, duration,
                 chroma(bl->chroma_location), chroma(el->chroma_location),
                 write_rgb, NULL, debug_frames);
    char rpu_path[64];
    int rpu_length =
        snprintf(rpu_path, sizeof(rpu_path), "rpus/frame-%03d.nal", emitted);
    need(rpu_length > 0 && (size_t)rpu_length < sizeof(rpu_path), "RPU path");
    FILE *rpu_output = fopen(rpu_path, "wbx");
    need(rpu_output != NULL, "exclusive accepted RPU output");
    need(fwrite(nalu, 1, raw->size + 2, rpu_output) == raw->size + 2,
         "complete accepted RPU write");
    need(fclose(rpu_output) == 0, "accepted RPU close");
    need(fprintf(timing_output, "%s\t%s\n", pts, duration) > 0,
         "timing row write");
    free(native[0]);
    free(native[1]);
    last_pts = bl->pts;
    emitted++;
    if (first_emitted_pts == AV_NOPTS_VALUE)
      first_emitted_pts = bl->pts;
    need(bl->pts <= INT64_MAX - bl->duration, "emitted frame end overflow");
    last_emitted_end = bl->pts + bl->duration;
    drop_pair();
  }
}
static void receive(struct layer *layer) {
  AVFrame *frame = av_frame_alloc();
  need(frame != NULL, "decode frame allocation");
  int result;
  while ((result = avcodec_receive_frame(layer->decoder, frame)) >= 0) {
    need(layer->count < PAIR_WINDOW, "pair window capacity");
    if (layer->index == 1 && discover_el && expected_w[1] == 0) {
      need(frame->width >= 32 && frame->height >= 32 &&
               frame->width % 2 == 0 && frame->height % 2 == 0 &&
               ((frame->width == expected_w[0] &&
                 frame->height == expected_h[0]) ||
                (2 * (int64_t)frame->width == expected_w[0] &&
                 2 * (int64_t)frame->height == expected_h[0])),
           "unsupported observed enhancement raster ratio");
      expected_w[1] = frame->width;
      expected_h[1] = frame->height;
    }
    need(frame->format == AV_PIX_FMT_YUV420P10LE &&
             frame->width == expected_w[layer->index] &&
             frame->height == expected_h[layer->index] &&
             frame->pts != AV_NOPTS_VALUE &&
             !(frame->flags & AV_FRAME_FLAG_CORRUPT),
         "decoded geometry/representation/PTS or corrupt frame");
    for (int i = 0; i < layer->count; i++)
      need(layer->queue[i]->pts != frame->pts, "duplicate decoded layer PTS");
    layer->queue[layer->count++] = av_frame_clone(frame);
    need(layer->queue[layer->count - 1] != NULL,
         "bounded queued frame reference");
    av_frame_unref(frame);
    render_pairs();
    if (window_done)
      break;
  }
  need(window_done || result == AVERROR(EAGAIN) || result == AVERROR_EOF,
       "decoder receive");
  av_frame_free(&frame);
}
static struct layer open_layer(AVStream *stream, int index) {
  struct layer out = {.index = index};
  const AVBitStreamFilter *filter = av_bsf_get_by_name("dovi_split");
  need(filter && av_bsf_alloc(filter, &out.split) >= 0, "dovi_split backend");
  need(avcodec_parameters_copy(out.split->par_in, stream->codecpar) >= 0,
       "split parameters");
  out.split->time_base_in = stream->time_base;
  need(av_opt_set(out.split, "mode", index ? "el" : "bl_rpu",
                  AV_OPT_SEARCH_CHILDREN) >= 0 &&
           av_bsf_init(out.split) >= 0,
       "split initialize");
  const AVCodec *codec = avcodec_find_decoder(AV_CODEC_ID_HEVC);
  need(codec != NULL, "HEVC decoder");
  out.decoder = avcodec_alloc_context3(codec);
  need(out.decoder &&
           avcodec_parameters_to_context(out.decoder, out.split->par_out) >= 0,
       "decoder parameters");
  out.decoder->pkt_timebase = out.split->time_base_out;
  out.decoder->width = expected_w[index];
  out.decoder->height = expected_h[index];
  out.decoder->thread_count = 1;
  out.decoder->max_pixels = discover_el && index == 1
                                ? (int64_t)expected_w[0] * expected_h[0]
                                : (int64_t)expected_w[index] * expected_h[index];
  out.decoder->err_recognition = AV_EF_CAREFUL | AV_EF_EXPLODE;
  need(avcodec_open2(out.decoder, codec, NULL) >= 0,
       "independent decoder open");
  return out;
}
static void send_layer(struct layer *layer, AVPacket *packet) {
  AVPacket *copy = av_packet_clone(packet);
  need(copy && av_bsf_send_packet(layer->split, copy) >= 0, "split send");
  av_packet_free(&copy);
  AVPacket *out = av_packet_alloc();
  need(out != NULL, "split output");
  int result;
  while ((result = av_bsf_receive_packet(layer->split, out)) >= 0) {
    need(out->pts != AV_NOPTS_VALUE &&
             avcodec_send_packet(layer->decoder, out) >= 0,
         "actual split PTS and decoder send");
    receive(layer);
    av_packet_unref(out);
    if (window_done)
      break;
  }
  need(window_done || result == AVERROR(EAGAIN) || result == AVERROR_EOF,
       "split receive");
  av_packet_free(&out);
}
int main(int argc, char **argv) {
  need(argc == 11 || argc == 12 || argc == 15 ||
           (argc == 16 && !strcmp(argv[15], "base-rpu")),
       "usage: segment_decode_render SOURCE OUTPUT_DIR VIDEO_INDEX "
       "MAX_FRAMES BL_W BL_H EL_W EL_H DEBUG OUTPUT_POLICY [NUT_OUTPUT [START "
       "END MAX_PREROLL [base-rpu]]]");
  bool streaming = argc >= 12;
  base_only = argc == 16;
  window_mode = argc >= 15;
  if (window_mode) {
    window_start = rational(argv[12]);
    window_end = rational(argv[13]);
    preroll_cap = (unsigned)number(argv[14], 0, 512);
    need(av_compare_ts(window_start.num, (AVRational){1, window_start.den},
                       window_end.num, (AVRational){1, window_end.den}) < 0,
         "ordered nonempty window");
    __int128 delta = (__int128)window_end.num * window_start.den -
                     (__int128)window_start.num * window_end.den;
    need(delta <= (__int128)3 * window_start.den * window_end.den,
         "initial window duration at most three seconds");
    av_max_alloc(64ULL * 1024 * 1024);
  }
  int stream_index = number(argv[3], 0, 255);
  frame_cap = number(argv[4], 1, 64);
  expected_w[0] = number(argv[5], 64, 3840);
  expected_h[0] = number(argv[6], 64, 2160);
  expected_w[1] = number(argv[7], 0, 3840);
  expected_h[1] = number(argv[8], 0, 2160);
  discover_el = expected_w[1] == 0 && expected_h[1] == 0;
  need((expected_w[1] == 0) == (expected_h[1] == 0),
       "enhancement discovery requires both dimensions zero");
  need(!discover_el || window_mode, "enhancement discovery requires window mode");
  need(!base_only || (expected_w[1] == 0 && expected_h[1] == 0),
       "base-only mode has no declared EL");
  debug_frames = number(argv[9], 0, 1) != 0;
  // argv[10] reserves an explicit output policy; never interpreted as a display
  // target.
  need(strcmp(argv[10], "bt2020-pq-master-clip") == 0,
       "unsupported output policy");
  need(
      expected_w[0] % 2 == 0 && expected_h[0] % 2 == 0 &&
          expected_w[1] % 2 == 0 && expected_h[1] % 2 == 0 &&
          (discover_el ||
           (expected_w[0] == expected_w[1] && expected_h[0] == expected_h[1]) ||
           (expected_w[0] == 2 * expected_w[1] &&
            expected_h[0] == 2 * expected_h[1])),
      "unsupported layer raster ratio");
  need(streaming || (uint64_t)frame_cap * expected_w[0] * expected_h[0] * 6 <=
                        512ULL * 1024 * 1024,
       "bounded segment RGB scratch budget");
  struct stat st;
  need(stat(argv[2], &st) == 0 && S_ISDIR(st.st_mode),
       "private output directory");
  need(chdir(argv[2]) == 0, "output directory enter");
  need(mkdir("rpus", 0700) == 0, "fresh accepted RPU directory");
  if (debug_frames)
    need(mkdir("outputs", 0700) == 0, "fresh debug output directory");
  if (!streaming)
    rgb_output = fopen("reconstructed.rgb48le", "wbx");
  timing_output = fopen("timing.tsv", "wx");
  need((streaming || rgb_output) && timing_output,
       "exclusive private output files");
  AVFormatContext *input = avformat_alloc_context();
  need(input != NULL, "input context");
  input->probesize = 1024 * 1024;
  input->max_analyze_duration = AV_TIME_BASE;
  input->max_probe_packets = 32;
  input->max_streams = 256;
  if (window_mode)
    open_movie_io(input, argv[1]);
  int opened = avformat_open_input(&input, argv[1], NULL, NULL);
  need(!movie_io.exhausted, "window source read budget exhausted");
  need(opened >= 0, "timestamped segment open");
  need(input->nb_streams > 0 && input->nb_streams <= 256,
       "bounded stream inventory");
  AVDictionary *probe_options[256] = {0};
  char pixels[32];
  snprintf(pixels, sizeof(pixels), "%lld",
           (long long)expected_w[0] * expected_h[0]);
  for (unsigned i = 0; i < input->nb_streams; i++) {
    need(av_dict_set(&probe_options[i], "threads", "1", 0) >= 0 &&
             av_dict_set(&probe_options[i], "max_pixels", pixels, 0) >= 0,
         "bounded probe decoder options");
  }
  int probed = avformat_find_stream_info(input, probe_options);
  for (unsigned i = 0; i < input->nb_streams; i++)
    av_dict_free(&probe_options[i]);
  need(!movie_io.exhausted, "window source read budget exhausted");
  need(probed >= 0, "bounded stream discovery");
  need((unsigned)stream_index < input->nb_streams &&
           input->streams[stream_index]->codecpar->codec_type ==
               AVMEDIA_TYPE_VIDEO &&
           input->streams[stream_index]->codecpar->codec_id == AV_CODEC_ID_HEVC,
       "selected absolute HEVC video stream");
  AVStream *stream = input->streams[stream_index];
  source_time_base = stream->time_base;
  if (base_only) {
    const AVPacketSideData *config = av_packet_side_data_get(
        stream->codecpar->coded_side_data, stream->codecpar->nb_coded_side_data,
        AV_PKT_DATA_DOVI_CONF);
    need(config && config->size >= sizeof(AVDOVIDecoderConfigurationRecord),
         "declared source Dolby Vision configuration");
    const AVDOVIDecoderConfigurationRecord *cfg = (const void *)config->data;
    source_profile = cfg->dv_profile;
    need(cfg->dv_version_major == 1 && cfg->bl_present_flag &&
             cfg->rpu_present_flag && cfg->dv_md_compression == 0 &&
             ((source_profile == 5 && !cfg->el_present_flag &&
               cfg->dv_bl_signal_compatibility_id == 0) ||
              (source_profile == 8 && !cfg->el_present_flag &&
               cfg->dv_bl_signal_compatibility_id == 1) ||
              (source_profile == 7 && cfg->dv_bl_signal_compatibility_id == 6)),
         "supported base-only DV configuration");
  }
  if (window_mode)
    need(stream->codecpar->extradata_size >= 23 &&
             stream->codecpar->extradata[0] == 1 &&
             (stream->codecpar->extradata[21] & 3) == 3,
         "window requires four-byte hvcC source framing");

  need(source_time_base.num > 0 && source_time_base.den > 0,
       "positive source time base");
  if (window_mode) {
    need(stream->start_time == 0,
         "initial window requires observed zero source origin");
    int64_t target =
        av_rescale_q_rnd(window_start.num, (AVRational){1, window_start.den},
                         source_time_base, AV_ROUND_DOWN);
    need(target >= 0 && target < INT64_MAX, "representable seek target");
    need(avformat_seek_file(input, stream_index, INT64_MIN, target, target,
                            AVSEEK_FLAG_BACKWARD) >= 0,
         "keyframe seek before exact window start");
  }
  if (streaming)
    open_nut(argv[11]);
  layers[0] = open_layer(stream, 0);
  if (!base_only)
    layers[1] = open_layer(stream, 1);
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "demux packet");
  int result;
  unsigned packets = 0, video_packets = 0;
  while ((result = av_read_frame(input, packet)) >= 0) {
    need(++packets <= 4096 && packet->size <= 16 * 1024 * 1024,
         "finite segment packet/byte envelope");
    if (packet->stream_index == stream_index) {
      need(++video_packets <=
               (window_mode ? 2 * (preroll_cap + frame_cap + 1) : 256),
           "bounded video access units");
      if (window_mode)
        inspect_coded(packet);
      send_layer(&layers[0], packet);
      if (!window_done && !base_only)
        send_layer(&layers[1], packet);
    }
    av_packet_unref(packet);
    if (window_done)
      break;
  }
  need(!movie_io.exhausted, "window source read budget exhausted");
  source_eof = result == AVERROR_EOF;
  need(window_done || source_eof, "complete bounded source read");
  for (int k = 0; !window_done && k < (base_only ? 1 : 2); k++) {
    need(avcodec_send_packet(layers[k].decoder, NULL) >= 0, "decoder drain");
    receive(&layers[k]);
  }
  need(emitted > 0 &&
           (window_done || (layers[0].count == 0 && layers[1].count == 0)),
       "nonempty complete pair coverage");
  if (window_mode) {
    need(coded_count == (unsigned)emitted,
         "missing decoded coded-window picture");
    for (unsigned i = 0; i < coded_count; i++)
      need(coded_window[i].emitted, "coded window picture never emitted");
  }
  if (window_mode && !window_done) {
    need(source_eof && last_emitted_end != AV_NOPTS_VALUE &&
             compare_window(last_emitted_end, window_end) >= 0,
         "source EOF before requested window end");
    need(last_emitted_end < INT64_MAX, "EOF extent overflow");
    if (stream->duration != AV_NOPTS_VALUE && stream->duration > 0) {
      need(stream->start_time <= INT64_MAX - stream->duration &&
               last_emitted_end + 1 >= stream->start_time + stream->duration &&
               last_emitted_end - 1 <= stream->start_time + stream->duration,
           "source EOF disagrees with declared video extent");
    } else {
      need(input->nb_streams == 1 && input->duration != AV_NOPTS_VALUE &&
               input->duration > 0 &&
               av_compare_ts(last_emitted_end + 1, source_time_base,
                             input->duration,
                             (AVRational){1, AV_TIME_BASE}) >= 0 &&
               av_compare_ts(last_emitted_end - 1, source_time_base,
                             input->duration,
                             (AVRational){1, AV_TIME_BASE}) <= 0,
           "source EOF extent unavailable or inconsistent");
    }
  }
  if (streaming) {
    need(av_write_trailer(nut_output) >= 0, "NUT trailer");
    need(avio_closep(&nut_output->pb) >= 0, "NUT output close");
    avformat_free_context(nut_output);
    nut_output = NULL;
  }
  need((!rgb_output || fclose(rgb_output) == 0) && fclose(timing_output) == 0,
       "close completed outputs");
  if (base_only)
    base_gpu_close();
  else
    gpu_close();
  for (int k = 0; k < (base_only ? 1 : 2); k++) {
    for (int i = 0; i < layers[k].count; i++)
      av_frame_free(&layers[k].queue[i]);
    av_bsf_free(&layers[k].split);
    avcodec_free_context(&layers[k].decoder);
  }
  av_packet_free(&packet);
  avformat_close_input(&input);
  if (movie_avio) {
    av_freep(&movie_avio->buffer);
    avio_context_free(&movie_avio);
    need(close(movie_io.fd) == 0, "close held movie IO");
  }
  if (window_mode) {
    printf("{\"kind\":\"window_complete\",\"requested_start\":\"%lld/"
           "%d\",\"requested_end\":\"%lld/%d\",\"source_origin\":\"0/"
           "1\",\"origin_provenance\":\"observed_stream_start_time\","
           "\"frames\":%d,\"preroll_pairs\":%u,\"paired_pictures\":%u,"
           "\"boundary_pts_ticks\":%lld,\"boundary_observed\":%s,\"source_eof_"
           "observed\":%s,\"first_emitted_pts_ticks\":%lld,\"last_emitted_end_"
           "ticks\":%lld,\"source_time_base\":\"%d/"
           "%d\",\"source_bytes_read\":%llu,\"duration_provenance\":\"decoder_"
           "observed\",\"membership\":\"frame_pts_in_half_open_window_"
           "durations_unclipped\",\"production_qualified\":false}\n",
           (long long)window_start.num, window_start.den,
           (long long)window_end.num, window_end.den, emitted, preroll, paired,
           (long long)boundary_pts, window_done ? "true" : "false",
           source_eof ? "true" : "false", (long long)first_emitted_pts,
           (long long)last_emitted_end, source_time_base.num,
           source_time_base.den, (unsigned long long)movie_io.bytes);
  }
  printf("{\"kind\":\"segment_complete\",\"frames\":%d,\"pair_window_bound\":"
         "16,\"gpu_contexts\":1,\"output_policy\":\"bt2020-pq-master-clip\","
         "\"production_qualified\":false}\n",
         emitted);
  if (base_only)
    printf("{\"kind\":\"base_processing_complete\",\"profile\":%d,"
           "\"frames\":%d,\"decoded_layers\":1,\"el_bound\":false,"
           "\"fel_contributed\":false,\"creative_trims_applied\":false,"
           "\"production_qualified\":false}\n", source_profile, emitted);
  need(fflush(stdout) == 0, "observations flush");
  return 0;
}
