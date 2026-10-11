"""Independent sample-table checks for original Linux P5 envelope controls."""
import hashlib
import json
from pathlib import Path
import struct
import unittest

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "crates/plurxd/fixtures/linux-dolby"


def tables(data):
    found = {}
    def visit(blob):
        offset = 0
        while offset < len(blob):
            size, kind = struct.unpack_from(">I4s", blob, offset)
            if size < 8 or offset + size > len(blob):
                raise ValueError("invalid MP4 box")
            payload = blob[offset + 8:offset + size]
            if kind in {b"moov", b"trak", b"mdia", b"minf", b"stbl"}:
                visit(payload)
            elif kind in {b"stsz", b"stco", b"stts", b"ctts", b"stsc"}:
                if kind in found:
                    raise ValueError("multiple video tracks")
                found[kind] = payload
            offset += size
    visit(data)
    return found


def samples(data, table):
    uniform, count = struct.unpack_from(">II", table[b"stsz"], 4)
    if uniform or count != 24 or struct.unpack_from(">I", table[b"stco"], 4)[0] != 1:
        raise ValueError("unexpected single-track sample layout")
    offset = struct.unpack_from(">I", table[b"stco"], 8)[0]
    result = []
    for length in struct.unpack_from(">24I", table[b"stsz"], 12):
        sample = data[offset:offset + length]
        offset += length
        nals, position = [], 0
        while position < len(sample):
            size = struct.unpack_from(">I", sample, position)[0]
            position += 4
            nal = sample[position:position + size]
            if size < 2 or len(nal) != size:
                raise ValueError("truncated NAL")
            nals.append(nal)
            position += size
        result.append(nals)
    return result


class LinuxDolbyFixtureCase(unittest.TestCase):
    def test_4k_missing_rpu_control_preserves_coded_frames_and_presentation_tables(self):
        manifest = json.loads((CORPUS / "manifest.json").read_text())
        self.assertEqual(manifest["source_shape"], [3840, 2160])
        self.assertEqual(manifest["frame_rate"], [24, 1])
        self.assertEqual(manifest["dolby_vision_level"], 6)
        self.assertEqual(manifest["license"], "CC0-1.0")
        parsed = []
        for case in manifest["cases"]:
            data = (CORPUS / case["path"]).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), case["sha256"])
            self.assertEqual(len(data), case["byte_length"])
            self.assertEqual(data.count(b"dvcC" + bytes((1, 0, 10, 53))), 1)
            table = tables(data)
            parsed.append((table, samples(data, table)))
        for kind in [b"stts", b"ctts", b"stsc"]:
            self.assertEqual(parsed[0][0][kind], parsed[1][0][kind])
        for index, (positive, negative) in enumerate(zip(parsed[0][1], parsed[1][1])):
            def is_rpu(nal):
                return (nal[0] >> 1) & 63 == 62
            self.assertEqual(sum(map(is_rpu, positive)), 1)
            self.assertEqual(sum(map(is_rpu, negative)), 0 if index == 12 else 1)
            self.assertEqual([nal for nal in positive if not is_rpu(nal)],
                             [nal for nal in negative if not is_rpu(nal)])


if __name__ == "__main__":
    unittest.main()
