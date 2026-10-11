// Bounded timestamped RGB48LE -> NUT using public system libavformat.
#include <errno.h>
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/pixfmt.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
static void need(int ok, const char *msg) {
  if (!ok) {
    fprintf(stderr, "RGB mux refused: %s\n", msg);
    exit(1);
  }
}
static int number(const char *s, int low, int high) {
  char *end;
  errno = 0;
  long n = strtol(s, &end, 10);
  need(!errno && end != s && !*end && n >= low && n <= high, "numeric bounds");
  return (int)n;
}
static int64_t rational_field(char **cursor, int64_t low, int64_t high,
                              char delimiter) {
  char *end;
  errno = 0;
  long long n = strtoll(*cursor, &end, 10);
  need(!errno && end != *cursor && *end == delimiter && n >= low && n <= high,
       "exact bounded rational field");
  *cursor = end + 1;
  return (int64_t)n;
}
static int64_t gcd(int64_t a, int64_t b) {
  while (b) {
    int64_t r = a % b;
    a = b;
    b = r;
  }
  return a;
}
struct timing {
  int64_t pn, dn;
  int pd, dd;
};
int main(int argc, char **argv) {
  need(argc == 7,
       "usage: mux_rgb RGB48LE TIMING.tsv OUTPUT_NUT WIDTH HEIGHT MAX_FRAMES");
  int width = number(argv[4], 64, 3840), height = number(argv[5], 64, 2160),
      cap = number(argv[6], 1, 64);
  need(!(width % 2) && !(height % 2), "even raster");
  size_t bytes = (size_t)width * height * 6;
  need(bytes * cap <= 512ULL * 1024 * 1024 && bytes <= INT_MAX,
       "RGB payload budget");
  FILE *rgb = fopen(argv[1], "rb"), *timing = fopen(argv[2], "r");
  need(rgb && timing, "inputs");
  struct timing rows[64];
  int count = 0;
  int64_t den = 1;
  char line[160];
  while (fgets(line, sizeof(line), timing)) {
    need(count < cap, "excess timing rows");
    char *cursor = line;
    int64_t p = rational_field(&cursor, 0, INT64_MAX, '/');
    int pd = (int)rational_field(&cursor, 1, INT_MAX, '\t');
    int64_t d = rational_field(&cursor, 1, INT64_MAX, '/');
    int dd = (int)rational_field(&cursor, 1, INT_MAX, '\n');
    need(*cursor == 0, "no trailing rational fields");
    rows[count++] = (struct timing){p, d, pd, dd};
    int values[2] = {pd, dd};
    for (int k = 0; k < 2; k++) {
      int64_t factor = values[k] / gcd(den, values[k]);
      need(den <= INT_MAX / factor, "bounded exact common time base");
      den *= factor;
    }
  }
  need(!ferror(timing) && count > 0, "nonempty complete timing");
  AVFormatContext *out = NULL;
  need(avformat_alloc_output_context2(&out, NULL, "nut", argv[3]) >= 0 && out,
       "NUT muxer");
  AVStream *stream = avformat_new_stream(out, NULL);
  need(stream != NULL, "stream");
  stream->time_base = (AVRational){1, (int)den};
  AVCodecParameters *p = stream->codecpar;
  p->codec_type = AVMEDIA_TYPE_VIDEO;
  p->codec_id = AV_CODEC_ID_RAWVIDEO;
  p->format = AV_PIX_FMT_RGB48LE;
  p->codec_tag = avcodec_pix_fmt_to_codec_tag(AV_PIX_FMT_RGB48LE);
  need(p->codec_tag != 0, "explicit RGB48 codec tag");
  p->width = width;
  p->height = height;
  p->bits_per_coded_sample = 48;
  p->color_primaries = AVCOL_PRI_BT2020;
  p->color_trc = AVCOL_TRC_SMPTE2084;
  p->color_space = AVCOL_SPC_RGB;
  p->color_range = AVCOL_RANGE_JPEG;
  need(avio_open(&out->pb, argv[3], AVIO_FLAG_WRITE) >= 0, "output open");
  need(avformat_write_header(out, NULL) >= 0, "header");
  int64_t previous = -1;
  for (int i = 0; i < count; i++) {
    struct timing t = rows[i];
    need(t.pn <= INT64_MAX / (den / t.pd) && t.dn <= INT64_MAX / (den / t.dd),
         "time conversion overflow");
    int64_t pts = t.pn * (den / t.pd), duration = t.dn * (den / t.dd);
    need(pts > previous, "unique ordered PTS");
    previous = pts;
    AVPacket *pkt = av_packet_alloc();
    need(pkt && av_new_packet(pkt, (int)bytes) >= 0, "packet");
    need(fread(pkt->data, 1, bytes, rgb) == bytes, "full RGB frame");
    pkt->stream_index = 0;
    pkt->pts = pkt->dts = pts;
    pkt->duration = duration;
    pkt->flags = AV_PKT_FLAG_KEY;
    av_packet_rescale_ts(pkt, (AVRational){1, (int)den}, stream->time_base);
    need(av_interleaved_write_frame(out, pkt) >= 0, "timestamped packet write");
    av_packet_free(&pkt);
  }
  need(fgetc(rgb) == EOF && !ferror(rgb), "no extra RGB");
  need(av_write_trailer(out) >= 0, "trailer");
  need(avio_closep(&out->pb) >= 0, "output close");
  avformat_free_context(out);
  need(fclose(rgb) == 0 && fclose(timing) == 0, "input close");
  return 0;
}
