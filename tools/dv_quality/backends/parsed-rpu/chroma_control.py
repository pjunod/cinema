"""Independent sample-phase controls for point-filtered centered/left 4:2:0."""
import hashlib
import json
import math
from pathlib import Path
import struct
import sys
W=H=16

def rgb(x,y):
    return ((.2 if x<8 else .6)+x/1000, .3, (.2 if y<8 else .7)+y/1000)

def generate(root):
    root=Path(root)
    y=[512]*(W*H)
    u=[(200 if x<8 else 700)+x+2*z for z in range(H) for x in range(W)]
    v=[(300 if z<8 else 600)+2*x+z for z in range(H) for x in range(W)]
    (root/'native444.yuv').write_bytes(struct.pack('<768H',*(y+u+v)))
    codes=[round(c*65535) for y in range(H) for x in range(W) for c in rgb(x,y)]
    (root/'rgb-edge.rgb48le').write_bytes(struct.pack('<768H',*codes))


def check(root):
    root=Path(root);rows=[]
    source=struct.unpack('<768H',(root/'native444.yuv').read_bytes())
    for location,phase in [('center',1),('left',0)]:
        raw=(root/f'native-{location}.yuv').read_bytes()
        if len(raw)!=768: raise ValueError('truncated native phase output')
        actual=struct.unpack('<384H',raw)
        expected=list(source[:256])
        for component in (1,2):
            expected.extend(source[component*256+(2*y+1)*W+2*x+phase]
                            for y in range(8) for x in range(8))
        err=max(abs(a-b) for a,b in zip(actual,expected))
        if err!=0: raise ValueError(f'native {location} phase mismatch: {err}')
        # Independent NCL scalar conversion of exact input RGB48 values.
        encoded=struct.unpack('<768H',(root/'rgb-edge.rgb48le').read_bytes())
        def ncl(x,y):
            off=3*(y*W+x);r,g,b=[encoded[off+c]/65535 for c in range(3)]
            yy=.2627*r+.6780*g+.0593*b
            return (64+876*yy,512+896*(b-yy)/1.8814,512+896*(r-yy)/1.4746)
        expected=[round(ncl(x,y)[0]) for y in range(H) for x in range(W)]
        for component in (1,2):
            expected.extend(round(ncl(2*x+phase,2*y+1)[component])
                            for y in range(8) for x in range(8))
        raw_rgb=(root/f'rgb-{location}.yuv').read_bytes()
        if len(raw_rgb)!=768: raise ValueError('truncated RGB phase output')
        actual_rgb=struct.unpack('<384H',raw_rgb)
        error=max(abs(a-b) for a,b in zip(actual_rgb,expected))
        if error>1: raise ValueError(f'RGB {location} arithmetic/phase mismatch: {error}')
        rows.append({'location':location,'point_source_x_phase':phase,'point_source_y_phase':1,
                     'native_max_code_error':err,'rgb_ncl_max_code_error':error,
                     'native_output_sha256':hashlib.sha256(raw).hexdigest(),
                     'rgb_output_sha256':hashlib.sha256(raw_rgb).hexdigest()})
    if (root/'rgb-center.yuv').read_bytes()==(root/'rgb-left.yuv').read_bytes():
        raise ValueError('phase control failed to distinguish chroma siting')
    return {'scope':'analytic chroma-siting mechanics only','rgb_rounding_bound_codes':1,
            'bound_chosen_before_rgb_control_execution':True,'results':rows}

if __name__=='__main__':
    command,folder=sys.argv[1:]
    if command=='generate': generate(folder)
    elif command=='check': print(json.dumps(check(folder),indent=2,allow_nan=False))
    else: raise ValueError('expected generate or check')
