#!/usr/bin/env python3
"""Private offline syntax oracle, not a decoder or original S11 qualification.

References: ITU-T H.222.0 sections 2.4.3/2.4.4 (TS/PSI/PES), H.264
7.3/7.4.1.2.4 and Annex B (NAL/picture headers); primary implementation
cross-checks FFmpeg n8.0 libavformat/mpegts.c and libavcodec/h264_ps.c.
Deliberately supported subset: one stable program, one H264 PID, AUD-delimited
progressive 8-bit 4:2:0 AVC, no slice groups/scaling matrices/POC type1 or
extension NALs. Unsupported does not mean invalid; it means this oracle refuses.
Only slice/header identity is parsed: no entropy/pixel decode or full H264
conformance assertion. A type5 header result is not physical decoder acceptance.
"""
import argparse
import hashlib
import json
import os
import re
import stat
import time
from fractions import Fraction

MAX_SEGMENT = 32 * 1024 * 1024
MAX_NAL = 16 * 1024 * 1024
MAX_JSON = 1024 * 1024


class Refusal(ValueError):
    pass


def need(condition, reason):
    if not condition:
        raise Refusal(reason)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def unique_json(data):
    def pairs(rows):
        result = {}
        for key, value in rows:
            need(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=pairs)


def identity(value):
    return (value.st_dev, value.st_ino, value.st_size,
            value.st_mtime_ns, value.st_ctime_ns, value.st_mode, value.st_uid)


class OwnedRoot:
    """Flat readonly files opened relative to a held non-symlink directory FD."""
    def __init__(self, path, uid=501, deadline=None):
        need(os.path.isabs(path) and os.path.realpath(path) == path, 'noncanonical root')
        self.path, self.uid, self.deadline = path, uid, deadline
        self.fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.start = os.fstat(self.fd)
        try:
            need(self.start.st_uid == uid and not self.start.st_mode & 0o077,
                 'root ownership/privacy')
            self.check()
        except BaseException:
            os.close(self.fd)
            raise

    def __enter__(self):
        return self

    def __exit__(self, *unused):
        os.close(self.fd)

    def check(self):
        need(self.deadline is None or time.monotonic() < self.deadline, 'deadline')
        current = os.stat(self.path, follow_symlinks=False)
        need(stat.S_ISDIR(current.st_mode) and
             (current.st_dev, current.st_ino) == (self.start.st_dev, self.start.st_ino),
             'changed root path')

    def read(self, name, expected, cap):
        need(re.fullmatch(r'[A-Za-z0-9_.-]{1,80}', name) and name not in ('.', '..'),
             'nonflat path')
        need(re.fullmatch(r'[0-9a-f]{64}', expected) is not None, 'missing hash pin')
        self.check()
        try:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=self.fd)
        except OSError as error:
            raise Refusal('file open refused') from error
        try:
            start = os.fstat(fd)
            need(stat.S_ISREG(start.st_mode) and start.st_uid == self.uid and
                 not start.st_mode & 0o222 and start.st_size <= cap,
                 'file type/owner/readonly/bound')
            chunks, count = [], 0
            while True:
                self.check()
                chunk = os.read(fd, min(65536, cap + 1 - count))
                if not chunk:
                    break
                count += len(chunk)
                need(count <= cap, 'file byte bound')
                chunks.append(chunk)
            data = b''.join(chunks)
            end = os.fstat(fd)
            path_end = os.stat(name, dir_fd=self.fd, follow_symlinks=False)
            need(identity(start) == identity(end) == identity(path_end), 'changed held file')
            need(len(data) == start.st_size and sha(data) == expected, 'file hash mismatch')
            return data, {'name': name, 'bytes': count, 'sha256': expected,
                          'dev': start.st_dev, 'inode': start.st_ino,
                          'mtime_ns': start.st_mtime_ns, 'ctime_ns': start.st_ctime_ns}
        finally:
            os.close(fd)


def crc_mpeg(data):
    value = 0xffffffff
    for byte in data:
        value ^= byte << 24
        for unused in range(8):
            value = ((value << 1) ^ (0x04c11db7 if value & 0x80000000 else 0)) & 0xffffffff
    return value


def exclusive_result(path, data, uid, deadline):
    """Create one bounded result relative to a held private output directory."""
    need(os.path.isabs(path), 'nonabsolute output')
    parent, name = os.path.split(path)
    need(re.fullmatch(r'[A-Za-z0-9_.-]{1,80}', name) and name not in ('.', '..'),
         'nonflat output')
    need(len(data) <= MAX_JSON, 'aggregate result bound')
    with OwnedRoot(parent, uid, deadline) as root:
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=root.fd)
        try:
            start = os.fstat(fd)
            with os.fdopen(fd, 'wb', closefd=False) as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            root.check()
            end = os.fstat(fd)
            named = os.stat(name, dir_fd=root.fd, follow_symlinks=False)
            need(stat.S_ISREG(end.st_mode) and end.st_uid == uid and
                 end.st_size == len(data) and
                 (start.st_dev, start.st_ino) == (end.st_dev, end.st_ino) ==
                 (named.st_dev, named.st_ino), 'changed output identity')
        finally:
            os.close(fd)


class Sections:
    def __init__(self):
        self.buffer = bytearray()

    def consume(self, data):
        self.buffer.extend(data)
        output = []
        while self.buffer:
            if self.buffer[0] == 0xff:
                need(all(x == 0xff for x in self.buffer), 'PSI stuffing')
                self.buffer.clear()
                break
            if len(self.buffer) < 3:
                break
            length = ((self.buffer[1] & 15) << 8) | self.buffer[2]
            need(9 <= length <= 1021, 'PSI section length')
            total = length + 3
            if len(self.buffer) < total:
                break
            section = bytes(self.buffer[:total])
            del self.buffer[:total]
            need(crc_mpeg(section) == 0, 'PSI CRC')
            need(section[1] & 0xf0 == 0xb0 and section[5] & 0xc1 == 0xc1,
                 'PSI syntax/current')
            need(section[6:8] == b'\0\0', 'multi-section PSI unsupported')
            output.append(section)
        need(len(self.buffer) <= 1024, 'PSI buffer bound')
        return output

    def feed(self, payload, start):
        if start:
            need(payload, 'missing PSI pointer')
            pointer = payload[0]
            need(pointer < len(payload), 'PSI pointer bound')
            before, after = payload[1:1 + pointer], payload[1 + pointer:]
            if self.buffer:
                output = self.consume(before)
                need(not self.buffer, 'unfinished PSI at pointer')
            else:
                need(all(x == 0xff for x in before), 'orphan PSI prefix')
                output = []
            return output + self.consume(after)
        need(self.buffer, 'orphan PSI continuation')
        return self.consume(payload)


def timestamp(raw, prefix):
    need(len(raw) == 5 and raw[0] >> 4 == prefix and
         all(raw[i] & 1 for i in (0, 2, 4)), 'PES timestamp markers')
    return ((raw[0] & 14) << 29) | (raw[1] << 22) | \
        ((raw[2] & 254) << 14) | (raw[3] << 7) | (raw[4] >> 1)


def pes_parts(raw, serial, ts_offset):
    need(len(raw) >= 9 and raw[:3] == b'\0\0\1' and 0xe0 <= raw[3] <= 0xef,
         'PES video prefix')
    declared = int.from_bytes(raw[4:6], 'big')
    need(not declared or len(raw) == declared + 6, 'PES declared length')
    need(raw[6] & 0xc0 == 0x80 and not raw[6] & 0x30, 'PES marker/scrambling')
    flags, header_len = raw[7], raw[8]
    need(not flags & 0x3f and flags >> 6 != 1, 'unsupported PES optional fields')
    need(9 + header_len < len(raw), 'PES header/payload bound')
    offset, pts, dts = 9, None, None
    if flags >> 6 == 2:
        pts = timestamp(raw[offset:offset + 5], 2)
        offset += 5
    elif flags >> 6 == 3:
        pts = timestamp(raw[offset:offset + 5], 3)
        dts = timestamp(raw[offset + 5:offset + 10], 1)
        offset += 10
    need(offset <= 9 + header_len and all(x == 255 for x in raw[offset:9 + header_len]),
         'PES optional/header stuffing')
    return raw[9 + header_len:], {'pes': serial, 'pts': pts, 'dts': dts, 'ts_offset': ts_offset}


def demux(raw):
    need(0 < len(raw) <= MAX_SEGMENT and len(raw) % 188 == 0, 'TS size/truncation')
    continuity, sections = {}, {0: Sections()}
    pat, pmt = None, None
    video_pid = None
    pending, pending_offset = bytearray(), None
    elementary, spans = bytearray(), []

    def finish_pes():
        if pending:
            payload, facts = pes_parts(bytes(pending), len(spans), pending_offset)
            facts.update(start=len(elementary), end=len(elementary) + len(payload))
            elementary.extend(payload)
            spans.append(facts)
            need(len(spans) <= 4096 and len(elementary) <= MAX_SEGMENT, 'ES/span bound')
            pending.clear()

    for offset in range(0, len(raw), 188):
        packet = raw[offset:offset + 188]
        need(packet[0] == 0x47 and not packet[1] & 0x80 and not packet[3] & 0xc0,
             'TS sync/TEI/scrambling')
        pid = ((packet[1] & 31) << 8) | packet[2]
        start, control, counter = bool(packet[1] & 64), (packet[3] >> 4) & 3, packet[3] & 15
        need(control != 0, 'TS adaptation control')
        position = 4
        if control & 2:
            length = packet[position]
            position += 1
            need(position + length <= 188, 'adaptation length')
            if length:
                flags = packet[position]
                need(not flags & 0x80, 'discontinuity unsupported')
                required = 1 + (6 if flags & 0x10 else 0) + (6 if flags & 8 else 0)
                need(length >= required, 'adaptation PCR/OPCR bound')
                # Optional adaptation contents do not supply random-access authority.
                need(not flags & 7, 'adaptation private/splice/extension unsupported')
            position += length
        if not control & 1:
            need(position == 188, 'adaptation-only size')
            if pid in continuity and pid != 8191:
                need(counter == continuity[pid], 'adaptation-only continuity')
            continue
        need(position < 188, 'empty TS payload')
        payload = packet[position:]
        if pid == 8191:
            continue
        if pid in continuity:
            need(counter == (continuity[pid] + 1) % 16, 'TS continuity/duplicate')
        continuity[pid] = counter
        if pid in sections:
            for section in sections[pid].feed(payload, start):
                if pid == 0:
                    need(section[0] == 0 and (len(section) - 12) % 4 == 0, 'PAT syntax')
                    programs = []
                    for cursor in range(8, len(section) - 4, 4):
                        program = int.from_bytes(section[cursor:cursor + 2], 'big')
                        need(section[cursor + 2] & 0xe0 == 0xe0, 'PAT PID reserved')
                        ppid = ((section[cursor + 2] & 31) << 8) | section[cursor + 3]
                        if program:
                            programs.append((program, ppid))
                    need(len(programs) == 1 and 16 < programs[0][1] < 8191, 'PAT program ambiguity')
                    need(pat is None or pat == programs[0], 'changed PAT')
                    pat = programs[0]
                    sections.setdefault(pat[1], Sections())
                else:
                    need(pat and section[0] == 2 and int.from_bytes(section[3:5], 'big') == pat[0]
                         and len(section) >= 16, 'PMT syntax/program')
                    need(section[8] & 0xe0 == 0xe0 and section[10] & 0xf0 == 0xf0,
                         'PMT reserved')
                    info_length = ((section[10] & 15) << 8) | section[11]
                    cursor, entries = 12 + info_length, []
                    need(cursor <= len(section) - 4, 'PMT program descriptors')
                    while cursor < len(section) - 4:
                        need(cursor + 5 <= len(section) - 4, 'PMT stream header')
                        kind = section[cursor]
                        need(section[cursor + 1] & 0xe0 == 0xe0 and section[cursor + 3] & 0xf0 == 0xf0,
                             'PMT stream reserved')
                        ep = ((section[cursor + 1] & 31) << 8) | section[cursor + 2]
                        size = ((section[cursor + 3] & 15) << 8) | section[cursor + 4]
                        cursor += 5 + size
                        need(cursor <= len(section) - 4, 'PMT ES descriptors')
                        entries.append((kind, ep))
                    need(len(entries) <= 32 and len({x[1] for x in entries}) == len(entries), 'PMT PID ambiguity')
                    videos = [ep for kind, ep in entries if kind == 0x1b]
                    need(len(videos) == 1 and all(kind in (0x1b, 0x0f) for kind, ep in entries),
                         'unsupported/ambiguous PMT streams')
                    value = (tuple(entries), ((section[8] & 31) << 8) | section[9])
                    need(pmt is None or pmt == value, 'changed PMT')
                    pmt, video_pid = value, videos[0]
        elif pid == video_pid:
            if start:
                finish_pes()
                pending_offset = offset
            else:
                need(pending, 'orphan video PES continuation')
            pending.extend(payload)
            need(len(pending) <= MAX_SEGMENT, 'PES buffer bound')
        else:
            need(pmt is not None or pid == 17, 'payload before program discovery')
            if pmt:
                need(pid == 17 or pid in {ep for kind, ep in pmt[0]}, 'unmapped payload PID')
    need(pat and pmt and video_pid is not None, 'missing PAT/PMT')
    need(all(not value.buffer for value in sections.values()), 'truncated PSI')
    finish_pes()
    need(elementary and spans, 'missing video PES')
    return bytes(elementary), spans, video_pid


def rbsp(payload):
    output, zeros = bytearray(), 0
    for index, value in enumerate(payload):
        if zeros >= 2:
            if value == 3:
                need(index + 1 < len(payload) and payload[index + 1] <= 3, 'RBSP escape')
                zeros = 0
                continue
            need(value > 2, 'unescaped RBSP sequence')
        output.append(value)
        zeros = zeros + 1 if value == 0 else 0
    return bytes(output)


class Bits:
    def __init__(self, raw):
        self.raw, self.position = raw, 0

    def u(self, count):
        need(0 <= count <= 64 and self.position + count <= len(self.raw) * 8, 'truncated bit header')
        result = 0
        for unused in range(count):
            result = (result << 1) | ((self.raw[self.position // 8] >> (7 - self.position % 8)) & 1)
            self.position += 1
        return result

    def ue(self, cap=65535):
        zeros = 0
        while not self.u(1):
            zeros += 1
            need(zeros <= 31, 'Golomb bound')
        value = (1 << zeros) - 1 + self.u(zeros)
        need(value <= cap, 'Golomb value bound')
        return value

    def se(self, cap=65535):
        value = self.ue(cap * 2)
        return (value + 1) // 2 if value & 1 else -(value // 2)

    def more(self):
        remaining = len(self.raw) * 8 - self.position
        if remaining > 8:
            return True
        value = self.u(remaining)
        self.position -= remaining
        return value != (1 << (remaining - 1)) if remaining else False

    def trailing(self):
        need(self.u(1) == 1, 'RBSP trailing stop')
        while self.position % 8:
            need(self.u(1) == 0, 'RBSP trailing alignment')
        need(self.position == len(self.raw) * 8, 'RBSP trailing bytes')


def hrd(bits):
    count = bits.ue(31) + 1
    bits.u(8)
    for unused in range(count):
        bits.ue(0xffffffff)
        bits.ue(0xffffffff)
        bits.u(1)
    bits.u(20)


def vui(bits):
    if bits.u(1):
        aspect = bits.u(8)
        if aspect == 255:
            need(bits.u(16) > 0 and bits.u(16) > 0, 'VUI SAR')
    if bits.u(1):
        bits.u(1)
    if bits.u(1):
        bits.u(4)
        if bits.u(1):
            bits.u(24)
    if bits.u(1):
        bits.ue(5)
        bits.ue(5)
    if bits.u(1):
        need(bits.u(32) > 0 and bits.u(32) > 0, 'VUI timing')
        bits.u(1)
    nal_hrd = bits.u(1)
    if nal_hrd:
        hrd(bits)
    vcl_hrd = bits.u(1)
    if vcl_hrd:
        hrd(bits)
    if nal_hrd or vcl_hrd:
        bits.u(1)
    bits.u(1)
    if bits.u(1):
        bits.u(1)
        for unused in range(6):
            bits.ue()


def parse_sps(payload):
    bits = Bits(rbsp(payload))
    profile, constraints, level = bits.u(8), bits.u(8), bits.u(8)
    need(profile in (66, 77, 88, 100) and constraints & 3 == 0, 'unsupported SPS profile')
    sid = bits.ue(31)
    if profile == 100:
        need(bits.ue(3) == 1 and bits.ue(6) == 0 and bits.ue(6) == 0,
             'unsupported chroma/bit depth')
        need(bits.u(1) == 0 and bits.u(1) == 0, 'SPS bypass/scaling unsupported')
    frame_bits = bits.ue(12) + 4
    poc_type = bits.ue(2)
    need(poc_type != 1, 'POC type1 unsupported')
    poc_bits = bits.ue(12) + 4 if poc_type == 0 else 0
    bits.ue(16)
    bits.u(1)
    width, height = bits.ue(511) + 1, bits.ue(511) + 1
    need(bits.u(1) == 1, 'interlaced SPS unsupported')
    bits.u(1)
    crop = [bits.ue(8192) for unused in range(4)] if bits.u(1) else [0, 0, 0, 0]
    if bits.u(1):
        vui(bits)
    bits.trailing()
    need(width * 16 > 2 * (crop[0] + crop[1]) and height * 16 > 2 * (crop[2] + crop[3]),
         'SPS crop')
    return {'id': sid, 'frame_bits': frame_bits, 'poc_type': poc_type,
            'poc_bits': poc_bits, 'mbs': width * height, 'profile': profile,
            'width': width * 16 - 2 * (crop[0] + crop[1]),
            'height': height * 16 - 2 * (crop[2] + crop[3])}


def parse_pps(payload, sequences):
    bits = Bits(rbsp(payload))
    pid, sid = bits.ue(255), bits.ue(31)
    need(sid in sequences, 'PPS missing SPS reference')
    bits.u(1)
    bottom = bits.u(1)
    need(bits.ue(7) == 0, 'slice groups unsupported')
    bits.ue(31)
    bits.ue(31)
    bits.u(1)
    bits.u(2)
    bits.se(51)
    bits.se(51)
    bits.se(12)
    bits.u(1)
    bits.u(1)
    redundant = bits.u(1)
    if bits.more():
        bits.u(1)
        need(bits.u(1) == 0, 'PPS scaling unsupported')
        bits.se(12)
    bits.trailing()
    return {'id': pid, 'sps': sid, 'bottom': bottom, 'redundant': redundant}


def slice_prefix(nal, pictures, sequences):
    kind, reference = nal[0] & 31, (nal[0] >> 5) & 3
    need(kind in (1, 5), 'unsupported VCL type')
    bits = Bits(rbsp(nal[1:]))
    first, slice_type, pid = bits.ue(), bits.ue(9), bits.ue(255)
    need(pid in pictures, 'slice missing PPS reference')
    pps = pictures[pid]
    need(pps['sps'] in sequences, 'slice missing SPS reference')
    sps = sequences[pps['sps']]
    need(first < sps['mbs'], 'first macroblock bound')
    frame = bits.u(sps['frame_bits'])
    idr_id = bits.ue(65535) if kind == 5 else None
    if kind == 5:
        need(reference != 0 and slice_type % 5 == 2 and frame == 0, 'invalid IDR prefix')
    poc = bits.u(sps['poc_bits']) if sps['poc_type'] == 0 else None
    delta = bits.se() if sps['poc_type'] == 0 and pps['bottom'] else 0
    need(not pps['redundant'] or bits.ue(127) == 0, 'redundant pictures unsupported')
    return {'first_mb': first, 'slice_type': slice_type % 5, 'pps': pid,
            'kind': kind, 'signature': (pid, frame, bool(reference), kind == 5,
                                      idr_id, poc, delta),
            'sps': pps['sps'], 'geometry': (sps['width'], sps['height'])}


def annex_nals(elementary):
    starts = []
    cursor = 0
    while cursor + 3 <= len(elementary):
        if elementary[cursor:cursor + 3] == b'\0\0\1':
            start = cursor - 1 if cursor and elementary[cursor - 1] == 0 else cursor
            starts.append((start, cursor + 3))
            cursor += 3
        else:
            cursor += 1
    need(starts and all(x == 0 for x in elementary[:starts[0][0]]), 'Annex-B prefix')
    need(len(starts) <= 16384, 'NAL count bound')
    for index, (start, payload) in enumerate(starts):
        end = starts[index + 1][0] if index + 1 < len(starts) else len(elementary)
        while end > payload and elementary[end - 1] == 0:
            end -= 1
        need(0 < end - payload <= MAX_NAL, 'empty/oversize NAL')
        nal = elementary[payload:end]
        need(not nal[0] & 128, 'NAL forbidden bit')
        yield start, payload, end, nal


AUD_SLICES = ({2}, {2, 0}, {2, 0, 1}, {4}, {4, 3}, {2, 4}, {2, 4, 0, 3}, {0, 1, 2, 3, 4})


def parse_segment(raw):
    elementary, spans, video_pid = demux(raw)
    sequences, pictures, parameter_bytes = {}, {}, {}
    units, active = [], None
    used_pes = set()

    def finish():
        if active is None:
            return
        need(active['slices'], 'AUD without picture')
        first = active['slices'][0]
        relevant = [span for span in spans if span['start'] <= active['offset'] < span['end']]
        need(len(relevant) == 1 and relevant[0]['pts'] is not None, 'AU without unique PTS-bearing PES')
        origin = relevant[0]
        need(origin['pes'] not in used_pes, 'multiple AUs share one PES timestamp')
        # An AU crossing a second timestamped PES has an ambiguous timestamp
        # unless a separate AU begins there; no one-PES-equals-one-frame inference.
        for span in spans:
            if active['offset'] < span['start'] < active['end']:
                need(span['pts'] is None, 'timestamp inside continued AU')
        used_pes.add(origin['pes'])
        units.append({'es_offset': active['offset'], 'es_end': active['end'],
                      'pes': origin['pes'], 'ts_offset': origin['ts_offset'],
                      'pts90k': origin['pts'], 'dts90k': origin['dts'],
                      'vcl_nal_type': first['kind'], 'idr': first['kind'] == 5,
                      'slice_type': first['slice_type'], 'slices': len(active['slices']),
                      'pps': first['pps'], 'sps': first['sps'],
                      'geometry': list(first['geometry'])})

    for offset, payload, end, nal in annex_nals(elementary):
        kind = nal[0] & 31
        need(kind in (1, 5, 6, 7, 8, 9, 10, 11, 12), 'unsupported NAL syntax')
        if kind == 9:
            if active is not None:
                active['end'] = offset
            finish()
            bits = Bits(rbsp(nal[1:]))
            aud_type = bits.u(3)
            bits.trailing()
            active = {'offset': offset, 'end': end, 'aud_type': aud_type, 'slices': []}
        elif kind in (7, 8):
            need(active is None or not active['slices'], 'parameter set after picture before AUD')
            value = parse_sps(nal[1:]) if kind == 7 else parse_pps(nal[1:], sequences)
            key = (kind, value['id'])
            need(key not in parameter_bytes or parameter_bytes[key] == nal, 'changed parameter set')
            parameter_bytes[key] = nal
            (sequences if kind == 7 else pictures)[value['id']] = value
        elif kind in (1, 5):
            need(active is not None, 'VCL without AUD')
            value = slice_prefix(nal, pictures, sequences)
            need(value['slice_type'] in AUD_SLICES[active['aud_type']], 'AUD slice-type contradiction')
            if active['slices']:
                previous = active['slices'][-1]
                need(value['first_mb'] > previous['first_mb'] and
                     value['signature'] == previous['signature'], 'ambiguous multiple pictures without AUD')
            else:
                need(value['first_mb'] == 0, 'incomplete first slice')
            active['slices'].append(value)
            active['end'] = end
        elif kind == 6:
            need(active is None or not active['slices'], 'SEI after picture before AUD')
        elif kind in (10, 11):
            need(active is not None and active['slices'], 'orphan end NAL')
        if active is not None:
            active['end'] = max(active['end'], end)
    finish()
    need(units and len(units) <= 4096, 'AU count bound')
    need(len({unit['pts90k'] for unit in units}) == len(units), 'duplicate AU PTS')
    need(units[0]['pts90k'] == min(unit['pts90k'] for unit in units), 'leading presentation before first AU')
    need(all(span['pts'] is None or span['pes'] in used_pes for span in spans), 'orphan PES timestamp')
    return {'video_pid': video_pid, 'access_units': units,
            'first_idr': units[0]['idr'], 'idr_count': sum(unit['idr'] for unit in units),
            'scope': 'parsed header identity, not decoded/conformance/device acceptance'}


def census(root_path, receipt_hash, output, uid=501, seconds=60, max_segments=35):
    deadline = time.monotonic() + seconds
    need(os.path.dirname(output) != root_path, 'output in readonly input root')
    aggregate, results = 0, []
    with OwnedRoot(root_path, uid, deadline) as root:
        receipt_raw, receipt_fact = root.read('receipt.json', receipt_hash, MAX_JSON)
        receipt = unique_json(receipt_raw)
        entries = receipt['segments']
        need(1 <= len(entries) <= max_segments <= 35, 'segment count')
        names = [f'seg{index:05d}.ts' for index in range(len(entries))]
        need([row['name'] for row in entries] == names, 'segment order/names')
        actual = sorted(name for name in os.listdir(root.fd) if name.endswith('.ts'))
        need(actual == names, 'extra/missing TS objects')
        playlist = receipt['playlist']
        playlist_raw, playlist_fact = root.read(playlist['name'], playlist['sha256'], MAX_JSON)
        playlist_lines = playlist_raw.decode('utf-8').splitlines()
        need([line for line in playlist_lines if line and not line.startswith('#')] == names,
             'playlist object mismatch')
        for entry in entries:
            raw, media_fact = root.read(entry['name'], entry['sha256'], MAX_SEGMENT)
            aggregate += len(raw)
            need(aggregate <= 512 * 1024 * 1024, 'aggregate media bound')
            probe_raw, probe_fact = root.read(entry['probe_name'], entry['probe_sha256'], MAX_NAL)
            probe = unique_json(probe_raw)
            parsed = parse_segment(raw)
            units = parsed['access_units']
            packets = probe['packets']
            times = [Fraction(row['pts_time']) for row in packets]
            ticks = [Fraction(unit['pts90k'], 90000) for unit in units]
            need(len(times) == len(ticks) and len(set(times)) == len(times), 'old packet/new AU count')
            need(all(abs(old - new) <= Fraction(1, 90000)
                     for old, new in zip(sorted(times), sorted(ticks))), 'old packet/new AU PTS mismatch')
            parsed.update(media=media_fact, raw_probe=probe_fact,
                          old_keyflag_pts=[row['pts_time'] for row in packets if 'K' in row['flags']])
            results.append(parsed)
            root.check()
        document = {'version': 1, 'kind': 'NEW-retained-byte-H264-NAL-syntax-oracle',
                    'receipt': receipt_fact, 'playlist': playlist_fact,
                    'provenance': receipt['provenance'], 'input_bytes': aggregate,
                    'segments': results, 'first_idr_count': sum(row['first_idr'] for row in results),
                    'limits': 'old-runtime synthetic internal bytes; no original/public/current/device/HDR fidelity claim'}
        encoded = json.dumps(document, sort_keys=True, separators=(',', ':')).encode()
        need(len(encoded) <= MAX_JSON, 'aggregate result bound')
        need(time.monotonic() < deadline, 'deadline')
        exclusive_result(output, encoded, uid, deadline)
        return {'bytes': len(encoded), 'sha256': sha(encoded), 'segments': len(results)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True)
    parser.add_argument('--receipt-sha256', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--uid', type=int, default=501)
    parser.add_argument('--deadline-seconds', type=int, default=60)
    args = parser.parse_args()
    need(1 <= args.deadline_seconds <= 60, 'deadline option')
    print(json.dumps(census(args.root, args.receipt_sha256, args.output,
                            args.uid, args.deadline_seconds), sort_keys=True))


if __name__ == '__main__':
    main()
