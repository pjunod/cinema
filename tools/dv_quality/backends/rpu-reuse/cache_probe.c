// Private pinned-source probe: not a production API or an HEVC picture decoder.
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <libavformat/avformat.h>
#include <libavutil/dovi_meta.h>
#include "libavcodec/dovi_rpu.h"

static void require(int ok, const char *why)
{
    if (!ok) {
        fprintf(stderr, "Controlled cache probe refusal: %s\n", why);
        exit(1);
    }
}

static void state(const char *phase, const DOVIContext *context, int result)
{
    printf("{\"kind\":\"cache_state\",\"phase\":\"%s\",\"result\":%d,"
           "\"profile\":%d,\"compression\":%d,\"mapping_present\":%s,"
           "\"cache_zero_present\":%s,\"color_present\":%s,\"luma\":[%lld,%lld],"
           "\"source_min_pq\":%d}\n", phase, result, context->cfg.dv_profile,
           context->cfg.dv_md_compression, context->mapping ? "true" : "false",
           context->vdr[0] ? "true" : "false", context->color ? "true" : "false",
           context->mapping ? (long long)context->mapping->curves[0].poly_coef[0][0] : -1LL,
           context->mapping ? (long long)context->mapping->curves[0].poly_coef[0][1] : -1LL,
           context->color ? context->color->source_min_pq : -1);
    fflush(stdout);
}

static int parse(DOVIContext *context, const char *path)
{
    uint8_t payload[4096];
    FILE *file = fopen(path, "rb");
    require(file != NULL, "RBSP input exists");
    size_t length = fread(payload, 1, sizeof(payload), file);
    require(!ferror(file) && length > 0 && length < sizeof(payload) && fclose(file) == 0,
            "bounded complete RBSP");
    return ff_dovi_rpu_parse(context, payload, length,
                            AV_EF_CAREFUL | AV_EF_COMPLIANT | AV_EF_EXPLODE | AV_EF_CRCCHECK);
}

int main(int argc, char **argv)
{
    require(argc == 5, "usage: cache_probe actual-container seed.rbsp reuse.rbsp same|cold|flush|wrong-compression|no-reset");
    const char *mode = argv[4];
    require(!strcmp(mode, "same") || !strcmp(mode, "cold") || !strcmp(mode, "flush") ||
            !strcmp(mode, "wrong-compression") || !strcmp(mode, "no-reset"), "known mode");
    AVFormatContext *input = NULL;
    require(avformat_open_input(&input, argv[1], NULL, NULL) >= 0 &&
            avformat_find_stream_info(input, NULL) >= 0, "actual muxed container");
    int index = av_find_best_stream(input, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    require(index >= 0, "actual video stream");
    AVCodecParameters *par = input->streams[index]->codecpar;
    const AVPacketSideData *side = av_packet_side_data_get(par->coded_side_data,
        par->nb_coded_side_data, AV_PKT_DATA_DOVI_CONF);
    require(side && side->size == sizeof(AVDOVIDecoderConfigurationRecord), "actual muxed config");
    DOVIContext context = {0};
    memcpy(&context.cfg, side->data, sizeof(context.cfg));
    require(context.cfg.dv_profile == 8 && context.cfg.dv_md_compression == 1 &&
            context.cfg.dv_bl_signal_compatibility_id == 6 && context.cfg.el_present_flag == 0,
            "supported actual configuration");
    state("initial", &context, 0);
    if (strcmp(mode, "cold")) {
        int result = parse(&context, argv[2]);
        state("full_seed", &context, result);
        require(result == 0, "valid actual seed");
    }
    if (!strcmp(mode, "flush")) {
        ff_dovi_ctx_flush(&context);
        state("after_flush_before_parse", &context, 0);
    } else if (!strcmp(mode, "wrong-compression")) {
        context.cfg.dv_md_compression = AV_DOVI_COMPRESSION_NONE;
        state("wrong_compression_before_parse", &context, 0);
    } else if (!strcmp(mode, "no-reset")) {
        state("declared_new_epoch_without_reset", &context, 0);
    }
    int result = parse(&context, argv[3]);
    state("reuse_parse", &context, result);
    int negative = !strcmp(mode, "cold") || !strcmp(mode, "flush") || !strcmp(mode, "wrong-compression");
    require(result == (negative ? AVERROR_INVALIDDATA : 0), "exact expected parser result");
    ff_dovi_ctx_unref(&context);
    avformat_close_input(&input);
    return 0;
}
