#define _POSIX_C_SOURCE 200809L
/* OFFLINE diagnostic only: demux -> dovi_split -> independent software HEVC
 * decoders. No renderer, composer, encoder, player, or packaging behavior.
 * Build: clang -std=c11 -O2 -Wall -Wextra -Werror capture_layers.c \
 *   $(pkg-config --cflags --libs libavformat libavcodec libavutil) -o capture_layers
 * Usage: capture_layers INPUT NEW_OUTPUT_DIR START_N/D END_N/D VIDEO_INDEX
 * Output is provisional until summary.json exists AND every selected frame's
 * canonical hashes/timestamps match the retained accepted-source trace.
 * Single-thread decoding differs from the production helper's four frame
 * workers; equality to that trace, not this tool, establishes source identity.
 */
#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <libavcodec/avcodec.h>
#include <libavcodec/bsf.h>
#include <libavformat/avformat.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/opt.h>
#include <libavutil/sha.h>

#define CAP 4096
#define QUEUE 64
struct bound { int64_t n; int d; };
struct coded { int64_t pts, duration; char rpu[65], packet[65]; bool used; };
struct layer { AVBSFContext *bsf; AVCodecContext *decoder; AVFrame *q[QUEUE]; int count; };
static struct layer layers[2];
static struct coded coded[CAP];
static unsigned coded_count, accepted, selected_coded, pairs;
static AVRational tb;
static struct bound start, end;
static FILE *events, *planes[2], *rpus;
static int64_t offsets[2], rpu_offset, last_pts = AV_NOPTS_VALUE, last_end;
static int widths[2], heights[2];
static bool boundary_observed;
static int64_t first_paired_pts = AV_NOPTS_VALUE;

static void need(bool ok, const char *why) {
  if (!ok) { fprintf(stderr, "capture refused: %s\n", why); exit(1); }
}
static void check(int result, const char *why) {
  if (result < 0) {
    char message[AV_ERROR_MAX_STRING_SIZE];
    av_strerror(result, message, sizeof(message));
    fprintf(stderr, "capture refused: %s: %s\n", why, message); exit(1);
  }
}
static struct bound parse(const char *s) {
  char *p; errno = 0; int64_t n = strtoll(s, &p, 10);
  need(!errno && p != s && *p == '/' && n >= 0, "nonnegative rational numerator");
  const char *q = p + 1; errno = 0; long d = strtol(q, &p, 10);
  need(!errno && p != q && !*p && d > 0 && d <= INT_MAX, "positive rational denominator");
  return (struct bound){n, (int)d};
}
static int cmp(int64_t pts, struct bound b) {
  return av_compare_ts(pts, tb, b.n, (AVRational){1, b.d});
}
static bool selected(int64_t pts) { return cmp(pts, start) >= 0 && cmp(pts, end) < 0; }
static void digest(const uint8_t *bytes, size_t n, char hex[65]) {
  struct AVSHA *sha = av_sha_alloc(); uint8_t hash[32];
  need(sha && av_sha_init(sha, 256) == 0, "SHA256 initialization");
  av_sha_update(sha, bytes, n); av_sha_final(sha, hash); av_free(sha);
  for (int i = 0; i < 32; i++) snprintf(hex + 2*i, 3, "%02x", hash[i]);
}
static void source_digest(const char *path, char hex[65]) {
  FILE *f = fopen(path, "rb"); struct AVSHA *sha = av_sha_alloc(); uint8_t block[65536], hash[32];
  need(f && sha && av_sha_init(sha, 256) == 0, "source hash open/init");
  size_t n;
  while ((n = fread(block, 1, sizeof(block), f))) av_sha_update(sha, block, n);
  need(!ferror(f) && fclose(f) == 0, "source hash read/close");
  av_sha_final(sha, hash); av_free(sha);
  for (int i = 0; i < 32; i++) snprintf(hex + 2*i, 3, "%02x", hash[i]);
}
static void json_string(FILE *f, const char *s) {
  need(fputc('"', f) != EOF, "JSON string open");
  for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
    if (*p == '"' || *p == '\\') need(fputc('\\', f) != EOF && fputc(*p, f) != EOF, "JSON escape");
    else if (*p < 32) need(fprintf(f, "\\u%04x", *p) > 0, "JSON control escape");
    else need(fputc(*p, f) != EOF, "JSON string byte");
  }
  need(fputc('"', f) != EOF, "JSON string close");
}
static FILE *output(const char *root, const char *name) {
  char path[4096]; int n = snprintf(path, sizeof(path), "%s/%s", root, name);
  need(n > 0 && (size_t)n < sizeof(path), "bounded output path");
  FILE *f = fopen(path, "wbx"); need(f != NULL, "exclusive new output file"); return f;
}
/* Inspect original four-byte hvcC packets before either splitter/decoder. */
static void inspect(const AVPacket *p) {
  need(p->pts != AV_NOPTS_VALUE && p->duration > 0 && !(p->flags & AV_PKT_FLAG_CORRUPT),
       "original packet PTS/duration/integrity");
  need(coded_count < CAP, "bounded coded access-unit count");
  int bl = 0, el = 0, nrpu = 0; const uint8_t *rpu = NULL; size_t rpu_size = 0, at = 0;
  while (at < (size_t)p->size) {
    need((size_t)p->size-at >= 4, "NAL length prefix");
    const uint8_t *b = p->data + at;
    uint32_t n = (uint32_t)b[0]<<24 | (uint32_t)b[1]<<16 | (uint32_t)b[2]<<8 | b[3]; at += 4;
    need(n >= 2 && n <= (size_t)p->size-at, "NAL bounds"); b = p->data + at;
    int type = (b[0] >> 1) & 63;
    need(((b[0]&1)<<5 | (b[1]>>3)) == 0 && (b[1]&7), "NAL layer/temporal header");
    if (type < 32) { need(n >= 3, "BL slice header"); bl += !!(b[2]&128); }
    if (type == 63) {
      need(n >= 4 && (((b[2]&1)<<5 | (b[3]>>3)) == 0) && (b[3]&7), "EL wrapper/header");
      if (((b[2]>>1)&63) < 32) { need(n >= 5, "EL slice header"); el += !!(b[4]&128); }
    }
    if (type == 62) { nrpu++; rpu = b; rpu_size = n; }
    at += n;
  }
  need(bl == 1 && el == 1 && nrpu == 1, "one BL/EL picture and fresh RPU per source AU");
  for (unsigned i = 0; i < coded_count; i++) need(coded[i].pts != p->pts, "unique source PTS");
  struct coded *c = &coded[coded_count++]; c->pts = p->pts; c->duration = p->duration;
  digest(rpu, rpu_size, c->rpu); digest(p->data, p->size, c->packet);
  if (selected(c->pts)) selected_coded++;
}
static uint8_t *pack(const AVFrame *f, size_t *size) {
  *size = (size_t)f->width*f->height*3;
  uint8_t *out = malloc(*size); need(out != NULL, "canonical allocation"); size_t at = 0;
  for (int c = 0; c < 3; c++) {
    int w = c ? f->width/2 : f->width, h = c ? f->height/2 : f->height;
    need(f->data[c] && f->linesize[c] >= 2*w, "canonical planar stride");
    for (int y = 0; y < h; y++) {
      const uint8_t *row = f->data[c] + (size_t)y*f->linesize[c];
      for (int x = 0; x < w; x++) need((row[2*x] | (row[2*x+1]<<8)) <= 1023, "ten-bit code bounds");
      memcpy(out+at, row, (size_t)w*2); at += (size_t)w*2;
    }
  }
  need(at == *size, "canonical packed extent"); return out;
}
static void pair(void) {
  while (layers[0].count && layers[1].count) {
    AVFrame *bl = layers[0].q[0], *el = layers[1].q[0];
    need(bl->pts == el->pts && bl->duration == el->duration && bl->duration > 0,
         "exact BL/EL PTS/duration association");
    need(last_pts == AV_NOPTS_VALUE || bl->pts > last_pts, "strict paired presentation order");
    if (!pairs) need((bl->flags & AV_FRAME_FLAG_KEY) && (el->flags & AV_FRAME_FLAG_KEY) &&
                     cmp(bl->pts, start) <= 0, "independent keyframe before window");
    if (!pairs) first_paired_pts = bl->pts;
    pairs++; last_pts = bl->pts;
    if (cmp(bl->pts, end) >= 0) boundary_observed = true;
    struct coded *c = NULL;
    for (unsigned i = 0; i < coded_count; i++) if (coded[i].pts == bl->pts) c = &coded[i];
    need(c && !c->used && c->duration == bl->duration, "unique decoded/source AU association");
    AVFrameSideData *raw = av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_RPU_BUFFER);
    need(raw && raw->size > 0 && raw->size < 4096, "fresh decoder-attached RPU");
    uint8_t rpu[4098] = {0x7c, 0x01}; memcpy(rpu+2, raw->data, raw->size); char rpu_hash[65];
    digest(rpu, raw->size+2, rpu_hash); need(!strcmp(rpu_hash, c->rpu), "coded/decoded RPU identity");
    c->used = true;
    if (selected(bl->pts)) {
      need(accepted < 1024 && bl->pts >= 0 && bl->pts <= INT64_MAX-bl->duration &&
           bl->pts <= INT64_MAX/tb.num && bl->duration <= INT64_MAX/tb.num,
           "bounded capture duration/count and rational representation");
      char hashes[2][65]; size_t sizes[2];
      for (int k = 0; k < 2; k++) {
        AVFrame *f = layers[k].q[0]; uint8_t *bytes = pack(f, &sizes[k]);
        digest(bytes, sizes[k], hashes[k]);
        need(fwrite(bytes, 1, sizes[k], planes[k]) == sizes[k], "complete canonical plane write"); free(bytes);
      }
      need(fwrite(rpu, 1, raw->size+2, rpus) == raw->size+2, "complete RPU write");
      need(fprintf(events, "{\"kind\":\"accepted_source_pair\",\"frame\":%u,\"pts\":\"%"PRId64"/%d\",\"duration\":\"%"PRId64"/%d\",\"bl_sha256\":\"%s\",\"el_sha256\":\"%s\",\"rpu_sha256\":\"%s\",\"packet_sha256\":\"%s\",\"bl_width\":%d,\"bl_height\":%d,\"el_width\":%d,\"el_height\":%d,\"bl_offset\":%"PRId64",\"el_offset\":%"PRId64",\"rpu_offset\":%"PRId64",\"rpu_size\":%zu}\n",
                   accepted, bl->pts*(int64_t)tb.num, tb.den, bl->duration*(int64_t)tb.num, tb.den,
                   hashes[0], hashes[1], rpu_hash, c->packet, bl->width, bl->height, el->width, el->height,
                   offsets[0], offsets[1], rpu_offset, raw->size+2) > 0, "event write");
      offsets[0] += sizes[0]; offsets[1] += sizes[1]; rpu_offset += raw->size+2;
      last_end = bl->pts+bl->duration; accepted++;
    }
    for (int k = 0; k < 2; k++) {
      av_frame_free(&layers[k].q[0]);
      memmove(layers[k].q, layers[k].q+1, (size_t)(--layers[k].count)*sizeof(AVFrame *));
    }
  }
}
static void receive(struct layer *l, int index) {
  AVFrame *f = av_frame_alloc(); need(f != NULL, "frame allocation"); int result;
  while ((result = avcodec_receive_frame(l->decoder, f)) >= 0) {
    need(f->format == AV_PIX_FMT_YUV420P10LE && f->width > 0 && f->height > 0 &&
         f->width <= 4096 && f->height <= 2160 && !(f->width%2) && !(f->height%2) &&
         f->pts != AV_NOPTS_VALUE && !(f->flags & AV_FRAME_FLAG_CORRUPT) && !f->decode_error_flags,
         "decoded ten-bit geometry/PTS/integrity");
    if (!widths[index]) { widths[index] = f->width; heights[index] = f->height; }
    need(widths[index] == f->width && heights[index] == f->height, "stable layer dimensions");
    if (widths[0] && widths[1]) need((widths[0] == widths[1] && heights[0] == heights[1]) ||
         (widths[0] == 2*widths[1] && heights[0] == 2*heights[1]), "supported EL raster ratio");
    need(l->count < QUEUE, "bounded pair queue");
    l->q[l->count++] = av_frame_clone(f); need(l->q[l->count-1] != NULL, "queued frame clone");
    av_frame_unref(f); pair();
  }
  need(result == AVERROR(EAGAIN) || result == AVERROR_EOF, "decoder receive status"); av_frame_free(&f);
}
static void split_receive(struct layer *l, int index) {
  AVPacket *p = av_packet_alloc(); need(p != NULL, "split packet allocation"); int result;
  while ((result = av_bsf_receive_packet(l->bsf, p)) >= 0) {
    need(p->pts != AV_NOPTS_VALUE && !(p->flags & AV_PKT_FLAG_CORRUPT), "split packet timestamp/integrity");
    int sent = avcodec_send_packet(l->decoder, p);
    if (sent == AVERROR(EAGAIN)) { receive(l, index); sent = avcodec_send_packet(l->decoder, p); }
    check(sent, "decoder send"); receive(l, index); av_packet_unref(p);
  }
  need(result == AVERROR(EAGAIN) || result == AVERROR_EOF, "split receive status"); av_packet_free(&p);
}
static void send(struct layer *l, int index, const AVPacket *p) {
  AVPacket *copy = p ? av_packet_clone(p) : NULL; need(!p || copy, "source packet clone");
  int result = av_bsf_send_packet(l->bsf, copy);
  if (result == AVERROR(EAGAIN)) { split_receive(l, index); result = av_bsf_send_packet(l->bsf, copy); }
  check(result, "split send"); av_packet_free(&copy); split_receive(l, index);
}
static void open_layer(struct layer *l, const AVStream *s, int index) {
  const AVBitStreamFilter *bsf = av_bsf_get_by_name("dovi_split");
  need(bsf != NULL, "dovi_split available"); check(av_bsf_alloc(bsf, &l->bsf), "split allocation");
  check(avcodec_parameters_copy(l->bsf->par_in, s->codecpar), "split input parameters");
  l->bsf->time_base_in = s->time_base;
  check(av_opt_set(l->bsf, "mode", index ? "el" : "bl_rpu", AV_OPT_SEARCH_CHILDREN), "split mode");
  check(av_bsf_init(l->bsf), "split initialization");
  need(av_cmp_q(l->bsf->time_base_out, tb) == 0, "unchanged split timestamp units");
  const AVCodec *codec = avcodec_find_decoder(AV_CODEC_ID_HEVC); need(codec != NULL, "HEVC decoder");
  l->decoder = avcodec_alloc_context3(codec); need(l->decoder != NULL, "decoder allocation");
  check(avcodec_parameters_to_context(l->decoder, l->bsf->par_out), "split output decoder parameters");
  l->decoder->pkt_timebase = l->bsf->time_base_out;
  l->decoder->thread_count = 1; l->decoder->thread_type = FF_THREAD_FRAME;
  l->decoder->max_pixels = (int64_t)s->codecpar->width*s->codecpar->height;
  l->decoder->err_recognition = AV_EF_CAREFUL | AV_EF_EXPLODE;
  check(avcodec_open2(l->decoder, codec, NULL), "decoder open");
  need(l->decoder->thread_count == 1 && !l->decoder->hw_device_ctx, "single-thread software diagnostic");
  char extra[65]; digest(l->bsf->par_out->extradata, l->bsf->par_out->extradata_size, extra);
  fprintf(events, "{\"kind\":\"decoder\",\"layer\":%d,\"threads\":1,\"active_thread_type\":%d,\"split_extradata_sha256\":\"%s\",\"time_base\":\"%d/%d\"}\n", index, l->decoder->active_thread_type, extra, l->bsf->time_base_out.num, l->bsf->time_base_out.den);
}
int main(int argc, char **argv) {
  need(argc == 6, "usage: capture_layers INPUT NEW_OUTPUT_DIR START_N/D END_N/D VIDEO_INDEX");
  start = parse(argv[3]); end = parse(argv[4]);
  need(av_compare_ts(start.n, (AVRational){1,start.d}, end.n, (AVRational){1,end.d}) < 0, "ordered window");
  char *p; errno = 0; long stream_index = strtol(argv[5], &p, 10);
  need(!errno && p != argv[5] && !*p && stream_index >= 0 && stream_index < 256, "absolute stream index");
  struct stat source_stat;
  need(stat(argv[1], &source_stat) == 0 && S_ISREG(source_stat.st_mode), "local regular source file");
  need(mkdir(argv[2], 0700) == 0, "new exclusive output directory");
  events = output(argv[2], "events.jsonl"); planes[0] = output(argv[2], "bl.yuv");
  planes[1] = output(argv[2], "el.yuv"); rpus = output(argv[2], "rpus.bin");
  char source_sha[65]; source_digest(argv[1], source_sha);
  AVFormatContext *input = NULL; check(avformat_open_input(&input, argv[1], NULL, NULL), "source open");
  check(avformat_find_stream_info(input, NULL), "stream discovery");
  need((unsigned)stream_index < input->nb_streams, "selected stream exists");
  AVStream *s = input->streams[stream_index]; tb = s->time_base;
  need(s->codecpar->codec_type == AVMEDIA_TYPE_VIDEO && s->codecpar->codec_id == AV_CODEC_ID_HEVC &&
       tb.num > 0 && tb.den > 0 && s->codecpar->width > 0 && s->codecpar->height > 0 &&
       s->codecpar->width <= 4096 && s->codecpar->height <= 2160, "bounded HEVC source");
  need(s->codecpar->extradata_size >= 23 && s->codecpar->extradata[0] == 1 &&
       (s->codecpar->extradata[21]&3) == 3, "four-byte hvcC source framing");
  const AVPacketSideData *conf = av_packet_side_data_get(s->codecpar->coded_side_data,
      s->codecpar->nb_coded_side_data, AV_PKT_DATA_DOVI_CONF);
  need(conf && conf->size >= sizeof(AVDOVIDecoderConfigurationRecord), "declared Dolby configuration");
  const AVDOVIDecoderConfigurationRecord *dv = (const void *)conf->data;
  need(dv->dv_version_major == 1 && dv->dv_profile == 7 && dv->bl_present_flag &&
       dv->el_present_flag && dv->rpu_present_flag && !dv->dv_md_compression &&
       dv->dv_bl_signal_compatibility_id == 6, "supported dual-layer Profile 7 declaration");
  fprintf(events, "{\"kind\":\"source\",\"source_sha256\":\"%s\",\"av_version\":\"%s\",\"time_base\":\"%d/%d\",\"decoder_threads\":1,\"software_only\":true,\"normative_composer\":false}\n", source_sha, av_version_info(), tb.num, tb.den);
  open_layer(&layers[0], s, 0); open_layer(&layers[1], s, 1);
  AVPacket *packet = av_packet_alloc(); need(packet != NULL, "demux packet allocation"); int result; unsigned packets = 0;
  /* Read original source from its beginning through EOF, including all trailing
   * references; retain original timestamp/side data and drain each pipeline. */
  while ((result = av_read_frame(input, packet)) >= 0) {
    need(++packets <= 16384 && packet->size <= 16*1024*1024, "finite input packet envelope");
    if (packet->stream_index == stream_index) { inspect(packet); send(&layers[0], 0, packet); send(&layers[1], 1, packet); }
    av_packet_unref(packet);
  }
  need(result == AVERROR_EOF, "complete source read to EOF");
  for (int k = 0; k < 2; k++) send(&layers[k], k, NULL);
  for (int k = 0; k < 2; k++) { check(avcodec_send_packet(layers[k].decoder, NULL), "decoder drain"); receive(&layers[k], k); }
  fprintf(stderr, "coverage: queues=%d/%d accepted=%u selected_coded=%u pairs=%u coded=%u boundary=%d last_selected_end=%"PRId64"\n", layers[0].count, layers[1].count, accepted, selected_coded, pairs, coded_count, boundary_observed, last_end);
  need(!layers[0].count && !layers[1].count && accepted > 0 && accepted == selected_coded &&
       (boundary_observed || cmp(last_end, end) >= 0),
       "complete coded/decoded window coverage");
  unsigned before_random_access = 0;
  for (unsigned i = 0; i < coded_count; i++) {
    if (coded[i].used) continue;
    /* A clipped open-GOP source may contain leading RASL packets after its
     * initial CRA in decode order. HEVC suppresses those earlier presentations
     * at random access. Admit no absent picture inside/after the window, and
     * retain every non-output original AU explicitly rather than relabeling it. */
    need(coded[i].pts < first_paired_pts && cmp(coded[i].pts, start) < 0,
         "missing coded picture at/after initial paired random access");
    before_random_access++;
    need(fprintf(events, "{\"kind\":\"coded_before_initial_random_access_not_output\",\"pts_ticks\":%"PRId64",\"duration_ticks\":%"PRId64",\"packet_sha256\":\"%s\",\"rpu_sha256\":\"%s\"}\n",
                 coded[i].pts, coded[i].duration, coded[i].packet, coded[i].rpu) > 0, "pre-random-access receipt");
  }
  need(pairs + before_random_access == coded_count, "explicit complete source accounting");
  need(fclose(events) == 0 && fclose(planes[0]) == 0 && fclose(planes[1]) == 0 && fclose(rpus) == 0, "complete output close");
  FILE *summary = output(argv[2], "summary.json");
  need(fprintf(summary, "{\"capture_complete\":true,\"canonical_trace_comparison_required\":true,\"source_path\":") > 0, "summary input path");
  json_string(summary, argv[1]);
  need(fprintf(summary, ",\"source_bytes\":%"PRId64",\"source_sha256\":\"%s\",\"window_start\":\"%"PRId64"/%d\",\"window_end\":\"%"PRId64"/%d\",\"video_index\":%ld,\"time_base\":\"%d/%d\",\"bl_width\":%d,\"bl_height\":%d,\"el_width\":%d,\"el_height\":%d,\"libavformat_version\":%u,\"libavcodec_version\":%u,\"libavutil_version\":%u,\"selected_frames\":%u,\"total_pairs\":%u,\"coded_aus\":%u,\"non_output_before_random_access\":%u,\"decoder_threads\":1,\"normative_composer\":false}\n",
               (int64_t)source_stat.st_size, source_sha, start.n, start.d, end.n, end.d, stream_index, tb.num, tb.den,
               widths[0], heights[0], widths[1], heights[1], avformat_version(), avcodec_version(), avutil_version(),
               accepted, pairs, coded_count, before_random_access) > 0 && fclose(summary) == 0, "completion receipt");
  av_packet_free(&packet); avformat_close_input(&input);
  for (int k = 0; k < 2; k++) { avcodec_free_context(&layers[k].decoder); av_bsf_free(&layers[k].bsf); }
  printf("captured %u selected pairs from %u source AUs; canonical trace comparison remains required\n", accepted, coded_count);
  return 0;
}
