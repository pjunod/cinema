// Offline full-RPU insertion and public pinned compression BSF; no runtime compressor.
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <libavcodec/bsf.h>
#include <libavformat/avformat.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/intreadwrite.h>
#include <libavutil/opt.h>

static void require(int ok, const char *why)
{
    if (!ok) {
        fprintf(stderr, "Controlled mux refusal: %s\n", why);
        exit(1);
    }
}

int main(int argc, char **argv)
{
    require(argc == 5, "usage: mux_reuse BL.mkv full-RPU-dir OUT.mkv normal|missing-rpu|p7-refusal");
    require(!strcmp(argv[4], "normal") || !strcmp(argv[4], "missing-rpu") ||
            !strcmp(argv[4], "p7-refusal"), "known mode");
    AVFormatContext *input = NULL, *output = NULL;
    require(avformat_open_input(&input, argv[1], NULL, NULL) >= 0 &&
            avformat_find_stream_info(input, NULL) >= 0, "input container");
    int index = av_find_best_stream(input, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    require(index >= 0, "video stream");
    AVStream *source = input->streams[index];
    require(source->codecpar->codec_id == AV_CODEC_ID_HEVC && source->codecpar->width == 64 &&
            source->codecpar->height == 64 && source->codecpar->profile == AV_PROFILE_HEVC_MAIN_10,
            "actual bounded Main10 base");
    require(source->codecpar->extradata_size >= 23 && source->codecpar->extradata[0] == 1 &&
            (source->codecpar->extradata[21] & 3) == 3, "four-byte NAL lengths");
    AVBSFContext *compressor = NULL;
    require(av_bsf_alloc(av_bsf_get_by_name("dovi_rpu"), &compressor) >= 0 &&
            avcodec_parameters_copy(compressor->par_in, source->codecpar) >= 0,
            "public compression filter");
    AVPacketSideData *side = av_packet_side_data_new(&compressor->par_in->coded_side_data,
        &compressor->par_in->nb_coded_side_data, AV_PKT_DATA_DOVI_CONF,
        sizeof(AVDOVIDecoderConfigurationRecord), 0);
    require(side != NULL, "DV input configuration");
    AVDOVIDecoderConfigurationRecord *cfg = (void *)side->data;
    memset(cfg, 0, sizeof(*cfg));
    cfg->dv_version_major = 1;
    cfg->dv_profile = !strcmp(argv[4], "p7-refusal") ? 7 : 8;
    cfg->dv_level = 1;
    cfg->rpu_present_flag = cfg->bl_present_flag = 1;
    cfg->dv_bl_signal_compatibility_id = 6;
    compressor->time_base_in = source->time_base;
    require(av_opt_set(compressor, "compression", "limited", AV_OPT_SEARCH_CHILDREN) >= 0,
            "limited compression option");
    int status = av_bsf_init(compressor);
    printf("{\"kind\":\"compressor_init\",\"status\":%d,\"requested_profile\":%d}\n", status, cfg->dv_profile);
    fflush(stdout);
    require(status >= 0, "compression initialization");
    require(avformat_alloc_output_context2(&output, NULL, "matroska", argv[3]) >= 0 && output,
            "output context");
    AVStream *stream = avformat_new_stream(output, NULL);
    require(stream && avcodec_parameters_copy(stream->codecpar, compressor->par_out) >= 0,
            "actual compressor output configuration");
    stream->time_base = compressor->time_base_out;
    stream->avg_frame_rate = stream->r_frame_rate = (AVRational){25, 1};
    require(avio_open(&output->pb, argv[3], AVIO_FLAG_WRITE) >= 0 &&
            avformat_write_header(output, NULL) >= 0, "output header");
    AVPacket *packet = av_packet_alloc(), *encoded = av_packet_alloc();
    require(packet && encoded, "packet allocation");
    const int64_t pts_ms[] = {0, 40, 110, 140, 230, 300};
    int count = 0, read_status;
    while ((read_status = av_read_frame(input, packet)) >= 0) {
        if (packet->stream_index != index) {
            av_packet_unref(packet);
            continue;
        }
        require(count < 6 && packet->size > 0 && packet->size < 1024 * 1024,
                "bounded six access units");
        int picture = -1;
        for (int i = 0; i < 6; i++)
            if (!av_compare_ts(packet->pts, source->time_base, pts_ms[i], (AVRational){1, 1000}))
                picture = i;
        require(picture >= 0, "actual PTS matches authored picture");
        char path[1024];
        int n = snprintf(path, sizeof(path), "%s/p8-frame%d.nal", argv[2], picture);
        require(n > 0 && n < (int)sizeof(path), "RPU path bounded");
        FILE *file = fopen(path, "rb");
        require(file != NULL, "full RPU exists");
        uint8_t rpu[4096];
        size_t size = fread(rpu, 1, sizeof(rpu), file);
        require(!ferror(file) && size > 2 && size < sizeof(rpu) && fclose(file) == 0 &&
                rpu[0] == 0x7c && rpu[1] == 1, "complete bounded RPU NAL");
        require(av_grow_packet(packet, size + 4) >= 0, "insert RPU allocation");
        AV_WB32(packet->data + packet->size - size - 4, size);
        memcpy(packet->data + packet->size - size, rpu, size);
        packet->duration = av_rescale_q(picture < 5 ? pts_ms[picture + 1] - pts_ms[picture] : 41,
                                       (AVRational){1, 1000}, source->time_base);
        require(av_bsf_send_packet(compressor, packet) >= 0 &&
                av_bsf_receive_packet(compressor, encoded) >= 0, "actual compression result");
        // The public CBS filter emits Annex B, even with length-prefixed input.
        int rpu_prefix = -1, rpu_start = -1, rpu_length = 0;
        for (int scan = 0; scan + 3 < encoded->size; scan++) {
            int prefix = 0;
            if (encoded->data[scan] == 0 && encoded->data[scan + 1] == 0) {
                if (encoded->data[scan + 2] == 1)
                    prefix = 3;
                else if (encoded->data[scan + 2] == 0 && encoded->data[scan + 3] == 1)
                    prefix = 4;
            }
            if (!prefix)
                continue;
            int begin = scan + prefix;
            require(begin + 2 <= encoded->size, "emitted Annex B NAL header");
            if (((encoded->data[begin] >> 1) & 63) == 62) {
                require(rpu_start < 0, "one emitted RPU per packet");
                rpu_prefix = scan;
                rpu_start = begin;
                rpu_length = encoded->size - begin;
            } else {
                require(rpu_start < 0, "emitted RPU is last NAL");
            }
            scan = begin + 1;
        }
        require(rpu_start >= 0 && rpu_length > 2, "actual emitted RPU");
        n = snprintf(path, sizeof(path), "%s.picture-%d.nal", argv[3], picture);
        require(n > 0 && n < (int)sizeof(path), "emitted RPU path");
        file = fopen(path, "wb");
        require(file && fwrite(encoded->data + rpu_start, 1, rpu_length, file) == (size_t)rpu_length &&
                fclose(file) == 0, "actual emitted packet RPU artifact");
        if (!strcmp(argv[4], "missing-rpu") && picture == 2)
            av_shrink_packet(encoded, rpu_prefix);
        printf("{\"kind\":\"compressed_packet\",\"coded_arrival\":%d,\"picture\":%d,"
               "\"pts\":\"%lld/%d\",\"duration\":\"%lld/%d\",\"keyframe\":%s}\n",
               count, picture, (long long)(encoded->pts * source->time_base.num), source->time_base.den,
               (long long)(encoded->duration * source->time_base.num), source->time_base.den,
               encoded->flags & AV_PKT_FLAG_KEY ? "true" : "false");
        av_packet_rescale_ts(encoded, compressor->time_base_out, stream->time_base);
        encoded->stream_index = stream->index;
        require(av_interleaved_write_frame(output, encoded) >= 0, "compressed packet mux");
        av_packet_unref(encoded);
        count++;
    }
    require(read_status == AVERROR_EOF && count == 6, "six complete source packets");
    require(av_bsf_send_packet(compressor, NULL) >= 0 &&
            av_bsf_receive_packet(compressor, encoded) == AVERROR_EOF, "offline compressor drain");
    require(av_write_trailer(output) >= 0 && avio_closep(&output->pb) >= 0, "container close");
    av_packet_free(&packet);
    av_packet_free(&encoded);
    av_bsf_free(&compressor);
    avformat_close_input(&input);
    avformat_free_context(output);
    return 0;
}
