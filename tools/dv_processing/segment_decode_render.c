// Finite timestamped source segment -> persistent native-plane FEL renderer.
// No fixture-definition hashes, test variants, production route or publication.
#include "fel_renderer.h"
#include "nut_timing.h"
#include <errno.h>
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
static int expected_w[2], expected_h[2], frame_cap, emitted;
static bool debug_frames, initialized;
static FILE *rgb_output, *timing_output;
static AVFormatContext *nut_output;
static AVStream *nut_stream;
static int64_t output_pts, output_duration, nut_tick_scale;
static int64_t last_pts = AV_NOPTS_VALUE;
static AVRational source_time_base;
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
static void render_pairs(void) {
  while (layers[0].count && layers[1].count) {
    AVFrame *bl = layers[0].queue[0], *el = layers[1].queue[0];
    need(bl->pts == el->pts && bl->duration == el->duration && bl->duration > 0,
         "unmatched or ambiguous BL/EL PTS/duration");
    need(emitted < frame_cap &&
             (last_pts == AV_NOPTS_VALUE || bl->pts > last_pts),
         "duplicate/out-of-order or excess frame");
    AVFrameSideData *raw =
        av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_RPU_BUFFER);
    need(raw && raw->size > 0 && raw->size < 4096,
         "fresh decoder-attached RPU required; cached metadata insufficient");
    uint8_t nalu[4098] = {0x7c, 0x01};
    memcpy(nalu + 2, raw->data, raw->size);
    uint16_t *native[2] = {pack(bl), pack(el)};
    if (!initialized) {
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
    hash_bytes((uint8_t *)native[0], (size_t)bl->width * bl->height * 3,
               bl_hash);
    hash_bytes((uint8_t *)native[1], (size_t)el->width * el->height * 3,
               el_hash);
    hash_bytes(nalu, raw->size + 2, rpu_hash);
    printf("{\"kind\":\"accepted_source_pair\",\"frame\":%d,\"pts\":\"%s\","
           "\"duration\":\"%s\",\"bl_sha256\":\"%s\",\"el_sha256\":\"%s\","
           "\"rpu_sha256\":\"%s\",\"bl_width\":%d,\"bl_height\":%d,\"el_"
           "width\":%d,\"el_height\":%d}\n",
           emitted, pts, duration, bl_hash, el_hash, rpu_hash, bl->width,
           bl->height, el->width, el->height);
    output_pts = bl->pts;
    output_duration = bl->duration;
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
    for (int k = 0; k < 2; k++) {
      av_frame_free(&layers[k].queue[0]);
      layers[k].count--;
      memmove(layers[k].queue, layers[k].queue + 1,
              (size_t)layers[k].count * sizeof(AVFrame *));
    }
  }
}
static void receive(struct layer *layer) {
  AVFrame *frame = av_frame_alloc();
  need(frame != NULL, "decode frame allocation");
  int result;
  while ((result = avcodec_receive_frame(layer->decoder, frame)) >= 0) {
    need(layer->count < PAIR_WINDOW, "pair window capacity");
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
  }
  need(result == AVERROR(EAGAIN) || result == AVERROR_EOF, "decoder receive");
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
  out.decoder->max_pixels = (int64_t)expected_w[index] * expected_h[index];
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
  }
  need(result == AVERROR(EAGAIN) || result == AVERROR_EOF, "split receive");
  av_packet_free(&out);
}
int main(int argc, char **argv) {
  need(argc == 11 || argc == 12,
       "usage: segment_decode_render SOURCE OUTPUT_DIR VIDEO_INDEX "
       "MAX_FRAMES BL_W BL_H EL_W EL_H DEBUG OUTPUT_POLICY [NUT_OUTPUT]");
  bool streaming = argc == 12;
  int stream_index = number(argv[3], 0, 255);
  frame_cap = number(argv[4], 1, 64);
  expected_w[0] = number(argv[5], 64, 3840);
  expected_h[0] = number(argv[6], 64, 2160);
  expected_w[1] = number(argv[7], 32, 3840);
  expected_h[1] = number(argv[8], 32, 2160);
  debug_frames = number(argv[9], 0, 1) != 0;
  // argv[10] reserves an explicit output policy; never interpreted as a display
  // target.
  need(strcmp(argv[10], "bt2020-pq-master-clip") == 0,
       "unsupported output policy");
  need(
      expected_w[0] % 2 == 0 && expected_h[0] % 2 == 0 &&
          expected_w[1] % 2 == 0 && expected_h[1] % 2 == 0 &&
          ((expected_w[0] == expected_w[1] && expected_h[0] == expected_h[1]) ||
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
  need(avformat_open_input(&input, argv[1], NULL, NULL) >= 0,
       "timestamped segment open");
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
  need(probed >= 0, "bounded stream discovery");
  need((unsigned)stream_index < input->nb_streams &&
           input->streams[stream_index]->codecpar->codec_type ==
               AVMEDIA_TYPE_VIDEO &&
           input->streams[stream_index]->codecpar->codec_id == AV_CODEC_ID_HEVC,
       "selected absolute HEVC video stream");
  AVStream *stream = input->streams[stream_index];
  source_time_base = stream->time_base;
  if (streaming)
    open_nut(argv[11]);
  layers[0] = open_layer(stream, 0);
  layers[1] = open_layer(stream, 1);
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "demux packet");
  int result;
  unsigned packets = 0, video_packets = 0;
  while ((result = av_read_frame(input, packet)) >= 0) {
    need(++packets <= 4096 && packet->size <= 16 * 1024 * 1024,
         "finite segment packet/byte envelope");
    if (packet->stream_index == stream_index) {
      need(++video_packets <= 256, "bounded video access units");
      send_layer(&layers[0], packet);
      send_layer(&layers[1], packet);
    }
    av_packet_unref(packet);
  }
  need(result == AVERROR_EOF, "complete bounded source segment");
  for (int k = 0; k < 2; k++) {
    need(avcodec_send_packet(layers[k].decoder, NULL) >= 0, "decoder drain");
    receive(&layers[k]);
  }
  need(emitted > 0 && layers[0].count == 0 && layers[1].count == 0,
       "nonempty complete pair coverage");
  if (streaming) {
    need(av_write_trailer(nut_output) >= 0, "NUT trailer");
    need(avio_closep(&nut_output->pb) >= 0, "NUT output close");
    avformat_free_context(nut_output);
    nut_output = NULL;
  }
  need((!rgb_output || fclose(rgb_output) == 0) && fclose(timing_output) == 0,
       "close completed outputs");
  gpu_close();
  for (int k = 0; k < 2; k++) {
    av_bsf_free(&layers[k].split);
    avcodec_free_context(&layers[k].decoder);
  }
  av_packet_free(&packet);
  avformat_close_input(&input);
  printf("{\"kind\":\"segment_complete\",\"frames\":%d,\"pair_window_bound\":"
         "16,\"gpu_contexts\":1,\"output_policy\":\"bt2020-pq-master-clip\","
         "\"production_qualified\":false}\n",
         emitted);
  need(fflush(stdout) == 0, "observations flush");
  return 0;
}
