"""Focused limits, prerequisite provenance, HDR tags and clean-refusal controls."""
from pathlib import Path
import hashlib
import json
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import check_hdr10
import registered_limits
import verify_prerequisites
import verify_refusals
import dependency_inventory

ROOT = Path(__file__).resolve().parent


class ReplayContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def dependency_fixture(self):
        source = self.root/'source'
        for name in ('src/dovi_tool/dolby_vision/lib.rs','src/DoViBaker/include/header.h',
                     'src/DoViBaker/DoViBaker/DoViProcessor.cpp','deps/lib/libdovi.a'):
            path=source/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(b'locked')
        return source,dependency_inventory.inventory(source,'cpu')

    def test_extra_build_file_rejected(self):
        for name in ('src/dovi_tool/dolby_vision/build.rs','src/dovi_tool/.cargo/config.toml'):
            source,expected=self.dependency_fixture()
            path=source/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(b'extra')
            with self.assertRaisesRegex(ValueError,'exact dependency inventory'):
                dependency_inventory.validate(source,'cpu',expected)
            path.unlink()

    def test_link_target_mutation_rejected(self):
        source=self.root/'gpu'
        for name in ('prefix/lib/a.so.374','prefix/lib/b.so.374','include/header.h','lib/static.a'):
            path=source/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(b'locked')
        link=source/'prefix/lib/a.so';link.symlink_to('a.so.374')
        expected=dependency_inventory.inventory(source,'gpu')
        link.unlink();link.symlink_to('b.so.374')
        with self.assertRaisesRegex(ValueError,'exact dependency inventory'):
            dependency_inventory.validate(source,'gpu',expected)

    def test_postcopy_corruption_rejected(self):
        source,expected=self.dependency_fixture()
        original=dependency_inventory.shutil.copyfile
        def corrupt(src,dst):
            result=original(src,dst)
            if str(dst).endswith('lib.rs'):
                Path(dst).write_bytes(b'corrupted after verified read')
            return result
        with patch('dependency_inventory.shutil.copyfile',side_effect=corrupt):
            with self.assertRaisesRegex(ValueError,'exact dependency inventory'):
                dependency_inventory.copy_verified(source,self.root/'destination','cpu',expected)

    def test_locked_copy_passes(self):
        source,expected=self.dependency_fixture()
        dependency_inventory.copy_verified(source,self.root/'destination','cpu',expected)
        dependency_inventory.validate(self.root/'destination','cpu',expected)

    def test_prerequisite_file_mutation_rejected(self):
        path = self.root/'source.cpp';path.write_bytes(b'original')
        expected = {'source.cpp':hashlib.sha256(path.read_bytes()).hexdigest()}
        verify_prerequisites.verify_files(self.root,expected)
        path.write_bytes(b'mutated')
        with self.assertRaisesRegex(ValueError,'identity differs'):
            verify_prerequisites.verify_files(self.root,expected)

    def test_prerequisite_image_wrong_platform_rejected(self):
        image = 'sha256:'+'a'*64
        record = [{'Id':image,'Os':'linux','Architecture':'amd64'}]
        fake = subprocess.CompletedProcess([],0,json.dumps(record),'')
        with patch('verify_prerequisites.subprocess.run',return_value=fake):
            with self.assertRaisesRegex(ValueError,'identity/platform'):
                verify_prerequisites.verify_image(image,'arm64')

    def test_prerequisite_image_wrong_identity_rejected(self):
        image = 'sha256:'+'a'*64
        record = [{'Id':'sha256:'+'b'*64,'Os':'linux','Architecture':'arm64'}]
        fake = subprocess.CompletedProcess([],0,json.dumps(record),'')
        with patch('verify_prerequisites.subprocess.run',return_value=fake):
            with self.assertRaisesRegex(ValueError,'identity/platform'):
                verify_prerequisites.verify_image(image,'arm64')

    def test_nonfinite_json_and_overflow_rejected(self):
        path = self.root/'limits.json'
        for text in ('{"cap":NaN}','{"cap":Infinity}','{"cap":1e400}'):
            path.write_text(text)
            with self.assertRaises(ValueError):
                registered_limits.read_json(path)

    def refusals(self):
        shutil.copyfile(ROOT/'negative-admission.json',self.root/'negative-admission.json')
        for name in ('matrix-rejected','trim-rejected'):
            shutil.copytree(ROOT/'gpu'/name,self.root/'gpu'/name)
            (self.root/'gpu'/name/'outputs').mkdir(exist_ok=True)

    def test_exact_refusals_pass(self):
        self.refusals();self.assertEqual(verify_refusals.verify(self.root)['unsupported_matrix'],1)

    def test_absent_refusal_output_directory_passes(self):
        self.refusals()
        for directory in ('matrix-rejected','trim-rejected'):
            (self.root/'gpu'/directory/'outputs').rmdir()
        self.assertEqual(verify_refusals.verify(self.root)['unsupported_matrix'],1)

    def test_refusal_output_nondirectory_rejected(self):
        self.refusals();path=self.root/'gpu/matrix-rejected/outputs'
        path.rmdir();path.write_bytes(b'bad')
        with self.assertRaisesRegex(ValueError,'produced output'):
            verify_refusals.verify(self.root)

    def test_crash_after_diagnostic_rejected(self):
        self.refusals();path=self.root/'negative-admission.json'
        data=json.loads(path.read_text());data['unsupported_matrix']=139;path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError,'exit must'):
            verify_refusals.verify(self.root)

    def test_extra_diagnostic_rejected(self):
        self.refusals();path=self.root/'gpu/matrix-rejected/probe.stderr'
        path.write_text(path.read_text()+'Segmentation fault\n')
        with self.assertRaisesRegex(ValueError,'diagnostic differs'):
            verify_refusals.verify(self.root)

    def test_refusal_frame_event_rejected(self):
        self.refusals();path=self.root/'gpu/trim-rejected/probe.jsonl'
        path.write_text('{"kind":"frame","render_ok":true}\n')
        with self.assertRaisesRegex(ValueError,'render/frame event'):
            verify_refusals.verify(self.root)

    def test_refusal_output_rejected(self):
        self.refusals();(self.root/'gpu/trim-rejected/outputs/unexpected.rgb48le').write_bytes(b'bad')
        with self.assertRaisesRegex(ValueError,'produced output'):
            verify_refusals.verify(self.root)

    def test_wrong_hdr10_tags_and_count_rejected(self):
        for name in ('reconstructed16-reference.rgb48le','reconstructed16.rgb48le','reconstructed64.rgb48le',
                     'reconstructed64.yuv420p10le','decoded.yuv420p10le','decoded-hdr10.rgb48le',
                     'baseline-decoded.yuv420p10le','preregistered-controls.json','probe.json'):
            shutil.copyfile(ROOT/name,self.root/name)
        original_root = check_hdr10.ROOT;check_hdr10.ROOT=self.root
        self.addCleanup(setattr,check_hdr10,'ROOT',original_root)
        original = json.loads((self.root/'probe.json').read_text())
        for field,value in [('color_range','pc'),('color_transfer','bt709'),('chroma_location','left'),('pix_fmt','yuv420p')]:
            data=json.loads(json.dumps(original));data['streams'][0][field]=value
            (self.root/'probe.json').write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError,'HDR10 tags'):
                check_hdr10.verify()
        data=json.loads(json.dumps(original));data['frames'].pop()
        (self.root/'probe.json').write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError,'HDR10 tags'):
            check_hdr10.verify()


if __name__ == '__main__':
    unittest.main()
