// Actual demux -> pinned dovi_split -> independent HEVC decoders.
#include <stdio.h>
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
};

static struct layer create_layer(const char *name, const char *mode,
                                 AVStream *stream, const char *directory)
{
    struct layer layer = {.name=name,.directory=directory};
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
    int size = snprintf(path,sizeof(path),"%s/%s-frame-%03d.%s",layer->directory,
                        layer->name,layer->frames,suffix);
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
        printf("{\"kind\":\"decoded_frame\",\"layer\":\"%s\",\"display_emission\":%d,",
               layer->name,layer->frames);
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
        printf("{\"kind\":\"split_packet\",\"layer\":\"%s\",\"coded_arrival\":%d,",layer->name,arrival);
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

int main(int argc,char **argv)
{
    require(argc==3,"usage: decode_layers compound.mkv existing-output-dir");
    printf("{\"kind\":\"environment\",\"ffmpeg_version\":\"%s\",\"libavcodec_version\":%u}\n",av_version_info(),avcodec_version());
    AVFormatContext *input=NULL;
    require(avformat_open_input(&input,argv[1],NULL,NULL)>=0 &&
            avformat_find_stream_info(input,NULL)>=0,"open actual container");
    int index=av_find_best_stream(input,AVMEDIA_TYPE_VIDEO,-1,-1,NULL,0);
    require(index>=0,"container video stream");
    AVStream *stream=input->streams[index];
    struct layer bl=create_layer("bl","bl_rpu",stream,argv[2]);
    struct layer el=create_layer("el","el",stream,argv[2]);
    AVPacket *packet=av_packet_alloc();
    require(packet!=NULL,"demux packet allocation");
    int arrival=0,result;
    while ((result=av_read_frame(input,packet))>=0) {
        if (packet->stream_index==index) {
            require(arrival<6,"six-packet demux bound");
            printf("{\"kind\":\"demux_packet\",\"coded_arrival\":%d,",arrival);
            timing("pts",packet->pts,stream->time_base);printf(",");
            timing("dts",packet->dts,stream->time_base);printf(",");
            timing("duration",packet->duration,stream->time_base);printf("}\n");
            send_split(&bl,packet,arrival);
            send_split(&el,packet,arrival);
            arrival++;
        }
        av_packet_unref(packet);
    }
    require(result==AVERROR_EOF && arrival==6,"complete real container read");
    require(avcodec_send_packet(bl.decoder,NULL)>=0 && avcodec_send_packet(el.decoder,NULL)>=0,"decoder drain send");
    receive(&bl);receive(&el);
    printf("{\"kind\":\"summary\",\"bl_frames\":%d,\"el_frames\":%d,\"full_fel_qualified\":false}\n",bl.frames,el.frames);
    av_packet_free(&packet);
    av_bsf_free(&bl.split);av_bsf_free(&el.split);
    avcodec_free_context(&bl.decoder);avcodec_free_context(&el.decoder);
    avformat_close_input(&input);
    return 0;
}
