"""Extract the first Michael5400 served F+ window from local HTTP captures.

Supply the exact init and its byte concatenation with the served fragments,
without remuxing. --source-pts-offset is a caller-proven rational in seconds:
source PTS seconds = served PTS ticks * served time base + offset. This tool
does not discover an offset or authenticate HTTP capture provenance. It emits
served_refit_eval.py readback and receipt documents, plus the selected raw
UNSPEC62 NALs. Input and scratch paths belong to the caller's task.
"""
import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

MAX_INPUT = 512 * 1024 * 1024
MAX_PACKETS = 4096
MAX_INIT = 4 * 1024 * 1024
TIME_BASE = "1/1000"
WINDOW = (Fraction(125, 1000), Fraction(2127, 1000))
FRAME_COUNT = 48
PIVOTS = [0, 128, 128, 128, 128, 128, 128, 128, 127]
START_CODE = re.compile(rb"\x00{2,}\x01")
VIDEO_CONFIG = {
    "box": "dvvC", "dv_profile": 8, "dv_bl_signal_compatibility_id": 1,
    "rpu_present_flag": 1, "bl_present_flag": 1, "el_present_flag": 0,
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha_bytes(data):
    return hashlib.sha256(data).hexdigest()


def input_receipt(path, cap):
    require(path.is_absolute() and path.is_file(), f"expected an absolute input file: {path}")
    size = path.stat().st_size
    require(0 < size <= cap, f"input outside size bounds: {path}")
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path.resolve()), "bytes": size, "sha256": digest}


def run(*command):
    return subprocess.run(command, check=True, capture_output=True, timeout=120).stdout


def boxes(data, begin, end):
    while begin < end:
        require(end - begin >= 8, "truncated ISO BMFF box")
        size = int.from_bytes(data[begin:begin + 4], "big")
        kind = data[begin + 4:begin + 8]
        header = 8
        if size == 1:
            require(end - begin >= 16, "truncated extended ISO BMFF box")
            size = int.from_bytes(data[begin + 8:begin + 16], "big")
            header = 16
        if size == 0:
            size = end - begin
        require(header <= size <= end - begin, "invalid ISO BMFF box size")
        yield kind, begin + header, begin + size
        begin += size


def init_config(data):
    def video_config(begin, end, level=0):
        if level == 5:
            return
        for kind, start, stop in boxes(data, begin, end):
            if kind in (b"moov", b"trak", b"mdia", b"minf", b"stbl"):
                yield from video_config(start, stop, level + 1)
            elif kind == b"stsd":
                require(stop - start >= 8, "truncated sample description")
                entries = list(boxes(data, start + 8, stop))
                require(len(entries) == int.from_bytes(data[start + 4:start + 8], "big"),
                        "sample description count mismatch")
                for entry, first, last in entries:
                    if entry in (b"hvc1", b"hev1", b"dvh1", b"dvhe"):
                        require(last - first >= 78, "truncated HEVC sample entry")
                        for child, low, high in boxes(data, first + 78, last):
                            if child in (b"dvcC", b"dvvC"):
                                yield child, data[low:high]

    # moov → trak → mdia → minf → stbl → stsd
    matches = list(video_config(0, len(data), -1))
    require(len(matches) == 1 and matches[0][0] == b"dvvC", "init must carry exactly one HEVC dvvC record")
    payload = matches[0][1]
    require(len(payload) >= 24, "truncated dvvC record")
    packed = int.from_bytes(payload[2:4], "big")
    config = {"box": "dvvC", "dv_profile": (packed >> 9) & 127,
              "rpu_present_flag": (packed >> 2) & 1, "el_present_flag": (packed >> 1) & 1,
              "bl_present_flag": packed & 1, "dv_bl_signal_compatibility_id": payload[4] >> 4}
    require(config == VIDEO_CONFIG, "init does not signal BL+RPU Profile 8.1 without EL")
    return config


def packet_metadata(ffprobe, path):
    document = json.loads(run(
        ffprobe, "-v", "error", "-select_streams", "v:0", "-show_streams", "-show_packets",
        "-show_data_hash", "sha256", "-show_entries",
        "stream=codec_name,time_base:stream_side_data:packet=pts,dts,duration,size,data_hash",
        "-read_intervals", f"%+#{MAX_PACKETS + 1}", "-of", "json", path,
    ))
    streams = document["streams"]
    require(len(streams) == 1 and streams[0]["codec_name"] == "hevc", f"expected one selected HEVC stream: {path}")
    packets = document["packets"]
    require(0 < len(packets) <= MAX_PACKETS, f"packet count exceeds bounds: {path}")
    time_base = Fraction(streams[0]["time_base"])
    require(time_base > 0, f"invalid video time base: {path}")
    pts = [row["pts"] for row in packets]
    require(all(type(value) is int for value in pts) and len(set(pts)) == len(pts), f"invalid or duplicate packet PTS: {path}")
    return streams[0], time_base, packets


def annexb_nals(data):
    markers = list(START_CODE.finditer(data))
    require(markers and not data[:markers[0].start()].strip(b"\0"), "malformed Annex B access unit")
    for index, marker in enumerate(markers):
        end = markers[index + 1].start() if index + 1 < len(markers) else len(data)
        # Terminal zero bytes are Annex B trailing_zero_8bits, not part of
        # the canonical NAL payload used by this proof.
        nal = data[marker.end():end].rstrip(b"\0")
        require(len(nal) >= 2 and not nal[0] & 128 and nal[1] & 7, "invalid HEVC NAL header")
        yield (nal[0] >> 1) & 63, ((nal[0] & 1) << 5) | (nal[1] >> 3), nal


def access_unit(data):
    rpus = []
    vcl_hash = hashlib.sha256()
    vcl_count, first_slices, wrapped_el = 0, 0, 0
    for kind, layer, nal in annexb_nals(data):
        if kind < 32:
            require(layer == 0 and len(nal) >= 3, "invalid base-layer slice")
            first_slices += bool(nal[2] & 128)
            vcl_count += 1
            vcl_hash.update(bytes([kind]))
            vcl_hash.update(len(nal).to_bytes(8, "big"))
            vcl_hash.update(nal)
        if kind == 62:
            require(layer == 0, "non-base-layer RPU")
            rpus.append(nal)
        if kind == 63:
            wrapped_el += 1
    require(first_slices == 1 and vcl_count > 0 and len(rpus) == 1,
            "expected one BL picture and one UNSPEC62 RPU per access unit")
    return vcl_hash.hexdigest(), vcl_count, wrapped_el, rpus[0]


def dump_units(ffmpeg, media, scratch, count):
    scratch.mkdir()
    run(ffmpeg, "-nostdin", "-v", "error", "-i", media, "-map", "0:v:0", "-c", "copy",
        "-bsf:v", "hevc_mp4toannexb", "-frames:v", str(MAX_PACKETS + 1),
        "-f", "image2", "-start_number", "0", scratch / "au-%05d.hevc")
    packets = sorted(scratch.iterdir())
    expected = [f"au-{index:05d}.hevc" for index in range(count)]
    require([path.name for path in packets] == expected, "one Annex B access unit per probed packet required")
    require(sum(path.stat().st_size for path in packets) <= MAX_INPUT + 32 * 1024 * 1024,
            "Annex B output exceeds expected framing overhead")
    return packets


def capture_frames(events_path, source):
    documents = [json.loads(line) for line in events_path.read_text().splitlines()]
    source_rows = [doc for doc in documents if doc.get("kind") == "source"]
    require(len(source_rows) == 1 and source_rows[0].get("source_sha256") == source["sha256"]
            and source_rows[0].get("time_base") == TIME_BASE, "capture source identity mismatch")
    rows = [doc for doc in documents if doc.get("kind") == "accepted_source_pair"]
    require([row["frame"] for row in rows] == list(range(FRAME_COUNT)), "capture must identify 48 frames")
    clocks = [Fraction(row["pts"]) for row in rows]
    require(clocks == sorted(set(clocks)) and clocks[0] == WINDOW[0] and clocks[-1] < WINDOW[1]
            and clocks[-1] + Fraction(rows[-1]["duration"]) == WINDOW[1], "capture clocks differ from requested window")
    return dict(zip(clocks, rows, strict=True))


def select_packets(packets, time_base, offset, captured):
    by_clock = {}
    for index, row in enumerate(packets):
        clock = row["pts"] * time_base + offset
        if WINDOW[0] <= clock < WINDOW[1]:
            require(clock in captured and clock not in by_clock, f"unmatched or duplicate packet PTS: {clock}")
            by_clock[clock] = (index, row)
    require(set(by_clock) == set(captured), "packet clock transform does not cover all captured source frames")
    return by_clock


def parse_writer_row(document, source_pts, nal):
    header = document["header"]
    require(document["dovi_profile"] == 8 and header["coefficient_data_type"] == 0 and
            header["coefficient_log2_denom"] == 23 and header["bl_bit_depth_minus8"] == 2 and
            header["vdr_bit_depth_minus8"] == 4 and header["disable_residual_flag"] is True,
            "served RPU does not have the expected profile and coefficient encoding")
    curves = document["rpu_data_mapping"]["curves"]
    require(len(curves) == 3, "served RPU requires three mapping components")
    luma, *chroma = curves
    require(luma["mapping_idc"] == "Polynomial" and luma["pivots"] == PIVOTS
            and luma["poly_order_minus1"] == [1] * 8 and not any(luma["linear_interp_flag"]),
            "served luma is not the expected piecewise quadratic")

    def join(integer, fraction, grid):
        require(type(integer) is int and type(fraction) is int and 0 <= fraction < 2 ** 23,
                "invalid wire coefficient")
        encoded = integer * 2 ** 23 + fraction
        require(-64 * 2 ** 23 <= encoded < 63 * 2 ** 23 and encoded % (2 ** (23 - grid)) == 0,
                "served coefficient is outside the published grid")
        return encoded / 2 ** 23

    def coefficients(integers, fractions, count, grid):
        require(len(integers) == len(fractions) == count, "invalid coefficient count")
        return [join(i, f, grid) for i, f in zip(integers, fractions, strict=True)]

    require(len(luma["poly_coef_int"]) == len(luma["poly_coef"]) == 8, "invalid number of luma pieces")
    luma_coef = [coefficients(i, f, 3, 23)
                 for i, f in zip(luma["poly_coef_int"], luma["poly_coef"], strict=True)]
    mmr = []
    for item in chroma:
        require(item["mapping_idc"] == "MMR" and item["pivots"] == [0, 1023]
                and item["mmr_order_minus1"] == [2], "served chroma is not one third-order MMR piece")
        ci, co, i, f = (item[key] for key in ("mmr_constant_int", "mmr_constant", "mmr_coef_int", "mmr_coef"))
        require(len(ci) == len(co) == len(i) == len(f) == 1 and len(i[0]) == len(f[0]) == 3,
                "invalid MMR pieces or orders")
        mmr.append({"constant": join(ci[0], co[0], 12),
                    "coef": [coefficients(a, b, 7, 12) for a, b in zip(i[0], f[0], strict=True)]})
    return {"source_pts": source_pts, "rpu_sha256": sha_bytes(nal), "bytes": len(nal),
            "profile": 8, "bl_bit_depth": 10, "vdr_bit_depth": 12, "coefficient_log2_denom": 23,
            "disable_residual_flag": True, "luma_pivot_deltas": PIVOTS, "luma_coef": luma_coef, "chroma": mmr}


def write_json(path, document):
    with path.open("x") as stream:
        json.dump(document, stream, indent=2, allow_nan=False)
        stream.write("\n")


def extract(args):
    refs = {name: input_receipt(getattr(args, name), MAX_INIT if name in ("init", "events") else MAX_INPUT)
            for name in ("source", "served", "init", "events")}
    tools = {}
    for name, flags in (("ffmpeg", "-version"), ("ffprobe", "-version"), ("dovi_tool", "--version")):
        executable = getattr(args, name)
        tools[name] = input_receipt(executable, MAX_INPUT)
        tools[name]["version"] = run(executable, flags).decode().splitlines()[0]
    init_bytes = args.init.read_bytes()
    with args.served.open("rb") as stream:
        require(stream.read(len(init_bytes)) == init_bytes, "served media must begin with the exact captured init")
    config = init_config(init_bytes)
    captured = capture_frames(args.events, refs["source"])
    _, source_tb, source_packets = packet_metadata(args.ffprobe, args.source)
    served_stream, served_tb, served_packets = packet_metadata(args.ffprobe, args.served)
    require(source_tb == Fraction(args.source_time_base), "probed source video time base differs from capture")
    dv = [side for side in served_stream.get("side_data_list", [])
          if side.get("side_data_type") == "DOVI configuration record"]
    require(len(dv) == 1 and all(dv[0].get(key) == value for key, value in config.items() if key != "box"),
            "probe Dolby signaling differs from the captured init")
    source = select_packets(source_packets, source_tb, Fraction(0), captured)
    served = select_packets(served_packets, served_tb, args.source_pts_offset, captured)

    selected, proofs = [], []
    with tempfile.TemporaryDirectory(prefix="served-refit-au-", dir=args.scratch_root) as scratch:
        scratch = Path(scratch)
        source_units = dump_units(args.ffmpeg, args.source, scratch / "source", len(source_packets))
        served_units = dump_units(args.ffmpeg, args.served, scratch / "served", len(served_packets))
        for clock, capture in captured.items():
            source_index, source_packet = source[clock]
            served_index, served_packet = served[clock]
            source_data, served_data = source_units[source_index].read_bytes(), served_units[served_index].read_bytes()
            source_hash, source_count, source_el, source_rpu = access_unit(source_data)
            served_hash, served_count, served_el, served_rpu = access_unit(served_data)
            require(source_hash == served_hash and source_count == served_count and source_el > 0 and served_el == 0,
                    f"copied base-layer VCL or layer custody mismatch at {clock}")
            require(sha_bytes(source_rpu) == capture["rpu_sha256"], f"source RPU hash differs from capture at {clock}")
            require(source_packet.get("data_hash", "").lower() == "sha256:" + capture["packet_sha256"],
                    f"original compressed source packet hash differs from capture at {clock}")
            ticks = int(clock / source_tb)
            selected.append((ticks, served_rpu))
            proofs.append({"source_pts": ticks, "served_pts": served_packet["pts"], "frame": capture["frame"],
                           "source_decode_index": source_index, "served_decode_index": served_index,
                           "source_rpu_sha256": sha_bytes(source_rpu), "served_rpu_sha256": sha_bytes(served_rpu),
                           "source_au_sha256": sha_bytes(source_data), "served_au_sha256": sha_bytes(served_data),
                           "source_packet": source_packet, "served_packet": served_packet,
                           "bl_vcl_sha256": served_hash, "bl_vcl_nals": served_count})

        # Export each canonical NAL independently. The extracted JSON row
        # remains bound to its bytes even if the CLI ever reorders a batch.
        readback = []
        for ticks, nal in selected:
            binary = scratch / "selected.rpu.bin"
            export = scratch / "selected.rpu.json"
            # dovi_tool's RPU file format uses a start code in place
            # of the two-byte HEVC UNSPEC62 header.
            require(nal[:2] == b"\x7c\x01", f"unexpected UNSPEC62 header at {ticks}")
            binary.write_bytes(b"\0\0\0\1" + nal[2:])
            run(args.dovi_tool, "export", "-i", binary, "-d", f"all={export}")
            rows = json.loads(export.read_text())
            require(len(rows) == 1, f"unexpected RPU export count at {ticks}")
            readback.append(parse_writer_row(rows[0], ticks, nal))
            export.unlink()

    args.output.mkdir()
    for row, (ticks, nal) in zip(readback, selected, strict=True):
        name = f"source-pts-{ticks}.nal"
        (args.output / name).write_bytes(nal)
        row["out"] = name
    readback_path = args.output / "readback.json"
    write_json(readback_path, {"frames": readback})
    receipt = {"schema": 1, "kind": "served_refit_extraction", "artifacts": refs, "tools": tools,
               "extractor_sha256": sha_bytes(Path(__file__).read_bytes()),
               "source_sha256": refs["source"]["sha256"], "served_sha256": refs["served"]["sha256"],
               "init_sha256": refs["init"]["sha256"],
               "readback_sha256": sha_bytes(readback_path.read_bytes()), "video_config": config,
               "source_time_base": args.source_time_base, "served_time_base": str(served_tb),
               "source_pts_offset_seconds": str(args.source_pts_offset), "frames": proofs,
               "scope": "local caller-supplied captures; HTTP receipt authority is external",
               "vcl_hash": "ordered layer-zero VCL NAL type, length u64 big-endian, canonical Annex B payload"}
    write_json(args.output / "served_receipt.json", receipt)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "served", "init", "events", "ffmpeg", "ffprobe", "dovi-tool", "scratch-root", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--source-time-base", required=True, choices=[TIME_BASE])
    parser.add_argument("--source-pts-offset", required=True, type=Fraction, metavar="N/D")
    args = parser.parse_args()
    try:
        require(args.scratch_root.is_absolute() and args.scratch_root.is_dir(),
                "scratch root must be an existing absolute directory")
        require(args.output.is_absolute() and not args.output.exists(), "output must be a new absolute directory")
        extract(args)
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or b"").decode(errors="replace")[-2000:]
        parser.exit(1, f"served F+ extraction: {error}\n{detail}\n")
    except (OSError, ValueError, TypeError, IndexError, KeyError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"served F+ extraction: {error}\n")


if __name__ == "__main__":
    main()
