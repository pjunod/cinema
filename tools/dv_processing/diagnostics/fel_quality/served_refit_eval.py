"""Measure the Michael5400 served F+ baseline on the frozen four-model lattice.

Run: python served_refit_eval.py REQUEST.json NEW_RESULT.json

Request schema 1 has window ["125/1000", "2127/1000"] and an artifacts
object keyed by source, bl, el, rpus, events, metadata, master_binding,
readback, init, and served_receipt. Each artifact is {path, sha256} with an
absolute pathname. The capture artifacts are capture_layers/metadata output.
master_binding is the successful compare_composed.py receipt for those planes.
readback contains frames in rpu_refit_write's schema with source_pts (integer
source ticks) and rpu_sha256 (the served UNSPEC62 NAL hash) added to each row.
By default every readback row belongs to the requested window; an explicit
allow_extra_readback_frames:true allows unrelated clocks outside the window.

The caller's packet-extraction receipt must provide source_sha256,
init_sha256, readback_sha256, source_time_base ("1/1000"), served_time_base,
source_pts_offset_seconds, artifacts and frames as emitted by
extract_served_refit.py, and video_config
{box:"dvvC", dv_profile:8, dv_bl_signal_compatibility_id:1,
rpu_present_flag:1, bl_present_flag:1, el_present_flag:0}. The extractor
owns the HTTP, AU, copied BL and time mapping proofs; this scorer verifies
the receipt's input bindings, not their independent authenticity.
"""
import argparse
from fractions import Fraction
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sys

for variable in ("OPENBLAS_NUM_THREADS", "VECLIB_MAXIMUM_THREADS", "OMP_NUM_THREADS",
                 "MKL_NUM_THREADS", "BLIS_NUM_THREADS"):
    os.environ[variable] = "1"
sys.dont_write_bytecode = True

import numpy as np

import four_model_audit as fm
from dv_patch_check import reshape

HERE = Path(__file__).resolve().parent
PROTOCOL = HERE / "protocol-lattice-michael5400-20261111.json"
FROZEN = {
    "four_model_audit.py": "5269e01224be889a45a657fb346984db584186a8935c038fe9520261a8bf1380",
    "vector_metrics.py": "b8c4afd3bb6d2ad8fa6389f7d443233d2da53df9b113786a14b27c5931ae63c6",
}
ARTIFACTS = {
    "source", "bl", "el", "rpus", "events", "metadata",
    "master_binding", "readback", "init", "served_receipt",
}
WINDOW = ["125/1000", "2127/1000"]
TIME_BASE = "1/1000"
WIDTH, HEIGHT = 3840, 2160
ROI = [0, 32, WIDTH, 2128]
FRAME_COUNT = 48
PIVOT_DELTAS = [0, 128, 128, 128, 128, 128, 128, 128, 127]
VIDEO_CONFIG = {
    "box": "dvvC", "dv_profile": 8, "dv_bl_signal_compatibility_id": 1,
    "rpu_present_flag": 1, "bl_present_flag": 1, "el_present_flag": 0,
}
IDENTITY_MAPPING = {
    "pivots": [0, 1023], "pieces": [{"mapping_idc": 0, "poly_order": 1, "poly_coef": [0, 8388608, 0]}],
    "nlq_offset": 512, "vdr_in_max": 1048576, "slope": 2048, "threshold": 0,
}


def require(condition, context):
    if not condition:
        raise ValueError(context)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def is_sha(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def require_fields(value, expected, label):
    for name, expected_value in expected.items():
        got = value.get(name)
        require(type(got) is type(expected_value) and got == expected_value,
                f"{label}: invalid {name}")


def bind_artifacts(request):
    require_fields(request, {"schema": 1, "window": WINDOW}, "request")
    references = request["artifacts"]
    require(set(references) == ARTIFACTS, "request artifact names differ from schema 1")
    paths = {}
    for label, reference in references.items():
        path = Path(reference["path"])
        require(path.is_absolute() and is_sha(reference["sha256"]), f"{label}: invalid artifact reference")
        require(sha(path) == reference["sha256"], f"{label}: artifact SHA-256 mismatch")
        paths[label] = path.resolve(strict=True)
    require(len(set(paths.values())) == len(paths), "artifact paths must be distinct")
    return paths


def read_jsonl(path):
    with path.open() as stream:
        return [json.loads(line) for line in stream]


def validate_capture(events, metadata, paths, source_sha):
    sources = [event for event in events if event.get("kind") == "source"]
    require(len(sources) == 1, "capture must identify exactly one source")
    require_fields(sources[0], {"source_sha256": source_sha, "time_base": TIME_BASE,
                               "software_only": True, "decoder_threads": 1}, "capture source")
    pairs = [event for event in events if event.get("kind") == "accepted_source_pair"]
    require(len(pairs) == len(metadata) == FRAME_COUNT, "capture or metadata does not contain 48 frames")
    previous = None
    with paths["bl"].open("rb") as bl, paths["el"].open("rb") as el, paths["rpus"].open("rb") as rpus:
        for frame, (pair, observed) in enumerate(zip(pairs, metadata, strict=True)):
            require_fields(pair, {"frame": frame, "bl_width": WIDTH, "bl_height": HEIGHT,
                                  "el_width": WIDTH // 2, "el_height": HEIGHT // 2}, "capture frame")
            pts = Fraction(pair["pts"])
            require(Fraction(WINDOW[0]) <= pts < Fraction(WINDOW[1]) and
                    (previous is None or pts > previous), f"capture frame {frame}: invalid or duplicate PTS")
            ticks = pts / Fraction(TIME_BASE)
            require(ticks.denominator == 1, f"capture frame {frame}: non-integral source clock")
            require_fields(observed, {"frame": frame, "pts_ticks": int(ticks),
                                      "rpu_sha256": pair["rpu_sha256"]}, "metadata frame")
            previous = pts
            rpu_size = pair["rpu_size"]
            require(type(rpu_size) is int and 2 <= rpu_size <= 65536, "capture RPU length is invalid")
            for field, stream, size in (("bl", bl, WIDTH * HEIGHT * 3),
                                        ("el", el, WIDTH * HEIGHT * 3 // 4),
                                        ("rpu", rpus, rpu_size)):
                require(stream.tell() == pair[f"{field}_offset"], f"capture frame {frame}: noncontiguous {field}")
                raw = stream.read(size)
                require(len(raw) == size and hashlib.sha256(raw).hexdigest() == pair[f"{field}_sha256"],
                        f"capture frame {frame}: {field} hash mismatch")
        require(all(not stream.read(1) for stream in (bl, el, rpus)), "capture contains trailing bytes")
    require(Fraction(pairs[0]["pts"]) == Fraction(WINDOW[0]) and
            Fraction(pairs[-1]["pts"]) + Fraction(pairs[-1]["duration"]) == Fraction(WINDOW[1]),
            "capture does not span the requested window")
    return [frame["pts_ticks"] for frame in metadata]


def fixture_color(metadata):
    color = metadata[0]["color"]
    for row in metadata:
        require_fields(row["header"], {"coef_data_type": 0, "coef_log2_denom": 23,
                        "bl_bit_depth": 10, "el_bit_depth": 10, "vdr_bit_depth": 12,
                        "disable_residual_flag": 0}, "source RPU header")
        mapping = row["mapping"]
        require_fields(mapping, {"color_space": 0, "chroma_format_idc": 0, "nlq_method_idc": 0,
                       "num_x_partitions": 1, "num_y_partitions": 1}, "source mapping")
        require(mapping["curves"] == [IDENTITY_MAPPING] * 3, "unsupported source identity mapping or NLQ")
        require_fields(row["color"], {"signal_bit_depth": 12, "signal_color_space": 0,
                       "signal_chroma_format": 0, "signal_full_range_flag": 1}, "source color")
        for key in ("ycc_to_rgb_matrix", "ycc_to_rgb_offset", "rgb_to_lms_matrix"):
            require(row["color"][key] == color[key], f"source {key} varies by frame")

    def numerators(key, count, denominator):
        items = color[key]
        require(len(items) == count and all(len(item) == 2 and type(item[0]) is int and
                type(item[1]) is int and item[1] == denominator for item in items), f"invalid {key}")
        return [item[0] for item in items]

    matrix = np.array(numerators("ycc_to_rgb_matrix", 9, 8192), dtype=np.int64).reshape(3, 3)
    offsets = np.array(numerators("ycc_to_rgb_offset", 3, 2 ** 28), dtype=np.int64)
    lms = np.array(numerators("rgb_to_lms_matrix", 9, 16384), dtype=np.float64).reshape(3, 3)
    return matrix, offsets, fm.HPE_INVERSE @ (lms / 16384)


def validate_master(receipt, paths, events_sha):
    area = [ROI[0], WIDTH - ROI[2], ROI[1], HEIGHT - ROI[3]]
    expected = FRAME_COUNT * len(range(ROI[0], ROI[2], 8)) * len(range(ROI[1], ROI[3], 8))
    require_fields(receipt, {"kind": "compare_composed", "exact": True, "dump_complete": True,
                   "frames_compared": FRAME_COUNT, "capture_frames": FRAME_COUNT,
                   "position_errors": 0, "areas": [area]}, "helper master")
    for label in ("fit", "held_out"):
        require_fields(receipt[label], {"samples": expected, "mismatches": 0}, f"helper master {label}")
    for label in ("bl", "el"):
        require(Path(receipt["inputs"][label]).resolve(strict=True) == paths[label],
                f"helper master used different {label} planes")
    require(receipt["inputs"]["events_sha256"] == events_sha, "helper master events SHA-256 mismatch")
    require_fields(receipt["frozen_tools"], FROZEN, "helper master frozen tools")


def validate_curves(row):
    require_fields(row, {"profile": 8, "bl_bit_depth": 10, "vdr_bit_depth": 12,
                   "coefficient_log2_denom": 23, "disable_residual_flag": True,
                   "luma_pivot_deltas": PIVOT_DELTAS}, "served RPU")
    require(is_sha(row["rpu_sha256"]) and type(row["bytes"]) is int and row["bytes"] > 2,
            "missing served RPU byte identity")

    def grid(value, bits):
        return (type(value) in (int, float) and math.isfinite(value) and -64 <= value < 63
                and (value * 2 ** bits) % 1 == 0)

    luma = row["luma_coef"]
    require(len(luma) == 8 and all(len(piece) == 3 and all(grid(value, 23) for value in piece)
                                 for piece in luma), "invalid served luma coefficient lattice")
    chroma = row["chroma"]
    require(len(chroma) == 2 and all(grid(channel["constant"], 12) and len(channel["coef"]) == 3 and
            all(len(order) == 7 and all(grid(value, 12) for value in order) for order in channel["coef"])
            for channel in chroma), "invalid served Q12 chroma coefficient lattice")


def bind_readback(document, receipt, refs, ticks, allow_extra):
    require_fields(receipt, {"source_sha256": refs["source"]["sha256"], "init_sha256": refs["init"]["sha256"],
                             "readback_sha256": refs["readback"]["sha256"],
                             "source_time_base": TIME_BASE}, "served extraction receipt")
    require_fields(receipt["video_config"], VIDEO_CONFIG, "served init profile")
    wanted = set(ticks)
    low, high = (int(Fraction(value) / Fraction(TIME_BASE)) for value in WINDOW)
    by_pts = {}
    for row in document["frames"]:
        pts = row["source_pts"]
        require(type(pts) is int and pts not in by_pts, "readback contains an invalid or duplicate PTS")
        by_pts[pts] = row
        if pts not in wanted:
            require(allow_extra and not low <= pts < high, f"unexpected readback PTS {pts}")
            continue
        validate_curves(row)
    require(wanted <= by_pts.keys(), "readback is missing one or more requested source PTS")
    return [by_pts[pts] for pts in ticks]


def verify_extraction(receipt, paths, refs, events, curves):
    require_fields(receipt, {"schema": 1, "kind": "served_refit_extraction"}, "served extraction receipt")
    artifacts = receipt["artifacts"]
    for name in ("source", "init", "events"):
        require(artifacts[name]["sha256"] == refs[name]["sha256"],
                f"extraction receipt used different {name} bytes")
    served = artifacts["served"]
    medium = Path(served["path"])
    require(medium.is_absolute() and is_sha(served["sha256"]) and
            0 < served["bytes"] <= 512 * 1024 * 1024 and medium.stat().st_size == served["bytes"],
            "invalid served capture reference")
    require(sha(medium) == served["sha256"] == receipt["served_sha256"], "served capture SHA-256 mismatch")

    served_time = Fraction(receipt["served_time_base"])
    offset = Fraction(receipt["source_pts_offset_seconds"])
    require(served_time > 0, "invalid served clock")
    pairs = [event for event in events if event.get("kind") == "accepted_source_pair"]
    proofs = receipt["frames"]
    require(len(proofs) == len(curves) == len(pairs), "extraction proof must cover the scored window")
    for proof, row, pair in zip(proofs, curves, pairs, strict=True):
        pts = row["source_pts"]
        require_fields(proof, {"frame": pair["frame"], "source_pts": pts,
                               "source_rpu_sha256": pair["rpu_sha256"],
                               "served_rpu_sha256": row["rpu_sha256"]}, "served packet proof")
        source_hash = proof["source_packet"]["data_hash"].lower()
        require(source_hash == "sha256:" + pair["packet_sha256"], f"source packet association differs at PTS {pts}")
        require(type(proof["served_pts"]) is int and proof["served_pts"] == proof["served_packet"]["pts"] and
                proof["served_pts"] * served_time + offset == pts * Fraction(TIME_BASE),
                f"served packet clock transform differs at PTS {pts}")
        require(is_sha(proof["bl_vcl_sha256"]) and type(proof["bl_vcl_nals"]) is int and proof["bl_vcl_nals"] > 0,
                f"missing copied VCL proof at PTS {pts}")
        name = f"source-pts-{pts}.nal"
        raw = paths["readback"].parent / name
        require(row["out"] == name and not raw.is_symlink() and raw.stat().st_size == row["bytes"] and
                sha(raw) == row["rpu_sha256"], f"served RPU bytes differ at PTS {pts}")


def centered_reshape(curve, base):
    composed = np.clip(reshape(curve, *np.moveaxis(base, -1, 0)), 0, 4095).astype(np.int64)
    residual = composed - 4 * base
    return np.where(residual < 0, residual - 1, residual)


def lattice(start, stop):
    return np.sort(np.concatenate([np.arange(start + phase, stop, 8) for phase in (0, 1)]))


def sample_frame(bl, el, index, xs, ys):
    base_planes = fm.planes(bl, WIDTH, HEIGHT, index)
    enhancement_planes = fm.planes(el, WIDTH // 2, HEIGHT // 2, index)
    base_chunks, residual_chunks = [], []
    for lo in range(0, HEIGHT, 64):
        hi = min(lo + 64, HEIGHT)
        rows = ys[(ys >= lo) & (ys < hi)] - lo
        if len(rows):
            base, centered = fm.sample_codes(base_planes, enhancement_planes, base_planes,
                                             WIDTH, HEIGHT, lo, hi)["R"]
            base_chunks.append(base[rows][:, xs].reshape(-1, 3))
            residual_chunks.append(centered[rows][:, xs].reshape(-1, 3))
    return np.concatenate(base_chunks), np.concatenate(residual_chunks)


def summarize(values, band_index, bands):
    return {
        "full": fm.vm.stats(values),
        "bands": {f"[{lo},{hi if hi is not None else 'inf'})":
                  fm.vm.stats(values[band_index == index]) or {"count": 0}
                  for index, (lo, hi) in enumerate(bands)},
    }


def score(paths, metadata, curves, bands):
    bl = np.memmap(paths["bl"], dtype="<u2", mode="r").reshape(FRAME_COUNT, -1)
    el = np.memmap(paths["el"], dtype="<u2", mode="r").reshape(FRAME_COUNT, -1)
    xs, ys = lattice(ROI[0], ROI[2]), lattice(ROI[1], ROI[3])
    samples = len(xs) * len(ys)
    color = fixture_color(metadata)
    upper_bounds = [hi for _, hi in bands if hi is not None]
    result = {}
    for model in fm.MODEL_IDS:
        shape = (FRAME_COUNT, samples)
        errors = {name: np.empty(shape, dtype=np.float64) for name in ("F_identity", "served_Fplus")}
        masks = np.empty(shape, dtype=np.uint8)
        frames = []
        for index, curve in enumerate(curves):
            base, enhancement = sample_frame(bl, el, index, xs, ys)
            reference = fm.model_rgb(model, base, enhancement, *color)
            identity = fm.model_rgb(model, base, None, *color)
            fitted = fm.model_rgb(model, base, centered_reshape(curve, base), *color)
            errors["F_identity"][index] = fm.vm.delta(reference, identity)
            errors["served_Fplus"][index] = fm.vm.delta(reference, fitted)
            luminance = 10000 * fm.eotf(reference.astype(np.float64) / 65535) @ fm.Y_WEIGHTS
            masks[index] = np.searchsorted(upper_bounds, luminance, side="right")
            frames.append({"source_pts": curve["source_pts"], "rpu_sha256": curve["rpu_sha256"],
                           "arms": {arm: summarize(values[index], masks[index], bands)
                                    for arm, values in errors.items()}})
        result[model] = {"frames": frames,
                         "pooled": {arm: summarize(values, masks, bands) for arm, values in errors.items()}}
    return result


def evaluate(request_path):
    request_sha = sha(request_path)
    request = json.loads(request_path.read_text())
    refs = request["artifacts"]
    allow_extra = request.get("allow_extra_readback_frames", False)
    require(type(allow_extra) is bool, "allow_extra_readback_frames must be a Boolean")
    paths = bind_artifacts(request)
    tools = {path.name: sha(path) for path in (Path(__file__), HERE / "four_model_audit.py",
                                               HERE / "vector_metrics.py", HERE / "dv_patch_check.py", PROTOCOL)}
    require_fields(tools, FROZEN, "frozen scorer")
    require(Path(fm.__file__).resolve() == HERE / "four_model_audit.py" and
            Path(fm.vm.__file__).resolve() == HERE / "vector_metrics.py", "scorer imports escaped their source")
    events, metadata = read_jsonl(paths["events"]), read_jsonl(paths["metadata"])
    ticks = validate_capture(events, metadata, paths, refs["source"]["sha256"])
    validate_master(json.loads(paths["master_binding"].read_text()), paths, refs["events"]["sha256"])
    receipt = json.loads(paths["served_receipt"].read_text())
    curves = bind_readback(json.loads(paths["readback"].read_text()), receipt, refs, ticks, allow_extra)
    verify_extraction(receipt, paths, refs, events, curves)
    bands = json.loads(PROTOCOL.read_text())["luminance_bands_reference_weighted_linear_Y_nits"]
    return {"schema": 1, "kind": "served_fplus_baseline", "request_sha256": request_sha,
            "artifacts": refs, "scorers": tools, "window": WINDOW, "source_time_base": TIME_BASE,
            "sampling": {"roi": ROI, "stride": 8, "axis_phases": [0, 1], "frames": FRAME_COUNT},
            "authority": {"reference": "frozen four-model reconstruction; helper composition agreed on fit/held-out parity lattices",
                          "served": "caller extraction receipt identifies the served init and per-picture curve readback",
                          "scope": "sampled offline baseline only"},
            "models": score(paths, metadata, curves, bands)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("request", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    try:
        require(not args.output.exists(), "output already exists")
        result = evaluate(args.request)
        with args.output.open("x") as stream:
            json.dump(result, stream, indent=2, allow_nan=False)
            stream.write("\n")
    except (OSError, ValueError, TypeError, KeyError, IndexError) as error:
        parser.exit(1, f"served F+ baseline: {error}\n")


if __name__ == "__main__":
    main()
