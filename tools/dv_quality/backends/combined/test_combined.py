"""Focused corruption checks on temporary copies; no renderer/encoder reruns."""
import importlib.util,json,shutil,struct,sys,tempfile,unittest
from pathlib import Path
BASE=Path(sys.argv.pop(1)).resolve();sys.path.insert(0,str(BASE));from check_combined import verify
class CombinedControls(unittest.TestCase):
    def setUp(self):
        self.t=tempfile.TemporaryDirectory();self.root=Path(self.t.name)/'copy'
        shutil.copytree(BASE,self.root,ignore=shutil.ignore_patterns('prefix','include','lib','ffmpeg-prefix','owned-runner','runtime','__pycache__'))
    def tearDown(self):self.t.cleanup()
    def refuse(self):
        with self.assertRaises((ValueError,OSError,KeyError)):verify(self.root)
    def change(self,name):
        p=self.root/name;b=bytearray(p.read_bytes());b[0]^=1;p.write_bytes(b)
    def test_valid_mechanics_and_honest_stricter_limit(self):
        r=verify(self.root);self.assertTrue(r['pts_preserved']);self.assertFalse(r['initial_stricter_yuv_envelope_met']);self.assertFalse(r['production_admission'])
    def test_changed_input_container(self):self.change('source.mkv');self.refuse()
    def test_swapped_actual_rpu(self):
        a=self.root/'b2-normal/frames/bl-frame-000.rpu.nal';b=self.root/'b2-normal/frames/bl-frame-001.rpu.nal';aa=a.read_bytes();a.write_bytes(b.read_bytes());b.write_bytes(aa);self.refuse()
    def test_changed_actual_el(self):self.change('b2-normal/frames/el-frame-002.yuv420p10le');self.refuse()
    def test_changed_close_gpu_output(self):self.change('render-2/outputs/nonzero-rendered.rgb48le');self.refuse()
    def test_changed_encoded_media_with_stale_probe(self):self.change('hdr10.mkv');self.refuse()
    def test_changed_probe_pts(self):
        p=self.root/'hdr10.ffprobe.json';r=json.loads(p.read_text());next(e for e in r['packets_and_frames'] if e['type']=='frame')['pts']+=1;p.write_text(json.dumps(r));self.refuse()
    def test_truncated_decoded_output(self):
        p=self.root/'decoded.rgb48le';p.write_bytes(p.read_bytes()[:-2]);self.refuse()
    def test_missing_terminal_stage(self):(self.root/'encode.execution.json').unlink();self.refuse()
    def test_nonfinite_limits(self):
        p=self.root/'limits.json';r=json.loads(p.read_text());r['max_gpu_pq_error']=float('nan');p.write_text(json.dumps(r));self.refuse()
    def event_mutation(self,change):
        p=self.root/'render-0.execution.json';r=json.loads(p.read_text());change(r);p.write_text(json.dumps(r))
        q=self.root/'execution-binding.json';b=json.loads(q.read_text());b['render-0']=r;q.write_text(json.dumps(b));self.refuse()
    def test_wrong_tool_and_empty_maps_self_consistent(self):
        self.event_mutation(lambda r:r.update(argv=['/bin/true'],program_sha256='0'*64,input_sha256={},output_sha256={}))
    def test_wrong_tool_identity(self):self.event_mutation(lambda r:r.update(program_sha256='0'*64))
    def test_wrong_consumed_frame_argv(self):self.event_mutation(lambda r:r['argv'].__setitem__(2,'/work/b2-normal/frames/bl-frame-001.yuv420p10le'))
    def test_wrong_stage_timing(self):self.event_mutation(lambda r:r['argv'].__setitem__(-2,'42/1000'))
    def test_empty_stage_maps(self):self.event_mutation(lambda r:r.update(input_sha256={},output_sha256={}))
    def test_wrong_cwd_or_material_environment(self):self.event_mutation(lambda r:r.update(cwd='/tmp',environment={}))
    def test_frame_order_substitution(self):
        p=self.root/'rgb48le.bin';b=p.read_bytes();p.write_bytes(b[24576:49152]+b[:24576]+b[49152:]);self.refuse()
if __name__=='__main__':unittest.main()
