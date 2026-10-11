/* Original decoder seek/flush association experiment. CC0-1.0. */
#include <stdio.h>
#include <string.h>
#include "libavcodec/avcodec.h"
#include "libavformat/avformat.h"
#include "libavutil/opt.h"
#include "libavutil/dovi_meta.h"
#include "libavutil/hwcontext.h"
#include "libavutil/log.h"

static enum AVPixelFormat hardware_format(AVCodecContext *codec, const enum AVPixelFormat *formats)
{
    (void)codec;
    for (const enum AVPixelFormat *format = formats; *format != AV_PIX_FMT_NONE; format++)
        if (*format == AV_PIX_FMT_VIDEOTOOLBOX)
            return *format;
    return AV_PIX_FMT_NONE;
}

static int read_frames(AVFormatContext *format, AVCodecContext *codec, int stream,
                       int limit, int *frames, int64_t forbidden, int *published)
{
    AVPacket *packet = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    int ret = 0;
    while (*frames < limit && (ret = av_read_frame(format, packet)) >= 0) {
        if (packet->stream_index != stream) {
            av_packet_unref(packet);
            continue;
        }
        ret = avcodec_send_packet(codec, packet);
        av_packet_unref(packet);
        if (ret < 0)
            break;
        while ((ret = avcodec_receive_frame(codec, frame)) >= 0) {
            if (frame->pts == forbidden)
                *published = 1;
            if (!av_frame_get_side_data(frame, AV_FRAME_DATA_DOVI_METADATA)) {
                ret = AVERROR_INVALIDDATA;
                goto done;
            }
            AVFrameSideData *side = av_frame_get_side_data(frame, AV_FRAME_DATA_DOVI_METADATA);
            const AVDOVIColorMetadata *color = av_dovi_get_color((AVDOVIMetadata *)side->data);
            printf("frame_pts=%lld dm_metadata_id=%u source_max_pq=%u\n",
                   (long long)frame->pts, (unsigned)color->dm_metadata_id, (unsigned)color->source_max_pq);
            (*frames)++;
            av_frame_unref(frame);
        }
        if (ret != AVERROR(EAGAIN) && ret != AVERROR_EOF)
            break;
        ret = 0;
    }
 done:
    av_packet_free(&packet);
    av_frame_free(&frame);
    return ret;
}

int main(int argc, char **argv)
{
    AVFormatContext *format = NULL;
    AVCodecContext *codec = NULL;
    const AVCodec *decoder;
    int stream, ret, before = 0, after = 0, published = 0;
    int negative;
    int64_t target;
    if ((argc != 3 && argc != 4) || (strcmp(argv[2], "positive") && strcmp(argv[2], "negative")))
        return 2;
    negative = !strcmp(argv[2], "negative");
    if (avformat_open_input(&format, argv[1], NULL, NULL) < 0 ||
        avformat_find_stream_info(format, NULL) < 0)
        return 3;
    stream = av_find_best_stream(format, AVMEDIA_TYPE_VIDEO, -1, -1, &decoder, 0);
    if (stream < 0 || !(codec = avcodec_alloc_context3(decoder)))
        return 4;
    avcodec_parameters_to_context(codec, format->streams[stream]->codecpar);
    codec->thread_count = 1;
    codec->err_recognition = AV_EF_EXPLODE;
    if (argc == 4) {
        if (strcmp(argv[3], "videotoolbox") ||
            av_hwdevice_ctx_create(&codec->hw_device_ctx, AV_HWDEVICE_TYPE_VIDEOTOOLBOX, NULL, NULL, 0) < 0)
            return 10;
        av_log_set_level(AV_LOG_VERBOSE);
        codec->get_format = hardware_format;
        codec->hwaccel_flags |= AV_HWACCEL_FLAG_REQUIRE_HARDWARE;
    }
    if (av_opt_set(codec->priv_data, "strict_dovi", "1", 0) < 0 ||
        avcodec_open2(codec, decoder, NULL) < 0)
        return 5;
    ret = read_frames(format, codec, stream, 3, &before, -1, &published);
    if (ret < 0 || before != 3)
        return 6;
    target = av_rescale_q(1, (AVRational){1, 1}, format->streams[stream]->time_base);
    ret = avformat_seek_file(format, stream, INT64_MIN, target, target, AVSEEK_FLAG_BACKWARD);
    if (ret < 0)
        return 7;
    avcodec_flush_buffers(codec);
    published = 0;
    ret = read_frames(format, codec, stream, 3, &after, target, &published);
    printf("before=%d after=%d decode_result=%d affected_frame_published=%d\n",
           before, after, ret, published);
    avcodec_free_context(&codec);
    avformat_close_input(&format);
    if (negative)
        return ret == AVERROR_INVALIDDATA && after == 0 && !published ? 0 : 8;
    return ret >= 0 && after == 3 && published ? 0 : 9;
}
