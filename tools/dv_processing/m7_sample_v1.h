// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_M7_SAMPLE_V1_H
#define PLURX_M7_SAMPLE_V1_H

#include <libavutil/intreadwrite.h>

// The little-endian stream starts with PLXM7F01 for calibrated P7 FEL or
// PLXM7C01 for its separately admitted authored P8.1 candidate. Metadata-only
// census streams use PLXM7S01 and PLXM7A01 respectively. Their samples/frame,
// bytes/sample, FRM1 sample count and END1 sample total are all zero.
// Header: eight magic bytes, then sixteen u32s in this order: video stream
// index, clock numerator/denominator, BL width/height, half-open ROI x0/y0/x1/y1,
// stride, phase bitmask (on both axes), samples/frame, bytes/sample, frames,
// first/last inclusive ordinal in the input. Two i64s give the first/exclusive
// end PTS. Candidate ordinals and clocks are local to the authored clip.
// Every following chunk is a four-byte tag and u32 byte count for its body.
// FRM1: u64 input ordinal, i64 PTS and duration, u32 sample count and RPU byte
// count, then three 64-byte lowercase ASCII SHA-256s (coded AU, RPU NAL, BL VCL).
// The VCL digest includes each slice NAL with its original four-byte length.
// The fresh RPU NAL, including its two-byte HEVC header, carries the frame DM.
// At most 4097 NAL bytes follow the digests, then the sample array, ordered by
// increasing y then x over the lattice. Source records are eight u16s: x, y,
// raw BL Y/Cb/Cr and raw EL Y/Cb/Cr. Candidate records are five u16s: x, y and
// raw authored BL Y/Cb/Cr. The caller pairs clips through the authoring receipt
// and compares the same-frame DM and candidate RPU mapping.
// END1: u64 frame and sample totals. A reader must require exact lengths,
// coverage and EOF after END1, plus the successful exit of the writer.
enum {
  M7_SAMPLE_MAX_FRAMES = 48,
  M7_SAMPLE_MAX_PACKETS = 16384,
  M7_SAMPLE_FEL_RECORD_BYTES = 16,
  M7_SAMPLE_CANDIDATE_RECORD_BYTES = 10,
  M7_SAMPLE_FRAME_FIELDS = 32,
  M7_SAMPLE_DIGEST_BYTES = 64,
};

struct m7_sample_v1 {
  FILE *file;
  int roi[4];
  int64_t end_pts;
  uint32_t samples_per_frame, record_bytes;
};

static void m7_sample_v1_write(FILE *file, const void *bytes, size_t size) {
  need(fwrite(bytes, 1, size, file) == size, "M7 sample stream write");
}

static void m7_sample_v1_open(struct m7_sample_v1 *sample, const char *path,
                              int stream_index, AVRational clock, int width, int height,
                              int64_t first, int64_t last, int64_t first_pts,
                              bool candidate, bool census) {
  const int *roi = sample->roi;
  refit_content(roi[2] <= width && roi[3] <= height &&
                    roi[2] - roi[0] >= 2 && roi[3] - roi[1] >= 2,
                "M7 ROI fits the decoded raster and contains both phases");
  int columns = refit_axis_count(roi[0], roi[2], 0) + refit_axis_count(roi[0], roi[2], 1);
  int rows = refit_axis_count(roi[1], roi[3], 0) + refit_axis_count(roi[1], roi[3], 1);
  if (!census) {
    sample->samples_per_frame = (uint32_t)(columns * rows);
    sample->record_bytes = candidate ? M7_SAMPLE_CANDIDATE_RECORD_BYTES : M7_SAMPLE_FEL_RECORD_BYTES;
  }

  int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
  need(fd >= 0, "new exclusive M7 sample output");
  sample->file = fdopen(fd, "wb");
  need(sample->file != NULL, "M7 sample output stream");

  const uint32_t fields[] = {
      stream_index, clock.num, clock.den, width, height, roi[0], roi[1], roi[2], roi[3],
      REFIT_STRIDE, 3, sample->samples_per_frame, sample->record_bytes,
      (uint32_t)(last - first + 1), (uint32_t)first, (uint32_t)last,
  };
  uint8_t header[88];
  const char *magic = census ? (candidate ? "PLXM7A01" : "PLXM7S01")
                             : (candidate ? "PLXM7C01" : "PLXM7F01");
  memcpy(header, magic, 8);
  for (size_t i = 0; i < sizeof(fields) / sizeof(*fields); i++)
    AV_WL32(header + 8 + 4 * i, fields[i]);
  AV_WL64(header + 72, (uint64_t)first_pts);
  AV_WL64(header + 80, (uint64_t)sample->end_pts);
  m7_sample_v1_write(sample->file, header, sizeof(header));
}

static void m7_sample_v1_frame(const struct m7_sample_v1 *sample, const AVFrame *bl,
                               const AVFrame *el, const AVDOVIMetadata *data,
                               int64_t ordinal, const uint8_t *nalu, size_t nalu_size) {
  refit_content(!el || refit_topleft_mapping(data),
                "M7 FEL requires identity mapping and calibrated linear NLQ");
  refit_content(refit_topleft_color(av_dovi_get_color(data)),
                "M7 requires the calibrated full-range PQ DM matrices and offsets");
  refit_content(bl->color_primaries == AVCOL_PRI_BT2020 && bl->color_trc == AVCOL_TRC_SMPTE2084 &&
                    bl->colorspace == AVCOL_SPC_BT2020_NCL && bl->color_range == AVCOL_RANGE_MPEG &&
                    bl->chroma_location == AVCHROMA_LOC_TOPLEFT &&
                    !(bl->flags & AV_FRAME_FLAG_INTERLACED) &&
                    (!el || !(el->flags & AV_FRAME_FLAG_INTERLACED)),
                "M7 requires progressive layers and a decoded BT.2020 PQ video-range TopLeft BL");
  need(bl->pts < sample->end_pts && bl->pts + bl->duration <= sample->end_pts,
       "decoded M7 frame lies within the requested clock interval");
  char rpu_sha[M7_SAMPLE_DIGEST_BYTES + 1];
  hash_bytes(nalu, nalu_size, rpu_sha);
  struct coded_input *coded = NULL;
  for (unsigned i = 0; i < coded_count; i++)
    if (coded_window[i].pts == bl->pts)
      coded = &coded_window[i];
  need(coded && !coded->emitted && coded->duration == bl->duration &&
           strcmp(coded->rpu_sha, rpu_sha) == 0,
       "M7 frame/RPU/duration must match the independent coded access unit");

  uint32_t count = sample->samples_per_frame;
  uint32_t bytes = count * sample->record_bytes;
  if (bytes) {
    if (!refit_records) {
      refit_records = malloc(bytes);
      need(refit_records != NULL, "one-frame M7 sample buffer");
      refit_records_capacity = bytes;
    }
    need(refit_records_capacity == bytes, "stable M7 lattice");
    uint8_t *record = refit_records;
    const int *roi = sample->roi;
    // Visit both phases inside each stride before advancing to the next stride.
    for (int y = roi[1], dy = 1; y < roi[3]; y += dy, dy = REFIT_STRIDE - dy) {
      for (int x = roi[0], dx = 1; x < roi[2]; x += dx, dx = REFIT_STRIDE - dx) {
        uint16_t base[3];
        AV_WL16(record, x);
        AV_WL16(record + 2, y);
        if (el) {
          uint16_t enhancement[3];
          refit_sample(bl, el, x, y, base, enhancement);
          for (int channel = 0; channel < 3; channel++)
            AV_WL16(record + 10 + 2 * channel, enhancement[channel]);
        } else {
          struct refit_sites sites = refit_pair(x, y, bl->width, bl->height);
          refit_base_sample(bl, x, y, &sites, base);
        }
        for (int channel = 0; channel < 3; channel++)
          AV_WL16(record + 4 + 2 * channel, base[channel]);
        record += sample->record_bytes;
      }
    }
    need((size_t)(record - refit_records) == bytes, "M7 lattice sample count");
  }

  uint8_t frame[8 + M7_SAMPLE_FRAME_FIELDS] = "FRM1";
  AV_WL32(frame + 4, M7_SAMPLE_FRAME_FIELDS + 3 * M7_SAMPLE_DIGEST_BYTES +
                         (uint32_t)nalu_size + bytes);
  AV_WL64(frame + 8, (uint64_t)ordinal);
  AV_WL64(frame + 16, (uint64_t)bl->pts);
  AV_WL64(frame + 24, (uint64_t)bl->duration);
  AV_WL32(frame + 32, count);
  AV_WL32(frame + 36, (uint32_t)nalu_size);
  m7_sample_v1_write(sample->file, frame, sizeof(frame));
  m7_sample_v1_write(sample->file, coded->packet_sha, M7_SAMPLE_DIGEST_BYTES);
  m7_sample_v1_write(sample->file, coded->rpu_sha, M7_SAMPLE_DIGEST_BYTES);
  m7_sample_v1_write(sample->file, coded->base_vcl_sha, M7_SAMPLE_DIGEST_BYTES);
  m7_sample_v1_write(sample->file, nalu, nalu_size);
  if (bytes)
    m7_sample_v1_write(sample->file, refit_records, bytes);
  coded->emitted = true;
}

static void m7_sample_v1_finish(const struct m7_sample_v1 *sample, int64_t frames) {
  need((int64_t)coded_count == frames, "one coded M7 access unit per decoded frame");
  for (unsigned i = 0; i < coded_count; i++)
    need(coded_window[i].emitted, "complete coded M7 access unit coverage");
  uint8_t trailer[24] = "END1";
  AV_WL32(trailer + 4, 16);
  AV_WL64(trailer + 8, (uint64_t)frames);
  AV_WL64(trailer + 16, (uint64_t)frames * sample->samples_per_frame);
  m7_sample_v1_write(sample->file, trailer, sizeof(trailer));
  need(fclose(sample->file) == 0, "complete M7 sample output close");
}

#endif
