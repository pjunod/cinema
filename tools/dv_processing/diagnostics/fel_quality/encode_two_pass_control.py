import pathlib,subprocess,json,time,hashlib,argparse,shutil,fractions
p=argparse.ArgumentParser(description="Calibrated-export candidate with explicit fixture cadence/static metadata");p.add_argument("--input",required=True);p.add_argument("--output-root",required=True);p.add_argument("--fixture",choices=["michael1800","michael5400"],required=True);p.add_argument("--expected-sha256",required=True);p.add_argument("--prefix",required=True);p.add_argument("--bitrate",type=int,default=68000);p.add_argument("--maxrate",type=int,default=100000);p.add_argument("--chroma-loc",type=int,choices=[0,2],default=0);p.add_argument("--bufsize",type=int,default=136000);p.add_argument("--variant",choices=["hdr-off","psyoff","no-sao","chroma-fine","hdr-off-chroma6","hdr-off-chroma9"],required=True);a=p.parse_args();assert pathlib.Path(a.prefix).name==a.prefix;num,den=(24,1) if a.fixture=="beekeeper1800" else (24000,1001);cll="0,0" if a.fixture=="beekeeper1800" else "464,147"
ROOT=pathlib.Path(a.output_root).resolve();out=ROOT/'corrected-encodes';out.mkdir(exist_ok=True);src=pathlib.Path(a.input).resolve();dst=out/(a.prefix+'-hdr.mp4');expected=a.expected_sha256;assert hashlib.sha256(src.read_bytes()).hexdigest()==expected;assert not dst.exists();assert a.prefix.startswith(a.fixture+'-');assert shutil.disk_usage(out).free>20*1024**3
probe_argv=['/opt/homebrew/bin/ffprobe','-v','error','-select_streams','v:0','-count_frames','-show_entries','stream=width,height,pix_fmt,r_frame_rate,time_base,nb_read_frames','-of','json',str(src)]
probe=json.loads(subprocess.check_output(probe_argv));st=probe['streams'][0];assert (st['width'],st['height'],st['pix_fmt'],int(st['nb_read_frames']))==(3840,2160,'yuv420p10le',48)
packets=json.loads(subprocess.check_output(['/opt/homebrew/bin/ffprobe','-v','error','-select_streams','v:0','-show_packets','-show_entries','packet=pts,dts,duration','-of','json',str(src)]))['packets'];assert len(packets)==48
tick=fractions.Fraction(st['time_base']);origin=int(packets[0]['pts'])*tick;timing_error=[str(int(pkt['pts'])*tick-origin-fractions.Fraction(i*den,num)) for i,pkt in enumerate(packets)];assert all(abs(fractions.Fraction(e))<=fractions.Fraction(1,1000) for e in timing_error);assert all(int(x['pts'])==int(x['dts']) for x in packets)
(out/(a.prefix+'-source-probe.json')).write_text(json.dumps({'argv':probe_argv,'probe':probe,'packets':packets,'origin':str(origin),'cadence':str(fractions.Fraction(den,num)),'source_tick_allowance':'1/1000','PTS_error_vs_origin_plus_n_over_24':timing_error,'NUT_header_rate_not_used':'diagnostic raw NUT declares its timebase as rate; encoder explicitly uses fixture cadence without dropping/repeating frames'},indent=2))
params=f'hdr10=1:hdr10-opt={0 if a.variant in ["hdr-off","hdr-off-chroma6","hdr-off-chroma9"] else 1}:aq-mode=2:repeat-headers=1:open-gop=0:bframes=0:level-idc=5.1:high-tier=1:hrd=1:chromaloc={a.chroma_loc}:scenecut=0:pools=none:frame-threads=1:wpp=0:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll={cll}'
params += {'hdr-off':'','psyoff':':psy-rd=0:psy-rdoq=0','no-sao':':sao=0','chroma-fine':':cbqpoffs=-6:crqpoffs=-6','hdr-off-chroma6':':cbqpoffs=-6:crqpoffs=-6','hdr-off-chroma9':':cbqpoffs=-9:crqpoffs=-9'}[a.variant]
argv=['nice','-n','15','/opt/homebrew/bin/ffmpeg','-nostdin','-hide_banner','-loglevel','verbose','-filter_threads','1','-threads','1','-r',f'{num}/{den}','-i',str(src),'-map','0:v:0','-an','-sn','-vf',f'setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc,format=yuv420p10le,settb=expr=1/{num},setpts=N*{den}','-c:v','libx265','-preset','slow','-profile:v','main10','-x265-params',params,'-b:v',str(a.bitrate)+'k','-maxrate',str(a.maxrate)+'k','-bufsize',str(a.bufsize)+'k','-bf','0','-flags','+cgop','-g','48','-keyint_min','48','-sc_threshold','0','-force_key_frames','expr:eq(mod(n,48),0)','-color_primaries','bt2020','-color_trc','smpte2084','-colorspace','bt2020nc','-color_range','tv','-fps_mode','passthrough','-enc_time_base',f'1:{num}','-frames:v','48','-threads:v','1','-movflags','+empty_moov+default_base_moof+frag_keyframe+frag_discont','-tag:v','hvc1','-f','mp4',str(dst)]
record={'fixture':a.fixture,'variant':a.variant,'status':'running','source':str(src),'source_sha256':expected,'source_frame_count':48,'target_mbps':a.bitrate/1000,'maxrate_mbps':a.maxrate/1000,'bufsize_mbit':a.bufsize/1000,'cadence':f'{num}/{den}','scope':'Two-pass offline control. Same requested coding settings across passes; no continuous-history/native/conformance claim.','pass_runs':[]}
stats=out/(a.prefix+'.stats');assert not stats.exists() and ':' not in str(stats)
recipe=out/(a.prefix+'-recipe.json');recipe.write_text(json.dumps(record,indent=2))
t=time.monotonic()
for pass_number in [1,2]:
    command=argv.copy();idx=command.index('-x265-params')+1
    csv=out/(a.prefix+f'-pass{pass_number}.csv')
    command[idx] += f':pass={pass_number}:stats={stats}:slow-firstpass=1:csv={csv}:csv-log-level=2'
    result_path=out/(a.prefix+'-pass1-hdr.mp4') if pass_number==1 else dst
    assert not result_path.exists();command[-1]=str(result_path)
    if pass_number==2:
        stat_bytes=stats.read_bytes();rows=__import__('re').findall(rb'^in:(\d+)',stat_bytes,__import__('re').M)
        assert len(rows)==48 and sorted(map(int,rows))==list(range(48)), 'first-pass stats must bind all48 input ordinals'
        record['stats_before_pass2']={'path':str(stats),'sha256':hashlib.sha256(stat_bytes).hexdigest(),'records':48}
    log=out/(a.prefix+f'-pass{pass_number}-encode.log')
    with log.open('xb') as f:completed=subprocess.run(command,stdout=f,stderr=subprocess.STDOUT,timeout=1200)
    record['pass_runs'].append({'pass':pass_number,'argv':command,'exit_code':completed.returncode,'log':str(log),'output':str(result_path)})
    recipe.write_text(json.dumps(record,indent=2))
    if completed.returncode:
        record['status']='encode_failed';recipe.write_text(json.dumps(record,indent=2));raise SystemExit(completed.returncode)
    text=log.read_text();assert '48 frames decoded' in text and '48 frames encoded' in text
    probe=json.loads(subprocess.check_output(['/opt/homebrew/bin/ffprobe','-v','error','-select_streams','v:0','-show_packets','-show_entries','packet=pts,dts,duration','-of','json',str(result_path)]))['packets']
    assert len(probe)==48 and all([int(p['pts']),int(p['dts']),int(p['duration'])]==[i*den,i*den,den] for i,p in enumerate(probe))
record['stats_after_pass2_sha256']=hashlib.sha256(stats.read_bytes()).hexdigest()
assert record['stats_before_pass2']['sha256']==record['stats_after_pass2_sha256'], 'pass2 must not overwrite pass1 stats'
record['argv']=record['pass_runs'][-1]['argv']
record.update(status='encoded',exit_code=0,elapsed_seconds=time.monotonic()-t,bytes=dst.stat().st_size,sha256=hashlib.sha256(dst.read_bytes()).hexdigest())
recipe.write_text(json.dumps(record,indent=2));print(json.dumps({k:v for k,v in record.items() if k!='pass_runs'},indent=2))
