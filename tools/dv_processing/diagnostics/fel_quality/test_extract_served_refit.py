"""Local wire and timestamp controls for the served F+ extractor."""
from fractions import Fraction
import unittest

from extract_served_refit import VIDEO_CONFIG, access_unit, init_config, select_packets


def box(kind, payload):
    return (len(payload) + 8).to_bytes(4, "big") + kind + payload


def init(compatibility=1):
    packed = (8 << 9) | (6 << 3) | 5
    payload = b"\x01\0" + packed.to_bytes(2, "big") + bytes([compatibility << 4]) + bytes(19)
    track = box(b"hvc1", bytes(78) + box(b"dvvC", payload))
    return box(b"styp", b"msdh") + box(b"moov", box(b"trak", box(b"mdia", box(
        b"minf", box(b"stbl", box(b"stsd", bytes(4) + b"\0\0\0\1" + track))))))


class ServedExtractorTests(unittest.TestCase):
    def test_requires_profile_81_in_a_sample_entry(self):
        self.assertEqual(init_config(init()), VIDEO_CONFIG)
        with self.assertRaisesRegex(ValueError, "Profile 8.1"):
            init_config(init(4))
        with self.assertRaisesRegex(ValueError, "HEVC dvvC"):
            init_config(box(b"free", init()))

    def test_annexb_proof_matches_base_layer_and_ignores_wrapped_el(self):
        vcl = b"\x26\x01\x80\x00\x00\x03\x01\x01"
        rpu = b"\x7c\x01\x19\x08\x09\x80"
        el = b"\x7e\x01\x20\x01\x80"
        source = access_unit(b"\0\0\0\1" + vcl + b"\0\0\1" + el + b"\0\0\1" + rpu)
        served = access_unit(b"\0\0\1" + vcl + b"\0\0\0\1" + rpu + b"\0")
        self.assertEqual(source[:2], served[:2])
        self.assertEqual(source[2:], (1, rpu))
        self.assertEqual(served[2:], (0, rpu))
        with self.assertRaisesRegex(ValueError, "one UNSPEC62"):
            access_unit(b"\0\0\1" + vcl)

    def test_clock_transform_uses_only_the_explicit_offset(self):
        captured = {Fraction(125, 1000): {}, Fraction(167, 1000): {}}
        packets = [{"pts": 0}, {"pts": 42}]
        bound = select_packets(packets, Fraction(1, 1000), Fraction(125, 1000), captured)
        self.assertEqual(list(bound.values()), [(0, packets[0]), (1, packets[1])])
        with self.assertRaisesRegex(ValueError, "unmatched"):
            select_packets(packets, Fraction(1, 1000), Fraction(126, 1000), captured)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            select_packets(packets + [packets[1]], Fraction(1, 1000), Fraction(125, 1000), captured)


if __name__ == "__main__":
    unittest.main()
