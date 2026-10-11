import pathlib,hashlib,json,struct,subprocess
r=pathlib.Path('target/qualification/served');rows=[]
def boxes(data,start=0,end=None):
 end=len(data) if end is None else end
 while start<end:
  size,typ=struct.unpack_from('>I4s',data,start);header=8
  if size==1:size=struct.unpack_from('>Q',data,start+8)[0];header=16
  if size==0:size=end-start
  if size<header or start+size>end:raise ValueError('box bounds')
  yield typ,data[start+header:start+size]
  start+=size
for d in sorted(x for x in r.iterdir() if x.is_dir()):
 init=next(d.glob('*.mp4'));segs=sorted(d.glob('*.m4s'))
 complete=d/'complete.bin';complete.write_bytes(init.read_bytes()+b''.join(x.read_bytes() for x in segs))
 probe=json.loads(subprocess.check_output(['/opt/homebrew/bin/ffprobe','-v','error','-show_streams','-show_packets','-show_entries','stream=index,codec_name,codec_type,has_b_frames,time_base,start_time,width,height:packet=stream_index,pts,dts,duration,flags','-of','json',str(complete)]))
 video=next(x for x in probe['streams'] if x['codec_type']=='video');audio=next(x for x in probe['streams'] if x['codec_type']=='audio');vp=[x for x in probe['packets'] if x['stream_index']==video['index']];ap=[x for x in probe['packets'] if x['stream_index']==audio['index']]
 wire=[]
 for segment in segs:
  for typ,body in boxes(segment.read_bytes()):
   if typ!=b'moof':continue
   for t,b in boxes(body):
    if t!=b'traf':continue
    for tt,bb in boxes(b):
     if tt==b'trun':wire.append({'segment':segment.name,'version':bb[0],'flags':int.from_bytes(bb[1:4]),'samples':int.from_bytes(bb[4:8])})
 decode=subprocess.run(['/opt/homebrew/bin/ffmpeg','-nostdin','-v','error','-xerror','-threads','2','-i',str(complete),'-f','null','-'],capture_output=True,timeout=30)
 pts=sorted(x['pts'] for x in vp);audio_gaps=[i for i in range(1,len(ap)) if ap[i]['pts']!=ap[i-1]['pts']+ap[i-1]['duration']]
 row={'asset':d.name,'init_sha256':hashlib.sha256(init.read_bytes()).hexdigest(),'segments':len(segs),'streams':probe['streams'],'video_packets':len(vp),'audio_packets':len(ap),'unique_pts':len(set(pts)),'nonzero_cto_packets':sum(x['pts']!=x['dts'] for x in vp),'video_pts_steps':sorted(set(pts[i]-pts[i-1] for i in range(1,len(pts)))),'audio_gap_count':len(audio_gaps),'strict_decode_exit':decode.returncode,'decode_errors':decode.stderr.decode(),'truns':wire,'media_bytes':sum(x.stat().st_size for x in segs)}
 rows.append(row);complete.unlink()
 print(d.name,row['nonzero_cto_packets'],row['strict_decode_exit'],row['audio_gap_count'],row['video_pts_steps'])
(r/'media-inspection.json').write_text(json.dumps(rows,indent=2)+'\n')
