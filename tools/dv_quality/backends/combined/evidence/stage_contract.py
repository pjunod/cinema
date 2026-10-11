"""Fixed six-picture proof invocation contract, independent of recorded events."""
from pathlib import Path

def contract(stage,pairs):
    common={'cwd':'/work','environment':{'LD_LIBRARY_PATH':None,'VK_ICD_FILENAMES':None,'XDG_RUNTIME_DIR':None,'LC_ALL':'C'}}
    if stage=='decode':argv=['/work/decode_layers','/work/source.mkv','/work/b2-normal/frames'];inputs=['source.mkv'];outputs=[f'b2-normal/frames/{layer}-frame-{i:03d}.yuv420p10le' for layer in ['bl','el'] for i in range(6)]+[f'b2-normal/frames/bl-frame-{i:03d}.rpu.nal' for i in range(6)]
    elif stage.startswith('render-'):
        i=int(stage[7:]);pair=pairs[i];inputs=[pair[k] for k in ['rpu_path','bl_path','el_path']]
        argv=['/work/fel_export',*['/work/'+p for p in inputs],pair['pts'],pair['duration']]
        outputs=[f'render-{i}/outputs/nonzero-{s}.{f}' for s in ['bl','el','reconstruction','rendered'] for f in ['rgba32f','rgb48le']]
        common['cwd']=f'/work/render-{i}';common['environment'].update(LD_LIBRARY_PATH='/work/prefix/lib/aarch64-linux-gnu',VK_ICD_FILENAMES='/usr/share/vulkan/icd.d/lvp_icd.aarch64.json',XDG_RUNTIME_DIR='/work/runtime')
    elif stage=='mux-rgb':argv=['/work/mux_rgb','/work/rgb48le.bin','/work/timing.tsv','/work/reconstructed.nut'];inputs=['rgb48le.bin','timing.tsv'];outputs=['reconstructed.nut']
    elif stage=='encode':
        argv=['/usr/bin/ffmpeg','-nostdin','-hide_banner','-n','-copyts','-i','/work/reconstructed.nut','-an','-vf','zscale=matrixin=gbr:primariesin=2020:transferin=smpte2084:rangein=full:matrix=2020_ncl:primaries=2020:transfer=smpte2084:range=limited:chromal=center:filter=point,format=yuv420p10le','-fps_mode','passthrough','-enc_time_base','1/1000','-r','24','-c:v','libx265','-profile:v','main10','-preset','ultrafast','-x265-params','qp=0:bframes=0:keyint=24:scenecut=0:repeat-headers=1:chromaloc=1:colorprim=9:transfer=16:colormatrix=9:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(100000000,50):pools=1:frame-threads=1','-color_primaries','bt2020','-color_trc','smpte2084','-colorspace','bt2020nc','-color_range','tv','/work/hdr10.mkv'];inputs=['reconstructed.nut'];outputs=['hdr10.mkv']
    elif stage=='probe':argv=['/usr/bin/ffprobe','-v','error','-show_streams','-show_packets','-show_frames','-of','json','/work/hdr10.mkv'];inputs=['hdr10.mkv'];outputs=[]
    elif stage=='decode-output':argv=['/usr/bin/ffmpeg','-nostdin','-hide_banner','-n','-i','/work/hdr10.mkv','-map','0:v:0','-fps_mode','passthrough','-pix_fmt','yuv420p10le','-f','rawvideo','/work/decoded.yuv420p10le'];inputs=['hdr10.mkv'];outputs=['decoded.yuv420p10le']
    elif stage=='decode-output-rgb':argv=['/usr/bin/ffmpeg','-nostdin','-hide_banner','-n','-i','/work/hdr10.mkv','-map','0:v:0','-fps_mode','passthrough','-vf','zscale=matrixin=2020_ncl:primariesin=2020:transferin=smpte2084:rangein=limited:chromalin=center:matrix=gbr:primaries=2020:transfer=smpte2084:range=full:filter=point:chromal=center:dither=none,format=gbrp16le','-pix_fmt','rgb48le','-f','rawvideo','/work/decoded.rgb48le'];inputs=['hdr10.mkv'];outputs=['decoded.rgb48le']
    else:raise ValueError('unknown stage')
    return dict(common,argv=argv,inputs=inputs,outputs=outputs)
