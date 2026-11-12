// SPDX-License-Identifier: Apache-2.0
// F+ statistics modes of segment_decode_render (FEL-RPU-REFIT-BUILD.md §3.2):
//
//   segment_decode_render --refit-stats SOURCE VIDEO_INDEX [--frames FIRST LAST]
//   segment_decode_render --dump-composed SOURCE VIDEO_INDEX FIRST LAST
//
// Either takes a trailing diagnostic [--area LEFT RIGHT TOP BOTTOM], which
// replaces every frame's L5 active area with these offsets, to reproduce an
// offline region of interest. Production callers do not pass it.
//
// Both decode a whole Profile 7 FEL title (or up to LAST) once, on the CPU,
// through the helper's own demux, dovi_split layer split, decoders, BL/EL
// pairing and RPU admission (map_fel_rpu), and compose stride-8 lattice
// pixels from each frame's own RPU (refit_compose.h). No GPU, Vulkan device
// or VA-API device is opened. FIRST and LAST are inclusive presentation-order
// frame ordinals from the start of the title.
//
// --refit-stats writes JSON lines to stdout: one per pooled scene,
//   {"kind":"refit_scene","first_frame":N,"frames":K,"first_pts":TICKS,
//    "time_base":"NUM/DEN","fit":RefitStats,"held_out":RefitStats}
// in presentation order, then one
//   {"kind":"refit_summary","frames":N,"scenes":S,"rpu_scenes":R,
//    "median_scene_frames":M,"degenerate":BOOL,"fit_samples":F,
//    "held_out_samples":H}
// where M is the median length of the RPU's own scenes before pooling and
// degenerate is M < 24 (scene_refresh_flag too frequent to mean scene cuts).
// RefitStats is plurx-core's dv_refit::RefitStats serde layout. Exit 0 only
// after a complete read; any refusal is exit 1 with one line on stderr, and
// the caller discards everything written.
//
// --dump-composed writes the lattice samples of frames FIRST..LAST
// ("plurx-composed-v1"): a JSON header line, then per frame one JSON line
//   {"kind":"composed_frame","frame":N,"pts":TICKS,"bl":[W,H],"el":[W,H],
//    "active_area":[LEFT,RIGHT,TOP,BOTTOM],"scene_refresh":BOOL,
//    "fit_samples":KF,"held_out_samples":KH}
// followed immediately by (KF + KH) binary records of eight little-endian
// uint16: x, y, base Y, Cb, Cr, composed Y, Cb, Cr. Fit-lattice records come
// first, each lattice row-major. A final {"kind":"dump_complete","frames":K}
// line ends a complete dump. compare_composed.py reads it.
//
// The playback window path is unchanged: these modes are dispatched before
// its argument parsing and share only its decode functions.

#include "refit_compose.h"

#define REFIT_MAX_TITLE_FRAMES 100000000
#define REFIT_MAX_PACKET_BYTES (16 * 1024 * 1024)

static bool refit_dump, refit_area_override;
static int refit_area[4];
static int64_t refit_first, refit_last = INT64_MAX, refit_paired, refit_composed;
static struct refit_scene *refit_raw;
static struct refit_pool *refit_pooled;
static struct refit_stats *refit_frame_stats[2];
static int64_t *refit_lengths;
static size_t refit_lengths_count, refit_lengths_capacity;
static double refit_samples[2];
static uint8_t *refit_records;
static size_t refit_records_capacity;
static struct budget_io refit_io = {.fd = -1, .upstream = -1};
static AVIOContext *refit_avio;

static bool refit_emit(const struct refit_scene *scene, void *unused) {
  (void)unused;
  return printf("{\"kind\":\"refit_scene\",\"first_frame\":%lld,\"frames\":%lld,"
                "\"first_pts\":%lld,\"time_base\":\"%d/%d\",\"fit\":",
                (long long)scene->first_frame, (long long)scene->frames,
                (long long)scene->first_pts, source_time_base.num,
                source_time_base.den) > 0 &&
         refit_stats_json(stdout, &scene->fit) && fputs(",\"held_out\":", stdout) != EOF &&
         refit_stats_json(stdout, &scene->held_out) && fputs("}\n", stdout) != EOF;
}

// An RPU scene ends: remember its length for the summary, then pool it.
static void refit_close_raw(void) {
  if (!refit_raw->frames)
    return;
  if (refit_lengths_count == refit_lengths_capacity) {
    size_t capacity = refit_lengths_capacity ? 2 * refit_lengths_capacity : 1024;
    int64_t *grown = realloc(refit_lengths, capacity * sizeof(*grown));
    need(grown != NULL, "scene length table allocation");
    refit_lengths = grown;
    refit_lengths_capacity = capacity;
  }
  refit_lengths[refit_lengths_count++] = refit_raw->frames;
  need(refit_pool_close(refit_pooled, refit_raw, refit_emit, NULL), "scene statistics write");
  memset(refit_raw, 0, sizeof(*refit_raw));
}

static inline uint16_t refit_code(const AVFrame *frame, int plane, int x, int y) {
  uint16_t v = ((const uint16_t *)(frame->data[plane] + (size_t)y * frame->linesize[plane]))[x];
  need(v <= 1023, "decoded ten-bit code");
  return v;
}

static void refit_frame(const AVFrame *bl, const AVFrame *el, int64_t ordinal) {
  AVFrameSideData *raw = av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_RPU_BUFFER);
  need(raw && raw->size > 0 && raw->size < 4096,
       "fresh decoder-attached RPU required; cached metadata insufficient");
  uint8_t nalu[4098] = {0x7c, 0x01};
  memcpy(nalu + 2, raw->data, raw->size);
  // The playback path's admission, refusals included; it prints nothing here.
  struct pl_dovi_metadata admitted;
  map_fel_rpu(bl, nalu, raw->size + 2, &admitted, false, bl->width, bl->height);
  const AVFrameSideData *side = av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_METADATA);
  need(side != NULL, "decoder-derived FEL metadata");
  const AVDOVIMetadata *data = (const void *)side->data;
  struct refit_mapping mapping;
  const char *refused = refit_mapping_from_dovi(data, &mapping);
  need(refused == NULL, refused ? refused : "");
  bool refresh = av_dovi_get_color(data)->scene_refresh_flag != 0;

  int w = bl->width, h = bl->height, ew = el->width, eh = el->height;
  need(bl->format == AV_PIX_FMT_YUV420P10LE && el->format == AV_PIX_FMT_YUV420P10LE &&
           bl->linesize[0] >= 2 * w && bl->linesize[1] >= w && bl->linesize[2] >= w &&
           el->linesize[0] >= 2 * ew && el->linesize[1] >= ew && el->linesize[2] >= ew,
       "software-decoded planar ten-bit layers");
  int area[4] = {0, 0, 0, 0}; // left, right, top, bottom
  const AVDOVIDmData *l5 = av_dovi_find_level(data, 5);
  if (refit_area_override) {
    memcpy(area, refit_area, sizeof(area));
  } else if (l5) {
    area[0] = l5->l5.left_offset;
    area[1] = l5->l5.right_offset;
    area[2] = l5->l5.top_offset;
    area[3] = l5->l5.bottom_offset;
  }
  need(area[0] + area[1] < w && area[2] + area[3] < h, "invalid active area metadata");
  int x0 = area[0], x1 = w - area[1], y0 = area[2], y1 = h - area[3];
  const int phases[2] = {REFIT_FIT_PHASE, REFIT_HELD_OUT_PHASE};
  int64_t counts[2];
  for (int k = 0; k < 2; k++)
    counts[k] = (int64_t)refit_axis_count(x0, x1, phases[k]) * refit_axis_count(y0, y1, phases[k]);

  uint8_t *record = NULL;
  if (refit_dump) {
    size_t bytes = (size_t)(counts[0] + counts[1]) * 16;
    if (bytes > refit_records_capacity) {
      uint8_t *grown = realloc(refit_records, bytes);
      need(grown != NULL, "dump record buffer");
      refit_records = grown;
      refit_records_capacity = bytes;
    }
    record = refit_records;
  } else {
    for (int k = 0; k < 2; k++)
      memset(refit_frame_stats[k], 0, sizeof(*refit_frame_stats[k]));
  }
  for (int k = 0; k < 2; k++) {
    int64_t taken = 0;
    for (int y = y0 + phases[k]; y < y1; y += REFIT_STRIDE) {
      for (int x = x0 + phases[k]; x < x1; x += REFIT_STRIDE) {
        struct refit_sites s = refit_pair(x, y, w, h, ew, eh);
        const uint16_t base[3] = {
            refit_code(bl, 0, x, y), refit_code(bl, 1, s.bl_chroma[0], s.bl_chroma[1]),
            refit_code(bl, 2, s.bl_chroma[0], s.bl_chroma[1])};
        const uint16_t enhancement[3] = {
            refit_code(el, 0, s.el_luma[0], s.el_luma[1]),
            refit_code(el, 1, s.el_chroma[0], s.el_chroma[1]),
            refit_code(el, 2, s.el_chroma[0], s.el_chroma[1])};
        uint16_t composed[3];
        refit_compose(&mapping, base, enhancement, composed);
        taken++;
        if (refit_dump) {
          const uint16_t fields[8] = {(uint16_t)x, (uint16_t)y, base[0], base[1], base[2],
                                      composed[0], composed[1], composed[2]};
          for (int f = 0; f < 8; f++) {
            *record++ = (uint8_t)fields[f];
            *record++ = (uint8_t)(fields[f] >> 8);
          }
        } else {
          refit_stats_add(refit_frame_stats[k], base, composed);
        }
      }
    }
    need(taken == counts[k], "lattice sample count");
    refit_samples[k] += (double)taken;
  }

  if (refit_dump) {
    size_t bytes = (size_t)(record - refit_records);
    need(printf("{\"kind\":\"composed_frame\",\"frame\":%lld,\"pts\":%lld,\"bl\":[%d,%d],"
                "\"el\":[%d,%d],\"active_area\":[%d,%d,%d,%d],\"scene_refresh\":%s,"
                "\"fit_samples\":%lld,\"held_out_samples\":%lld}\n",
                (long long)ordinal, (long long)bl->pts, w, h, ew, eh, area[0], area[1],
                area[2], area[3], refresh ? "true" : "false", (long long)counts[0],
                (long long)counts[1]) > 0 &&
             fwrite(refit_records, 1, bytes, stdout) == bytes,
         "composed dump write");
  } else {
    // A scene starts at FIRST and at every scene_refresh_flag after it.
    if (refit_raw->frames && refresh)
      refit_close_raw();
    if (!refit_raw->frames) {
      refit_raw->first_frame = ordinal;
      refit_raw->first_pts = bl->pts;
    }
    for (int k = 0; k < 2; k++)
      refit_stats_finish_frame(refit_frame_stats[k]);
    refit_stats_merge(&refit_raw->fit, refit_frame_stats[0]);
    refit_stats_merge(&refit_raw->held_out, refit_frame_stats[1]);
    refit_raw->frames++;
  }
  refit_composed++;
}

// receive()'s pair consumer in these modes, in place of render_pairs.
static void refit_pairs(void) {
  while (layers[0].count && layers[1].count) {
    AVFrame *bl = layers[0].queue[0], *el = layers[1].queue[0];
    need(bl->pts == el->pts && bl->duration == el->duration && bl->duration > 0,
         "unmatched or ambiguous BL/EL PTS/duration");
    need(last_pts == AV_NOPTS_VALUE || bl->pts > last_pts,
         "duplicate/out-of-order paired frame");
    last_pts = bl->pts;
    int64_t ordinal = refit_paired++;
    need(refit_paired <= REFIT_MAX_TITLE_FRAMES, "bounded title frame count");
    if (ordinal > refit_last) {
      window_done = true;
      drop_pair();
      return;
    }
    if (ordinal >= refit_first)
      refit_frame(bl, el, ordinal);
    drop_pair();
  }
}

static int refit_main(int argc, char **argv) {
  refit_dump = !strcmp(argv[1], "--dump-composed");
  refit_area_override = argc >= 5 && !strcmp(argv[argc - 5], "--area");
  if (refit_area_override) {
    for (int i = 0; i < 4; i++)
      refit_area[i] = number(argv[argc - 4 + i], 0, 4095);
    argc -= 5;
  }
  if (refit_dump)
    need(argc == 6, "usage: segment_decode_render --dump-composed SOURCE VIDEO_INDEX "
                    "FIRST LAST [--area LEFT RIGHT TOP BOTTOM]");
  else
    need(argc == 4 || (argc == 7 && !strcmp(argv[4], "--frames")),
         "usage: segment_decode_render --refit-stats SOURCE VIDEO_INDEX "
         "[--frames FIRST LAST] [--area LEFT RIGHT TOP BOTTOM]");
  refit_mode = true;
  // Refusals leave without exit-time teardown while decoder threads run.
  dv_threads_live = true;
  int stream_index = number(argv[3], 0, 255);
  if (argc >= 6) {
    refit_first = number(argv[argc - 2], 0, REFIT_MAX_TITLE_FRAMES - 1);
    refit_last = number(argv[argc - 1], 0, REFIT_MAX_TITLE_FRAMES - 1);
    need(refit_first <= refit_last, "ordered frame range");
  }
  av_max_alloc(64ULL * 1024 * 1024);
  static char stdout_buffer[1 << 20];
  setvbuf(stdout, stdout_buffer, _IOFBF, sizeof(stdout_buffer));

  AVFormatContext *input = avformat_alloc_context();
  need(input != NULL, "input context");
  input->probesize = 1024 * 1024;
  input->max_analyze_duration = AV_TIME_BASE;
  input->max_probe_packets = 32;
  input->max_streams = 256;
  // The whole title is read: lift the playback window's read budget for this
  // source only, as scan_title does.
  need(movie_source_open(&refit_io, argv[2]) >= 0,
       "held regular title source or private source capability");
  refit_io.read_limit = UINT64_MAX;
  uint8_t *buffer = av_malloc(65536);
  need(buffer != NULL, "bounded title IO buffer");
  refit_avio = avio_alloc_context(buffer, 65536, 0, &refit_io, movie_read, NULL, movie_seek);
  if (!refit_avio)
    av_free(buffer);
  need(refit_avio != NULL, "title IO context");
  input->pb = refit_avio;
  input->flags |= AVFMT_FLAG_CUSTOM_IO;
  need(movie_source_bind_demuxer(input) >= 0, "closed title demux protocol authority");
  need(avformat_open_input(&input, argv[2], NULL, NULL) >= 0, "title open");
  need(input->nb_streams > 0 && input->nb_streams <= 256, "bounded stream inventory");
  AVDictionary *probe_options[256] = {0};
  for (unsigned i = 0; i < input->nb_streams; i++)
    need(av_dict_set(&probe_options[i], "threads", "1", 0) >= 0 &&
             av_dict_set(&probe_options[i], "max_pixels", "8847360", 0) >= 0,
         "bounded probe decoder options");
  int probed = avformat_find_stream_info(input, probe_options);
  for (unsigned i = 0; i < input->nb_streams; i++)
    av_dict_free(&probe_options[i]);
  need(probed >= 0, "bounded stream discovery");
  need((unsigned)stream_index < input->nb_streams &&
           input->streams[stream_index]->codecpar->codec_type == AVMEDIA_TYPE_VIDEO &&
           input->streams[stream_index]->codecpar->codec_id == AV_CODEC_ID_HEVC,
       "selected absolute HEVC video stream");
  for (unsigned i = 0; i < input->nb_streams; i++)
    if (i != (unsigned)stream_index)
      input->streams[i]->discard = AVDISCARD_ALL;
  AVStream *stream = input->streams[stream_index];
  source_time_base = stream->time_base;
  need(source_time_base.num > 0 && source_time_base.den > 0, "positive source time base");
  const AVPacketSideData *config = av_packet_side_data_get(
      stream->codecpar->coded_side_data, stream->codecpar->nb_coded_side_data,
      AV_PKT_DATA_DOVI_CONF);
  need(config && config->size >= sizeof(AVDOVIDecoderConfigurationRecord),
       "declared source Dolby Vision configuration");
  const AVDOVIDecoderConfigurationRecord *cfg = (const void *)config->data;
  need(cfg->dv_profile == 7 && cfg->bl_present_flag && cfg->el_present_flag &&
           cfg->rpu_present_flag,
       "Profile 7 dual-layer source");
  expected_w[0] = stream->codecpar->width;
  expected_h[0] = stream->codecpar->height;
  need(expected_w[0] >= 64 && expected_w[0] <= 4096 && expected_h[0] >= 64 &&
           expected_h[0] <= 2160 && expected_w[0] % 2 == 0 && expected_h[0] % 2 == 0,
       "supported even raster bound");
  expected_w[1] = expected_h[1] = 0;
  discover_el = true;

  refit_raw = calloc(1, sizeof(*refit_raw));
  refit_pooled = calloc(1, sizeof(*refit_pooled));
  refit_frame_stats[0] = calloc(1, sizeof(*refit_frame_stats[0]));
  refit_frame_stats[1] = calloc(1, sizeof(*refit_frame_stats[1]));
  need(refit_raw && refit_pooled && refit_frame_stats[0] && refit_frame_stats[1],
       "scene statistics allocation");
  if (refit_dump)
    need(printf("{\"kind\":\"composed_dump\",\"format\":\"plurx-composed-v1\","
                "\"time_base\":\"%d/%d\",\"first\":%lld,\"last\":%lld,\"area\":\"%s\","
                "\"record_bytes\":16,"
                "\"record\":[\"x\",\"y\",\"base_y\",\"base_cb\",\"base_cr\","
                "\"composed_y\",\"composed_cb\",\"composed_cr\"],"
                "\"encoding\":\"uint16-le\"}\n",
                source_time_base.num, source_time_base.den, (long long)refit_first,
                (long long)refit_last, refit_area_override ? "override" : "l5") > 0,
         "composed dump header");

  layers[0] = open_layer(stream, 0);
  layers[1] = open_layer(stream, 1);
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "demux packet");
  int result = 0;
  while (!window_done && (result = av_read_frame(input, packet)) >= 0) {
    need(packet->size <= REFIT_MAX_PACKET_BYTES, "bounded packet size");
    if (packet->stream_index == stream_index) {
      send_layer(&layers[0], packet);
      if (!window_done)
        send_layer(&layers[1], packet);
    }
    av_packet_unref(packet);
  }
  if (!window_done) {
    need(result == AVERROR_EOF, "complete title read");
    for (int k = 0; k < 2 && !window_done; k++) {
      need(avcodec_send_packet(layers[k].decoder, NULL) >= 0, "decoder drain");
      receive(&layers[k]);
    }
  }
  if (!window_done)
    need(layers[0].count == 0 && layers[1].count == 0, "complete pair coverage");
  if (refit_last != INT64_MAX)
    need(refit_paired > refit_last, "requested frame range extends past the title");
  need(refit_composed > 0, "nonempty frame range");

  if (refit_dump) {
    need(printf("{\"kind\":\"dump_complete\",\"frames\":%lld}\n", (long long)refit_composed) > 0,
         "composed dump trailer");
  } else {
    refit_close_raw();
    need(refit_pool_finish(refit_pooled, refit_emit, NULL), "scene statistics write");
    double median = refit_median(refit_lengths, refit_lengths_count);
    int64_t scenes = refit_pooled->emitted;
    need(printf("{\"kind\":\"refit_summary\",\"frames\":%lld,\"scenes\":%lld,"
                "\"rpu_scenes\":%lld,\"median_scene_frames\":%.17g,\"degenerate\":%s,"
                "\"fit_samples\":%.0f,\"held_out_samples\":%.0f}\n",
                (long long)refit_composed, (long long)scenes,
                (long long)refit_pooled->rpu_scenes, median,
                median < REFIT_MIN_SCENE_FRAMES ? "true" : "false", refit_samples[0],
                refit_samples[1]) > 0,
         "refit summary write");
  }
  need(fflush(stdout) == 0, "observations flush");
  for (int k = 0; k < 2; k++) {
    for (int i = 0; i < layers[k].count; i++)
      av_frame_free(&layers[k].queue[i]);
    av_bsf_free(&layers[k].split);
    avcodec_free_context(&layers[k].decoder);
  }
  av_packet_free(&packet);
  avformat_close_input(&input);
  av_freep(&refit_avio->buffer);
  avio_context_free(&refit_avio);
  movie_source_close(&refit_io);
  free(refit_raw);
  free(refit_pooled);
  free(refit_frame_stats[0]);
  free(refit_frame_stats[1]);
  free(refit_lengths);
  free(refit_records);
  return 0;
}
