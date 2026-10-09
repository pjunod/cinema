"""Portable importer controls; no Docker/GPU/build fixture dependency."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import import_cross_render


class ImportControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / 'gpu'
        self.source.mkdir()
        self.destination = self.root / 'cross-render'
        for source_name, _, length in import_cross_render.inventory():
            path = self.source / source_name
            path.parent.mkdir(parents=True, exist_ok=True)
            data = bytes(length) if length is not None else (b'{"results": []}\n' if source_name.endswith('.json') else b'{"kind":"test"}\n')
            path.write_bytes(data)

    def test_explicit_import_copies_only_bounded_inventory(self):
        (self.source / 'unrelated-source-or-binary').write_bytes(b'never copy')
        receipt = import_cross_render.import_evidence(self.source, self.destination)
        self.assertEqual(receipt['copied_files'], 33)
        self.assertEqual(len([p for p in self.destination.rglob('*') if p.is_file()]), 33)
        self.assertFalse((self.destination / 'unrelated-source-or-binary').exists())

    def test_existing_destination_rejected_unchanged(self):
        self.destination.mkdir()
        sentinel = self.destination / 'existing'
        sentinel.write_bytes(b'keep')
        with self.assertRaises(FileExistsError):
            import_cross_render.import_evidence(self.source, self.destination)
        self.assertEqual(sentinel.read_bytes(), b'keep')
        self.assertEqual(list(self.destination.iterdir()), [sentinel])

    def test_missing_inventory_rejected_before_destination_creation(self):
        (self.source / 'p81_export_probe/frame-2/outputs/zero-rendered.rgb48le').unlink()
        with self.assertRaises(FileNotFoundError):
            import_cross_render.import_evidence(self.source, self.destination)
        self.assertFalse(self.destination.exists())

    def test_wrong_pixel_length_rejected(self):
        (self.source / 'hdr10_baseline_probe/frame-0/outputs/zero-rendered.rgb48le').write_bytes(b'bad')
        with self.assertRaises(ValueError):
            import_cross_render.import_evidence(self.source, self.destination)
        self.assertFalse(self.destination.exists())

    def test_oversized_probe_rejected(self):
        (self.source / 'p81_export_probe/frame-0/probe.jsonl').write_bytes(b'x' * 65537)
        with self.assertRaises(ValueError):
            import_cross_render.import_evidence(self.source, self.destination)
        self.assertFalse(self.destination.exists())

    def test_malformed_receipt_rejected(self):
        (self.source / 'four-frame-results.json').write_bytes(b'broken')
        with self.assertRaises(json.JSONDecodeError):
            import_cross_render.import_evidence(self.source, self.destination)
        self.assertFalse(self.destination.exists())

    def test_missing_source_rejected(self):
        with self.assertRaises(FileNotFoundError):
            import_cross_render.import_evidence(self.root / 'absent', self.destination)
        self.assertFalse(self.destination.exists())

    def test_missing_parent_rejected(self):
        with self.assertRaises(FileNotFoundError):
            import_cross_render.import_evidence(self.source, self.root / 'absent' / 'cross-render')

    def test_destination_inside_source_rejected(self):
        with self.assertRaisesRegex(ValueError, 'outside'):
            import_cross_render.import_evidence(self.source, self.source / 'new-output')
        self.assertFalse((self.source / 'new-output').exists())

    def test_cli_requires_explicit_source_and_destination(self):
        script = Path(import_cross_render.__file__)
        result = subprocess.run([sys.executable, str(script)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('--source', result.stderr)
        self.assertIn('--destination', result.stderr)
        self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
