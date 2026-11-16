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
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vaapi.h>
#include <libavutil/opt.h>
#include <libavutil/sha.h>
#include <limits.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <time.h>
#include <pthread.h>
#include <unistd.h>
#include <va/va.h>

#define PAIR_WINDOW 16
static bool base_only;
static int source_profile;
static int expected_w[2], expected_h[2], frame_cap, emitted;
static bool debug_frames, initialized, window_mode, window_done, first_pair;
static bool source_eof, discover_el;
// --session CONTROL_FD: one helper produces consecutive windows of one
// rendition. Demuxer, decoders, VA-API and Vulkan/libplacebo state carry
// over; the keyframe seek and its preroll happen only for the session's first
// window. Every window still has its own bounds, outputs, evidence directory
// and observations, and the daemon verifies and publishes each one alone.
static bool session_mode, positioned;
static int control_fd = -1, devnull_fd = -1;
// The window's private evidence directory (single-shot: OUTPUT_DIR). Every
// evidence file is opened relative to it; the helper never changes directory.
static int window_dir = -1, rpu_dir = -1;
// --audio-ordinal K: the session carries the selected soundtrack's packets,
// unchanged, to each window's encoder, so the encoder never opens the source.
// --audio-channels N: the soundtrack's channel count from the daemon's own
// probe. This build has no audio decoders or parsers, so a container whose
// channel count lives only in the codec configuration (AAC in MP4) leaves it
// unknown here; the carried stream's header needs it.
static int audio_ordinal = -1, audio_index = -1, audio_channels = 0;
// --refit-stats / --dump-composed (refit_mode.h): CPU-only F+ statistics.
// Decoded pairs go to refit_pairs instead of the renderer.
static bool refit_mode;
static void refit_pairs(void);
// GPU decode: PLURX_DV_DECODE_DEVICE names the admitted VA-API render node.
// Surfaces stay on the GPU and are mapped into the renderer; a host copy of
// a decoded 4K picture costs more than software decoding it.
// Without a usable device the helper decodes in software, as before GPU
// decode existed, and says why in its decoder trace.
static AVBufferRef *hw_device;
static bool hw_fell_back;
static const char *software_reason = "no-decode-device";
static unsigned preroll, paired, preroll_cap;
struct rational {
  int64_t num;
  int den;
};
static struct rational window_start, window_end;
static int64_t first_emitted_pts = AV_NOPTS_VALUE,
               last_emitted_end = AV_NOPTS_VALUE, boundary_pts = AV_NOPTS_VALUE;
#include "movie_source_io.h"
#include "source_reader.h"
static struct budget_io movie_io = {.fd = -1, .upstream = -1};
static AVIOContext *movie_avio;
static struct source_reader movie_reader = SOURCE_READER_INITIALIZER;
static struct movie_source_interrupt movie_interrupt = {.read_fd = -1, .write_fd = -1};
static bool movie_interrupt_initialized;

static FILE *rgb_output, *timing_output;
// Where this window's wall time went, for the daemon's per-window timing
// line: one flushed row per stage (name, microseconds since start, preroll,
// paired, emitted), so a window stopped at its deadline still shows how far
// it got. Diagnostic only: no failure here changes what is rendered or
// refused, and nothing reads it back to decide anything.
static FILE *stage_output;
static struct timespec helper_started;
// A session opens its source before any window exists. Those rows are kept
// here and written at the head of the first window's stages.tsv, whose clock
// starts at the session's start; later windows' clocks start at their command.
static char session_stage_rows[512];
static size_t session_stage_length;
static bool session_stage_written;
static void stage_mark(const char *name) {
  struct timespec now;
  if ((!stage_output && (!session_mode || session_stage_written)) ||
      clock_gettime(CLOCK_MONOTONIC, &now) != 0)
    return;
  long long us = (long long)(now.tv_sec - helper_started.tv_sec) * 1000000LL +
                 (now.tv_nsec - helper_started.tv_nsec) / 1000;
  if (!stage_output) {
    int length = snprintf(session_stage_rows + session_stage_length,
                          sizeof(session_stage_rows) - session_stage_length,
                          "%s\t%lld\t%u\t%u\t%d\n", name, us, preroll, paired, emitted);
    if (length > 0 && (size_t)length < sizeof(session_stage_rows) - session_stage_length)
      session_stage_length += (size_t)length;
    return;
  }
  if (fprintf(stage_output, "%s\t%lld\t%u\t%u\t%d\n", name, us, preroll,
              paired, emitted) < 0 ||
      fflush(stage_output) != 0) {
    fclose(stage_output);
    stage_output = NULL;
  }
}
static AVFormatContext *nut_output;
static AVStream *nut_stream;
static int64_t output_pts, output_duration, nut_tick_scale;
static int64_t last_pts = AV_NOPTS_VALUE;
static AVRational source_time_base;
static struct rational source_origin;
static int64_t source_origin_ticks;
static const char *source_origin_provenance;
struct coded_input {
  int64_t pts, duration;
  char packet_sha[65], rpu_sha[65];
  bool emitted;
};
static struct coded_input coded_window[64];
static unsigned coded_count;
// Session carry-over across a window boundary. Coded AUs at or after the
// window end were demuxed and decoded for the decoder's reorder lookahead;
// the next window inspects them again against its own bounds. Video AUs
// demuxed after the window ended (while its soundtrack tail was read) were
// never decoded; the next window decodes them first.
#define CARRIED_CODED_LIMIT 64
#define PENDING_VIDEO_LIMIT 256
static AVPacket *carried_coded[CARRIED_CODED_LIMIT];
static unsigned carried_coded_count;
static AVPacket *pending_video[PENDING_VIDEO_LIMIT];
static unsigned pending_video_count;

// The selected soundtrack. Each window's packets [from, end) are written,
// unchanged, to the window's Matroska audio pipe. The session keeps a bounded
// ring of recent packets so the next window's preroll starts where its own
// encoder needs it, without a second source read.
// TrueHD carries about 1200 packets a second; the ring holds the window's
// soundtrack from a cold seek's keyframe onwards.
#define AUDIO_RING_LIMIT 65536
#define AUDIO_RING_BYTES (64U * 1024U * 1024U)
static AVStream *audio_stream;
static AVPacket **audio_ring;
static size_t audio_ring_count, audio_ring_bytes;
// End (audio ticks) of the newest packet trimmed from the ring, and of the
// newest packet demuxed. A window whose preroll reaches before the first is
// refused: its encoder would be handed a soundtrack with a hole.
static int64_t audio_trimmed_end = INT64_MIN, audio_have_end = INT64_MIN;
static bool window_audio;
static struct rational audio_from, audio_end;

struct layer {
  AVBSFContext *split;
  AVCodecContext *decoder;
  AVFrame *queue[PAIR_WINDOW];
  int count, index;
};
static struct layer layers[2];
// Matroska commonly exposes a selected track's endpoint through DURATION
// while leaving AVStream.duration unknown. Parse only its fixed decimal clock;
// the container/audio maximum cannot stand in for this video declaration.
static int video_duration_ns(const char *text, int64_t *duration) {
  if (!text) return 0;
  size_t length = strnlen(text, 33);
  if (length < 18 || length > 25) return 0;
  size_t hours_digits = length - 16;
  if (text[hours_digits] != ':' || text[hours_digits + 3] != ':' ||
      text[hours_digits + 6] != '.') return 0;
  int64_t hours = 0, minute = 0, second = 0, fraction = 0;
  for (size_t i = 0; i < length; i++) {
    if (i == hours_digits || i == hours_digits + 3 || i == hours_digits + 6) continue;
    if (text[i] < '0' || text[i] > '9') return 0;
    int digit = text[i] - '0';
    if (i < hours_digits) hours = hours * 10 + digit;
    else if (i < hours_digits + 3) minute = minute * 10 + digit;
    else if (i < hours_digits + 6) second = second * 10 + digit;
    else fraction = fraction * 10 + digit;
  }
  if (minute >= 60 || second >= 60 || hours > INT64_MAX / 3600000000000LL) return 0;
  int64_t tail = (minute * 60 + second) * 1000000000LL + fraction;
  int64_t whole = hours * 3600000000000LL;
  if (whole > INT64_MAX - tail || whole + tail <= 0) return 0;
  *duration = whole + tail;
  return 1;
}

static void need(int ok, const char *reason) {
  if (!ok) {
    fprintf(stderr, "Segment refused: %s\n", reason);
    // A refusal invalidates every output. Terminate threads together rather
    // than closing a source/driver descriptor underneath an active reader.
    // The parent still requires actual reap and SourceInput settlement.
    fflush(stdout);
    fflush(stderr);
    _exit(1);
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
// Convert the frozen held-probe epoch exactly; never round it onto a different
// source clock or infer it from mutable partial-demux AVStream timestamps.
static int origin_in_source_ticks(struct rational origin, AVRational clock,
                                 int64_t *ticks) {
  if (origin.num < 0 || origin.den <= 0 || clock.num <= 0 || clock.den <= 0)
    return 0;
  __int128 numerator = (__int128)origin.num * clock.den;
  __int128 denominator = (__int128)origin.den * clock.num;
  if (numerator % denominator || numerator / denominator > INT64_MAX)
    return 0;
  *ticks = (int64_t)(numerator / denominator);
  return 1;
}
static int bind_source_origin(bool held, struct rational supplied,
                              const AVStream *stream, struct rational *origin,
                              int64_t *ticks) {
  if (!held) {
    // Private legacy zero-origin controls carry no held-probe authority. Only
    // their actually observed zero is meaningful; unknown/nonzero still refuse.
    if (stream->start_time != 0)
      return 0;
    supplied = (struct rational){0, 1};
  }
  if (!origin_in_source_ticks(supplied, stream->time_base, ticks))
    return 0;
  *origin = supplied;
  return 1;
}
static int declared_video_endpoint(int64_t origin, int64_t duration,
                                   int64_t *endpoint) {
  if (origin < 0 || duration <= 0 || origin > INT64_MAX - duration)
    return 0;
  *endpoint = origin + duration;
  return 1;
}
static int compare_window(int64_t pts, struct rational value) {
  return av_compare_ts(pts, source_time_base, value.num,
                       (AVRational){1, value.den});
}
static ssize_t movie_reader_raw_read(void *opaque, uint8_t *buffer, size_t size) {
  int result = movie_read(opaque, buffer, (int)size);
  return result == AVERROR_EOF ? 0 : result;
}
static int64_t movie_reader_raw_seek(void *opaque, int64_t offset, int whence) {
  return movie_seek(opaque, offset, whence);
}
static uint64_t movie_reader_charged(void *opaque) {
  struct budget_io *io = opaque;
  return io->remote ? io->requested_bytes : io->bytes;
}
static int movie_reader_prepare(void *opaque) {
  struct budget_io *io = opaque;
  if (!movie_interrupt_initialized) {
    int result = movie_source_interrupt_init(&movie_interrupt);
    if (result < 0) return result;
    movie_interrupt_initialized = true;
  }
  io->interrupt = &movie_interrupt;
  return movie_source_interrupt_reset(&movie_interrupt);
}
static void movie_reader_interrupt(void *opaque) {
  struct budget_io *io = opaque;
  if (io->interrupt) movie_source_interrupt_stop(io->interrupt);
}
static struct source_reader_io movie_reader_source = {
  .context = &movie_io, .read = movie_reader_raw_read, .seek = movie_reader_raw_seek,
  .charged = movie_reader_charged, .limit = WINDOW_READ_LIMIT,
  .exhausted = &movie_io.exhausted, .prepare = movie_reader_prepare,
  .interrupt = movie_reader_interrupt,
};
static int queued_movie_read(void *opaque, uint8_t *buffer, int size) {
  if (size <= 0) return AVERROR(EINVAL);
  if (!movie_reader.active) return movie_read(opaque, buffer, size);
  ssize_t result = source_reader_read(&movie_reader, buffer, (size_t)size);
  return result == 0 ? AVERROR_EOF : (int)result;
}
static int64_t queued_movie_seek(void *opaque, int64_t offset, int whence) {
  if ((whence & AVSEEK_SIZE) || !movie_reader.active)
    return movie_seek(opaque, offset, whence);
  return source_reader_seek(&movie_reader, &movie_reader_source,
                            offset, whence & ~AVSEEK_FORCE);
}
static void open_movie_io(AVFormatContext *input, const char *source) {
  need(movie_source_open(&movie_io, source) >= 0,
       "held seekable movie source or private source capability");
  uint8_t *buffer = av_malloc(65536);
  need(buffer != NULL, "bounded movie IO buffer");
  movie_avio = avio_alloc_context(buffer, 65536, 0, &movie_io, queued_movie_read, NULL,
                                  queued_movie_seek);
  if (!movie_avio) av_free(buffer);
  need(movie_avio != NULL, "movie IO context");
  input->pb = movie_avio;
  input->flags |= AVFMT_FLAG_CUSTOM_IO;
  need(movie_source_bind_demuxer(input) >= 0, "closed demux protocol authority");
}
// A queued export may still be sampling the decoded surfaces of the frame
// just rendered; references held here keep the decoder from reusing them
// until that frame has been written.
static AVFrame *held[2];
static void release_held(void) {
  for (int k = 0; k < 2; k++)
    av_frame_free(&held[k]);
}
static void hold_rendered_pair(void) {
  release_held();
  for (int k = 0; k < (base_only ? 1 : 2); k++) {
    held[k] = av_frame_clone(layers[k].queue[0]);
    need(held[k] != NULL, "rendered surface hold");
  }
}
static void drop_pair(void) {
  for (int k = 0; k < (base_only ? 1 : 2); k++) {
    av_frame_free(&layers[k].queue[0]);
    layers[k].count--;
    memmove(layers[k].queue, layers[k].queue + 1,
            (size_t)layers[k].count * sizeof(AVFrame *));
  }
}
// A blocked pipe provides backpressure without a frame queue: at most one
// frame is being written while the next is rendered.
static void write_frame(const uint8_t *rgb, size_t bytes, int64_t pts,
                        int64_t duration);
// Exported frames are written to the encoder pipe on a writer thread, so a
// frame's 25 MB pipe write overlaps the next frame's decode and GPU work. The
// yuv420p10 export downloads into alternating buffers, and a frame is queued
// only once the writer has finished the frame before it, so the buffer being
// written is never the one being downloaded into. Any other export frees its
// buffer when the sink returns, so it waits for its own write.
static pthread_t writer_thread;
static pthread_mutex_t writer_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t writer_cond = PTHREAD_COND_INITIALIZER;
static struct {
  const uint8_t *data;
  pl_buf buf;
  size_t bytes;
  int64_t pts, duration;
  bool pending, busy, stop;
} writer;
static void *writer_main(void *unused) {
  (void)unused;
  pthread_mutex_lock(&writer_lock);
  for (;;) {
    while (!writer.pending && !writer.stop)
      pthread_cond_wait(&writer_cond, &writer_lock);
    if (!writer.pending)
      break;
    const uint8_t *data = writer.data;
    pl_buf buf = writer.buf;
    size_t bytes = writer.bytes;
    int64_t pts = writer.pts, duration = writer.duration;
    writer.pending = false;
    writer.busy = true;
    pthread_mutex_unlock(&writer_lock);
    dv_yuv10_wait_buffer(buf);
    write_frame(data, bytes, pts, duration);
    pthread_mutex_lock(&writer_lock);
    writer.busy = false;
    pthread_cond_broadcast(&writer_cond);
  }
  pthread_mutex_unlock(&writer_lock);
  return NULL;
}
static void writer_wait_idle(void) {
  pthread_mutex_lock(&writer_lock);
  while (writer.pending || writer.busy)
    pthread_cond_wait(&writer_cond, &writer_lock);
  pthread_mutex_unlock(&writer_lock);
}
static void soundtrack_catch_up(void);
static void write_rgb(const uint8_t *rgb, size_t bytes, void *unused) {
  (void)unused;
  soundtrack_catch_up();
  writer_wait_idle();
  // The previous frame is written, so the GPU has finished reading its
  // decoded surfaces.
  release_held();
  pthread_mutex_lock(&writer_lock);
  writer.data = rgb;
  writer.buf = EXPORT_YUV10 ? dv_yuv10_context.frame_buf : NULL;
  writer.bytes = bytes;
  writer.pts = output_pts;
  writer.duration = output_duration;
  writer.pending = true;
  pthread_cond_broadcast(&writer_cond);
  pthread_mutex_unlock(&writer_lock);
  if (!EXPORT_YUV10)
    writer_wait_idle();
}
static void writer_start(void) {
  dv_threads_live = true;
  // A session starts one writer per window.
  writer.pending = writer.busy = writer.stop = false;
  need(pthread_create(&writer_thread, NULL, writer_main, NULL) == 0,
       "export writer thread");
}
static void writer_finish(void) {
  pthread_mutex_lock(&writer_lock);
  writer.stop = true;
  pthread_cond_broadcast(&writer_cond);
  pthread_mutex_unlock(&writer_lock);
  need(pthread_join(writer_thread, NULL) == 0, "export writer join");
  // A session's soundtrack writer and later windows keep threads alive.
  dv_threads_live = session_mode;
}
static void write_frame(const uint8_t *rgb, size_t bytes, int64_t pts,
                        int64_t duration) {
  if (!nut_output) {
    need(fwrite(rgb, 1, bytes, rgb_output) == bytes, "RGB frame write");
    return;
  }
  need(bytes <= INT_MAX && pts >= 0 && duration > 0,
       "bounded nonnegative NUT presentation interval");
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "NUT packet allocation");
  packet->data = (uint8_t *)rgb;
  packet->size = (int)bytes;
  packet->stream_index = nut_stream->index;
  need(nut_scale_interval(pts, duration, nut_tick_scale,
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
  enum AVPixelFormat format = EXPORT_YUV10 ? AV_PIX_FMT_YUV420P10LE : AV_PIX_FMT_RGB48LE;
  p->format = format;
  p->codec_tag = avcodec_pix_fmt_to_codec_tag(format);
  need(p->codec_tag != 0, "explicit raw NUT pixel tag");
  p->width = EXPORT_WIDTH;
  p->height = EXPORT_HEIGHT;
  p->bits_per_coded_sample = EXPORT_YUV10 ? 15 : 48;
  p->color_primaries = dv_target.sdr ? AVCOL_PRI_BT709 : AVCOL_PRI_BT2020;
  // H.273 has no pure gamma-2.4 identifier; conventional SDR is BT.709.
  p->color_trc = dv_target.sdr ? AVCOL_TRC_BT709 : AVCOL_TRC_SMPTE2084;
  p->color_space = EXPORT_YUV10 ? (dv_target.sdr ? AVCOL_SPC_BT709 : AVCOL_SPC_BT2020_NCL) : AVCOL_SPC_RGB;
  p->color_range = EXPORT_YUV10 ? AVCOL_RANGE_MPEG : AVCOL_RANGE_JPEG;
  if (EXPORT_YUV10)
    p->chroma_location = AVCHROMA_LOC_LEFT;
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
  if (frame->format == AV_PIX_FMT_VAAPI) {
    AVFrame *host = av_frame_alloc();
    need(host && av_hwframe_transfer_data(host, frame, 0) >= 0 &&
             host->format == AV_PIX_FMT_P010LE,
         "diagnostic decoded surface download");
    uint16_t *out = pack(host);
    av_frame_free(&host);
    return out;
  }
  size_t count = (size_t)frame->width * frame->height * 3 / 2;
  uint16_t *out = malloc(count * 2);
  need(out != NULL, "native pair allocation");
  size_t at = 0;
  if (frame->format == AV_PIX_FMT_P010LE) {
    // Same planar ten-bit codes the software decoder produces.
    for (int y = 0; y < frame->height; y++) {
      const uint16_t *row = (const uint16_t *)(frame->data[0] + (size_t)y * frame->linesize[0]);
      for (int x = 0; x < frame->width; x++) out[at++] = row[x] >> 6;
    }
    size_t u = at, v = at + count / 6;
    for (int y = 0; y < frame->height / 2; y++) {
      const uint16_t *row = (const uint16_t *)(frame->data[1] + (size_t)y * frame->linesize[1]);
      for (int x = 0; x < frame->width / 2; x++) {
        out[u++] = row[2 * x] >> 6;
        out[v++] = row[2 * x + 1] >> 6;
      }
    }
    return out;
  }
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
// One exclusive evidence file in a private directory the daemon created for
// this window. Opened relative to that directory, never through the process
// working directory, so a session's windows cannot reach each other's files.
static FILE *evidence_file(int directory, const char *name, int access, const char *mode) {
  int fd = openat(directory, name, access | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
  if (fd < 0)
    return NULL;
  FILE *file = fdopen(fd, mode);
  if (!file)
    close(fd);
  return file;
}
// The renderer reports its Vulkan runtime and GPU context once, when it
// creates them. A session's later windows render with that same context, so
// their own observations carry the same report.
static char *gpu_lines;
static size_t gpu_lines_size;
static off_t gpu_lines_mark(void) {
  need(fflush(stdout) == 0, "observations flush");
  off_t at = lseek(STDOUT_FILENO, 0, SEEK_CUR);
  need(at >= 0, "window observation offset");
  return at;
}
static void gpu_lines_keep(off_t before) {
  off_t after = gpu_lines_mark();
  need(after >= before && after - before <= 65536, "bounded GPU runtime report");
  gpu_lines_size = (size_t)(after - before);
  gpu_lines = malloc(gpu_lines_size + 1);
  need(gpu_lines != NULL &&
           pread(STDOUT_FILENO, gpu_lines, gpu_lines_size, before) == (ssize_t)gpu_lines_size,
       "GPU runtime report copy");
}

// The window's soundtrack goes to its own Matroska pipe on a writer thread,
// so a blocked audio pipe never stalls decoding and a blocked video pipe
// never starves the encoder's audio input. Matroska, not NUT: NUT has no
// codec tag for TrueHD, and the packets are carried unchanged.
static AVFormatContext *audio_output;
static int audio_output_fd = -1;
static bool audio_consumer_done;
static pthread_t audio_thread;
static pthread_mutex_t audio_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t audio_cond = PTHREAD_COND_INITIALIZER;
static struct {
  AVPacket *queue[AUDIO_RING_LIMIT];
  size_t head, count;
  bool stop;
} audio_writer;
static void *audio_writer_main(void *unused) {
  (void)unused;
  pthread_mutex_lock(&audio_lock);
  for (;;) {
    while (!audio_writer.count && !audio_writer.stop)
      pthread_cond_wait(&audio_cond, &audio_lock);
    if (!audio_writer.count)
      break;
    AVPacket *packet = audio_writer.queue[audio_writer.head];
    audio_writer.head = (audio_writer.head + 1) % AUDIO_RING_LIMIT;
    audio_writer.count--;
    pthread_mutex_unlock(&audio_lock);
    if (!audio_consumer_done) {
      packet->stream_index = 0;
      av_packet_rescale_ts(packet, audio_stream->time_base,
                           audio_output->streams[0]->time_base);
      int written = av_write_frame(audio_output, packet);
      // Once nothing more is queued, close the cluster and flush: the
      // encoder reads this pipe as it is written.
      pthread_mutex_lock(&audio_lock);
      bool idle = audio_writer.count == 0;
      pthread_mutex_unlock(&audio_lock);
      if (written >= 0 && idle)
        written = av_write_frame(audio_output, NULL);
      if (written >= 0 && idle) {
        avio_flush(audio_output->pb);
        written = audio_output->pb->error;
      }
      // The encoder stops reading its soundtrack once its output bound is
      // reached; what follows the window is no longer wanted. Any other
      // write failure refuses the window.
      if (written == AVERROR(EPIPE))
        audio_consumer_done = true;
      else
        need(written >= 0, "soundtrack packet write");
    }
    av_packet_free(&packet);
    pthread_mutex_lock(&audio_lock);
  }
  pthread_mutex_unlock(&audio_lock);
  return NULL;
}
static void audio_queue(const AVPacket *packet) {
  AVPacket *copy = av_packet_clone(packet);
  need(copy != NULL, "queued soundtrack packet");
  pthread_mutex_lock(&audio_lock);
  if (audio_writer.count == AUDIO_RING_LIMIT) {
    pthread_mutex_unlock(&audio_lock);
    need(0, "bounded queued soundtrack");
  }
  audio_writer.queue[(audio_writer.head + audio_writer.count) % AUDIO_RING_LIMIT] = copy;
  audio_writer.count++;
  pthread_cond_broadcast(&audio_cond);
  pthread_mutex_unlock(&audio_lock);
}
static void open_audio_output(int fd) {
  char path[32];
  snprintf(path, sizeof(path), "/proc/self/fd/%d", fd);
  // The encoder must demux the timestamps it would have read from the
  // source, in the source's own time base: its resampler rebuilds sample
  // positions from them, and the same packets on a millisecond clock and on
  // a sample clock come out of it up to half a millisecond apart. So a
  // Matroska source (millisecond clock) is carried in Matroska, and any
  // other in NUT, which keeps a sample-exact clock (MP4's); a codec NUT
  // cannot tag (TrueHD) is carried in Matroska.
  const AVOutputFormat *nut = av_guess_format("nut", NULL, NULL);
  bool millisecond = audio_stream->time_base.num == 1 && audio_stream->time_base.den == 1000;
  const char *container =
      !millisecond && nut &&
              avformat_query_codec(nut, audio_stream->codecpar->codec_id, FF_COMPLIANCE_NORMAL) == 1
          ? "nut" : "matroska";
  need(avformat_alloc_output_context2(&audio_output, NULL, container, path) >= 0 &&
           audio_output,
       "soundtrack output context");
  AVStream *out = avformat_new_stream(audio_output, NULL);
  need(out != NULL && avcodec_parameters_copy(out->codecpar, audio_stream->codecpar) >= 0,
       "soundtrack output stream");
  out->codecpar->codec_tag = 0;
  if (out->codecpar->ch_layout.nb_channels == 0 && audio_channels > 0)
    av_channel_layout_default(&out->codecpar->ch_layout, audio_channels);
  need(out->codecpar->ch_layout.nb_channels > 0 && out->codecpar->sample_rate > 0,
       "carried soundtrack channel count and rate");
  out->time_base = audio_stream->time_base;
  need(avio_open(&audio_output->pb, path, AVIO_FLAG_WRITE) >= 0, "soundtrack output open");
  need(avformat_write_header(audio_output, NULL) >= 0, "soundtrack output header");
  avio_flush(audio_output->pb);
  audio_output_fd = fd;
  audio_writer.head = audio_writer.count = 0;
  audio_writer.stop = false;
  audio_consumer_done = false;
  need(pthread_create(&audio_thread, NULL, audio_writer_main, NULL) == 0,
       "soundtrack writer thread");
}
static void close_audio_output(void) {
  pthread_mutex_lock(&audio_lock);
  audio_writer.stop = true;
  pthread_cond_broadcast(&audio_cond);
  pthread_mutex_unlock(&audio_lock);
  need(pthread_join(audio_thread, NULL) == 0, "soundtrack writer join");
  if (!audio_consumer_done) {
    int closed = av_write_trailer(audio_output);
    if (closed >= 0)
      closed = audio_output->pb->error;
    need(closed >= 0 || closed == AVERROR(EPIPE), "soundtrack trailer");
  }
  avio_closep(&audio_output->pb);
  avformat_free_context(audio_output);
  audio_output = NULL;
  need(close(audio_output_fd) == 0, "soundtrack pipe close");
  audio_output_fd = -1;
}
static int audio_compare(int64_t ticks, struct rational value) {
  return av_compare_ts(ticks, audio_stream->time_base, value.num,
                       (AVRational){1, value.den});
}
// A packet belongs to a window when any of its samples fall in [from, end).
static bool audio_in_window(const AVPacket *packet) {
  return audio_compare(packet->pts + packet->duration, audio_from) > 0 &&
         audio_compare(packet->pts, audio_end) < 0;
}
static int64_t video_demuxed_pts = AV_NOPTS_VALUE;
static bool audio_source_ended;
// More soundtrack is read only while the selected video is not far past the
// window's audio end. Interleaved sources keep both within a few seconds; a
// soundtrack that ends early is then padded by the encoder, never read for.
static bool audio_wanted(void) {
  if (!window_audio || audio_source_ended)
    return false;
  if (audio_have_end != INT64_MIN && audio_compare(audio_have_end, audio_end) >= 0)
    return false;
  struct rational horizon = {audio_end.num + 3LL * audio_end.den, audio_end.den};
  return video_demuxed_pts == AV_NOPTS_VALUE || compare_window(video_demuxed_pts, horizon) < 0;
}
static int64_t audio_last_pts = AV_NOPTS_VALUE;
static void audio_packet(const AVPacket *packet) {
  need(packet->pts != AV_NOPTS_VALUE && packet->duration >= 0 &&
           packet->pts <= INT64_MAX - packet->duration &&
           (audio_last_pts == AV_NOPTS_VALUE || packet->pts >= audio_last_pts),
       "timestamped monotonic soundtrack packet");
  audio_last_pts = packet->pts;
  // A container may leave a packet's duration out (Matroska blocks without
  // one). Its extent is then up to the next packet: the packet that holds
  // the interval's start is still carried, as the encoder's own demuxer
  // would have decoded it.
  if (audio_ring_count) {
    AVPacket *previous = audio_ring[audio_ring_count - 1];
    if (previous->duration == 0 && packet->pts > previous->pts) {
      bool carried = window_audio && audio_in_window(previous);
      previous->duration = packet->pts - previous->pts;
      if (previous->pts + previous->duration > audio_have_end)
        audio_have_end = previous->pts + previous->duration;
      if (window_audio && !carried && audio_in_window(previous))
        audio_queue(previous);
    }
  }
  need(audio_ring_count < AUDIO_RING_LIMIT &&
           audio_ring_bytes + (size_t)packet->size <= AUDIO_RING_BYTES,
       "bounded carried soundtrack");
  AVPacket *copy = av_packet_clone(packet);
  need(copy != NULL, "carried soundtrack packet");
  audio_ring[audio_ring_count++] = copy;
  audio_ring_bytes += (size_t)copy->size;
  if (audio_have_end == INT64_MIN || copy->pts + copy->duration > audio_have_end)
    audio_have_end = copy->pts + copy->duration;
  if (window_audio && audio_in_window(copy))
    audio_queue(copy);
}
// Keep what the next window's soundtrack preroll can need. Consecutive
// windows' preroll starts advance by the window's length to within two AAC
// frames; half a second of margin keeps every packet that start can reach.
static void audio_trim(void) {
  int64_t start_us = av_rescale(window_start.num, 1000000, window_start.den);
  int64_t end_us = av_rescale(window_end.num, 1000000, window_end.den);
  int64_t from_us = av_rescale(audio_from.num, 1000000, audio_from.den);
  int64_t keep = av_rescale_q(from_us + (end_us - start_us) - 500000,
                              (AVRational){1, 1000000}, audio_stream->time_base);
  size_t kept = 0;
  for (size_t i = 0; i < audio_ring_count; i++) {
    AVPacket *packet = audio_ring[i];
    if (packet->pts + packet->duration <= keep) {
      if (audio_trimmed_end == INT64_MIN || packet->pts + packet->duration > audio_trimmed_end)
        audio_trimmed_end = packet->pts + packet->duration;
      audio_ring_bytes -= (size_t)packet->size;
      av_packet_free(&packet);
    } else {
      audio_ring[kept++] = packet;
    }
  }
  audio_ring_count = kept;
}

// Four-byte hvcC packet framing, one first slice per BL/EL picture and one
// fresh RPU. This independent coded-stage trace predates either decoder or GPU.
static void inspect_coded(AVPacket *packet) {
  need(packet->pts != AV_NOPTS_VALUE && packet->duration > 0,
       "coded AU needs original PTS/positive duration");
  // Decode-order references can follow the requested presentation interval.
  // Their Dolby metadata does not describe any picture this window emits.
  // A session's next window inspects them again against its own bounds.
  if (compare_window(packet->pts, window_end) >= 0) {
    if (session_mode) {
      need(carried_coded_count < CARRIED_CODED_LIMIT,
           "bounded coded lookahead across a session window");
      carried_coded[carried_coded_count] = av_packet_clone(packet);
      need(carried_coded[carried_coded_count] != NULL, "carried coded AU");
      carried_coded_count++;
    }
    return;
  }
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
    input->emitted = false;
    hash_bytes(packet->data, packet->size, input->packet_sha);
    hash_bytes(rpu_bytes, rpu_len, input->rpu_sha);
    printf("{\"kind\":\"coded_window_input\",\"pts_ticks\":%lld,\"duration_"
           "ticks\":%lld,\"packet_sha256\":\"%s\",\"rpu_sha256\":\"%s\"}\n",
           (long long)input->pts, (long long)input->duration, input->packet_sha,
           input->rpu_sha);
  }
}
static void render_pairs(void) {
  while (!window_done && layers[0].count && (base_only || layers[1].count)) {
    AVFrame *bl = layers[0].queue[0],
            *el = base_only ? bl : layers[1].queue[0];
    need(bl->pts == el->pts && bl->duration == el->duration && bl->duration > 0,
         "unmatched or ambiguous BL/EL PTS/duration");
    need(last_pts == AV_NOPTS_VALUE || bl->pts > last_pts,
         "duplicate/out-of-order paired frame");
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
      // Observe the exclusive boundary before interpreting its RPU. A fresh
      // unsupported RPU belongs to the next window, which must refuse it there.
      // A session keeps the boundary pair queued: it is the next window's
      // first picture.
      if (compare_window(bl->pts, window_end) >= 0) {
        boundary_pts = bl->pts;
        window_done = true;
        if (!session_mode) {
          last_pts = bl->pts;
          drop_pair();
        }
        return;
      }
    }
    AVFrameSideData *raw =
        av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_RPU_BUFFER);
    need(raw && raw->size > 0 && raw->size < 4096,
         "fresh decoder-attached RPU required; cached metadata insufficient");
    uint8_t nalu[4098] = {0x7c, 0x01};
    memcpy(nalu + 2, raw->data, raw->size);
    struct base_mapping base_mapped = {0};
    if (base_only)
      base_metadata(bl, nalu, raw->size + 2, source_profile, &base_mapped);
    struct pl_dovi_metadata fel_mapped = {0};
    if (!base_only)
      map_fel_rpu(bl, nalu, raw->size + 2, &fel_mapped,
                  !window_mode || compare_window(bl->pts, window_start) >= 0,
                  bl->width, bl->height);
    if (window_mode) {
      if (compare_window(bl->pts, window_start) < 0) {
        last_pts = bl->pts;
        need(++preroll <= preroll_cap, "bounded preroll exhausted");
        drop_pair();
        continue;
      }
    }
    need(emitted < frame_cap, "emitted window frame cap exhausted");
    bool diagnostic_hashes = debug_frames || dv_frame_hashes();
    bool surfaces = bl->format == AV_PIX_FMT_VAAPI;
    need(base_only || surfaces == (el->format == AV_PIX_FMT_VAAPI),
         "both layers decoded the same way");
    // Mapped surfaces need no host copy; after an import failure the
    // renderer takes host copies for the rest of the window.
    bool mapped = surfaces && !dv_hw_host_copy;
    uint16_t *native[2] = {
        (base_only || mapped) && !diagnostic_hashes ? NULL : pack(bl),
        base_only || (mapped && !diagnostic_hashes) ? NULL : pack(el)};
    const AVFrame *hardware[2] = {bl, el};
    if (emitted == 0)
      stage_mark("first_frame");
    if (!initialized) {
      off_t before = session_mode ? gpu_lines_mark() : -1;
      if (base_only)
        base_gpu_init(expected_w[0], expected_h[0]);
      else
        gpu_init(expected_w[0], expected_h[0], expected_w[1], expected_h[1]);
      if (session_mode)
        gpu_lines_keep(before);
      initialized = true;
      stage_mark("gpu_ready");
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
      gpu_render_mapped(native, mapped ? hardware : NULL, nalu, raw->size + 2, emitted, pts, duration,
                 chroma(bl->chroma_location), chroma(el->chroma_location),
                 write_rgb, NULL, debug_frames, &fel_mapped);
    if (EXPORT_YUV10 && dv_yuv10_context.frame_buf)
      hold_rendered_pair();
    char rpu_path[64];
    int rpu_length =
        snprintf(rpu_path, sizeof(rpu_path), "frame-%03d.nal", emitted);
    need(rpu_length > 0 && (size_t)rpu_length < sizeof(rpu_path), "RPU path");
    FILE *rpu_output = evidence_file(rpu_dir, rpu_path, O_WRONLY, "wb");
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
    bool surface = frame->format == AV_PIX_FMT_VAAPI && frame->hw_frames_ctx &&
                   ((AVHWFramesContext *)frame->hw_frames_ctx->data)->sw_format ==
                       AV_PIX_FMT_P010LE;
    need((frame->format == AV_PIX_FMT_YUV420P10LE || surface) &&
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
    if (refit_mode)
      refit_pairs();
    else
      render_pairs();
    // A session keeps decoding what it was handed: frames past the window
    // end stay queued for the next window, and the other layer must see
    // every packet this one saw.
    if (window_done && !session_mode)
      break;
  }
  need((window_done && !session_mode) || result == AVERROR(EAGAIN) ||
           result == AVERROR_EOF,
       "decoder receive");
  av_frame_free(&frame);
}
// The device must decode HEVC Main10 itself; otherwise every window would be
// a single-threaded software decode behind a GPU device that does nothing.
static bool decodes_hevc_main10(AVBufferRef *device) {
  VADisplay display =
      ((AVVAAPIDeviceContext *)((AVHWDeviceContext *)device->data)->hwctx)->display;
  int profiles = vaMaxNumProfiles(display), entrypoints = vaMaxNumEntrypoints(display);
  if (profiles <= 0 || entrypoints <= 0)
    return false;
  VAProfile *profile = calloc((size_t)profiles, sizeof(*profile));
  VAEntrypoint *entrypoint = calloc((size_t)entrypoints, sizeof(*entrypoint));
  bool main10 = false, decode = false;
  if (profile && vaQueryConfigProfiles(display, profile, &profiles) == VA_STATUS_SUCCESS)
    for (int i = 0; i < profiles; i++)
      main10 |= profile[i] == VAProfileHEVCMain10;
  if (main10 && entrypoint &&
      vaQueryConfigEntrypoints(display, VAProfileHEVCMain10, entrypoint,
                               &entrypoints) == VA_STATUS_SUCCESS)
    for (int i = 0; i < entrypoints; i++)
      decode |= entrypoint[i] == VAEntrypointVLD;
  free(profile);
  free(entrypoint);
  return main10 && decode;
}
static enum AVPixelFormat hardware_format(AVCodecContext *context,
                                          const enum AVPixelFormat *offered) {
  (void)context;
  for (const enum AVPixelFormat *p = offered; *p != AV_PIX_FMT_NONE; p++)
    if (*p == AV_PIX_FMT_VAAPI)
      return *p;
  // The decoder offers no GPU surface for this stream; decode it in software
  // and say so in the trace.
  hw_fell_back = true;
  for (const enum AVPixelFormat *p = offered; *p != AV_PIX_FMT_NONE; p++)
    if (*p == AV_PIX_FMT_YUV420P10LE)
      return *p;
  return AV_PIX_FMT_NONE;
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
  // Window admission reserves four frame workers per layer, plus the helper
  // caller and the one software-Vulkan worker. Disable nested slice threading.
  // A refit decodes in software with the same four workers per layer.
  out.decoder->thread_count = (window_mode || refit_mode) && !hw_device ? 4 : 1;
  out.decoder->thread_type = FF_THREAD_FRAME;
  if (hw_device) {
    out.decoder->hw_device_ctx = av_buffer_ref(hw_device);
    need(out.decoder->hw_device_ctx != NULL, "GPU decoder device reference");
    out.decoder->get_format = hardware_format;
    // Each layer queues up to PAIR_WINDOW decoded pictures while it waits
    // for its partner; the surface pool must hold them plus references.
    out.decoder->extra_hw_frames = PAIR_WINDOW + 4;
  }
  out.decoder->max_pixels = discover_el && index == 1
                                ? (int64_t)expected_w[0] * expected_h[0]
                                : (int64_t)expected_w[index] * expected_h[index];
  out.decoder->err_recognition = AV_EF_CAREFUL | AV_EF_EXPLODE;
  need(avcodec_open2(out.decoder, codec, NULL) >= 0,
       "independent decoder open");
  need(!window_mode || hw_device ||
           (out.decoder->thread_count == 4 &&
            out.decoder->active_thread_type == FF_THREAD_FRAME),
       "bounded frame-only decoder workers");
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
    if (window_done && !session_mode)
      break;
  }
  need((window_done && !session_mode) || result == AVERROR(EAGAIN) ||
           result == AVERROR_EOF,
       "split receive");
  av_packet_free(&out);
}
#include "refit_mode.h"
// What one window needs: its exact bounds and picture caps, its soundtrack
// interval, and where its outputs go. Single-shot reads it from argv, a
// session from each command on its control socket.
struct window_request {
  struct rational start, end;
  int frames;
  unsigned max_preroll;
  bool audio;
  struct rational audio_from, audio_end;
  const char *nut_path;
  int nut_fd, audio_fd;
};
static AVFormatContext *input;
static AVStream *stream;
static int stream_index;
static AVPacket *packet;
static bool streaming, decoders_open, decoders_drained;
static struct rational previous_end;
static void check_window_bounds(struct rational start, struct rational end) {
  need(av_compare_ts(start.num, (AVRational){1, start.den}, end.num,
                     (AVRational){1, end.den}) < 0,
       "ordered nonempty window");
  __int128 delta = (__int128)end.num * start.den - (__int128)start.num * end.den;
  need(delta <= (__int128)3 * start.den * end.den,
       "initial window duration at most three seconds");
}
static bool same_rational(struct rational a, struct rational b) {
  return (__int128)a.num * b.den == (__int128)b.num * a.den;
}
static void open_decoders(void) {
  const char *decode_device = getenv("PLURX_DV_DECODE_DEVICE");
  if (decode_device && *decode_device) {
    if (av_hwdevice_ctx_create(&hw_device, AV_HWDEVICE_TYPE_VAAPI,
                               decode_device, NULL, 0) < 0) {
      hw_device = NULL;
      software_reason = "decode-device-unavailable";
    } else if (!decodes_hevc_main10(hw_device)) {
      av_buffer_unref(&hw_device);
      software_reason = "no-hevc-main10-decode";
    }
  }
  layers[0] = open_layer(stream, 0);
  if (!base_only)
    layers[1] = open_layer(stream, 1);
  decoders_open = true;
  stage_mark("decoders");
}
// The keyframe seek a window's first picture needs, made once per session.
// When the window carries its soundtrack, the seek also reaches the start of
// the soundtrack's preroll, so the encoder is handed what it would have read.
static void seek_window(void) {
  need(compare_window(source_origin_ticks, window_start) <= 0,
       "window starts before frozen source origin");
  struct rational at = window_start;
  if (window_audio &&
      av_compare_ts(audio_from.num, (AVRational){1, audio_from.den}, at.num,
                    (AVRational){1, at.den}) < 0)
    at = audio_from;
  int64_t target = av_rescale_q_rnd(at.num, (AVRational){1, at.den},
                                    source_time_base, AV_ROUND_DOWN);
  need(target >= 0 && target < INT64_MAX, "representable seek target");
  need(avformat_seek_file(input, stream_index, INT64_MIN, target, target,
                          AVSEEK_FLAG_BACKWARD) >= 0,
       "keyframe seek before exact window start");
  stage_mark("seeked");
}
static const char *decoder_mode(void) {
  return !hw_device ? "software"
         : hw_fell_back ? "vaapi-software-fallback"
         : dv_hw_host_copy ? "vaapi-host-copy"
                           : "vaapi";
}
static const char *decoder_reason(void) {
  return !hw_device ? software_reason
         : hw_fell_back ? "no-gpu-surface-offered"
         : dv_hw_host_copy ? "surface-import-failed"
                           : "";
}
static void video_packet(AVPacket *video) {
  if (window_mode)
    inspect_coded(video);
  send_layer(&layers[0], video);
  // A session's other layer sees every packet the first one decoded.
  if (!base_only && (session_mode || !window_done))
    send_layer(&layers[1], video);
}
// One packet: a pending video AU the previous window demuxed, else the next
// demuxed packet, routed to the decoders, the soundtrack, or (past the window
// end) the next window's pending AUs. Returns av_read_frame's result.
static unsigned window_packets, window_video_packets;
static int pump(unsigned *packets, unsigned *video_packets) {
  if (!window_done && pending_video_count) {
    AVPacket *next = pending_video[0];
    memmove(pending_video, pending_video + 1,
            --pending_video_count * sizeof(*pending_video));
    av_packet_move_ref(packet, next);
    av_packet_free(&next);
    need(++*video_packets <= 2 * (preroll_cap + frame_cap + 1),
         "bounded video access units");
    video_packet(packet);
    av_packet_unref(packet);
    return 0;
  }
  int result = av_read_frame(input, packet);
  if (result < 0)
    return result;
  // The soundtrack is bounded by its own ring; every other packet by the
  // window's packet envelope.
  need((packet->stream_index == audio_index || ++*packets <= 4096) &&
           packet->size <= 16 * 1024 * 1024,
       "finite segment packet/byte envelope");
  if (packet->stream_index == stream_index) {
    if (packet->pts != AV_NOPTS_VALUE &&
        (video_demuxed_pts == AV_NOPTS_VALUE || packet->pts > video_demuxed_pts))
      video_demuxed_pts = packet->pts;
    if (window_done) {
      need(pending_video_count < PENDING_VIDEO_LIMIT,
           "bounded video read past a session window");
      pending_video[pending_video_count] = av_packet_clone(packet);
      need(pending_video[pending_video_count] != NULL, "pending video AU");
      pending_video_count++;
    } else {
      need(++*video_packets <= (window_mode ? 2 * (preroll_cap + frame_cap + 1) : 256),
           "bounded video access units");
      video_packet(packet);
    }
  } else if (packet->stream_index == audio_index) {
    audio_packet(packet);
  }
  av_packet_unref(packet);
  return 0;
}
// The encoder interleaves its two inputs: it stops taking pictures when its
// soundtrack falls behind them. Before a picture can block on the video
// pipe, the soundtrack it plays against (and half a second more) is demuxed;
// video read meanwhile stays coded, in order, for the window to decode next.
// So a source that stores its audio after its video never stalls a window.
static AVPacket *lookahead;
static void soundtrack_catch_up(void) {
  if (!window_audio || window_done)
    return;
  int64_t lead = av_rescale_q(1, (AVRational){1, 2}, source_time_base);
  while (audio_wanted() && pending_video_count < PENDING_VIDEO_LIMIT &&
         (audio_have_end == INT64_MIN ||
          av_compare_ts(audio_have_end, audio_stream->time_base, output_pts + lead,
                        source_time_base) < 0)) {
    int result = av_read_frame(input, lookahead);
    if (result == AVERROR_EOF) {
      audio_source_ended = true;
      return;
    }
    need(result >= 0, "source read");
    need((lookahead->stream_index == audio_index || ++window_packets <= 4096) &&
             lookahead->size <= 16 * 1024 * 1024,
         "finite segment packet/byte envelope");
    if (lookahead->stream_index == stream_index) {
      if (lookahead->pts != AV_NOPTS_VALUE &&
          (video_demuxed_pts == AV_NOPTS_VALUE || lookahead->pts > video_demuxed_pts))
        video_demuxed_pts = lookahead->pts;
      pending_video[pending_video_count] = av_packet_clone(lookahead);
      need(pending_video[pending_video_count] != NULL, "pending video AU");
      pending_video_count++;
    } else if (lookahead->stream_index == audio_index) {
      audio_packet(lookahead);
    }
    av_packet_unref(lookahead);
  }
}
static void run_window(const struct window_request *request) {
  window_start = request->start;
  window_end = request->end;
  frame_cap = request->frames;
  preroll_cap = request->max_preroll;
  window_audio = request->audio;
  audio_from = request->audio_from;
  audio_end = request->audio_end;
  emitted = 0;
  preroll = paired = coded_count = 0;
  window_done = source_eof = audio_source_ended = false;
  first_emitted_pts = last_emitted_end = boundary_pts = AV_NOPTS_VALUE;
  window_packets = window_video_packets = 0;
  unsigned *packets = &window_packets, *video_packets = &window_video_packets;
  uint64_t bytes_at_start = movie_io.bytes;
  // The read envelope, the packet envelope and the picture caps bound each
  // window; a session never spends one window's budget on the next.
  uint64_t charged = movie_reader_charged(&movie_io);
  need(charged <= UINT64_MAX - WINDOW_READ_LIMIT, "window read envelope overflow");
  movie_io.read_limit = charged + WINDOW_READ_LIMIT;
  movie_reader_source.limit = movie_io.read_limit;
  movie_io.exhausted = false;
  bool warm = positioned;
  if (warm) {
    need(!decoders_drained && same_rational(window_start, previous_end),
         "contiguous session window");
    // The picture chain began at the session's keyframe.
    first_pair = true;
    need(!window_audio || audio_trimmed_end == INT64_MIN ||
             audio_compare(audio_trimmed_end, audio_from) <= 0,
         "carried soundtrack covers the window's preroll");
  } else if (window_mode) {
    first_pair = false;
    seek_window();
  }
  if (window_mode) {
    // require() refusals in shared renderer code must also avoid destructors
    // throughout the reader-only interval before the export writer starts.
    dv_threads_live = true;
    // A session's reader keeps its read-ahead across windows; each window
    // only raises its envelope.
    need((movie_reader.active
              ? source_reader_set_limit(&movie_reader, &movie_reader_source)
              : source_reader_start(&movie_reader, &movie_reader_source)) == 0,
         "start owned source reader");
  }
  char nut_path[32];
  if (streaming) {
    if (request->nut_fd >= 0)
      snprintf(nut_path, sizeof(nut_path), "/proc/self/fd/%d", request->nut_fd);
    open_nut(request->nut_fd >= 0 ? nut_path : request->nut_path);
  }
  if (window_audio) {
    open_audio_output(request->audio_fd);
    for (size_t i = 0; i < audio_ring_count; i++)
      if (audio_in_window(audio_ring[i]))
        audio_queue(audio_ring[i]);
  }
  writer_start();
  if (!decoders_open)
    open_decoders();
  if (warm) {
    need(fwrite(gpu_lines, 1, gpu_lines_size, stdout) == gpu_lines_size,
         "GPU runtime report");
    AVPacket *carried[CARRIED_CODED_LIMIT];
    unsigned count = carried_coded_count;
    memcpy(carried, carried_coded, count * sizeof(*carried));
    carried_coded_count = 0;
    for (unsigned i = 0; i < count; i++) {
      inspect_coded(carried[i]);
      av_packet_free(&carried[i]);
    }
    render_pairs();
  }
  int result = 0;
  while (!window_done && (result = pump(packets, video_packets)) >= 0) {
  }
  need(!session_mode || result >= 0 || result == AVERROR_EOF, "source read");
  source_eof = result == AVERROR_EOF;
  audio_source_ended = source_eof;
  need(window_done || source_eof, "complete bounded source read");
  for (int k = 0; !window_done && k < (base_only ? 1 : 2); k++) {
    need(avcodec_send_packet(layers[k].decoder, NULL) >= 0, "decoder drain");
    receive(&layers[k]);
    decoders_drained = true;
  }
  // Every frame is rendered: finish its writes before the window checks.
  writer_finish();
  stage_mark("rendered");
  // The encoder has every picture: end its video now, before any soundtrack
  // tail is read, so it never waits on video while the tail is demuxed.
  if (streaming) {
    need(av_write_trailer(nut_output) >= 0, "NUT trailer");
    need(avio_closep(&nut_output->pb) >= 0, "NUT output close");
    avformat_free_context(nut_output);
    nut_output = NULL;
    // The NUT was written through /proc/self/fd; the received end is ours to
    // close, which is the encoder's end of video.
    if (request->nut_fd >= 0)
      need(close(request->nut_fd) == 0, "NUT pipe close");
  }
  // The soundtrack's tail past the window; video read meanwhile is the next
  // window's.
  int tail = 0;
  while (session_mode && window_done && audio_wanted() &&
         (tail = pump(packets, video_packets)) >= 0) {
  }
  need(tail >= 0 || tail == AVERROR_EOF, "source read");
  if (tail == AVERROR_EOF)
    audio_source_ended = true;
  if (!session_mode)
    need(source_reader_stop(&movie_reader) == 0, "stop and join owned source reader");
  need(!movie_io.exhausted, "window source read budget exhausted");
  release_held();
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
      int64_t endpoint;
      need(declared_video_endpoint(source_origin_ticks, stream->duration, &endpoint) &&
               last_emitted_end + 1 >= endpoint && last_emitted_end - 1 <= endpoint,
           "source EOF disagrees with declared video extent");
    } else if (av_dict_get(stream->metadata, "DURATION", NULL, 0)) {
      const AVDictionaryEntry *declared = av_dict_get(stream->metadata, "DURATION", NULL, 0);
      int64_t endpoint_ns;
      need(video_duration_ns(declared->value, &endpoint_ns),
           "invalid declared video DURATION extent");
      need(av_compare_ts(last_emitted_end + 1, source_time_base,
                         endpoint_ns, (AVRational){1, 1000000000}) >= 0 &&
               av_compare_ts(last_emitted_end - 1, source_time_base,
                             endpoint_ns, (AVRational){1, 1000000000}) <= 0,
           "source EOF disagrees with declared video DURATION extent");
      // A known container maximum may contradict the selected video, but
      // can never supply its missing declaration (other tracks may outlast it).
      need(input->duration == AV_NOPTS_VALUE || input->duration <= 0 ||
               av_compare_ts(last_emitted_end - 1, source_time_base,
                             input->duration, (AVRational){1, AV_TIME_BASE}) <= 0,
           "source EOF extent unavailable or inconsistent");
    } else {
      need(input->nb_streams == 1 &&
               input->duration != AV_NOPTS_VALUE && input->duration > 0 &&
               av_compare_ts(last_emitted_end + 1, source_time_base,
                             input->duration,
                             (AVRational){1, AV_TIME_BASE}) >= 0 &&
               av_compare_ts(last_emitted_end - 1, source_time_base,
                             input->duration,
                             (AVRational){1, AV_TIME_BASE}) <= 0,
           "source EOF extent unavailable or inconsistent");
    }
  }
  if (window_audio)
    close_audio_output();
  need((!rgb_output || fclose(rgb_output) == 0) && fclose(timing_output) == 0,
       "close completed outputs");
  rgb_output = timing_output = NULL;
  if (window_mode) {
    printf("{\"kind\":\"window_complete\",\"requested_start\":\"%lld/"
           "%d\",\"requested_end\":\"%lld/%d\",\"source_origin\":\"%lld/"
           "%d\",\"origin_provenance\":\"%s\","
           "\"frames\":%d,\"preroll_pairs\":%u,\"paired_pictures\":%u,"
           "\"boundary_pts_ticks\":%lld,\"boundary_observed\":%s,\"source_eof_"
           "observed\":%s,\"first_emitted_pts_ticks\":%lld,\"last_emitted_end_"
           "ticks\":%lld,\"source_time_base\":\"%d/"
           "%d\",\"source_bytes_read\":%llu,\"duration_provenance\":\"decoder_"
           "observed\",\"membership\":\"frame_pts_in_half_open_window_"
           "durations_unclipped\",\"production_qualified\":false}\n",
           (long long)window_start.num, window_start.den,
           (long long)window_end.num, window_end.den,
           (long long)source_origin.num, source_origin.den, source_origin_provenance,
           emitted, preroll, paired,
           (long long)boundary_pts, window_done ? "true" : "false",
           source_eof ? "true" : "false", (long long)first_emitted_pts,
           (long long)last_emitted_end, source_time_base.num,
           source_time_base.den, (unsigned long long)(movie_io.bytes - bytes_at_start));
  }
  printf("{\"kind\":\"decoder\",\"mode\":\"%s\",\"reason\":\"%s\"}\n",
         decoder_mode(), decoder_reason());
  printf("{\"kind\":\"segment_complete\",\"frames\":%d,\"pair_window_bound\":"
         "16,\"gpu_contexts\":1,\"output_policy\":\"%s\","
         "\"production_qualified\":false}\n",
         emitted, dv_target_policy_name());
  if (base_only)
    printf("{\"kind\":\"base_processing_complete\",\"profile\":%d,"
           "\"frames\":%d,\"decoded_layers\":1,\"el_bound\":false,"
           "\"fel_contributed\":false,"
           "\"production_qualified\":false}\n", source_profile, emitted);
  need(fflush(stdout) == 0, "observations flush");
  stage_mark("complete");
  if (stage_output)
    fclose(stage_output);
  stage_output = NULL;
  if (session_mode) {
    if (window_audio)
      audio_trim();
    positioned = true;
    previous_end = window_end;
  }
}
static void close_source(void) {
  if (initialized) {
    if (base_only)
      base_gpu_close();
    else
      gpu_close();
  }
  for (int k = 0; decoders_open && k < (base_only ? 1 : 2); k++) {
    for (int i = 0; i < layers[k].count; i++)
      av_frame_free(&layers[k].queue[i]);
    av_bsf_free(&layers[k].split);
    avcodec_free_context(&layers[k].decoder);
  }
  av_buffer_unref(&hw_device);
  av_packet_free(&packet);
  av_packet_free(&lookahead);
  for (unsigned i = 0; i < carried_coded_count; i++)
    av_packet_free(&carried_coded[i]);
  for (unsigned i = 0; i < pending_video_count; i++)
    av_packet_free(&pending_video[i]);
  for (size_t i = 0; i < audio_ring_count; i++)
    av_packet_free(&audio_ring[i]);
  free(audio_ring);
  free(gpu_lines);
  // The reader thread reads the source; it is joined before the source closes.
  need(source_reader_stop(&movie_reader) == 0, "stop and join owned source reader");
  avformat_close_input(&input);
  if (movie_avio) {
    av_freep(&movie_avio->buffer);
    avio_context_free(&movie_avio);
    movie_source_close(&movie_io);
  }
  source_reader_destroy(&movie_reader);
  if (movie_interrupt_initialized) movie_source_interrupt_destroy(&movie_interrupt);
}
// One window command: "window START END FRAMES MAX_PREROLL [AUDIO_FROM
// AUDIO_END]", with the window's NUT pipe, its private evidence directory
// and, when it carries the soundtrack, its audio pipe as SCM_RIGHTS. Zero
// means the daemon ended the session.
static int receive_window(struct window_request *request, int *directory) {
  char text[256];
  struct iovec iov = {.iov_base = text, .iov_len = sizeof(text) - 1};
  union {
    char buffer[CMSG_SPACE(3 * sizeof(int))];
    struct cmsghdr align;
  } control;
  struct msghdr message = {.msg_iov = &iov, .msg_iovlen = 1,
                           .msg_control = control.buffer,
                           .msg_controllen = sizeof(control.buffer)};
  ssize_t length;
  do
    length = recvmsg(control_fd, &message, 0);
  while (length < 0 && errno == EINTR);
  if (length == 0)
    return 0;
  need(length > 0 && !(message.msg_flags & (MSG_TRUNC | MSG_CTRUNC)),
       "bounded session window command");
  text[length] = 0;
  int fds[3], count = 0;
  for (struct cmsghdr *header = CMSG_FIRSTHDR(&message); header;
       header = CMSG_NXTHDR(&message, header)) {
    need(header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_RIGHTS &&
             header->cmsg_len >= CMSG_LEN(0),
         "session window descriptors");
    size_t received = (header->cmsg_len - CMSG_LEN(0)) / sizeof(int);
    need(count + received <= 3, "bounded session window descriptors");
    memcpy(fds + count, CMSG_DATA(header), received * sizeof(int));
    count += (int)received;
  }
  char *fields[8], *cursor = NULL;
  int n = 0;
  for (char *field = strtok_r(text, " \n", &cursor); field && n < 8;
       field = strtok_r(NULL, " \n", &cursor))
    fields[n++] = field;
  need((n == 5 || n == 7) && !strcmp(fields[0], "window"), "session window command");
  *request = (struct window_request){
      .start = rational(fields[1]), .end = rational(fields[2]),
      .frames = number(fields[3], 1, frame_cap),
      .max_preroll = (unsigned)number(fields[4], 0, (int)preroll_cap),
      .audio = n == 7, .nut_fd = -1, .audio_fd = -1};
  need(request->audio == (audio_index >= 0), "session soundtrack membership");
  if (request->audio) {
    request->audio_from = rational(fields[5]);
    request->audio_end = rational(fields[6]);
    need(av_compare_ts(request->audio_from.num, (AVRational){1, request->audio_from.den},
                       request->audio_end.num, (AVRational){1, request->audio_end.den}) < 0,
         "ordered soundtrack interval");
  }
  need(count == (request->audio ? 3 : 2), "session window descriptor count");
  check_window_bounds(request->start, request->end);
  request->nut_fd = fds[0];
  *directory = fds[1];
  if (request->audio)
    request->audio_fd = fds[2];
  return 1;
}
static void session_loop(void) {
  unsigned window_cap = (unsigned)frame_cap, preroll_bound = preroll_cap;
  for (;;) {
    // Each window's caps are its own, within the session's argv bounds.
    frame_cap = (int)window_cap;
    preroll_cap = preroll_bound;
    struct window_request request;
    if (!receive_window(&request, &window_dir))
      return;
    if (positioned && clock_gettime(CLOCK_MONOTONIC, &helper_started) != 0)
      helper_started = (struct timespec){0};
    need(mkdirat(window_dir, "rpus", 0700) == 0, "fresh accepted RPU directory");
    rpu_dir = openat(window_dir, "rpus", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    need(rpu_dir >= 0, "accepted RPU directory");
    timing_output = evidence_file(window_dir, "timing.tsv", O_WRONLY, "w");
    need(timing_output != NULL, "exclusive private output files");
    stage_output = evidence_file(window_dir, "stages.tsv", O_WRONLY, "w");
    if (stage_output && !session_stage_written &&
        fwrite(session_stage_rows, 1, session_stage_length, stage_output) != session_stage_length) {
      fclose(stage_output);
      stage_output = NULL;
    }
    session_stage_written = true;
    if (positioned)
      stage_mark("started");
    // This window's observations go to its own evidence file.
    int events = openat(window_dir, "renderer.jsonl",
                        O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
    need(events >= 0 && fflush(stdout) == 0 && dup2(events, STDOUT_FILENO) == STDOUT_FILENO &&
             close(events) == 0,
         "window observation output");
    run_window(&request);
    need(fflush(stdout) == 0 && dup2(devnull_fd, STDOUT_FILENO) == STDOUT_FILENO,
         "window observation close");
    need(close(rpu_dir) == 0 && close(window_dir) == 0, "window evidence close");
    rpu_dir = window_dir = -1;
    static const char done[] = "done";
    need(send(control_fd, done, sizeof(done) - 1, 0) == (ssize_t)(sizeof(done) - 1),
         "session window acknowledgement");
  }
}
int main(int argc, char **argv) {
  if (argc >= 2 && (!strcmp(argv[1], "--refit-stats") || !strcmp(argv[1], "--dump-composed")))
    return refit_main(argc, argv);
  if (clock_gettime(CLOCK_MONOTONIC, &helper_started) != 0)
    helper_started = (struct timespec){0};
  bool held_origin = false;
  struct rational supplied_origin = {0, 1};
  // Trailing options, each a name and one value, in any order.
  while (argc >= 3) {
    const char *name = argv[argc - 2], *value = argv[argc - 1];
    if (!strcmp(name, "--held-source-origin") && !held_origin) {
      supplied_origin = rational(value);
      held_origin = true;
    } else if (!strcmp(name, "--session") && !session_mode) {
      control_fd = number(value, 3, 1023);
      session_mode = true;
    } else if (!strcmp(name, "--audio-ordinal") && audio_ordinal < 0) {
      audio_ordinal = number(value, 0, 255);
    } else if (!strcmp(name, "--audio-channels") && audio_channels == 0) {
      audio_channels = number(value, 1, 64);
    } else {
      break;
    }
    argc -= 2;
  }
  need(argc == 11 || argc == 12 || argc == 15 ||
           (argc == 16 && !strcmp(argv[15], "base-rpu")) ||
           ((argc == 19 || argc == 21) && (!strcmp(argv[15], "base-rpu") ||
                           !strcmp(argv[15], "fel"))),
       "usage: segment_decode_render SOURCE OUTPUT_DIR VIDEO_INDEX "
       "MAX_FRAMES BL_W BL_H EL_W EL_H DEBUG OUTPUT_POLICY [NUT_OUTPUT [START "
       "END MAX_PREROLL [base-rpu|fel [EXPORT_W EXPORT_H EXPORT_POLICY "
       "[TITLE_MIN_1E4 TITLE_PEAK]]]]] [--held-source-origin N/D] "
       "[--session CONTROL_FD [--audio-ordinal K [--audio-channels N]]]; a session passes '-' for "
       "OUTPUT_DIR, NUT_OUTPUT, START and END");
  streaming = argc >= 12;
  base_only = argc >= 16 && !strcmp(argv[15], "base-rpu");
  window_mode = argc >= 15;
  need(!held_origin || window_mode, "held source origin requires a bounded window");
  need(!session_mode ||
           (window_mode && !strcmp(argv[2], "-") && !strcmp(argv[11], "-") &&
            !strcmp(argv[12], "-") && !strcmp(argv[13], "-")),
       "a session takes its windows and outputs from its control socket");
  need(audio_ordinal < 0 || session_mode, "a carried soundtrack requires a session");
  need(audio_channels == 0 || audio_ordinal >= 0, "a channel count requires a soundtrack");
  struct window_request single = {.nut_fd = -1, .audio_fd = -1};
  if (window_mode) {
    preroll_cap = (unsigned)number(argv[14], 0, 512);
    if (!session_mode) {
      single.start = rational(argv[12]);
      single.end = rational(argv[13]);
      check_window_bounds(single.start, single.end);
    }
    av_max_alloc(64ULL * 1024 * 1024);
  }
  stream_index = number(argv[3], 0, 255);
  frame_cap = number(argv[4], 1, 64);
  expected_w[0] = number(argv[5], 64, 4096);
  expected_h[0] = number(argv[6], 64, 2160);
  expected_w[1] = number(argv[7], 0, 4096);
  expected_h[1] = number(argv[8], 0, 2160);
  EXPORT_WIDTH = expected_w[0];
  EXPORT_HEIGHT = expected_h[0];
  if (argc >= 19) {
    EXPORT_WIDTH = number(argv[16], 32, 4096);
    EXPORT_HEIGHT = number(argv[17], 32, 2160);
    EXPORT_YUV10 = !strcmp(argv[18], "pq-bilinear-yuv420p10-v1");
    need((EXPORT_YUV10 || !strcmp(argv[18], "pq-bilinear-rgb48-v1")) &&
             EXPORT_WIDTH <= expected_w[0] && EXPORT_HEIGHT <= expected_h[0] &&
             EXPORT_WIDTH % 2 == 0 && EXPORT_HEIGHT % 2 == 0,
         "unsupported export geometry or policy");
  }
  // The software Vulkan renderer has one bounded worker, accounted alongside
  // each layer's four decoder threads. Never inherit the machine's core count.
  need(setenv("LP_NUM_THREADS", "1", 1) == 0,
       "bounded software Vulkan worker configuration");
  discover_el = expected_w[1] == 0 && expected_h[1] == 0;
  need((expected_w[1] == 0) == (expected_h[1] == 0),
       "enhancement discovery requires both dimensions zero");
  need(!discover_el || window_mode, "enhancement discovery requires window mode");
  need(!base_only || (expected_w[1] == 0 && expected_h[1] == 0),
       "base-only mode has no declared EL");
  debug_frames = number(argv[9], 0, 1) != 0;
  need(!debug_frames || !session_mode, "debug frame dumps are single-shot only");
  // Explicit control API only. Session/caps selection is a later PR.
  need(argc == 21 ? dv_target_policy(argv[10], argv[19], argv[20]) :
                       strcmp(argv[10], "bt2020-pq-master-clip") == 0,
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
  if (session_mode) {
    // A closed pipe is an error result, never a signal: the encoder ends
    // its soundtrack input once its output bound is reached.
    struct sigaction ignore = {.sa_handler = SIG_IGN};
    need(sigemptyset(&ignore.sa_mask) == 0 && sigaction(SIGPIPE, &ignore, NULL) == 0,
         "session pipe signal disposition");
    devnull_fd = open("/dev/null", O_WRONLY | O_CLOEXEC);
    need(devnull_fd >= 0 && fflush(stdout) == 0 &&
             dup2(devnull_fd, STDOUT_FILENO) == STDOUT_FILENO,
         "session observation sink");
  } else {
    struct stat st;
    need(stat(argv[2], &st) == 0 && S_ISDIR(st.st_mode),
         "private output directory");
    window_dir = open(argv[2], O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    need(window_dir >= 0, "output directory enter");
    // Debug dumps keep their legacy relative paths; production never sets it.
    if (debug_frames)
      need(fchdir(window_dir) == 0, "output directory enter");
    need(mkdirat(window_dir, "rpus", 0700) == 0, "fresh accepted RPU directory");
    rpu_dir = openat(window_dir, "rpus", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    need(rpu_dir >= 0, "fresh accepted RPU directory");
    if (debug_frames)
      need(mkdirat(window_dir, "outputs", 0700) == 0, "fresh debug output directory");
    if (!streaming)
      rgb_output = evidence_file(window_dir, "reconstructed.rgb48le", O_WRONLY, "wb");
    timing_output = evidence_file(window_dir, "timing.tsv", O_WRONLY, "w");
    need((streaming || rgb_output) && timing_output,
         "exclusive private output files");
    stage_output = evidence_file(window_dir, "stages.tsv", O_WRONLY, "w");
  }
  stage_mark("started");
  input = avformat_alloc_context();
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
  stage_mark("opened");
  need(input->nb_streams > 0 && input->nb_streams <= 256,
       "bounded stream inventory");
  // This helper consumes only the selected coded AU. Keep the original
  // inventory and soundtrack metadata, but avoid returning unrelated packets.
  // Some demuxers discover their streams while probing, so an unknown early
  // index/codec retains bounded discovery rather than acquiring a new refusal.
  if ((unsigned)stream_index < input->nb_streams &&
      input->streams[stream_index]->codecpar->codec_type == AVMEDIA_TYPE_VIDEO &&
      input->streams[stream_index]->codecpar->codec_id == AV_CODEC_ID_HEVC) {
    for (unsigned i = 0; i < input->nb_streams; i++)
      if (i != (unsigned)stream_index)
        input->streams[i]->discard = AVDISCARD_ALL;
  }
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
  stage_mark("probed");
  need((unsigned)stream_index < input->nb_streams &&
           input->streams[stream_index]->codecpar->codec_type ==
               AVMEDIA_TYPE_VIDEO &&
           input->streams[stream_index]->codecpar->codec_id == AV_CODEC_ID_HEVC,
       "selected absolute HEVC video stream");
  // The soundtrack is the encoder's ordinal: the K-th audio stream, in the
  // container's order, exactly as `-map 0:a:K` selects it.
  if (audio_ordinal >= 0) {
    for (unsigned i = 0, seen = 0; i < input->nb_streams && audio_index < 0; i++)
      if (input->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO &&
          seen++ == (unsigned)audio_ordinal)
        audio_index = (int)i;
    need(audio_index >= 0, "selected soundtrack stream");
    audio_stream = input->streams[audio_index];
    need(audio_stream->time_base.num > 0 && audio_stream->time_base.den > 0,
         "positive soundtrack time base");
    audio_ring = calloc(AUDIO_RING_LIMIT, sizeof(*audio_ring));
    need(audio_ring != NULL, "carried soundtrack ring");
  }
  for (unsigned i = 0; i < input->nb_streams; i++)
    input->streams[i]->discard =
        i == (unsigned)stream_index || (int)i == audio_index ? AVDISCARD_DEFAULT
                                                             : AVDISCARD_ALL;
  stream = input->streams[stream_index];
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
    need(bind_source_origin(held_origin, supplied_origin, stream,
                            &source_origin, &source_origin_ticks),
         "window requires an exact held source origin or observed legacy zero");
    source_origin_provenance = held_origin
        ? "observed_held_format_and_selected_video_probe"
        : "observed_stream_start_time";
  }
  dv_host_pack = pack;
  packet = av_packet_alloc();
  lookahead = av_packet_alloc();
  need(packet != NULL && lookahead != NULL, "demux packet");
  if (session_mode) {
    session_loop();
  } else {
    single.frames = frame_cap;
    single.max_preroll = preroll_cap;
    single.nut_path = streaming ? argv[11] : NULL;
    run_window(&single);
  }
  close_source();
  return 0;
}
