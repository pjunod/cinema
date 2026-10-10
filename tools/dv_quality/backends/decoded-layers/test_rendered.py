"""Corrupt float/integer scientific artifacts must never pass arithmetic checks."""
import shutil, struct, subprocess, sys, tempfile, unittest, json
from pathlib import Path
BUNDLE = Path(__file__).resolve().parent

class RenderedCheckerTest(unittest.TestCase):

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        shutil.copytree(SOURCE / 'rpu-tags', self.root / 'rpu-tags')
        shutil.copyfile(SOURCE / 'source-definition.json', self.root / 'source-definition.json')
        for b in (0, 2):
            for name in (f'bl-b{b}.mkv', f'bl-b{b}.ffprobe.json', f'el-b{b}.ffprobe.json'):
                shutil.copyfile(SOURCE / name, self.root / name)
            shutil.copytree(SOURCE / f'b{b}-normal', self.root / f'b{b}-normal')

    def tearDown(self):
        self.temp.cleanup()

    def run_checker(self):
        return subprocess.run([sys.executable, str(BUNDLE / 'check_rendered.py'), str(self.root)], capture_output=True).returncode

    def test_valid(self):
        self.assertEqual(self.run_checker(), 0)

    def test_nan_inf_rgb_and_alpha_refused(self):
        p = self.root / 'b0-normal/render/frame-0/outputs/zero-reconstruction.rgba32f'
        raw = p.read_bytes()
        for offset in (0, 12):
            for value in (float('nan'), float('inf')):
                with self.subTest(offset=offset, value=str(value)):
                    p.write_bytes(raw[:offset] + struct.pack('<f', value) + raw[offset + 4:])
                    self.assertNotEqual(self.run_checker(), 0)
        p.write_bytes(raw)

    def test_truncated_integer_and_float_refused(self):
        for extension in ['rgb48le', 'rgba32f']:
            p = self.root / f'b0-normal/render/frame-0/outputs/zero-reconstruction.{extension}'
            raw = p.read_bytes()
            p.write_bytes(raw[:-1])
            self.assertNotEqual(self.run_checker(), 0)
            p.write_bytes(raw)

    def test_wrong_shifted_integer_refused(self):
        p = self.root / 'b0-normal/render/frame-0/outputs/shifted-reconstruction.rgb48le'
        raw = p.read_bytes()
        p.write_bytes(b'\x00\x00' + raw[2:])
        self.assertNotEqual(self.run_checker(), 0)
    def modify_probe(self, mutate):
        path = self.root / 'b0-normal/render/frame-0/probe.jsonl'
        events = [json.loads(line) for line in path.read_text().splitlines()]
        mutate(events)
        path.write_text(''.join(json.dumps(event) + '\n' for event in events))
        self.assertNotEqual(self.run_checker(), 0)

    def test_changed_actual_renderer_pts_refused(self):
        self.modify_probe(lambda events: events[2].update(pts='1/1000'))

    def test_changed_actual_renderer_duration_refused(self):
        self.modify_probe(lambda events: events[2].update(duration='1/24'))

    def test_failed_or_wrong_el_flag_refused(self):
        self.modify_probe(lambda events: events[3].update(el_bound=False))

    def test_numeric_boolean_flag_refused(self):
        self.modify_probe(lambda events: events[3].update(render_ok=1))

    def test_missing_actual_renderer_event_refused(self):
        self.modify_probe(lambda events: events.pop())

    def test_swapped_association_refused(self):
        folder = self.root / 'b0-normal/render'
        shutil.copyfile(folder / 'frame-1/association.json', folder / 'frame-0/association.json')
        self.assertNotEqual(self.run_checker(), 0)

    def test_forged_input_hash_refused(self):
        path = self.root / 'b0-normal/render/frame-0/association.json'
        data = json.loads(path.read_text())
        data['bl_sha256'] = '0' * 64
        path.write_text(json.dumps(data))
        self.assertNotEqual(self.run_checker(), 0)

    def test_actual_decoded_el_substitution_refused(self):
        path = self.root / 'b0-normal/frames/el-frame-000.yuv420p10le'
        path.write_bytes(b'\0\0' + path.read_bytes()[2:])
        self.assertNotEqual(self.run_checker(), 0)

    def test_actual_raw_rpu_substitution_refused(self):
        shutil.copyfile(self.root / 'rpu-tags/p7-frame1.nal', self.root / 'b0-normal/frames/bl-frame-000.rpu.nal')
        self.assertNotEqual(self.run_checker(), 0)

if __name__ == '__main__':
    SOURCE = Path(sys.argv[1]).resolve()
    unittest.main(argv=[sys.argv[0]])
