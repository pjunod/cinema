/*
 * Offline FEL diagnostic: in-process libavcodec/libx265 encode of a calibrated
 * export with an optional per-frame quantOffsets map carried as
 * AV_FRAME_DATA_REGIONS_OF_INTEREST side data (FFmpeg libx265.c,
 * libx265_encode_set_roi). Everything else mirrors the retained ffmpeg CLI
 * encode: encoder options arrive verbatim as key=value pairs and go to
 * avcodec_open2 as one dictionary, the way the CLI passes them.
 *
 * Map modes:
 *   none  - no side data (equivalence gate E0 against the CLI baseline)
 *   zero  - one full-frame region with qoffset 0 (gate E1)
 *   lumazm - as luma, plus one full-frame region (listed last, so lower
 *           priority) of +min(|QP_OFFSET|, |QP_OFFSET|*c/(1-c)) for the
 *           remaining blocks, c = the frame's selected fraction: zero mean.
 *   luma  - per grid block, mean of the encoder's own input Y' (10-bit
 *           limited) -> nits via the ST 2084 EOTF; blocks below the declared
 *           threshold get the declared QP offset. Source-derived only.
 *
 * Grid follows libx265.c: 8 when qg-size is 8, otherwise 16.
 * No playback, packaging or deployment change.
 *
 * Usage: roi_encode IN.nut OUT.mp4 MAP.json MODE GRID THRESH_NITS QP_OFFSET
 *        FRAMES NUM DEN TB_DEN MOVFLAGS TAG [key=value ...]
 */
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/frame.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void die(const char *what, int err) {
    char buf[256] = "";
    if (err) av_strerror(err, buf, sizeof buf);
    fprintf(stderr, "roi_encode: %s %s\n", what, buf);
    exit(2);
}

static double pq_eotf_nits(double e) {
    const double m1 = 2610.0 / 16384, m2 = 2523.0 / 4096 * 128, c1 = 3424.0 / 4096, c2 = 2413.0 / 4096 * 32,
                 c3 = 2392.0 / 4096 * 32;
    if (e <= 0) return 0;
    if (e > 1) e = 1;
    double p = pow(e, 1 / m2);
    double n = p - c1;
    if (n < 0) n = 0;
    return 10000.0 * pow(n / (c2 - c3 * p), 1 / m1);
}

static int write_packets(AVCodecContext *enc, AVFormatContext *oc, AVStream *st, int *count) {
    AVPacket *pkt = av_packet_alloc();
    int ret;
    while ((ret = avcodec_receive_packet(enc, pkt)) >= 0) {
        av_packet_rescale_ts(pkt, enc->time_base, st->time_base);
        pkt->stream_index = st->index;
        (*count)++;
        if ((ret = av_interleaved_write_frame(oc, pkt)) < 0) die("write frame", ret);
    }
    av_packet_free(&pkt);
    return ret == AVERROR(EAGAIN) || ret == AVERROR_EOF ? 0 : ret;
}

int main(int argc, char **argv) {
    if (argc < 14) {
        fprintf(stderr, "usage: see source header\n");
        return 2;
    }
    const char *in = argv[1], *out = argv[2], *mapjson = argv[3], *mode = argv[4];
    int grid = atoi(argv[5]);
    double thresh = atof(argv[6]);
    int qpoff = atoi(argv[7]), frames = atoi(argv[8]), num = atoi(argv[9]), den = atoi(argv[10]),
        tbden = atoi(argv[11]);
    const char *movflags = argv[12], *tag = argv[13];
    int m_none = !strcmp(mode, "none"), m_zero = !strcmp(mode, "zero"), m_zm = !strcmp(mode, "lumazm"),
        m_luma = !strcmp(mode, "luma") || m_zm;
    if (!(m_none || m_zero || m_luma) || (grid != 8 && grid != 16) || strlen(tag) != 4) die("bad arguments", 0);

    /* CLI ran with -loglevel verbose; libx265.c forwards the av_log level to x265's log-level,
       which x265 records in its info SEI. */
    av_log_set_level(AV_LOG_VERBOSE);
    AVFormatContext *ic = NULL;
    int ret;
    if ((ret = avformat_open_input(&ic, in, NULL, NULL)) < 0) die("open input", ret);
    if ((ret = avformat_find_stream_info(ic, NULL)) < 0) die("stream info", ret);
    int vs = av_find_best_stream(ic, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (vs < 0) die("no video", vs);
    const AVCodec *dc = avcodec_find_decoder(ic->streams[vs]->codecpar->codec_id);
    AVCodecContext *dec = avcodec_alloc_context3(dc);
    avcodec_parameters_to_context(dec, ic->streams[vs]->codecpar);
    if ((ret = avcodec_open2(dec, dc, NULL)) < 0) die("open decoder", ret);

    const AVCodec *ec = avcodec_find_encoder_by_name("libx265");
    if (!ec) die("no libx265", 0);
    AVCodecContext *enc = avcodec_alloc_context3(ec);
    enc->width = dec->width;
    enc->height = dec->height;
    enc->pix_fmt = dec->pix_fmt;
    if (enc->pix_fmt != AV_PIX_FMT_YUV420P10LE) die("expected yuv420p10le input", 0);
    enc->time_base = (AVRational){1, tbden};
    enc->framerate = (AVRational){num, den};
    enc->sample_aspect_ratio = (AVRational){0, 1};
    /* ffmpeg_enc.c copies the filtered frame's colour tags into the context before opening. */
    enc->color_range = AVCOL_RANGE_MPEG;
    enc->color_primaries = AVCOL_PRI_BT2020;
    enc->color_trc = AVCOL_TRC_SMPTE2084;
    enc->colorspace = AVCOL_SPC_BT2020_NCL;
    enc->field_order = AV_FIELD_PROGRESSIVE;
    enc->bits_per_raw_sample = 10;
    AVFormatContext *oc = NULL;
    if ((ret = avformat_alloc_output_context2(&oc, NULL, "mp4", out)) < 0) die("alloc output", ret);
    /* As ffmpeg_mux_init.c: a global-header muxer (mp4) makes the encoder emit extradata. */
    if (oc->oformat->flags & AVFMT_GLOBALHEADER) enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    AVDictionary *opts = NULL;
    for (int i = 14; i < argc; i++) {
        char *eq = strchr(argv[i], '=');
        if (!eq) die("option without =", 0);
        *eq = 0;
        av_dict_set(&opts, argv[i], eq + 1, 0);
        *eq = '=';
    }
    if ((ret = avcodec_open2(enc, ec, &opts)) < 0) die("open encoder", ret);
    if (av_dict_count(opts)) {
        const AVDictionaryEntry *e = NULL;
        /* Reported, not fatal: the driver requires this set to equal the CLI's own "has not been used" set. */
        while ((e = av_dict_iterate(opts, e))) fprintf(stderr, "roi_encode: unused option %s=%s\n", e->key, e->value);
    }

    AVStream *st = avformat_new_stream(oc, NULL);
    avcodec_parameters_from_context(st->codecpar, enc);
    st->codecpar->codec_tag = MKTAG(tag[0], tag[1], tag[2], tag[3]);
    st->time_base = enc->time_base;
    if ((ret = avio_open(&oc->pb, out, AVIO_FLAG_WRITE)) < 0) die("open output", ret);
    AVDictionary *mux = NULL;
    av_dict_set(&mux, "movflags", movflags, 0);
    if ((ret = avformat_write_header(oc, &mux)) < 0) die("write header", ret);

    FILE *mj = fopen(mapjson, "wx");
    if (!mj) die("map json exists or cannot be created", 0);
    fprintf(mj, "{\"mode\":\"%s\",\"grid\":%d,\"threshold_nits\":%.9g,\"qp_offset\":%d,\"qp_range\":63,"
                "\"luminance\":\"block mean of encoder-input Y' (10-bit limited, (Y-64)/876) -> ST2084 EOTF nits\",\"frames\":[",
            mode, grid, thresh, qpoff);

    AVPacket *ipkt = av_packet_alloc();
    AVFrame *fr = av_frame_alloc();
    int n = 0, outcount = 0, eof = 0;
    int bw = (dec->width + grid - 1) / grid, bh = (dec->height + grid - 1) / grid;
    while (n < frames) {
        ret = avcodec_receive_frame(dec, fr);
        if (ret == AVERROR(EAGAIN)) {
            if (eof) die("decoder starved", 0);
            if (av_read_frame(ic, ipkt) < 0) {
                eof = 1;
                avcodec_send_packet(dec, NULL);
                continue;
            }
            if (ipkt->stream_index == vs) avcodec_send_packet(dec, ipkt);
            av_packet_unref(ipkt);
            continue;
        }
        if (ret < 0) die("decode", ret);
        fr->pts = (int64_t)n * den * tbden / num / 1; /* n*1001 at 1/24000 for 24000/1001 */
        /* CLI -force_key_frames expr:eq(mod(n,48),0) -> forced_kf_apply() */
        fr->pict_type = n % 48 == 0 ? AV_PICTURE_TYPE_I : AV_PICTURE_TYPE_NONE;
        fr->color_range = AVCOL_RANGE_MPEG;
        fr->color_primaries = AVCOL_PRI_BT2020;
        fr->color_trc = AVCOL_TRC_SMPTE2084;
        fr->colorspace = AVCOL_SPC_BT2020_NCL;
        int selected = 0;
        double positive = 0;
        if (m_zero) {
            AVFrameSideData *sd = av_frame_new_side_data(fr, AV_FRAME_DATA_REGIONS_OF_INTEREST, sizeof(AVRegionOfInterest));
            AVRegionOfInterest *r = (AVRegionOfInterest *)sd->data;
            *r = (AVRegionOfInterest){.self_size = sizeof *r, .top = 0, .bottom = fr->height, .left = 0, .right = fr->width,
                                      .qoffset = (AVRational){0, 1}};
        } else if (m_luma) {
            unsigned char *sel = calloc((size_t)bw * bh, 1);
            int runs = 0;
            for (int by = 0; by < bh; by++)
                for (int bx = 0; bx < bw; bx++) {
                    double sum = 0;
                    int cnt = 0;
                    for (int y = by * grid; y < (by + 1) * grid && y < fr->height; y++) {
                        const uint16_t *row = (const uint16_t *)(fr->data[0] + (size_t)y * fr->linesize[0]);
                        for (int x = bx * grid; x < (bx + 1) * grid && x < fr->width; x++) sum += row[x], cnt++;
                    }
                    double nits = pq_eotf_nits((sum / cnt - 64.0) / 876.0);
                    if (nits < thresh) {
                        sel[by * bw + bx] = 1;
                        selected++;
                        if (bx == 0 || !sel[by * bw + bx - 1]) runs++;
                    }
                }
            double c = (double)selected / (bw * bh), pos = 0;
            if (m_zm && selected < bw * bh) {
                pos = fabs((double)qpoff) * c / (1 - c);
                if (pos > fabs((double)qpoff)) pos = fabs((double)qpoff);
            }
            positive = pos;
            int extra = pos > 0 ? 1 : 0;
            if (runs + extra) {
                AVFrameSideData *sd = av_frame_new_side_data(fr, AV_FRAME_DATA_REGIONS_OF_INTEREST,
                                                             (size_t)(runs + extra) * sizeof(AVRegionOfInterest));
                AVRegionOfInterest *r = (AVRegionOfInterest *)sd->data;
                int k = 0;
                for (int by = 0; by < bh; by++)
                    for (int bx = 0; bx < bw; bx++) {
                        if (!sel[by * bw + bx] || (bx && sel[by * bw + bx - 1])) continue;
                        int ex = bx;
                        while (ex + 1 < bw && sel[by * bw + ex + 1]) ex++;
                        r[k++] = (AVRegionOfInterest){.self_size = sizeof *r, .top = by * grid, .bottom = (by + 1) * grid,
                                                      .left = bx * grid, .right = (ex + 1) * grid,
                                                      .qoffset = (AVRational){qpoff, 63}};
                    }
                if (k != runs) die("run count mismatch", 0);
                if (extra)
                    r[k++] = (AVRegionOfInterest){.self_size = sizeof *r, .top = 0, .bottom = fr->height, .left = 0,
                                                  .right = fr->width, .qoffset = (AVRational){(int)lround(pos * 1e6), 63000000}};
            }
            free(sel);
        }
        fprintf(mj, "%s{\"frame\":%d,\"selected_blocks\":%d,\"blocks\":%d,\"positive_qp_offset\":%.6f}", n ? "," : "", n,
                selected, bw * bh, positive);
        if ((ret = avcodec_send_frame(enc, fr)) < 0) die("send frame", ret);
        av_frame_unref(fr);
        if ((ret = write_packets(enc, oc, st, &outcount)) < 0) die("receive packet", ret);
        n++;
    }
    avcodec_send_frame(enc, NULL);
    if ((ret = write_packets(enc, oc, st, &outcount)) < 0) die("flush", ret);
    if ((ret = av_write_trailer(oc)) < 0) die("trailer", ret);
    fprintf(mj, "],\"encoded_frames\":%d,\"packets\":%d}\n", n, outcount);
    fclose(mj);
    avio_closep(&oc->pb);
    fprintf(stderr, "roi_encode: %d frames, %d packets\n", n, outcount);
    return outcount == frames ? 0 : 3;
}
