"""Original terminal-AU controls preserve timing and prove true RPU absence separately."""
import importlib.machinery
import importlib.util
from pathlib import Path
import struct
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("p5_terminal_controls", str(ROOT / "scripts/append-macos-p5-terminal-nals"))
spec = importlib.util.spec_from_loader(loader.name, loader)
TOOL = importlib.util.module_from_spec(spec)
loader.exec_module(TOOL)
SOURCE = ROOT / "crates/plurxd/fixtures/macos-processing/strict_p5_fresh.mp4"


def length_prefixed_nals(data):
    units = []
    offset = 0
    while offset < len(data):
        size = int.from_bytes(data[offset:offset + 4], "big")
        units.append(data[offset + 4:offset + 4 + size])
        offset += 4 + size
    return units


class P5TerminalNalCase(unittest.TestCase):
    def test_terminal_positives_preserve_every_original_sample_and_timing_table(self):
        source = SOURCE.read_bytes()
        original_boxes = dict(TOOL.boxes(source))
        original_mdat = original_boxes[b"mdat"]
        original_tables = TOOL.tables(source)
        sizes = struct.unpack_from(">24I", original_tables[b"stsz"], 12)
        first23 = sum(sizes[:-1])
        endings = {"eos": bytes([72, 1, 128]), "eob": bytes([74, 1, 128]),
                   "eos_eob": bytes([72, 1, 128]) + b"\0\0\0\3" + bytes([74, 1, 128])}
        for name, ending in endings.items():
            with self.subTest(terminal=name):
                image = TOOL.terminal_cases(source)[name]
                changed_boxes = dict(TOOL.boxes(image))
                self.assertEqual(changed_boxes[b"mdat"][:first23], original_mdat[:first23])
                self.assertEqual(changed_boxes[b"mdat"], original_mdat + b"\0\0\0\3" + ending)
                self.assertEqual(changed_boxes[b"ftyp"], original_boxes[b"ftyp"])
                tables = TOOL.tables(image)
                self.assertEqual(tables[b"stco"], original_tables[b"stco"])
                self.assertEqual(tables[b"stsc"], original_tables[b"stsc"])
                # The only moov change is the final sample byte count. Codec
                # configuration, DTS/CTS, sync samples and all other metadata
                # must remain byte-for-byte the encoded source's declaration.
                normalized = image.replace(tables[b"stsz"], original_tables[b"stsz"], 1)
                self.assertEqual(dict(TOOL.boxes(normalized))[b"moov"], original_boxes[b"moov"])

    def test_missing_control_deletes_the_current_rpu_and_keeps_legal_terminators(self):
        source = SOURCE.read_bytes()
        original_mdat = dict(TOOL.boxes(source))[b"mdat"]
        sizes = struct.unpack_from(">24I", TOOL.tables(source)[b"stsz"], 12)
        first23 = sum(sizes[:-1])
        original_final = length_prefixed_nals(original_mdat[first23:])
        self.assertEqual((original_final[-1][0] >> 1) & 63, 62)
        changed = dict(TOOL.boxes(TOOL.terminal_cases(source)["true_missing"]))[b"mdat"]
        self.assertEqual(changed[:first23], original_mdat[:first23])
        final_nals = length_prefixed_nals(changed[first23:])
        self.assertEqual(final_nals[:-2], original_final[:-1])
        self.assertEqual([nal[0] >> 1 for nal in final_nals[-2:]], [36, 37])
        self.assertNotIn(62, [(nal[0] >> 1) & 63 for nal in final_nals])

    def test_unbound_source_cannot_be_given_terminal_fixture_provenance(self):
        with self.assertRaisesRegex(ValueError, "hash-bound"):
            TOOL.terminal_cases(SOURCE.read_bytes() + b"changed source")


if __name__ == "__main__":
    unittest.main()
