import pathlib,json,subprocess,time,math,hashlib
r=pathlib.Path('target/qualification');inspection=json.loads((r/'served/media-inspection.json').read_text());rows=[]
for title in ['animation','live-action']:
 i=next(x for x in inspection if x['asset']==title+'-bf0');v=next(x for x in i['streams'] if x['codec_type']=='video');fps=30 if title=='animation' else 24;count=i['video_packets'];reference=r/'score-reference.mkv'
 graph=f"trim=start=0,fps={fps}:start_time=0,scale={v['width']}:{v['height']},format=yuv420p,tpad=stop_mode=clone:stop_duration=65,trim=end_frame={count},setpts=PTS-STARTPTS"
 subprocess.run(['/opt/homebrew/bin/ffmpeg','-nostdin','-v','error','-y','-threads','2','-filter_threads','1','-copyts','-i',str(r/'public-clips'/f'{title}.mkv'),'-map','0:v:0','-an','-vf',graph,'-c:v','ffv1','-threads','2',str(reference)],check=True,timeout=60)
 row={'title':title,'reference_filter':graph,'reference_frames':count,'reference_sha256':hashlib.sha256(reference.read_bytes()).hexdigest(),'modes':{}}
 for bf in [0,2]:
  d=r/'served'/f'{title}-bf{bf}';media=r/'score-media.mp4';media.write_bytes(next(d.glob('*.mp4')).read_bytes()+b''.join(x.read_bytes() for x in sorted(d.glob('*.m4s'))));score=r/'score.json'
  vg=f"[0:v]setpts=PTS-STARTPTS[d];[1:v]setpts=PTS-STARTPTS[r];[d][r]libvmaf=model=version=vmaf_v0.6.1:n_threads=2:n_subsample=1:log_fmt=json:log_path={score}:shortest=1:repeatlast=0"
  subprocess.run(['/opt/homebrew/bin/ffmpeg','-nostdin','-v','error','-threads','2','-filter_complex_threads','1','-i',str(media),'-threads','2','-i',str(reference),'-lavfi',vg,'-an','-f','null','-'],check=True,timeout=90)
  scores=json.loads(score.read_text())['frames'];assert len(scores)==count
  values=[x['metrics']['vmaf'] for x in scores];assert all(math.isfinite(x) for x in values)
  row['modes'][str(bf)]={'mean':sum(values)/count,'p10':sorted(values)[math.ceil(count/10)-1],'frames':count,'media_bytes':next(x for x in inspection if x['asset']==f'{title}-bf{bf}')['media_bytes'],'first10':values[:10]};media.unlink();score.unlink()
 reference.unlink();rows.append(row);(r/'vod-quality.json').write_text(json.dumps(rows,indent=2)+'\n');print(title,row['modes'],flush=True)
