"""Verify all coded picture NAL payloads and exact packet clocks across Rust authoring."""
import json,hashlib,struct,subprocess,argparse
from pathlib import Path

def sha(b):return hashlib.sha256(b).hexdigest()
def inspect(path):
 raw=path.read_bytes();configs=[]
 def walk(start,end):
  p=start
  while p<end:
   assert p+8<=end
   size,tag=struct.unpack_from('>I4s',raw,p);head=8
   if size==1:size=struct.unpack_from('>Q',raw,p+8)[0];head=16
   if size==0:size=end-p
   assert size>=head and p+size<=end
   if tag in {b'moov',b'trak',b'mdia',b'minf',b'stbl'}:walk(p+head,p+size)
   elif tag==b'stsd':
    assert struct.unpack_from('>I',raw,p+head+4)[0]==1
    q=p+head+8;n=struct.unpack_from('>I',raw,q)[0];walk(q+86,q+n)
   elif tag==b'hvcC':configs.append(raw[p+head:p+size])
   p+=size
  assert p==end
 walk(0,len(raw));assert len(configs)==1
 ls=(configs[0][21]&3)+1
 d=json.loads(subprocess.check_output(['/opt/homebrew/bin/ffprobe','-v','error','-select_streams','v:0','-show_packets','-show_streams','-show_entries','packet=pos,size,pts,dts,duration:stream=time_base','-of','json',str(path)]))
 rows=[]
 for pkt in d['packets']:
  b=raw[int(pkt['pos']):int(pkt['pos'])+int(pkt['size'])];q=0;vcl=[];rpu=[]
  while q<len(b):
   assert q+ls<=len(b)
   n=int.from_bytes(b[q:q+ls],'big');q+=ls;nal=b[q:q+n];q+=n;assert n>=2 and q<=len(b);typ=(nal[0]>>1)&63
   if typ<=31:vcl.append({'type':typ,'bytes':n,'sha256':sha(nal)})
   elif typ==62:rpu.append(sha(nal))
  assert q==len(b) and vcl
  rows.append({'timing':{k:pkt[k] for k in ['pts','dts','duration']},'VCL':vcl,'RPU':rpu})
 return {'file':str(path),'sha256':sha(raw),'time_base':d['streams'][0]['time_base'],'frames':rows}
p=argparse.ArgumentParser();p.add_argument('input',type=Path);p.add_argument('output',type=Path);p.add_argument('receipt',type=Path);a=p.parse_args();assert not a.receipt.exists()
i,o=inspect(a.input),inspect(a.output);assert len(i['frames'])==len(o['frames'])==48
checks={'all48_VCL_payloads_identical':all(x['VCL']==y['VCL'] for x,y in zip(i['frames'],o['frames'])),'track_timebase_identical':i['time_base']==o['time_base'],'all48_packet_clocks_identical':all(x['timing']==y['timing'] for x,y in zip(i['frames'],o['frames'])),'authored_each_has_exactly_one_RPU':all(len(y['RPU'])==1 for y in o['frames'])}
a.receipt.write_text(json.dumps({'scope':'Coded-picture payload and packet-clock preservation only; no display/native/HRD qualification.','input':i,'output':o,'checks':checks},indent=2)+'\n');print(json.dumps(checks));assert all(checks.values())
