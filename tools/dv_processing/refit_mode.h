// SPDX-License-Identifier: Apache-2.0
// F+ statistics modes of segment_decode_render (FEL-RPU-REFIT-BUILD.md §3.2):
//
//   segment_decode_render --refit-stats SOURCE VIDEO_INDEX [--threads N]
//                         [--frames FIRST LAST | --resume FIRST FIRST_PTS]
//   segment_decode_render --dump-composed SOURCE VIDEO_INDEX FIRST LAST
//   segment_decode_render --export-composed-topleft SOURCE VIDEO_INDEX FIRST LAST NEW_OUTPUT.yuv
//   segment_decode_render --m7-sample-fel-v1 SOURCE VIDEO_INDEX FIRST LAST
//                         FIRST_PTS END_PTS X0 Y0 X1 Y1 NEW_OUTPUT
//   segment_decode_render --m7-sample-candidate-v1 AUTHORED VIDEO_INDEX 0 LAST
//                         FIRST_PTS END_PTS X0 Y0 X1 Y1 NEW_OUTPUT
//
// The first two modes take a trailing diagnostic [--area LEFT RIGHT TOP BOTTOM], which
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
// --export-composed-topleft uses the same admitted CPU composition and
// requires exactly 48 frames. It writes an exclusive full-raster raw
// yuv420p10le file and a stdout JSONL sidecar with the versioned color tags,
// source clocks, RPU and frame hashes. The final export_complete record
// follows a successful close of the complete raw file.
//
// --m7-sample-fel-v1 seeks on exact source ticks, bounds preroll, and writes
// raw BL/EL lattice samples plus frame and coded AU identities to a new file.
// FIRST/LAST and FIRST_PTS/END_PTS come from the held source's frame census.
// --m7-sample-candidate-v1 reads the entire selected authored P8.1 clip, with
// ordinal zero and clocks in that clip. Both modes bind each decoded RPU to
// its coded access unit. The matching --m7-census-fel-v1 and
// --m7-census-candidate-v1 commands use the same arguments and emit frame
// identity only. The FEL census verifies absolute presentation ordinals from
// the start of the held stream, within the finite read and prefix envelope.
// --m7-export-full-planes-v1 uses the same FEL window arguments and writes
// complete original BL and composed TopLeft planes for the exact candidate
// crop. The M7 header defines the distinct wire representations.
//
// --refit-stats writes JSON lines to stdout: one per pooled scene,
//   {"kind":"refit_scene","first_frame":N,"frames":K,"first_pts":TICKS,
//    "end_pts":TICKS,"time_base":"NUM/DEN","rpu_scene_frames":[L,...],
//    "fit":RefitStats,"held_out":RefitStats}
// in presentation order (end_pts: the scene's last frame's pts plus its
// duration; rpu_scene_frames: the lengths of the RPU's own scenes pooled
// into it, in order, summing to K), then one
//   {"kind":"refit_summary","frames":N,"scenes":S,"rpu_scenes":R,
//    "median_scene_frames":M,"degenerate":BOOL,"fit_samples":F,
//    "held_out_samples":H,"first_pts":TICKS,"end_pts":TICKS,
//    "time_base":"NUM/DEN"}
// where M is the median length of the RPU's own scenes before pooling and
// degenerate is M < 24 (scene_refresh_flag too frequent to mean scene cuts).
// RefitStats is plurx-core's dv_refit::RefitStats serde layout. Exit 0 only
// after a complete read. A refusal writes one line on stderr (libav's own
// logging is silenced) and states its class in the exit status:
// - 3 (REFIT_EXIT_CONTENT): the title's content is refused — not a dual-layer
//   Profile 7 FEL stream, an enhancement layer the frozen pairing does not
//   cover, RPU metadata the composer or the parser refuses, an invalid
//   active area. The same bits refuse the same way every time.
// - 1: anything else — the source could not be read, an allocation, the
//   decoder, a write. Worth another attempt.
// Scenes already written before a refusal are complete; the caller decides
// what to keep.
//
// --threads N sets the frame workers of each of the two layer decoders
// (default 4); the process then runs 2N decoder workers and this thread.
//
// --resume FIRST FIRST_PTS continues an interrupted run at a pooled scene
// it already wrote: frame ordinal FIRST, presenting at FIRST_PTS. The run
// seeks to the random-access point at or before FIRST_PTS, decodes the
// preroll without composing it, and composes and pools from that frame on,
// numbering it FIRST. A pooled scene's start is a point where pooling holds
// nothing pending, so every scene from FIRST on is exactly the uninterrupted
// run's (controls/test_refit_resume.py checks it at every boundary). The
// summary describes this run's frames only; with every scene's
// rpu_scene_frames the caller rebuilds the whole title's summary from the
// scenes before FIRST and this run's. FIRST_PTS must be a frame's pts after
// a random-access point (exit 1 otherwise).

#include "refit_compose.h"
#include "refit_topleft.h"

// About 11.5 hours at 24 frames per second.
#define REFIT_MAX_TITLE_FRAMES 1000000
// Frames decoded between the random-access point and a resume point.
#define REFIT_MAX_PREROLL 4096
#define REFIT_MAX_PACKET_BYTES (16 * 1024 * 1024)

#define REFIT_EXIT_CONTENT 3

static bool refit_dump, refit_area_override, refit_topleft, refit_m7, refit_m7_census, refit_m7_planes;
static FILE *refit_topleft_output;
// --resume: the resume point, and the preroll decoded before it.
static int64_t refit_resume_frame = -1, refit_resume_pts, refit_preroll;
static bool refit_resume_started, refit_preroll_seen;
// The next RPU scene length a written pooled scene has not yet claimed.
static size_t refit_lengths_cursor;
static int refit_area[4];
static int64_t refit_first, refit_last = INT64_MAX, refit_paired, refit_composed;
static int64_t refit_first_pts, refit_end_pts;
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

// A presentation time in stream ticks, as a scene line wrote it.
static int64_t refit_ticks(const char *text) {
  char *end;
  errno = 0;
  long long value = strtoll(text, &end, 10);
  need(!errno && end != text && !*end && value > INT64_MIN && value < INT64_MAX,
       "integer resume presentation time");
  return value;
}

// A refusal of the title's content (see the header): exit REFIT_EXIT_CONTENT.
static void refit_content(int ok, const char *reason) {
  if (!ok) {
    refusal_exit = REFIT_EXIT_CONTENT;
    need(0, reason);
  }
}

static bool refit_emit(const struct refit_scene *scene, void *unused) {
  (void)unused;
  if (printf("{\"kind\":\"refit_scene\",\"first_frame\":%lld,\"frames\":%lld,"
             "\"first_pts\":%lld,\"end_pts\":%lld,\"time_base\":\"%d/%d\","
             "\"rpu_scene_frames\":[",
             (long long)scene->first_frame, (long long)scene->frames,
             (long long)scene->first_pts, (long long)scene->end_pts, source_time_base.num,
             source_time_base.den) <= 0)
    return false;
  // Pooled scenes are written in frame order and cover whole RPU scenes, so
  // the RPU scenes pooled into this one are the next unclaimed lengths.
  int64_t covered = 0;
  for (bool first = true; covered < scene->frames; first = false) {
    if (refit_lengths_cursor >= refit_lengths_count)
      return false;
    int64_t length = refit_lengths[refit_lengths_cursor++];
    if (printf(first ? "%lld" : ",%lld", (long long)length) <= 0)
      return false;
    covered += length;
  }
  return covered == scene->frames && fputs("],\"fit\":", stdout) != EOF &&
         refit_stats_json(stdout, &scene->fit) && fputs(",\"held_out\":", stdout) != EOF &&
         refit_stats_json(stdout, &scene->held_out) && fputs("}\n", stdout) != EOF;
}

// An RPU scene ends: remember its length for the summary, then pool it.
static void refit_close_raw(void) {
  if (!refit_raw->frames)
    return;
  need(refit_lengths_count < REFIT_MAX_TITLE_FRAMES, "bounded scene count");
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

// The decoder's parsed mapping (what the composition uses) against the raw
// RPU's own parse: every pivot and coefficient must agree. Also the frame's
// scene_refresh_flag, which must come from this RPU's own DM data.
static inline int64_t refit_fixed(int64_t integer, uint64_t fraction) {
  return integer * (INT64_C(1) << 23) + (int64_t)fraction;
}
static bool refit_scene_refresh(const uint8_t *nalu, size_t size, const AVDOVIMetadata *data) {
  DoviRpuOpaque *rpu = dovi_parse_unspec62_nalu(nalu, size);
  refit_content(rpu != NULL && dovi_rpu_get_error(rpu) == NULL, "RPU parser cross-check");
  const DoviRpuDataMapping *raw = dovi_rpu_get_data_mapping(rpu);
  const DoviVdrDmData *dm = dovi_rpu_get_vdr_dm_data(rpu);
  refit_content(raw != NULL && dm != NULL, "fresh RPU mapping and DM data");
  const AVDOVIDataMapping *parsed = av_dovi_get_mapping(data);
  bool same = true;
  for (int c = 0; c < 3 && same; c++) {
    const DoviReshapingCurve *k = &raw->curves[c];
    const AVDOVIReshapingCurve *f = &parsed->curves[c];
    int pieces = f->num_pivots - 1;
    same = refit_pivots_agree(k->pivots.data, k->pivots.len, f->pivots, f->num_pivots);
    for (int i = 0; same && i < pieces; i++)
      same = (int)k->mapping_idc == (int)f->mapping_idc[i];
    if (same && k->mapping_idc == 0) {
      const DoviPolynomialCurve *poly = k->polynomial;
      same = poly && poly->poly_order_minus1.len == (size_t)pieces &&
             poly->poly_coef_int.len == (size_t)pieces && poly->poly_coef.len == (size_t)pieces;
      for (int i = 0; same && i < pieces; i++) {
        int order = (int)poly->poly_order_minus1.data[i] + 1;
        const DoviI64Data *integer = poly->poly_coef_int.list[i];
        const DoviU64Data *fraction = poly->poly_coef.list[i];
        same = order == f->poly_order[i] && integer && fraction &&
               integer->len == (size_t)order + 1 && fraction->len == (size_t)order + 1;
        for (int j = 0; same && j <= order; j++)
          same = refit_fixed(integer->data[j], fraction->data[j]) == f->poly_coef[i][j];
      }
    } else if (same) {
      const DoviMMRCurve *mmr = k->mmr;
      same = mmr && mmr->mmr_order_minus1.len == (size_t)pieces &&
             mmr->mmr_constant_int.len == (size_t)pieces &&
             mmr->mmr_constant.len == (size_t)pieces && mmr->mmr_coef_int.len == (size_t)pieces &&
             mmr->mmr_coef.len == (size_t)pieces;
      for (int i = 0; same && i < pieces; i++) {
        int order = (int)mmr->mmr_order_minus1.data[i] + 1;
        same = order == f->mmr_order[i] &&
               refit_fixed(mmr->mmr_constant_int.data[i], mmr->mmr_constant.data[i]) ==
                   f->mmr_constant[i] &&
               mmr->mmr_coef_int.list[i] && mmr->mmr_coef.list[i] &&
               mmr->mmr_coef_int.list[i]->len == (size_t)order &&
               mmr->mmr_coef.list[i]->len == (size_t)order;
        for (int j = 0; same && j < order; j++) {
          const DoviI64Data *integer = mmr->mmr_coef_int.list[i]->list[j];
          const DoviU64Data *fraction = mmr->mmr_coef.list[i]->list[j];
          same = integer && fraction && integer->len == 7 && fraction->len == 7;
          for (int t = 0; same && t < 7; t++)
            same = refit_fixed(integer->data[t], fraction->data[t]) == f->mmr_coef[i][j][t];
        }
      }
    }
  }
  bool refresh = dm->scene_refresh_flag != 0;
  bool agree = same && refresh == (av_dovi_get_color(data)->scene_refresh_flag != 0);
  dovi_rpu_free_vdr_dm_data(dm);
  dovi_rpu_free_data_mapping(raw);
  dovi_rpu_free(rpu);
  refit_content(agree, "decoder and RPU parser mappings agree");
  return refresh;
}

static inline void refit_base_sample(const AVFrame *bl, int x, int y,
                                     const struct refit_sites *sites, uint16_t base[3]) {
  base[0] = refit_code(bl, 0, x, y);
  base[1] = refit_code(bl, 1, sites->bl_chroma[0], sites->bl_chroma[1]);
  base[2] = refit_code(bl, 2, sites->bl_chroma[0], sites->bl_chroma[1]);
}

static void refit_sample(const AVFrame *bl, const AVFrame *el, int x, int y,
                         uint16_t base[3], uint16_t enhancement[3]) {
  struct refit_sites sites = refit_pair(x, y, bl->width, bl->height);
  refit_base_sample(bl, x, y, &sites, base);
  enhancement[0] = refit_code(el, 0, sites.el_luma[0], sites.el_luma[1]);
  enhancement[1] = refit_code(el, 1, sites.el_chroma[0], sites.el_chroma[1]);
  enhancement[2] = refit_code(el, 2, sites.el_chroma[0], sites.el_chroma[1]);
}

static void refit_topleft_pixels(const AVFrame *bl, const AVFrame *el,
                                 const struct refit_mapping *mapping,
                                 const AVDOVIColorMetadata *color,
                                 uint8_t *pixels_out) {
  size_t pixels = (size_t)bl->width * bl->height;
  uint8_t *cb = pixels_out + 2 * pixels;
  uint8_t *cr = cb + pixels / 2;
  for (int y = 0; y < bl->height; y++) {
    for (int x = 0; x < bl->width; x++) {
      uint16_t base[3], enhancement[3], composed[3], rgb[3];
      refit_sample(bl, el, x, y, base, enhancement);
      refit_compose(mapping, base, enhancement, composed);
      refit_topleft_rgb(color, composed, rgb);
      int64_t luma;
      uint16_t code = refit_topleft_luma(rgb, &luma);
      refit_topleft_store(pixels_out, (size_t)y * bl->width + x, code);
      if (!(x % 2) && !(y % 2)) {
        size_t chroma = (size_t)(y / 2) * (bl->width / 2) + x / 2;
        refit_topleft_store(cb, chroma, refit_topleft_chroma(rgb[2], luma, 18814));
        refit_topleft_store(cr, chroma, refit_topleft_chroma(rgb[0], luma, 14746));
      }
    }
  }
}

#include "m7_sample_v1.h"
static struct m7_sample_v1 refit_m7_sample;

static void refit_topleft_frame(const AVFrame *bl, const AVFrame *el,
                              const AVDOVIMetadata *data,
                              const struct refit_mapping *mapping, int64_t ordinal,
                              const uint8_t *nalu, size_t nalu_size) {
  const AVDOVIColorMetadata *color = av_dovi_get_color(data);
  refit_content(refit_topleft_mapping(data), "TopLeft v1 requires identity mapping and calibrated linear NLQ");
  refit_content(refit_topleft_color(color), "TopLeft v1 requires the calibrated Q13/Q28 full-range color matrix");
  size_t pixels = (size_t)bl->width * bl->height;
  size_t bytes = 3 * pixels;
  if (!refit_records) {
    refit_records = malloc(bytes);
    need(refit_records != NULL, "one-frame TopLeft export buffer");
    refit_records_capacity = bytes;
  }
  need(bytes == refit_records_capacity, "stable TopLeft raster");
  refit_topleft_pixels(bl, el, mapping, color, refit_records);
  char rpu_sha[65], frame_sha[65];
  hash_bytes(nalu, nalu_size, rpu_sha);
  hash_bytes(refit_records, bytes, frame_sha);
  need(fwrite(refit_records, 1, bytes, refit_topleft_output) == bytes &&
           printf("{\"kind\":\"export_frame\",\"ordinal\":%lld,\"pts_ticks\":%lld,"
                  "\"duration_ticks\":%lld,\"bytes\":%zu,\"source_rpu_sha256\":\"%s\","
                  "\"frame_sha256\":\"%s\"}\n", (long long)ordinal, (long long)bl->pts,
                  (long long)bl->duration, bytes, rpu_sha, frame_sha) > 0,
       "TopLeft frame and receipt write");
}

static void refit_frame(const AVFrame *bl, const AVFrame *el, int64_t ordinal) {
  need(bl->pts <= INT64_MAX - bl->duration, "frame end overflow");
  AVFrameSideData *raw = av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_RPU_BUFFER);
  refit_content(raw && raw->size > 0 && raw->size < 4096,
                "fresh decoder-attached RPU required; cached metadata insufficient");
  uint8_t nalu[4098] = {0x7c, 0x01};
  memcpy(nalu + 2, raw->data, raw->size);
  if (!el) {
    struct base_mapping admitted;
    base_metadata(bl, nalu, raw->size + 2, source_profile, &admitted);
    const AVFrameSideData *side = av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_METADATA);
    refit_content(side != NULL, "decoder-derived P8.1 metadata");
    const AVDOVIMetadata *data = (const void *)side->data;
    refit_scene_refresh(nalu, raw->size + 2, data);
    int w = bl->width;
    refit_content(bl->format == AV_PIX_FMT_YUV420P10LE &&
                      bl->linesize[0] >= 2 * w && bl->linesize[1] >= w && bl->linesize[2] >= w,
                  "software-decoded planar ten-bit candidate");
    m7_sample_v1_frame(&refit_m7_sample, bl, NULL, data, NULL, ordinal, nalu, raw->size + 2);
    refit_end_pts = bl->pts + bl->duration;
    refit_composed++;
    return;
  }
  // The playback path's admission, refusals included; it prints nothing here.
  struct pl_dovi_metadata admitted;
  // Its refusals are refusals of this RPU's content.
  dv_content_refusal_exit = REFIT_EXIT_CONTENT;
  map_fel_rpu(bl, nalu, raw->size + 2, &admitted, false, bl->width, bl->height);
  dv_content_refusal_exit = EXIT_FAILURE;
  const AVFrameSideData *side = av_frame_get_side_data(bl, AV_FRAME_DATA_DOVI_METADATA);
  refit_content(side != NULL, "decoder-derived FEL metadata");
  const AVDOVIMetadata *data = (const void *)side->data;
  struct refit_mapping mapping;
  const char *refused = refit_mapping_from_dovi(data, &mapping);
  refit_content(refused == NULL, refused ? refused : "");
  bool refresh = refit_scene_refresh(nalu, raw->size + 2, data);

  int w = bl->width, h = bl->height, ew = el->width, eh = el->height;
  // The frozen pairing covers a half-resolution EL only.
  refit_content(2 * ew == w && 2 * eh == h, "half-resolution enhancement layer");
  refit_content(bl->format == AV_PIX_FMT_YUV420P10LE && el->format == AV_PIX_FMT_YUV420P10LE &&
           bl->linesize[0] >= 2 * w && bl->linesize[1] >= w && bl->linesize[2] >= w &&
           el->linesize[0] >= 2 * ew && el->linesize[1] >= ew && el->linesize[2] >= ew,
       "software-decoded planar ten-bit layers");
  if (refit_m7) {
    m7_sample_v1_frame(&refit_m7_sample, bl, el, data, &mapping, ordinal, nalu, raw->size + 2);
    refit_end_pts = bl->pts + bl->duration;
    refit_composed++;
    return;
  }
  if (refit_topleft) {
    refit_topleft_frame(bl, el, data, &mapping, ordinal, nalu, raw->size + 2);
    refit_composed++;
    return;
  }
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
  refit_content(area[0] + area[1] < w && area[2] + area[3] < h, "invalid active area metadata");
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
        uint16_t base[3], enhancement[3], composed[3];
        refit_sample(bl, el, x, y, base, enhancement);
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
    refit_raw->end_pts = bl->pts + bl->duration;
    for (int k = 0; k < 2; k++)
      refit_stats_finish_frame(refit_frame_stats[k]);
    refit_stats_merge(&refit_raw->fit, refit_frame_stats[0]);
    refit_stats_merge(&refit_raw->held_out, refit_frame_stats[1]);
    refit_raw->frames++;
  }
  if (!refit_composed)
    refit_first_pts = bl->pts;
  refit_end_pts = bl->pts + bl->duration;
  refit_composed++;
}

// receive()'s pair consumer in these modes, in place of render_pairs.
static void refit_pairs(void) {
  while (layers[0].count && (base_only || layers[1].count)) {
    AVFrame *bl = layers[0].queue[0], *el = base_only ? NULL : layers[1].queue[0];
    need(bl->duration > 0 && (!el || (bl->pts == el->pts && bl->duration == el->duration)),
         el ? "unmatched or ambiguous BL/EL PTS/duration" : "positive M7 candidate frame duration");
    need(last_pts == AV_NOPTS_VALUE || bl->pts > last_pts,
         "duplicate/out-of-order paired frame");
    last_pts = bl->pts;
    if (refit_m7_census && !base_only && !refit_paired)
      need((bl->flags & AV_FRAME_FLAG_KEY) && (el->flags & AV_FRAME_FLAG_KEY),
           "independent first BL/EL pair required for an absolute M7 census");
    if (refit_resume_frame >= 0 && !refit_resume_started) {
      // Decode from an independent random-access picture. FEL windows can
      // seek into preroll. The whole candidate starts on its first picture.
      if (!refit_preroll_seen) {
        need((bl->flags & AV_FRAME_FLAG_KEY) && (!el || (el->flags & AV_FRAME_FLAG_KEY)) &&
                 bl->pts <= refit_resume_pts,
             el ? "independent BL/EL random-access preroll required before the resume point"
                : "independent random-access frame required at the start of the M7 candidate");
        refit_preroll_seen = true;
      }
      if (bl->pts < refit_resume_pts) {
        need(!base_only, "M7 candidate must start at the first declared PTS");
        need(++refit_preroll <= REFIT_MAX_PREROLL, "bounded resume preroll");
        drop_pair();
        continue;
      }
      need(bl->pts == refit_resume_pts, "resume point is a frame of the title");
      refit_resume_started = true;
      refit_paired = refit_resume_frame;
    }
    int64_t ordinal = refit_paired++;
    need(refit_paired <= REFIT_MAX_TITLE_FRAMES, "bounded title frame count");
    if (ordinal > refit_last) {
      need(!base_only, "extra decoded frame in M7 candidate");
      need(!refit_m7 || bl->pts >= refit_m7_sample.end_pts,
           "extra M7 frame within the requested clock interval");
      window_done = true;
      drop_pair();
      return;
    }
    if (ordinal >= refit_first) {
      if (refit_m7_census && ordinal == refit_first)
        need(bl->pts == refit_resume_pts, "M7 census first presentation ordinal and PTS disagree");
      refit_frame(bl, el, ordinal);
    }
    drop_pair();
  }
}

// receive()'s admission of one decoded frame, without its drain loop.
static void refit_queue(struct layer *layer, AVFrame *frame) {
  need(layer->count < PAIR_WINDOW, "pair window capacity");
  if (layer->index == 1 && discover_el && expected_w[1] == 0) {
    need(frame->width >= 32 && frame->height >= 32 && frame->width % 2 == 0 &&
             frame->height % 2 == 0 &&
             ((frame->width == expected_w[0] && frame->height == expected_h[0]) ||
              (2 * (int64_t)frame->width == expected_w[0] &&
               2 * (int64_t)frame->height == expected_h[0])),
         "unsupported observed enhancement raster ratio");
    expected_w[1] = frame->width;
    expected_h[1] = frame->height;
  }
  need(frame->format == AV_PIX_FMT_YUV420P10LE && frame->width == expected_w[layer->index] &&
           frame->height == expected_h[layer->index] && frame->pts != AV_NOPTS_VALUE &&
           !(frame->flags & AV_FRAME_FLAG_CORRUPT),
       "decoded geometry/representation/PTS or corrupt frame");
  for (int i = 0; i < layer->count; i++)
    need(layer->queue[i]->pts != frame->pts, "duplicate decoded layer PTS");
  layer->queue[layer->count++] = av_frame_clone(frame);
  need(layer->queue[layer->count - 1] != NULL, "bounded queued frame reference");
  av_frame_unref(frame);
}

// Flush the active decoders and alternate layers so a queue cannot run ahead
// of its partner during drain.
static void refit_drain(void) {
  int layer_count = base_only ? 1 : 2;
  for (int k = 0; k < layer_count; k++)
    need(avcodec_send_packet(layers[k].decoder, NULL) >= 0, "decoder drain");
  AVFrame *frame = av_frame_alloc();
  need(frame != NULL, "decode frame allocation");
  bool done[2] = {false, base_only};
  while (!window_done && !(done[0] && done[1])) {
    for (int k = 0; k < layer_count && !window_done; k++) {
      if (done[k])
        continue;
      int result = avcodec_receive_frame(layers[k].decoder, frame);
      if (result == AVERROR_EOF) {
        done[k] = true;
        continue;
      }
      need(result >= 0, "decoder drain receive");
      refit_queue(&layers[k], frame);
      refit_pairs();
    }
  }
  av_frame_free(&frame);
}

static int refit_main(int argc, char **argv) {
  refit_dump = !strcmp(argv[1], "--dump-composed");
  refit_topleft = !strcmp(argv[1], "--export-composed-topleft");
  bool candidate_census = !strcmp(argv[1], "--m7-census-candidate-v1");
  refit_m7_census = candidate_census || !strcmp(argv[1], "--m7-census-fel-v1");
  base_only = candidate_census || !strcmp(argv[1], "--m7-sample-candidate-v1");
  refit_m7_planes = !strcmp(argv[1], "--m7-export-full-planes-v1");
  refit_m7 = base_only || refit_m7_census || refit_m7_planes || !strcmp(argv[1], "--m7-sample-fel-v1");
  refit_area_override = argc >= 5 && !strcmp(argv[argc - 5], "--area");
  if (refit_area_override) {
    for (int i = 0; i < 4; i++)
      refit_area[i] = number(argv[argc - 4 + i], 0, 4095);
    argc -= 5;
  }
  if (refit_m7_planes)
    need(argc == 13 && !refit_area_override,
         "usage: segment_decode_render --m7-export-full-planes-v1 SOURCE VIDEO_INDEX "
         "FIRST LAST FIRST_PTS END_PTS X0 Y0 X1 Y1 NEW_OUTPUT");
  else if (refit_m7_census)
    need(argc == 13 && !refit_area_override,
         "usage: segment_decode_render --m7-census-{fel,candidate}-v1 SOURCE VIDEO_INDEX "
         "FIRST LAST FIRST_PTS END_PTS X0 Y0 X1 Y1 NEW_OUTPUT");
  else if (base_only)
    need(argc == 13 && !refit_area_override,
         "usage: segment_decode_render --m7-sample-candidate-v1 AUTHORED VIDEO_INDEX 0 LAST "
         "FIRST_PTS END_PTS X0 Y0 X1 Y1 NEW_OUTPUT");
  else if (refit_m7)
    need(argc == 13 && !refit_area_override,
         "usage: segment_decode_render --m7-sample-fel-v1 SOURCE VIDEO_INDEX FIRST LAST "
         "FIRST_PTS END_PTS X0 Y0 X1 Y1 NEW_OUTPUT");
  else if (refit_topleft)
    need(argc == 7 && !refit_area_override,
         "usage: segment_decode_render --export-composed-topleft SOURCE VIDEO_INDEX FIRST LAST NEW_OUTPUT.yuv");
  else if (refit_dump)
    need(argc == 6, "usage: segment_decode_render --dump-composed SOURCE VIDEO_INDEX "
                    "FIRST LAST [--area LEFT RIGHT TOP BOTTOM]");
  else
    need(argc >= 4, "usage: segment_decode_render --refit-stats SOURCE VIDEO_INDEX "
                    "[--threads N] [--frames FIRST LAST | --resume FIRST FIRST_PTS] "
                    "[--area LEFT RIGHT TOP BOTTOM]");
  refit_mode = true;
  // Refusals leave without exit-time teardown while decoder threads run.
  dv_threads_live = true;
  int stream_index = number(argv[3], 0, 255);
  if (refit_dump || refit_topleft || refit_m7) {
    refit_first = number(argv[4], 0, REFIT_MAX_TITLE_FRAMES - 1);
    refit_last = number(argv[5], 0, REFIT_MAX_TITLE_FRAMES - 1);
    need(refit_first <= refit_last, "ordered frame range");
    if (refit_m7) {
      frame_cap = (int)(refit_last - refit_first + 1);
      need(frame_cap <= M7_SAMPLE_MAX_FRAMES, "bounded M7 frame count");
      need(!refit_m7_planes || frame_cap == REFIT_TOPLEFT_FRAMES,
           "M7 full-plane encoder input requires exactly 48 frames");
      need(!base_only || refit_first == 0, "M7 candidate ordinals start at zero");
      if (refit_m7_census && !base_only)
        need(refit_first <= REFIT_MAX_PREROLL, "bounded absolute M7 source census prefix");
      else
        refit_resume_frame = refit_first;
      refit_resume_pts = refit_ticks(argv[6]);
      refit_m7_sample.end_pts = refit_ticks(argv[7]);
      need(refit_resume_pts < refit_m7_sample.end_pts, "ordered M7 clock interval");
      for (int i = 0; i < 4; i++)
        refit_m7_sample.roi[i] = number(argv[8 + i], 0, 4096);
      refit_threads = 1;
    } else if (refit_topleft) {
      need(refit_last - refit_first + 1 == REFIT_TOPLEFT_FRAMES,
           "TopLeft export requires exactly 48 frames");
      refit_threads = 1;
    }
  } else {
    bool ranged = false, resumed = false;
    for (int i = 4; i < argc;) {
      if (!strcmp(argv[i], "--threads") && i + 1 < argc) {
        refit_threads = number(argv[i + 1], 1, 16);
        i += 2;
      } else if (!strcmp(argv[i], "--frames") && i + 2 < argc && !ranged && !resumed) {
        refit_first = number(argv[i + 1], 0, REFIT_MAX_TITLE_FRAMES - 1);
        refit_last = number(argv[i + 2], 0, REFIT_MAX_TITLE_FRAMES - 1);
        need(refit_first <= refit_last, "ordered frame range");
        ranged = true;
        i += 3;
      } else if (!strcmp(argv[i], "--resume") && i + 2 < argc && !ranged && !resumed) {
        refit_resume_frame = number(argv[i + 1], 1, REFIT_MAX_TITLE_FRAMES - 1);
        refit_resume_pts = refit_ticks(argv[i + 2]);
        resumed = true;
        i += 3;
      } else {
        need(0, "usage: segment_decode_render --refit-stats SOURCE VIDEO_INDEX "
                "[--threads N] [--frames FIRST LAST | --resume FIRST FIRST_PTS] "
                "[--area LEFT RIGHT TOP BOTTOM]");
      }
    }
  }
  // Decoder chatter must not change what a refusal looks like on stderr.
  av_log_set_level(AV_LOG_QUIET);
  av_max_alloc(64ULL * 1024 * 1024);
  static char stdout_buffer[1 << 20];
  setvbuf(stdout, stdout_buffer, _IOFBF, sizeof(stdout_buffer));

  AVFormatContext *input = avformat_alloc_context();
  need(input != NULL, "input context");
  input->probesize = 1024 * 1024;
  input->max_analyze_duration = AV_TIME_BASE;
  input->max_probe_packets = 32;
  input->max_streams = 256;
  // The bounded M7 window retains the playback read budget. The other
  // modes can read the whole title, as scan_title does.
  need(movie_source_open(&refit_io, argv[2]) >= 0,
       "held regular title source or private source capability");
  // A demuxer reads a title about once; allow four reads plus room for
  // probing and index seeks, and refuse anything that needs more.
  need(refit_io.length > 0 && (uint64_t)refit_io.length <= (UINT64_C(1) << 56),
       "bounded title length");
  if (!refit_m7)
    refit_io.read_limit = 4 * (uint64_t)refit_io.length + (UINT64_C(64) << 20);
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
  refit_content((unsigned)stream_index < input->nb_streams &&
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
  refit_content(config && config->size >= sizeof(AVDOVIDecoderConfigurationRecord),
                "declared source Dolby Vision configuration");
  const AVDOVIDecoderConfigurationRecord *cfg = (const void *)config->data;
  if (base_only) {
    refit_content(cfg->dv_version_major == 1 && cfg->dv_profile == 8 &&
                      cfg->dv_bl_signal_compatibility_id == 1 && cfg->dv_md_compression == 0 &&
                      cfg->bl_present_flag && !cfg->el_present_flag && cfg->rpu_present_flag,
                  "M7 candidate requires declared Profile 8.1 with BL and RPU only");
    source_profile = cfg->dv_profile;
  } else {
    refit_content(cfg->dv_profile == 7 && cfg->bl_present_flag && cfg->el_present_flag &&
                      cfg->rpu_present_flag,
                  "Profile 7 dual-layer source");
  }
  expected_w[0] = stream->codecpar->width;
  expected_h[0] = stream->codecpar->height;
  refit_content(expected_w[0] >= 64 && expected_w[0] <= 4096 && expected_h[0] >= 64 &&
                    expected_h[0] <= 2160 && expected_w[0] % 2 == 0 && expected_h[0] % 2 == 0,
                "supported even raster bound");
  expected_w[1] = expected_h[1] = 0;
  discover_el = !base_only;
  if (!base_only && refit_resume_frame >= 0)
    need(avformat_seek_file(input, stream_index, INT64_MIN, refit_resume_pts, refit_resume_pts,
                            AVSEEK_FLAG_BACKWARD) >= 0,
         "keyframe seek before the resume point");

  if (refit_topleft || refit_m7) {
    const AVCodecParameters *codec = stream->codecpar;
    refit_content(codec->color_primaries == AVCOL_PRI_BT2020 && codec->color_trc == AVCOL_TRC_SMPTE2084 &&
                      codec->color_space == AVCOL_SPC_BT2020_NCL && codec->color_range == AVCOL_RANGE_MPEG &&
                      codec->chroma_location == AVCHROMA_LOC_TOPLEFT,
                  refit_m7 ? "M7 requires declared BT.2020 PQ video range and TopLeft chroma"
                           : "TopLeft v1 requires declared BT.2020 PQ video range and TopLeft chroma");
  }
  if (refit_m7) {
    const AVCodecParameters *codec = stream->codecpar;
    refit_content(codec->extradata_size >= 23 && codec->extradata[0] == 1 &&
                      (codec->extradata[21] & 3) == 3,
                  "M7 requires four-byte hvcC NAL framing for coded AU binding");
    m7_sample_v1_open(&refit_m7_sample, argv[12], stream_index, source_time_base,
                      expected_w[0], expected_h[0], refit_first, refit_last, refit_resume_pts,
                      base_only, refit_m7_planes ? M7_STREAM_FULL_PLANES :
                          refit_m7_census ? M7_STREAM_CENSUS : M7_STREAM_LATTICE);
  }
  if (refit_topleft) {
    int fd = open(argv[6], O_WRONLY | O_CREAT | O_EXCL, 0600);
    need(fd >= 0, "new exclusive TopLeft output");
    refit_topleft_output = fdopen(fd, "wb");
    need(refit_topleft_output != NULL, "TopLeft output stream");
    need(printf("{\"kind\":\"composed_topleft_export\",\"format\":\"%s\","
                "\"time_base\":\"%d/%d\",\"first\":%lld,\"last\":%lld,"
                "\"video_index\":%d,\"width\":%d,\"height\":%d,\"pix_fmt\":\"yuv420p10le\","
                "\"color_primaries\":\"bt2020\",\"color_trc\":\"smpte2084\","
                "\"colorspace\":\"bt2020nc\",\"color_range\":\"tv\","
                "\"chroma_location\":\"topleft\"}\n",
                REFIT_TOPLEFT_FORMAT, source_time_base.num, source_time_base.den,
                (long long)refit_first, (long long)refit_last, stream_index, expected_w[0], expected_h[0]) > 0,
         "TopLeft export header");
  }
  if (!refit_m7) {
    refit_raw = calloc(1, sizeof(*refit_raw));
    refit_pooled = calloc(1, sizeof(*refit_pooled));
    refit_frame_stats[0] = calloc(1, sizeof(*refit_frame_stats[0]));
    refit_frame_stats[1] = calloc(1, sizeof(*refit_frame_stats[1]));
    need(refit_raw && refit_pooled && refit_frame_stats[0] && refit_frame_stats[1],
         "scene statistics allocation");
  }
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
  if (!base_only)
    layers[1] = open_layer(stream, 1);
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "demux packet");
  int result = 0;
  unsigned m7_packets = 0;
  while (!window_done && (result = av_read_frame(input, packet)) >= 0) {
    need(packet->size <= REFIT_MAX_PACKET_BYTES, "bounded packet size");
    if (refit_m7)
      need(++m7_packets <= M7_SAMPLE_MAX_PACKETS, "bounded M7 demux packet count");
    if (packet->stream_index == stream_index) {
      if (refit_m7) {
        need(packet->pts != AV_NOPTS_VALUE && packet->duration > 0,
             "M7 coded access unit requires PTS and positive duration");
        bool in_window = packet->pts >= refit_resume_pts && packet->pts < refit_m7_sample.end_pts;
        need(!base_only || in_window, "extra coded picture outside declared M7 candidate interval");
        if (in_window)
          inspect_coded_au(packet, run_current, true, false);
      }
      send_layer(&layers[0], packet);
      if (!window_done && !base_only)
        send_layer(&layers[1], packet);
    }
    av_packet_unref(packet);
  }
  if (refit_m7)
    need(!refit_io.exhausted, "bounded M7 source read budget");
  if (!window_done) {
    need(result == AVERROR_EOF, "complete title read");
    refit_drain();
  }
  if (!window_done)
    need(layers[0].count == 0 && layers[1].count == 0, "complete pair coverage");
  if (refit_last != INT64_MAX)
    need(refit_paired > refit_last, "requested frame range extends past the title");
  need(refit_composed > 0, "nonempty frame range");

  if (refit_m7) {
    need(refit_composed == frame_cap && refit_end_pts == refit_m7_sample.end_pts,
         "complete M7 frame and clock window");
    m7_sample_v1_finish(&refit_m7_sample, refit_composed);
  } else if (refit_topleft) {
    need(refit_composed == REFIT_TOPLEFT_FRAMES && fclose(refit_topleft_output) == 0,
         "complete 48-frame TopLeft output");
    need(printf("{\"kind\":\"export_complete\",\"format\":\"%s\",\"frames\":%lld}\n",
                REFIT_TOPLEFT_FORMAT, (long long)refit_composed) > 0, "TopLeft export trailer");
  } else if (refit_dump) {
    need(printf("{\"kind\":\"dump_complete\",\"frames\":%lld}\n", (long long)refit_composed) > 0,
         "composed dump trailer");
  } else {
    refit_close_raw();
    need(refit_pool_finish(refit_pooled, refit_emit, NULL), "scene statistics write");
    double median = refit_median(refit_lengths, refit_lengths_count);
    int64_t scenes = refit_pooled->emitted;
    need(printf("{\"kind\":\"refit_summary\",\"frames\":%lld,\"scenes\":%lld,"
                "\"rpu_scenes\":%lld,\"median_scene_frames\":%.17g,\"degenerate\":%s,"
                "\"fit_samples\":%.0f,\"held_out_samples\":%.0f,\"first_pts\":%lld,"
                "\"end_pts\":%lld,\"time_base\":\"%d/%d\"}\n",
                (long long)refit_composed, (long long)scenes,
                (long long)refit_pooled->rpu_scenes, median,
                median < REFIT_MIN_SCENE_FRAMES ? "true" : "false", refit_samples[0],
                refit_samples[1], (long long)refit_first_pts, (long long)refit_end_pts,
                source_time_base.num, source_time_base.den) > 0,
         "refit summary write");
  }
  need(fflush(stdout) == 0, "observations flush");
  for (int k = 0; k < (base_only ? 1 : 2); k++) {
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
