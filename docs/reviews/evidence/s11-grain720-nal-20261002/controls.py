"""Genuinely NEW tiny syntax controls, not decodable corpus fixtures.

Each test ID executes once. All parsed synthetic input bytes share a1MiB
aggregate budget. No production files, tools, network or subprocess calls.
"""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import sys
import time
import unittest
from unittest import mock

PARSER = Path('/private/tmp/s11-retained-ts-h264-nal-parser-20261002.py')
spec = importlib.util.spec_from_file_location('s11_private_nal', PARSER)
nal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nal)
TOTAL_INPUT = 0
INPUT_DIGESTS = []


def exercise(raw):
    global TOTAL_INPUT
    TOTAL_INPUT += len(raw)
    assert TOTAL_INPUT <= 1024 * 1024, 'aggregate synthetic input budget'
    INPUT_DIGESTS.append({'bytes': len(raw), 'sha256': nal.sha(raw)})
    return nal.parse_segment(raw)


class Writer:
    def __init__(self):
        self.bits = ''

    def u(self, value, count):
        assert 0 <= value < 1 << count
        self.bits += f'{value:0{count}b}'
        return self

    def ue(self, value):
        code = f'{value + 1:b}'
        self.bits += '0' * (len(code) - 1) + code
        return self

    def se(self, value):
        return self.ue(2 * value - 1 if value > 0 else -2 * value)

    def end(self):
        self.bits += '1'
        self.bits += '0' * (-len(self.bits) % 8)
        return int(self.bits, 2).to_bytes(len(self.bits) // 8, 'big')


def escaped(raw):
    out, zeros = bytearray(), 0
    for value in raw:
        if zeros >= 2 and value <= 3:
            out.append(3)
            zeros = 0
        out.append(value)
        zeros = zeros + 1 if value == 0 else 0
    return bytes(out)


def nalu(header, raw):
    return b'\0\0\0\1' + bytes([header]) + escaped(raw)


def sps(sid=0):
    b = Writer().u(66, 8).u(0, 8).u(30, 8).ue(sid)
    b.ue(0).ue(0).ue(0).ue(1).u(0, 1).ue(79).ue(44)
    b.u(1, 1).u(1, 1).u(0, 1).u(0, 1)
    return nalu(0x67, b.end())


def pps(pid=0, sid=0):
    b = Writer().ue(pid).ue(sid).u(0, 1).u(0, 1).ue(0)
    b.ue(0).ue(0).u(0, 1).u(0, 2).se(0).se(0).se(0)
    b.u(1, 1).u(0, 1).u(0, 1)
    return nalu(0x68, b.end())


def slice_nal(idr=True, first=0, frame=0, pid=0):
    # A header-prefix fixture only, deliberately not a decodable picture.
    b = Writer().ue(first).ue(2).ue(pid).u(frame, 4)
    if idr:
        b.ue(0)
    b.u(frame, 4)
    return nalu(0x65 if idr else 0x61, b.end())


def au(idr=True, parameters=True, aud=True, second=None, pid=0):
    value = nalu(9, Writer().u(0, 3).end()) if aud else b''
    if parameters:
        value += sps() + pps()
    value += slice_nal(idr=idr, pid=pid)
    if second:
        value += second
    return value


def section(header, tail):
    size = len(header) + len(tail) + 4
    body = bytes([header[0], 0xb0 | ((size - 3) >> 8), (size - 3) & 255]) + header[3:] + tail
    return body + nal.crc_mpeg(body).to_bytes(4, 'big')


def pat(programs=((1, 4096),)):
    tail = b''.join(p.to_bytes(2, 'big') + (0xe000 | pid).to_bytes(2, 'big') for p, pid in programs)
    return section(bytes([0, 0, 0, 0, 1, 0xc1, 0, 0]), tail)


def pmt(entries=((0x1b, 256),)):
    tail = b''.join(bytes([kind]) + (0xe000 | pid).to_bytes(2, 'big') + b'\xf0\0' for kind, pid in entries)
    return section(bytes([2, 0, 0, 0, 1, 0xc1, 0, 0, 0xe1, 0, 0xf0, 0]), tail)


def pts_bytes(value, prefix=2):
    return bytes([(prefix << 4) | (((value >> 30) & 7) << 1) | 1,
                  (value >> 22) & 255, (((value >> 15) & 127) << 1) | 1,
                  (value >> 7) & 255, ((value & 127) << 1) | 1])


def pes(es, pts=90000, timestamp_override=None):
    stamp = timestamp_override if timestamp_override is not None else (pts_bytes(pts) if pts is not None else b'')
    return b'\0\0\1\xe0\0\0\x80' + bytes([0x80 if stamp else 0, len(stamp)]) + stamp + es


def packet(pid, payload, start, counter):
    assert 0 < len(payload) <= 160
    length = 183 - len(payload)
    return bytes([0x47, (0x40 if start else 0) | (pid >> 8), pid & 255, 0x30 | counter]) + \
        bytes([length, 0]) + b'\xff' * (length - 1) + payload


def ts(peses, pat_bytes=None, pmt_bytes=None):
    value = packet(0, b'\0' + (pat_bytes if pat_bytes is not None else pat()), True, 0)
    value += packet(4096, b'\0' + (pmt_bytes if pmt_bytes is not None else pmt()), True, 0)
    counter = 0
    for raw in peses:
        for offset in range(0, len(raw), 160):
            value += packet(256, raw[offset:offset + 160], offset == 0, counter)
            counter = (counter + 1) % 16
    return value


class S11NALNewControls(unittest.TestCase):
    def refuses(self, raw, message):
        with self.assertRaisesRegex(nal.Refusal, message):
            exercise(raw)

    def test_new_idr_headers_cross_ts_and_pes_without_frame_inference(self):
        es = au(second=slice_nal(first=1)) + nalu(12, b'\xff' * 260 + b'\x80')
        result = exercise(ts([pes(es)]))
        self.assertTrue(result['first_idr'])
        self.assertEqual(1, len(result['access_units']))
        self.assertEqual(2, result['access_units'][0]['slices'])
        self.assertEqual([1280, 720], result['access_units'][0]['geometry'])
        split = exercise(ts([pes(es[:9]), pes(es[9:], pts=None)]))
        self.assertEqual(1, split['idr_count'])
        self.assertEqual(90000, split['access_units'][0]['pts90k'])

    def test_new_non_idr_i_slice_and_sei_65_byte_are_not_idr(self):
        es = nalu(9, Writer().u(0, 3).end()) + sps() + pps()
        es += nalu(6, b'\x05\x01\x65\x80') + slice_nal(idr=False)
        result = exercise(ts([pes(es)]))
        self.assertFalse(result['first_idr'])
        self.assertEqual(1, result['access_units'][0]['vcl_nal_type'])
        self.assertEqual(2, result['access_units'][0]['slice_type'])

    def test_new_transport_truncation_continuity_crc_and_program_ambiguity_refuse(self):
        good = ts([pes(au() + nalu(12, b'\xff' * 260 + b'\x80'))])
        self.refuses(good[:-1], 'TS size/truncation')
        corrupt = bytearray(good)
        corrupt[1] |= 0x80
        self.refuses(bytes(corrupt), 'TS sync/TEI')
        corrupt = bytearray(good)
        corrupt[3 * 188 + 3] = (corrupt[3 * 188 + 3] & 0xf0) | 7
        self.refuses(bytes(corrupt), 'TS continuity')
        corrupt_pat = pat()[:-1] + bytes([pat()[-1] ^ 1])
        self.refuses(ts([pes(au())], pat_bytes=corrupt_pat), 'PSI CRC')
        self.refuses(ts([pes(au())], pat_bytes=pat(((1, 4096), (2, 4097)))), 'PAT program ambiguity')
        self.refuses(ts([pes(au())], pmt_bytes=pmt(((0x1b, 256), (0x1b, 257)))), 'ambiguous PMT')

    def test_new_missing_aud_multiple_pictures_and_shared_pes_pts_refuse(self):
        self.refuses(ts([pes(au(aud=False))]), 'VCL without AUD')
        self.refuses(ts([pes(au() + au(parameters=False))]), 'multiple AUs share')
        self.refuses(ts([pes(au(second=slice_nal(first=1, frame=1, idr=False)))]), 'ambiguous multiple pictures')
        self.refuses(ts([pes(au(second=slice_nal(first=0)))]), 'ambiguous multiple pictures')

    def test_new_parameter_references_and_invalid_idr_prefix_refuse(self):
        self.refuses(ts([pes(au(parameters=False))]), 'missing PPS')
        es = nalu(9, Writer().u(0, 3).end()) + pps() + slice_nal()
        self.refuses(ts([pes(es)]), 'PPS missing SPS')
        es = nalu(9, Writer().u(0, 3).end()) + sps() + pps(sid=1) + slice_nal()
        self.refuses(ts([pes(es)]), 'PPS missing SPS')
        self.refuses(ts([pes(au(pid=1))]), 'missing PPS')
        self.refuses(ts([pes(au(second=slice_nal(first=1, frame=1)))]), 'invalid IDR prefix')

    def test_new_missing_marker_duplicate_and_continuation_pts_refuse(self):
        self.refuses(ts([pes(au(), pts=None)]), 'AU without unique PTS')
        stamp = bytearray(pts_bytes(90000))
        stamp[4] &= 254
        self.refuses(ts([pes(au(), timestamp_override=bytes(stamp))]), 'PES timestamp markers')
        es = au()
        self.refuses(ts([pes(es[:9]), pes(es[9:])]), 'timestamp inside continued AU')
        self.refuses(ts([pes(au()), pes(au(parameters=False))]), 'duplicate AU PTS')

    def test_new_two_auds_two_independent_pts_have_two_actual_access_units(self):
        result = exercise(ts([pes(au(), 90000), pes(au(parameters=False), 180000)]))
        self.assertEqual([90000, 180000], [unit['pts90k'] for unit in result['access_units']])
        self.assertEqual(2, result['idr_count'])

    def test_new_held_root_hash_symlink_traversal_and_path_swap_refuse(self):
        global TOTAL_INPUT
        data = b'tiny-owned-descriptor-control'
        TOTAL_INPUT += len(data) * 6
        INPUT_DIGESTS.append({'bytes': len(data) * 6, 'sha256': nal.sha(data),
                              'kind': 'six bounded held-file exercises'})
        assert TOTAL_INPUT <= 1024 * 1024
        with tempfile.TemporaryDirectory(prefix='s11-nal-new-control-', dir='/private/tmp') as temporary:
            root_path = Path(temporary)
            file = root_path / 'fixture.ts'
            file.write_bytes(data)
            file.chmod(0o400)
            with nal.OwnedRoot(str(root_path), os.getuid()) as root:
                actual, fact = root.read('fixture.ts', nal.sha(data), 64)
                self.assertEqual(data, actual)
                with self.assertRaisesRegex(nal.Refusal, 'nonflat'):
                    root.read('../fixture.ts', nal.sha(data), 64)
                with self.assertRaisesRegex(nal.Refusal, 'hash mismatch'):
                    root.read('fixture.ts', '0' * 64, 64)
                (root_path / 'link.ts').symlink_to(file)
                with self.assertRaisesRegex(nal.Refusal, 'file open refused'):
                    root.read('link.ts', nal.sha(data), 64)
                original_read = os.read
                changed = False

                def swapped(fd, cap):
                    nonlocal changed
                    value = original_read(fd, cap)
                    if not changed:
                        changed = True
                        file.rename(root_path / 'old.ts')
                        file.write_bytes(data)
                        file.chmod(0o400)
                    return value

                with mock.patch.object(os, 'read', swapped):
                    with self.assertRaisesRegex(nal.Refusal, 'changed held file'):
                        root.read('fixture.ts', nal.sha(data), 64)

    def test_new_exclusive_output_root_and_duplicate_json_refuse(self):
        global TOTAL_INPUT
        data = b'{"new":true}'
        TOTAL_INPUT += len(data) + 13
        INPUT_DIGESTS.append({'bytes': len(data) + 13, 'sha256': nal.sha(data),
                              'kind': 'bounded output/JSON control'})
        with tempfile.TemporaryDirectory(prefix='s11-nal-new-output-', dir='/private/tmp') as temporary:
            output = str(Path(temporary) / 'new.json')
            nal.exclusive_result(output, data, os.getuid(), time.monotonic() + 1)
            self.assertEqual(data, Path(output).read_bytes())
            with self.assertRaises(FileExistsError):
                nal.exclusive_result(output, data, os.getuid(), time.monotonic() + 1)
            with self.assertRaisesRegex(nal.Refusal, 'deadline'):
                nal.exclusive_result(str(Path(temporary) / 'late.json'), data,
                                     os.getuid(), time.monotonic() - 1)
            with self.assertRaisesRegex(nal.Refusal, 'duplicate JSON key'):
                nal.unique_json(b'{"x":1,"x":2}')


class JournalResult(unittest.TextTestResult):
    def addSuccess(self, test):
        super().addSuccess(test)
        JOURNAL['successful_ids'].append(test.id())
        save_journal()


JOURNAL = {'kind': 'NEW-tiny-syntax-controls-only', 'successful_ids': []}


def save_journal():
    path = Path(os.environ['S11_NAL_CONTROL_JOURNAL'])
    value = {**JOURNAL, 'input_bytes': TOTAL_INPUT, 'input_cases': len(INPUT_DIGESTS),
             'input_manifest_sha256': nal.sha(json.dumps(INPUT_DIGESTS, sort_keys=True).encode()),
             'parser_sha256': nal.sha(PARSER.read_bytes()),
             'controls_sha256': nal.sha(Path(__file__).read_bytes())}
    encoded = json.dumps(value, sort_keys=True).encode()
    assert len(encoded) <= 65536
    path.write_bytes(encoded)


if __name__ == '__main__':
    save_journal()
    # Retry dispatch can name only failed or genuinely new IDs; never rerun success.
    suite = (unittest.defaultTestLoader.loadTestsFromNames(sys.argv[1:],
             sys.modules[__name__]) if len(sys.argv) > 1 else
             unittest.defaultTestLoader.loadTestsFromTestCase(S11NALNewControls))
    result = unittest.TextTestRunner(verbosity=2, resultclass=JournalResult).run(
        suite)
    JOURNAL.update(complete=True, tests_run=result.testsRun,
                   failures=[test.id() for test, detail in result.failures],
                   errors=[test.id() for test, detail in result.errors])
    save_journal()
    raise SystemExit(0 if result.wasSuccessful() else 1)
