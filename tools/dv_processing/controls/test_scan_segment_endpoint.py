"""Untagged Matroska completeness controls for `scan_title`; no decode or GPU.

`segment-av.mkv` is the 18-picture `bl.mkv` video (0.75 s) with 1.5 s of PCM
audio, in ten clusters: six carry both tracks, the last four audio alone, as a
WEB-DL title's audio outlasts its video. ffmpeg wrote it, tagging each track
with DURATION. The
controls derive every other case from it in memory:

- untagged, as MakeMKV and older mkvmerge write titles, by turning the Tags
  element into an EBML Void of the same length;
- byte damage the Segment's declared span must refuse: one byte cut from the
  end, one byte appended, the Segment's size made unknown;
- tail damage with every byte present: a Cluster's ID made unknown, which
  matroskadec skips without an error, so its blocks are lost silently. Losing
  the last cluster shortens the audio; losing the last cluster that carries
  video loses the video's tail behind intact audio-only clusters, and the gap
  it leaves in the audio is what refuses it;
- the declared Segment duration voided, one tick late (allowed), or two ticks
  late (refused).

    python3 test_scan_segment_endpoint.py --scanner PATH [--baseline PATH]

`--baseline` is the scanner this one replaces. It must agree on the tagged
title and refuse the untagged one, which shows that only the new endpoint
changed. The Docker helper stage runs this script without it.
"""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "segment-av.mkv"
# What the scanner reports for the fixture's 18 base-layer pictures.
EXPECTED = {
    "schema": 1, "complete": True, "decoded_frames": 18, "valid_l1_frames": 0,
    "missing_l1_frames": 18, "invalid_l1_frames": 0, "max_l1_pq": None,
    "mastering_luminance": {"state": "unknown"},
    "mastering_primaries": {"state": "unknown"},
    "el_kind": {"state": "unknown"},
}
SEGMENT, INFO, TAGS, CLUSTER, DURATION = 0x18538067, 0x1549A966, 0x1254C367, 0x1F43B675, 0x4489
VOID, SIMPLE_BLOCK, VIDEO_TRACK = 0xEC, 0xA3, 1


def number(data, at, is_id=False):
    first, length = data[at], 1
    while not first & (0x80 >> (length - 1)):
        length += 1
    value = first if is_id else first & (0xFF >> length)
    for byte in data[at + 1:at + length]:
        value = (value << 8) | byte
    return value, at + length


def children(data, start, end):
    """(id, element start, payload start, payload end) of each child."""
    out = []
    while start < end:
        element = start
        ident, start = number(data, start, True)
        size, start = number(data, start)
        out.append((ident, element, start, start + size))
        start += size
    return out


def segment(data):
    _, at = number(data, 0, True)
    size, at = number(data, at)
    ident, size_at = number(data, at + size, True)
    assert ident == SEGMENT
    size, payload = number(data, size_at)
    return size_at, payload, payload + size


def void(data, element, end):
    """The bytes with [element, end) replaced by a Void of the same length."""
    total = end - element
    assert total >= 9
    return data[:element] + bytes([VOID, 0x01]) + (total - 9).to_bytes(7, "big") \
        + bytes(total - 9) + data[end:]


def level1(data, ident):
    _, payload, end = segment(data)
    return [child for child in children(data, payload, end) if child[0] == ident]


def untagged(data):
    (_, element, _, end), = level1(data, TAGS)
    return void(data, element, end)


def tracks(data, cluster):
    """The track numbers of a cluster's SimpleBlocks."""
    _, _, payload, end = cluster
    return {number(data, start)[0] for ident, _, start, _ in children(data, payload, end)
            if ident == SIMPLE_BLOCK}


def lose(data, cluster):
    """The bytes with this cluster's ID made one matroskadec skips silently."""
    element = cluster[1]
    return data[:element] + b"\x1f\x43\xb6\x76" + data[element + 4:]


def info_duration(data):
    (_, _, payload, end), = level1(data, INFO)
    (found,) = [child for child in children(data, payload, end) if child[0] == DURATION]
    assert found[3] - found[2] == 8, "double-precision Segment duration"
    return found


def with_duration(data, ticks):
    _, _, payload, end = info_duration(data)
    return data[:payload] + struct.pack(">d", ticks) + data[end:]


def scan(scanner, data, name, workdir):
    path = Path(workdir) / f"{name}.mkv"
    path.write_bytes(data)
    environment = dict(os.environ)
    process = subprocess.run([str(scanner), str(path), "0"], capture_output=True,
                             timeout=60, env=environment)
    return process.returncode, process.stdout, process.stderr


def expect(result, name, reason=None):
    code, stdout, stderr = result
    if reason is None:
        assert code == 0, (name, code, stderr)
        assert json.loads(stdout) == EXPECTED, (name, stdout)
    else:
        assert code == 1 and not stdout.strip(), (name, code, stdout)
        # libavformat may log before the refusal; the daemon reads the last line.
        assert stderr.endswith(f"DV title scan refused: {reason}\n".encode()), (name, stderr)
        assert stderr.count(b"DV title scan refused:") == 1, (name, stderr)


def test_untagged_matroska_segment_endpoint(scanner, baseline=None):
    tagged = FIXTURE.read_bytes()
    clusters = level1(tagged, CLUSTER)
    assert len(clusters) == 10 and level1(tagged, TAGS)
    plain = untagged(tagged)
    assert b"DURATION" not in plain
    clusters = level1(plain, CLUSTER)
    carrying_video = [c for c in clusters if VIDEO_TRACK in tracks(plain, c)]
    assert len(carrying_video) == 6 and VIDEO_TRACK not in tracks(plain, clusters[-1])
    declared = struct.unpack(">d", plain[info_duration(plain)[2]:info_duration(plain)[3]])[0]
    assert declared == 1500.0, declared
    size_at, _, _ = segment(plain)
    assert plain[size_at] == 0x01, "eight-byte Segment size"
    _, _, duration_payload, duration_end = info_duration(plain)
    cases = [
        ("tagged", tagged, None),
        ("untagged", plain, None),
        ("untagged-cut-one-byte", plain[:-1], "complete declared Matroska segment"),
        ("untagged-extra-byte", plain + b"\0", "complete declared Matroska segment"),
        ("untagged-unknown-size", plain[:size_at] + b"\x01" + b"\xff" * 7 + plain[size_at + 8:],
         "complete declared Matroska segment"),
        ("untagged-lost-last-cluster", lose(plain, clusters[-1]), "Matroska segment tail reached"),
        ("untagged-lost-video-tail", lose(plain, carrying_video[-1]),
         "Matroska segment tail reached"),
        ("untagged-no-duration", void(plain, duration_payload - 3, duration_end),
         "declared Matroska segment duration"),
        ("untagged-one-tick-late", with_duration(plain, 1501.0), None),
        ("untagged-two-ticks-late", with_duration(plain, 1502.0), "Matroska segment tail reached"),
    ]
    with tempfile.TemporaryDirectory() as workdir:
        for name, data, reason in cases:
            expect(scan(scanner, data, name, workdir), name, reason)
        if baseline:
            expect(scan(baseline, tagged, "baseline-tagged", workdir), "baseline-tagged")
            expect(scan(baseline, plain, "baseline-untagged", workdir), "baseline-untagged",
                   "independent complete selected-video endpoint unavailable")
    return len(cases) + (2 if baseline else 0)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--scanner", required=True, type=Path)
    parser.add_argument("--baseline", type=Path)
    args = parser.parse_args()
    count = test_untagged_matroska_segment_endpoint(args.scanner, args.baseline)
    print(f"segment endpoint controls: {count} cases pass", file=sys.stderr)
