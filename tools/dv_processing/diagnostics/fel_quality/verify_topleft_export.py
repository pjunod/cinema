"""Verify a new Model B CPU TopLeft input for a Michael calibration window.

Run: python verify_topleft_export.py REQUEST.json NEW_RECEIPT.json --scratch-root OWNED_DIRECTORY

Request schema 1 has ordinals [FIRST, LAST] (inclusive) and window [START, END]
(half-open source times on the 1/1000 clock) and an absolute video_index.
Its artifacts object contains source, bl, el, rpus, events, metadata,
master_binding, export, export_receipt, helper, bundle_manifest, ffprobe,
dovi_tool, source_pts_census, and source_pts_receipt. The latter is the
successful window acquisition receipt that binds the retained census to
this source and ordinal range. Each value is {path, sha256} with an absolute
path. Capture, source and master receipts use served_refit_eval.py's formats.
export is raw yuv420p10le and export_receipt is complete helper stdout.

All frames are checked for exact clocks, source-RPU binding and output byte
hashes. Every output code of relative frames 0 and 24 is independently
derived from the canonical BL/EL capture and compared. This verifies the
new representation and its sidecar metadata. It performs no encoding or
quality scoring and establishes no identity with the historical export.
"""
import argparse
from fractions import Fraction
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

for key in ("OPENBLAS_NUM_THREADS", "VECLIB_MAXIMUM_THREADS", "OMP_NUM_THREADS",
            "MKL_NUM_THREADS", "BLIS_NUM_THREADS"):
    os.environ[key] = "1"
sys.dont_write_bytecode = True

import numpy as np

import four_model_audit as fm
from served_refit_eval import (
    FRAME_COUNT, FROZEN, HEIGHT, TIME_BASE, WIDTH, fixture_color, is_sha,
    parse_window, read_jsonl, require, require_fields, sha, validate_capture, validate_master,
)

HERE = Path(__file__).resolve().parent
SCHEMA_ARTIFACTS = {
    "source", "bl", "el", "rpus", "events", "metadata", "master_binding",
    "export", "export_receipt", "helper", "bundle_manifest", "ffprobe", "dovi_tool",
    "source_pts_census", "source_pts_receipt",
}
FORMAT = {
    "kind": "composed_topleft_export", "format": "plurx-composed-model-b-topleft-v1",
    "time_base": TIME_BASE, "width": WIDTH, "height": HEIGHT, "pix_fmt": "yuv420p10le",
    "color_primaries": "bt2020", "color_trc": "smpte2084", "colorspace": "bt2020nc",
    "color_range": "tv", "chroma_location": "topleft",
}
FRAME_BYTES = WIDTH * HEIGHT * 3
SELECTED = (0, 24)
LMS_MATRIX = [7222, 8771, 390, 2654, 12430, 1300, 0, 422, 15962]
SOURCE_TAGS = {"color_range": "tv", "color_space": "bt2020nc", "color_transfer": "smpte2084",
               "color_primaries": "bt2020", "chroma_location": "topleft"}


def resolve_artifacts(artifacts):
    require(set(artifacts) == SCHEMA_ARTIFACTS, "request artifacts differ from schema 1")
    paths = {}
    for key, item in artifacts.items():
        path = Path(item["path"])
        require(path.is_absolute() and is_sha(item["sha256"]), f"invalid artifact: {key}")
        paths[key] = path.resolve(strict=True)
        require(sha(paths[key]) == item["sha256"], f"artifact SHA-256 mismatch: {key}")
    require(len(set(paths.values())) == len(paths), "artifact paths must be distinct")
    return paths


def validate_helper(paths, artifacts):
    manifest_path = paths["bundle_manifest"]
    manifest = json.loads(manifest_path.read_text())
    renderer = manifest["tools"]["renderer"]
    require(manifest["schema"] == 1 and is_sha(manifest["source_sha256"]),
            "invalid helper bundle identity")
    require((manifest_path.parent / renderer["path"]).resolve(strict=True) == paths["helper"] and
            renderer["sha256"] == artifacts["helper"]["sha256"], "bundle does not bind the exporter")


def verify_source_ordinals(paths, artifacts, first, last, ticks):
    receipt = json.loads(paths["source_pts_receipt"].read_text())
    require_fields(receipt, {"verdict": "pass", "expected_source_ordinals": [first, last]},
                   "source acquisition")
    require(receipt["source"]["sha256"] == artifacts["source"]["sha256"] and
            receipt["source_pt_census"]["sha256"] == artifacts["source_pts_census"]["sha256"],
            "source acquisition does not bind this packet census")
    source = json.loads(paths["source_pts_census"].read_text())
    streams = source["streams"]
    require(len(streams) == 1 and streams[0]["time_base"] == TIME_BASE, "source census clock differs")
    pts = [packet["pts"] for packet in source["packets"]]
    require(len(pts) > last and all(type(value) is int for value in pts) and len(set(pts)) == len(pts),
            "invalid or duplicate source census PTS")
    require(sorted(pts)[first:last + 1] == ticks, "captured PTS do not match the source presentation ordinals")


def verify_source_color(paths, pairs, metadata, video_index, scratch_root):
    probe = subprocess.run(
        [paths["ffprobe"], "-v", "error", "-select_streams", str(video_index),
         "-show_entries", "stream=index,color_range,color_space,color_transfer,color_primaries,chroma_location",
         "-of", "json", paths["source"]], capture_output=True, check=True, timeout=45,
    )
    streams = json.loads(probe.stdout)["streams"]
    require(len(streams) == 1, "expected one selected source stream")
    require_fields(streams[0], {**SOURCE_TAGS, "index": video_index}, "source stream color tags")
    for row in metadata:
        require(row["color"]["rgb_to_lms_matrix"] == [[value, 16384] for value in LMS_MATRIX],
                "unsupported captured RGB-to-LMS matrix")

    # Decode the exact captured raw NALs independently of the exporter. Their
    # offsets and hashes were already checked against the accepted source pairs.
    rpus = paths["rpus"].read_bytes()
    with tempfile.TemporaryDirectory(dir=scratch_root, prefix="topleft-rpu-") as temporary:
        work = Path(temporary)
        packed = work / "source.rpu.bin"
        decoded = work / "source.rpu.json"
        with packed.open("xb") as stream:
            for row in pairs:
                offset = row["rpu_offset"]
                nal = rpus[offset:offset + row["rpu_size"]]
                require(nal[:2] == b"\x7c\x01", "unsupported raw source RPU NAL header")
                stream.write(b"\0\0\0\1" + nal[2:])
        subprocess.run([paths["dovi_tool"], "export", "-i", packed, "-d", f"all={decoded}"],
                       capture_output=True, check=True, timeout=45)
        rows = json.loads(decoded.read_text())
    require(len(rows) == FRAME_COUNT, "independent raw RPU parser did not cover the requested window")
    fields = {"signal_eotf": 65535, "signal_eotf_param0": 0, "signal_eotf_param1": 0,
              "signal_eotf_param2": 0, "signal_bit_depth": 12, "signal_color_space": 0,
              "signal_chroma_format": 0, "signal_full_range_flag": 1}
    fields.update({f"rgb_to_lms_coef{i}": value for i, value in enumerate(LMS_MATRIX)})
    fields.update({f"ycc_to_rgb_coef{i}": int(value) for i, value in enumerate(fm.VECTOR_MATRIX.ravel())})
    fields.update({f"ycc_to_rgb_offset{i}": int(value) for i, value in enumerate(fm.VECTOR_OFFSETS)})
    for row in rows:
        require_fields(row["vdr_dm_data"], fields, "source RPU color and transfer")
    return {"stream": streams[0], "rpu": fields, "rpus_verified": len(rows)}


def reference_pixels(bl, el, frame, matrix, offsets):
    """Stream rows of the independent Python representation."""
    base = fm.planes(bl, WIDTH, HEIGHT, frame)
    enhancement = fm.planes(el, WIDTH // 2, HEIGHT // 2, frame)
    for lo in range(0, HEIGHT, 64):
        hi = min(lo + 64, HEIGHT)
        codes, residual = fm.sample_codes(base, enhancement, base, WIDTH, HEIGHT, lo, hi)["R"]
        composed = fm.composition12(codes, residual)
        rgb = fm.kernel_b_rgb(composed, matrix, offsets).astype(np.int64)
        y = rgb @ np.array([2627, 6780, 593], dtype=np.int64)
        luma_den = 10000 * 65535
        luma = 64 + (876 * y + luma_den // 2) // luma_den
        selected_y = y[::2, ::2]

        def chroma(channel, divisor):
            den = divisor * 65535
            signed = 896 * (10000 * rgb[::2, ::2, channel] - selected_y)
            return (512 * den + signed + den // 2) // den

        yield lo, hi, luma, chroma(2, 18814), chroma(0, 14746)


def verify_pixels(raw, paths, frame, matrix, offsets):
    raster = np.frombuffer(raw, dtype="<u2").reshape(1, -1)
    y, cb, cr = fm.planes(raster, WIDTH, HEIGHT, 0)
    bl = np.memmap(paths["bl"], dtype="<u2", mode="r").reshape(FRAME_COUNT, -1)
    el = np.memmap(paths["el"], dtype="<u2", mode="r").reshape(FRAME_COUNT, -1)
    for lo, hi, luma, chroma_b, chroma_r in reference_pixels(bl, el, frame, matrix, offsets):
        require(np.array_equal(y[lo:hi], luma), f"luma differs at frame {frame}, row {lo}")
        require(np.array_equal(cb[lo // 2:hi // 2], chroma_b), f"Cb differs at frame {frame}, row {lo // 2}")
        require(np.array_equal(cr[lo // 2:hi // 2], chroma_r), f"Cr differs at frame {frame}, row {lo // 2}")


def verify(request_path, scratch_root):
    request = json.loads(request_path.read_text())
    require_fields(request, {"schema": 1}, "request")
    first, last = request["ordinals"]
    require(type(first) is type(last) is int and 0 <= first <= last and last - first + 1 == FRAME_COUNT,
            "invalid 48-frame absolute ordinal range")
    video_index = request["video_index"]
    require(type(video_index) is int and 0 <= video_index <= 255, "invalid absolute video stream index")
    window = parse_window(request["window"])
    artifacts = request["artifacts"]
    paths = resolve_artifacts(artifacts)
    validate_helper(paths, artifacts)
    operators = {name: sha(HERE / name) for name in FROZEN}
    require(operators == FROZEN and Path(fm.__file__).resolve() == HERE / "four_model_audit.py" and
            Path(fm.vm.__file__).resolve() == HERE / "vector_metrics.py", "frozen reference operators changed")
    events, metadata = read_jsonl(paths["events"]), read_jsonl(paths["metadata"])
    ticks = validate_capture(events, metadata, paths, artifacts["source"]["sha256"], window)
    verify_source_ordinals(paths, artifacts, first, last, ticks)
    validate_master(json.loads(paths["master_binding"].read_text()), paths, artifacts["events"]["sha256"])
    matrix, offsets, _ = fixture_color(metadata)
    require(np.array_equal(matrix, fm.VECTOR_MATRIX) and np.array_equal(offsets, fm.VECTOR_OFFSETS),
            "metadata is outside the calibrated TopLeft v1 color domain")
    pairs = [row for row in events if row.get("kind") == "accepted_source_pair"]
    source_color = verify_source_color(paths, pairs, metadata, video_index, scratch_root)
    receipt = read_jsonl(paths["export_receipt"])
    require(len(receipt) == FRAME_COUNT + 2, "export requires a complete receipt")
    require_fields(receipt[0], {**FORMAT, "first": first, "last": last, "video_index": video_index}, "export header")
    require_fields(receipt[-1], {"kind": "export_complete", "format": FORMAT["format"], "frames": FRAME_COUNT}, "export trailer")
    require(paths["export"].stat().st_size == FRAME_COUNT * FRAME_BYTES, "incorrect raw export size")

    with paths["export"].open("rb") as stream:
        for relative, (record, source, tick) in enumerate(zip(receipt[1:-1], pairs, ticks, strict=True)):
            duration = Fraction(source["duration"]) / Fraction(TIME_BASE)
            require(duration.denominator == 1 and duration > 0, f"invalid source duration at {relative}")
            require_fields(record, {"kind": "export_frame", "ordinal": first + relative, "pts_ticks": tick,
                                    "duration_ticks": int(duration), "bytes": FRAME_BYTES,
                                    "source_rpu_sha256": source["rpu_sha256"]}, "export frame")
            raw = stream.read(FRAME_BYTES)
            require(len(raw) == FRAME_BYTES and hashlib.sha256(raw).hexdigest() == record["frame_sha256"],
                    f"export frame SHA-256 mismatch at {relative}")
            if relative in SELECTED:
                verify_pixels(raw, paths, relative, matrix, offsets)
        require(not stream.read(1), "export has trailing bytes")

    return {"schema": 1, "kind": "composed_topleft_verification", "exact": True,
            "format": FORMAT, "source_color": source_color, "window": request["window"], "ordinals": [first, last],
            "request_sha256": sha(request_path), "artifacts": artifacts, "verified_frames": FRAME_COUNT,
            "full_pixel_check_source_ordinals": [first + frame for frame in SELECTED],
            "pixels_compared": len(SELECTED) * FRAME_BYTES // 2,
            "operators": {**operators, Path(__file__).name: sha(Path(__file__)),
                          "served_refit_eval.py": sha(HERE / "served_refit_eval.py")},
            "scope": "new raw CPU representation and sidecar; no NUT, encode, score, or historical identity proof"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("request", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--scratch-root", type=Path, required=True)
    args = parser.parse_args()
    try:
        require(not args.output.exists(), "output already exists")
        require(args.scratch_root.is_absolute() and args.scratch_root.is_dir(),
                "scratch root must be an existing absolute directory")
        result = verify(args.request, args.scratch_root)
        with args.output.open("x") as stream:
            json.dump(result, stream, indent=2, allow_nan=False)
            stream.write("\n")
    except (OSError, ValueError, TypeError, KeyError, IndexError, subprocess.SubprocessError) as error:
        parser.exit(1, f"TopLeft export verification: {error}\n")


if __name__ == "__main__":
    main()
