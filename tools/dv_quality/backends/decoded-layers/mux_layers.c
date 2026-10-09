// Bounded synthetic fixture muxer. Preserves actual encoder-container timestamps.
#include <stdio.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <libavformat/avformat.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/intreadwrite.h>

#define COUNT 6
#define LIMIT (1024 * 1024)

static void require(int ok, const char *reason)
{
    if (!ok) {
        fprintf(stderr, "Controlled fixture refusal: %s\n", reason);
        exit(1);
    }
}

struct input {
    AVFormatContext *format;
    AVStream *stream;
    AVPacket *packet[COUNT];
};

static struct input open_input(const char *path)
{
    struct input input = {0};
    require(avformat_open_input(&input.format, path, NULL, NULL) >= 0, "input open");
    require(avformat_find_stream_info(input.format, NULL) >= 0, "input stream info");
    int stream = av_find_best_stream(input.format, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    require(stream >= 0, "video stream");
    input.stream = input.format->streams[stream];
    AVCodecParameters *par = input.stream->codecpar;
    require(par->codec_id == AV_CODEC_ID_HEVC && par->width == 64 && par->height == 64,
            "bounded 64x64 HEVC input");
    require(par->extradata_size >= 23 && par->extradata[0] == 1 &&
            (par->extradata[21] & 3) == 3, "four-byte length-prefixed HEVC");
    int count = 0;
    AVPacket *packet = av_packet_alloc();
    require(packet != NULL, "packet allocation");
    int result;
    while ((result = av_read_frame(input.format, packet)) >= 0) {
        if (packet->stream_index == stream) {
            require(count < COUNT, "six-packet input cap");
            require(packet->pts != AV_NOPTS_VALUE && packet->size > 0 &&
                    packet->size <= LIMIT, "valid bounded source packet PTS and size");
            input.packet[count] = av_packet_clone(packet);
            require(input.packet[count] != NULL, "packet clone");
            count++;
        }
        av_packet_unref(packet);
    }
    require(result == AVERROR_EOF && count == COUNT, "six complete encoded access units");
    av_packet_free(&packet);
    return input;
}

static void close_input(struct input *input)
{
    for (int i = 0; i < COUNT; i++)
        av_packet_free(&input->packet[i]);
    avformat_close_input(&input->format);
}

static int append_wrapped(uint8_t *destination, const AVPacket *packet)
{
    int cursor = 0, written = 0;
    while (cursor < packet->size) {
        require(packet->size - cursor >= 4, "inner NAL length prefix");
        uint32_t length = AV_RB32(packet->data + cursor);
        cursor += 4;
        require(length >= 2 && length <= (uint32_t)(packet->size - cursor), "inner NAL bounds");
        AV_WB32(destination + written, length + 2);
        written += 4;
        destination[written++] = 0x7e;
        destination[written++] = 0x01;
        memcpy(destination + written, packet->data + cursor, length);
        written += length;
        cursor += length;
    }
    return written;
}

static void timing(const char *key, int64_t value, AVRational time_base)
{
    printf("\"%s\":", key);
    if (value == AV_NOPTS_VALUE)
        printf("null");
    else
        printf("\"%lld/%d\"", (long long)(value * time_base.num), time_base.den);
}

int main(int argc, char **argv)
{
    require(argc == 6, "usage: mux_layers BL.mkv EL.mkv RPU-dir OUT.mkv normal|missing-el|swapped-el|swapped-rpu|missing-rpu");
    const char *mode = argv[5];
    require(!strcmp(mode,"normal") || !strcmp(mode,"missing-el") || !strcmp(mode,"swapped-rpu") || !strcmp(mode,"swapped-el") || !strcmp(mode,"missing-rpu"), "known control mode");
    struct input bl = open_input(argv[1]), el = open_input(argv[2]);
    AVFormatContext *output = NULL;
    require(avformat_alloc_output_context2(&output, NULL, "matroska", argv[4]) >= 0 && output,
            "output context");
    AVStream *stream = avformat_new_stream(output, NULL);
    require(stream && avcodec_parameters_copy(stream->codecpar, bl.stream->codecpar) >= 0,
            "base codec parameters");
    stream->time_base = bl.stream->time_base;
    AVCodecParameters *par = stream->codecpar;
    AVPacketSideData *configuration = av_packet_side_data_new(&par->coded_side_data,
        &par->nb_coded_side_data, AV_PKT_DATA_DOVI_CONF, sizeof(AVDOVIDecoderConfigurationRecord), 0);
    require(configuration != NULL, "synthetic DV configuration");
    AVDOVIDecoderConfigurationRecord *cfg = (void *)configuration->data;
    memset(cfg, 0, sizeof(*cfg));
    cfg->dv_version_major = 1;
    cfg->dv_profile = 7;
    cfg->dv_level = 1;
    cfg->rpu_present_flag = cfg->el_present_flag = cfg->bl_present_flag = 1;
    cfg->dv_bl_signal_compatibility_id = 6;
    AVPacketSideData *enhancement_configuration = av_packet_side_data_new(&par->coded_side_data,
        &par->nb_coded_side_data, AV_PKT_DATA_HEVC_CONF, el.stream->codecpar->extradata_size, 0);
    require(enhancement_configuration != NULL, "EL HEVC configuration");
    memcpy(enhancement_configuration->data, el.stream->codecpar->extradata,
           el.stream->codecpar->extradata_size);
    require(avio_open(&output->pb, argv[4], AVIO_FLAG_WRITE) >= 0, "output file");
    require(avformat_write_header(output, NULL) >= 0, "container header");
    for (int i = 0; i < COUNT; i++) {
        AVPacket *base = bl.packet[i], *enhancement = el.packet[i];
        require(av_compare_ts(base->pts,bl.stream->time_base,enhancement->pts,el.stream->time_base) == 0,
                "encoder BL/EL packet PTS match");
        require((base->dts == AV_NOPTS_VALUE && enhancement->dts == AV_NOPTS_VALUE) ||
                (base->dts != AV_NOPTS_VALUE && enhancement->dts != AV_NOPTS_VALUE &&
                 av_compare_ts(base->dts,bl.stream->time_base,enhancement->dts,el.stream->time_base) == 0),
                "encoder BL/EL packet DTS match");
        require(base->duration > 0 && enhancement->duration > 0 &&
                av_compare_ts(base->duration,bl.stream->time_base,enhancement->duration,el.stream->time_base) == 0,
                "encoder BL/EL packet durations match");
        // This index chooses a known synthetic RPU tag; it never assigns timestamps.
        int source = av_rescale_q(base->pts, bl.stream->time_base, (AVRational){1,24});
        require(source >= 0 && source < COUNT, "known logical source time");
        int el_source = source;
        if (!strcmp(mode,"swapped-el") && (source == 1 || source == 2)) {
            el_source = 3 - source;
            for (int other = 0; other < COUNT; other++) {
                int candidate = av_rescale_q(el.packet[other]->pts, el.stream->time_base, (AVRational){1,24});
                if (candidate == el_source)
                    enhancement = el.packet[other];
            }
        }
        int rpu_source = source;
        if (!strcmp(mode,"swapped-rpu") && (source == 1 || source == 2))
            rpu_source = 3 - source;
        char path[1024];
        int path_size = snprintf(path,sizeof(path),"%s/p7-frame%d.nal",argv[3],rpu_source);
        require(path_size > 0 && path_size < (int)sizeof(path), "RPU path bounds");
        FILE *file = fopen(path,"rb");
        require(file != NULL, "RPU fixture exists");
        uint8_t rpu[4096];
        size_t rpu_size = fread(rpu,1,sizeof(rpu),file);
        require(!ferror(file) && rpu_size > 2 && rpu_size < sizeof(rpu), "bounded RPU fixture");
        require(fclose(file) == 0 && rpu[0] == 0x7c && rpu[1] == 1, "complete UNSPEC62 RPU fixture");
        AVPacket *combined = av_packet_alloc();
        require(combined && av_new_packet(combined, base->size + 3*enhancement->size + (int)rpu_size + 4) >= 0,
                "bounded compound access unit allocation");
        require(av_packet_copy_props(combined,base) >= 0, "preserve packet properties");
        memcpy(combined->data,base->data,base->size);
        int used = base->size;
        bool omitted = !strcmp(mode,"missing-el") && source == 2;
        if (!omitted)
            used += append_wrapped(combined->data + used,enhancement);
        bool rpu_omitted = !strcmp(mode,"missing-rpu") && source == 2;
        if (!rpu_omitted) {
            AV_WB32(combined->data+used,rpu_size);
            used += 4;
            memcpy(combined->data+used,rpu,rpu_size);
            used += rpu_size;
        }
        av_shrink_packet(combined,used);
        combined->stream_index = stream->index;
        printf("{\"kind\":\"mux_packet\",\"coded_arrival\":%d,\"logical_source_frame\":%d,\"rpu_source_frame\":%d,\"el_source_frame\":%d,\"el_omitted\":%s,\"rpu_omitted\":%s,",i,source,rpu_source,el_source,omitted?"true":"false",rpu_omitted?"true":"false");
        timing("pts",base->pts,bl.stream->time_base);printf(",");
        timing("dts",base->dts,bl.stream->time_base);printf("}\n");
        av_packet_rescale_ts(combined,bl.stream->time_base,stream->time_base);
        require(av_interleaved_write_frame(output,combined) >= 0, "write compound packet");
        av_packet_free(&combined);
    }
    require(av_write_trailer(output) >= 0, "container trailer");
    require(avio_closep(&output->pb) >= 0, "container close");
    avformat_free_context(output);
    close_input(&bl);close_input(&el);
    return 0;
}
