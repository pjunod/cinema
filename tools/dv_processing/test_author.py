"""Focused encoded packet/timing regression; takes a built helper and real reconstruction inputs."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import os

class Authoring(unittest.TestCase):
    def test_reordered_packet_metadata_and_base(self):
        helper=os.environ['P81_HELPER']; source=Path(os.environ['P81_ENCODED'])
        timing=Path(os.environ['P81_TIMING']); rpus=Path(os.environ['P81_RPUS'])
        def run(*args):
            return subprocess.check_output(args,stderr=subprocess.DEVNULL)
        def probe(path):
            return json.loads(run('ffprobe','-v','error','-show_streams','-show_packets','-show_data','-of','json',str(path)))
        def payload(packet):
            return bytes.fromhex(''.join(line.split(':',1)[1].split('  ',1)[0].replace(' ','') for line in packet['data'].strip().splitlines()))
        with tempfile.TemporaryDirectory() as tmp:
            out=Path(tmp)/'authored.mkv'
            receipt=[json.loads(s) for s in run(helper,str(source),str(timing),str(rpus),str(out)).splitlines()]
            before=probe(source); after=probe(out)
            cfg=[d for d in after['streams'][0]['side_data_list'] if d['side_data_type']=='DOVI configuration record'][0]
            self.assertEqual((cfg['dv_profile'],cfg['el_present_flag'],cfg['dv_bl_signal_compatibility_id']),(8,0,1))
            if len(receipt)>1:
                self.assertNotEqual([r['renderer_frame'] for r in receipt],list(range(len(receipt))))
            nal_hashes=[]
            from fractions import Fraction
            durations=[Fraction(s.split('\t')[1].strip()) for s in timing.read_text().splitlines()]
            for original,authored,row in zip(before['packets'],after['packets'],receipt,strict=True):
                for field in ('pts_time',):
                    self.assertEqual(original.get(field),authored.get(field))
                self.assertEqual(Fraction(authored['duration'])*Fraction(after['streams'][0]['time_base']),durations[row['renderer_frame']])
                a=payload(original); b=payload(authored)
                self.assertEqual(b[:len(a)],a)
                size=int.from_bytes(b[len(a):len(a)+4],'big'); nal=b[len(a)+4:]
                self.assertEqual(len(nal),size); self.assertEqual(nal[:2],b'\x7c\x01')
                self.assertEqual(hashlib.sha256(nal).hexdigest(),row['adapted_rpu_sha256'])
                self.assertEqual(hashlib.sha256((rpus/f"frame-{row['renderer_frame']:03d}.nal").read_bytes()).hexdigest(),row['source_rpu_sha256'])
                nal_hashes.append(hashlib.sha256(nal).hexdigest())
            self.assertEqual(len(set(nal_hashes)),len(receipt))
            def decoded(path): return run('ffmpeg','-v','error','-i',str(path),'-map','0:v:0','-pix_fmt','yuv420p10le','-f','rawvideo','-')
            self.assertEqual(decoded(source),decoded(out))
            # Reuse the exact authored result as an invalid already-DV input.
            self.assertNotEqual(subprocess.run([helper,str(out),str(timing),str(rpus),str(Path(tmp)/'bad.mkv')],capture_output=True).returncode,0)
            bad=Path(tmp)/'bad.tsv'; lines=timing.read_text().splitlines(True)
            bad.write_text(lines[0]+lines[0]+''.join(lines[2:]))
            self.assertNotEqual(subprocess.run([helper,str(source),str(bad),str(rpus),str(Path(tmp)/'bad2.mkv')],capture_output=True).returncode,0)
            missing=Path(tmp)/'missing'; missing.mkdir()
            self.assertNotEqual(subprocess.run([helper,str(source),str(timing),str(missing),str(Path(tmp)/'bad3.mkv')],capture_output=True).returncode,0)

if __name__=='__main__': unittest.main()
