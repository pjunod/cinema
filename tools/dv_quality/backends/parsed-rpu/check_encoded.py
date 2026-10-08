"""Bind bounded HDR10 output and verify declared stream/frame sample metadata."""
import hashlib
import json
from pathlib import Path
import sys
root=Path(sys.argv[1])
probe=json.loads((root/'parsed-render-hdr10.ffprobe.json').read_text())
expected={'width':64,'height':64,'pix_fmt':'yuv420p10le','color_range':'tv',
          'color_space':'bt2020nc','color_primaries':'bt2020','color_transfer':'smpte2084',
          'chroma_location':'center'}
if len(probe['streams'])!=1 or len(probe['frames'])!=4:
    raise ValueError('expected one stream and four frames')
stream=probe['streams'][0]
if stream.get('profile')!='Main 10' or stream.get('r_frame_rate')!='24/1':
    raise ValueError('expected Main10 at24fps')
for record in [stream,*probe['frames']]:
    if any(record.get(key)!=value for key,value in expected.items()):
        raise ValueError('HDR10 stream/frame metadata differs from explicit sample recipe')
result={'scope':'synthetic encoding mechanics only','qualified':False,'frames':4,
        'input_rgb48_sha256':hashlib.sha256((root/'parsed-p81-four.rgb48le').read_bytes()).hexdigest(),
        'encoded_sha256':hashlib.sha256((root/'parsed-render-hdr10.hevc').read_bytes()).hexdigest(),
        'ffprobe_sha256':hashlib.sha256((root/'parsed-render-hdr10.ffprobe.json').read_bytes()).hexdigest(),
        'sample_recipe':{'chroma_location':'center','filter':'point'},
        'mastering_metadata_provenance':'synthetic BT2020 target10000/.005nits, not source RPU L6',
        'stream':{**expected,'profile':stream['profile'],'r_frame_rate':stream['r_frame_rate']}}
print(json.dumps(result,indent=2,allow_nan=False))
