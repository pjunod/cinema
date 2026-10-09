// Finite real timestamped raw-RGB intermediate using public libavformat.
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/pixfmt.h>
static void need(int ok,const char *msg){if(!ok){fprintf(stderr,"RGB mux refused: %s\n",msg);exit(1);}}
int main(int argc,char **argv){
 need(argc==4,"usage: mux_rgb RGB48LE TIMING.tsv OUTPUT.nut");
 FILE *rgb=fopen(argv[1],"rb"),*timing=fopen(argv[2],"r");need(rgb&&timing,"inputs");
 AVFormatContext *out=NULL;need(avformat_alloc_output_context2(&out,NULL,"nut",argv[3])>=0&&out,"NUT muxer");
 AVStream *stream=avformat_new_stream(out,NULL);need(stream!=NULL,"stream");
 stream->time_base=(AVRational){1,1000};stream->avg_frame_rate=(AVRational){24,1};
 AVCodecParameters *p=stream->codecpar;p->codec_type=AVMEDIA_TYPE_VIDEO;p->codec_id=AV_CODEC_ID_RAWVIDEO;
 p->format=AV_PIX_FMT_RGB48LE;p->codec_tag=avcodec_pix_fmt_to_codec_tag(AV_PIX_FMT_RGB48LE);need(p->codec_tag!=0,"explicit RGB48 codec tag");p->width=64;p->height=64;p->bits_per_coded_sample=48;
 p->color_primaries=AVCOL_PRI_BT2020;p->color_trc=AVCOL_TRC_SMPTE2084;p->color_space=AVCOL_SPC_RGB;p->color_range=AVCOL_RANGE_JPEG;
 need(avio_open(&out->pb,argv[3],AVIO_FLAG_WRITE)>=0,"output open");need(avformat_write_header(out,NULL)>=0,"header");
 for(int i=0;i<6;i++){
  long long pts,duration;need(fscanf(timing,"%lld\t%lld",&pts,&duration)==2&&pts>=0&&duration>0,"six exact timing rows");
  AVPacket *pkt=av_packet_alloc();need(pkt&&av_new_packet(pkt,24576)>=0,"packet");
  need(fread(pkt->data,1,24576,rgb)==24576,"full RGB frame");pkt->stream_index=0;pkt->pts=pkt->dts=pts;pkt->duration=duration;pkt->flags=AV_PKT_FLAG_KEY;
  av_packet_rescale_ts(pkt,(AVRational){1,1000},stream->time_base);need(av_interleaved_write_frame(out,pkt)>=0,"timestamped packet write");av_packet_free(&pkt);
 }
 int c;do{c=fgetc(timing);}while(c=='\n'||c=='\r'||c==' '||c=='\t');need(c==EOF&&!ferror(timing),"no extra timing");need(fgetc(rgb)==EOF&&!ferror(rgb),"no extra RGB");
 need(av_write_trailer(out)>=0,"trailer");need(avio_closep(&out->pb)>=0,"output close");avformat_free_context(out);need(fclose(rgb)==0&&fclose(timing)==0,"input close");return 0;
}
