// Actual demux -> pinned dovi_split -> independent HEVC decoders.
#include <stdio.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <libavformat/avformat.h>
#include <libavcodec/bsf.h>
#include <libavcodec/avcodec.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/opt.h>

static void require(int ok, const char *reason)
{
    if (!ok) {
        fprintf(stderr,"Controlled decode refusal: %s\n",reason);
        exit(1);
    }
}

static void timing(const char *key, int64_t value, AVRational time_base)
{
    printf("\"%s\":",key);
    if (value == AV_NOPTS_VALUE)
        printf("null");
    else
        printf("\"%lld/%d\"",(long long)(value*time_base.num),time_base.den);
}

struct layer {
    const char *name;
    const char *directory;
    AVBSFContext *split;
    AVCodecContext *decoder;
    int frames;
    int epoch;
    int context_id;
};

static struct layer create_layer(const char *name, const char *mode,
                                 AVStream *stream, const char *directory, int epoch, int context_id)
{
    struct layer layer = {.name=name,.directory=directory,.epoch=epoch,.context_id=context_id};
    const AVBitStreamFilter *filter = av_bsf_get_by_name("dovi_split");
    require(filter && av_bsf_alloc(filter,&layer.split) >= 0, "pinned dovi_split present");
    require(avcodec_parameters_copy(layer.split->par_in,stream->codecpar) >= 0,
            "split input parameters");
    layer.split->time_base_in = stream->time_base;
    require(av_opt_set(layer.split,"mode",mode,AV_OPT_SEARCH_CHILDREN) >= 0 &&
            av_bsf_init(layer.split) >= 0, "split initialization");
    const AVCodec *codec = avcodec_find_decoder(AV_CODEC_ID_HEVC);
    require(codec != NULL, "HEVC decoder present");
    layer.decoder = avcodec_alloc_context3(codec);
    require(layer.decoder && avcodec_parameters_to_context(layer.decoder,layer.split->par_out) >= 0,
            "independent decoder context");
    layer.decoder->pkt_timebase = layer.split->time_base_out;
    layer.decoder->thread_count = 1;
    layer.decoder->err_recognition = AV_EF_CAREFUL | AV_EF_EXPLODE;
    require(avcodec_open2(layer.decoder,codec,NULL) >= 0, "open HEVC decoder");
    return layer;
}

static FILE *open_artifact(const struct layer *layer, const char *suffix)
{
    char path[1024];
    int size = snprintf(path,sizeof(path),"%s/epoch-%d-%s-frame-%03d.%s",layer->directory,
                        layer->epoch,layer->name,layer->frames,suffix);
    require(size > 0 && size < (int)sizeof(path), "artifact path bounded");
    FILE *file = fopen(path,"wb");
    require(file != NULL,"artifact open");
    return file;
}

static void receive(struct layer *layer)
{
    AVFrame *frame = av_frame_alloc();
    require(frame != NULL,"frame allocation");
    int result;
    while ((result=avcodec_receive_frame(layer->decoder,frame)) >= 0) {
        require(layer->frames < 6 && frame->width == 64 && frame->height == 64 &&
                frame->format == AV_PIX_FMT_YUV420P10LE,"bounded native decoded frame");
        require(frame->pts != AV_NOPTS_VALUE && !(frame->flags & AV_FRAME_FLAG_CORRUPT),
                "actual decoded PTS and noncorrupt frame");
        FILE *pixels = open_artifact(layer,"yuv420p10le");
        for (int component=0;component<3;component++) {
            int width=component?32:64, height=component?32:64;
            for (int row=0;row<height;row++) {
                require(fwrite(frame->data[component]+row*frame->linesize[component],2,width,pixels)==(size_t)width,
                        "decoded row write");
            }
        }
        require(fclose(pixels)==0,"decoded frame close");
        AVFrameSideData *rpu=av_frame_get_side_data(frame,AV_FRAME_DATA_DOVI_RPU_BUFFER);
        AVFrameSideData *meta=av_frame_get_side_data(frame,AV_FRAME_DATA_DOVI_METADATA);
        int min=-1,max=-1,average=-1;
        if (meta) {
            const AVDOVIMetadata *dovi=(void *)meta->data;
            const AVDOVIDmData *l1=av_dovi_find_level(dovi,1);
            if (l1) {
                min=l1->l1.min_pq; max=l1->l1.max_pq; average=l1->l1.avg_pq;
            }
        }
        if (rpu) {
            require(rpu->size > 0 && rpu->size < 4096,"bounded decoder-attached RPU");
            FILE *metadata=open_artifact(layer,"rpu.nal");
            const uint8_t header[]={0x7c,0x01};
            require(fwrite(header,1,2,metadata)==2 &&
                    fwrite(rpu->data,1,rpu->size,metadata)==rpu->size,
                    "decoder-attached RPU write");
            require(fclose(metadata)==0,"RPU artifact close");
        }
        printf("{\"kind\":\"decoded_frame\",\"layer\":\"%s\",\"epoch\":%d,\"context_id\":%d,\"display_emission\":%d,",
               layer->name,layer->epoch,layer->context_id,layer->frames);
        timing("pts",frame->pts,layer->decoder->pkt_timebase);printf(",");
        timing("best_effort_pts",frame->best_effort_timestamp,layer->decoder->pkt_timebase);printf(",");
        timing("pkt_dts",frame->pkt_dts,layer->decoder->pkt_timebase);printf(",");
        timing("duration",frame->duration,layer->decoder->pkt_timebase);
        printf(",\"picture_type\":\"%c\",\"decoder_rpu_present\":%s,\"decoder_metadata_present\":%s,"
               "\"l1_min\":%d,\"l1_max\":%d,\"l1_average\":%d}\n",
               av_get_picture_type_char(frame->pict_type),rpu?"true":"false",meta?"true":"false",
               min,max,average);
        layer->frames++;
        av_frame_unref(frame);
    }
    require(result==AVERROR(EAGAIN)||result==AVERROR_EOF,"controlled decoder receive");
    av_frame_free(&frame);
}

static void send_split(struct layer *layer, AVPacket *packet, int arrival)
{
    AVPacket *copy=av_packet_clone(packet);
    require(copy && av_bsf_send_packet(layer->split,copy)>=0,"split packet send");
    av_packet_free(&copy);
    AVPacket *split=av_packet_alloc();
    require(split!=NULL,"split output allocation");
    int result;
    while ((result=av_bsf_receive_packet(layer->split,split))>=0) {
        require(split->pts!=AV_NOPTS_VALUE,"split retains real packet PTS");
        printf("{\"kind\":\"split_packet\",\"layer\":\"%s\",\"epoch\":%d,\"coded_arrival\":%d,",layer->name,layer->epoch,arrival);
        timing("pts",split->pts,layer->split->time_base_out);printf(",");
        timing("dts",split->dts,layer->split->time_base_out);printf(",");
        timing("duration",split->duration,layer->split->time_base_out);printf("}\n");
        require(avcodec_send_packet(layer->decoder,split)>=0,"HEVC packet decode send");
        receive(layer);
        av_packet_unref(split);
    }
    require(result==AVERROR(EAGAIN)||result==AVERROR_EOF,"split receives controlled result");
    av_packet_free(&split);
}


static AVFormatContext *open_container(const char *path, int *index)
{
    AVFormatContext *input = NULL;
    require(avformat_open_input(&input, path, NULL, NULL) >= 0 &&
            avformat_find_stream_info(input, NULL) >= 0, "open actual container");
    *index = av_find_best_stream(input, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    require(*index >= 0, "container video stream");
    return input;
}

static void context_timing(AVStream *stream, const char *phase)
{
    printf("{\"kind\":\"stream_timing_context\",\"phase\":\"%s\","
           "\"time_base\":\"%d/%d\",\"r_frame_rate\":\"%d/%d\","
           "\"avg_frame_rate\":\"%d/%d\",\"codec_framerate\":\"%d/%d\"}\n",
           phase, stream->time_base.num, stream->time_base.den,
           stream->r_frame_rate.num, stream->r_frame_rate.den,
           stream->avg_frame_rate.num, stream->avg_frame_rate.den,
           stream->codecpar->framerate.num, stream->codecpar->framerate.den);
}

static int read_part(AVFormatContext *input, int index, struct layer *bl,
                     struct layer *el, int limit)
{
    AVPacket *packet = av_packet_alloc();
    require(packet != NULL, "demux packet allocation");
    int arrival = 0, result = 0;
    while (arrival < limit && (result = av_read_frame(input, packet)) >= 0) {
        if (packet->stream_index == index) {
            printf("{\"kind\":\"demux_packet\",\"epoch\":%d,\"coded_arrival\":%d,", bl->epoch, arrival);
            timing("pts", packet->pts, input->streams[index]->time_base); printf(",");
            timing("dts", packet->dts, input->streams[index]->time_base); printf(",");
            timing("duration", packet->duration, input->streams[index]->time_base);
            printf(",\"keyframe\":%s}\n", packet->flags & AV_PKT_FLAG_KEY ? "true" : "false");
            send_split(bl, packet, arrival);
            send_split(el, packet, arrival);
            arrival++;
        }
        av_packet_unref(packet);
    }
    require(result >= 0 || result == AVERROR_EOF, "controlled actual container read");
    printf("{\"kind\":\"read_boundary\",\"epoch\":%d,\"packets\":%d,\"eof\":%s}\n",
           bl->epoch, arrival, result == AVERROR_EOF ? "true" : "false");
    av_packet_free(&packet);
    return arrival;
}

static void drain(struct layer *bl, struct layer *el)
{
    require(avcodec_send_packet(bl->decoder, NULL) >= 0 &&
            avcodec_send_packet(el->decoder, NULL) >= 0, "decoder drain send");
    receive(bl); receive(el);
    printf("{\"kind\":\"drain\",\"epoch\":%d,\"bl_frames\":%d,\"el_frames\":%d}\n",
           bl->epoch, bl->frames, el->frames);
}

static void destroy(struct layer *layer)
{
    av_bsf_free(&layer->split);
    avcodec_free_context(&layer->decoder);
}

int main(int argc, char **argv)
{
    require(argc == 4 || argc == 5,
            "usage: decode_timeline INPUT existing-output-dir normal|seek-reset|seek-no-reset|epochs-reset|epochs-no-reset [second-input]");
    const char *mode = argv[3];
    bool seek = !strcmp(mode, "seek-reset") || !strcmp(mode, "seek-no-reset");
    bool epochs = !strcmp(mode, "epochs-reset") || !strcmp(mode, "epochs-no-reset");
    bool reset = !strcmp(mode, "seek-reset") || !strcmp(mode, "epochs-reset");
    require(!strcmp(mode, "normal") || seek || epochs, "known timeline mode");
    require(epochs ? argc == 5 : argc == 4, "known timeline argument count");
    printf("{\"kind\":\"environment\",\"ffmpeg_version\":\"%s\",\"libavcodec_version\":%u}\n",
           av_version_info(), avcodec_version());
    int index;
    AVFormatContext *input = open_container(argv[1], &index);
    AVStream *stream = input->streams[index];
    context_timing(stream, "initial_open");
    struct layer bl = create_layer("bl", "bl_rpu", stream, argv[2], 0, 0);
    struct layer el = create_layer("el", "el", stream, argv[2], 0, 0);
    printf("{\"kind\":\"epoch_start\",\"epoch\":0,\"context_id\":0,\"fresh_contexts\":true}\n");
    if (seek) {
        require(read_part(input, index, &bl, &el, 2) == 2, "two real packets before seek");
        int64_t target = av_rescale_q(200, (AVRational){1, 1000}, stream->time_base);
        int result = avformat_seek_file(input, index, INT64_MIN, target, target, AVSEEK_FLAG_BACKWARD);
        require(result >= 0, "actual demuxer seek");
        context_timing(stream, "after_seek");
        if (reset) {
            av_bsf_flush(bl.split); av_bsf_flush(el.split);
            avcodec_flush_buffers(bl.decoder); avcodec_flush_buffers(el.decoder);
        }
        printf("{\"kind\":\"epoch_boundary\",\"from\":0,\"to\":1,\"reason\":\"actual_demux_seek\","
               "\"seek_result\":%d,\"target\":\"200/1000\",\"reset_applied\":%s,"
               "\"bsf_flush_calls\":%d,\"decoder_flush_calls\":%d}\n", result, reset ? "true" : "false", reset ? 2 : 0, reset ? 2 : 0);
        bl.epoch = el.epoch = 1;
        bl.frames = el.frames = 0;
        require(read_part(input, index, &bl, &el, 7) > 0, "real packets after seek");
        drain(&bl, &el);
    } else if (epochs) {
        require(read_part(input, index, &bl, &el, 7) == 3, "first independent segment packets");
        if (reset) drain(&bl, &el);
        avformat_close_input(&input);
        if (reset) { destroy(&bl); destroy(&el); }
        input = open_container(argv[4], &index);
        stream = input->streams[index];
        context_timing(stream, "second_segment_open");
        if (reset) {
            bl = create_layer("bl", "bl_rpu", stream, argv[2], 1, 1);
            el = create_layer("el", "el", stream, argv[2], 1, 1);
        } else {
            bl.epoch = el.epoch = 1;
            bl.frames = el.frames = 0;
        }
        printf("{\"kind\":\"epoch_boundary\",\"from\":0,\"to\":1,\"reason\":\"new_independent_segment_timestamp_reset\","
               "\"reset_applied\":%s,\"fresh_contexts\":%s,\"context_id\":%d}\n",
               reset ? "true" : "false", reset ? "true" : "false", reset ? 1 : 0);
        require(read_part(input, index, &bl, &el, 7) == 3, "second independent segment packets");
        drain(&bl, &el);
    } else {
        require(read_part(input, index, &bl, &el, 7) == 6, "six real VFR packets");
        drain(&bl, &el);
    }
    destroy(&bl); destroy(&el);
    avformat_close_input(&input);
    return 0;
}
