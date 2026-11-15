#!/usr/bin/env python3
"""Bind an offline capture to retained source events and rehash every raw frame.

This proves source identity for the selected window, not Dolby composition.
All paths are local CLI arguments. A pass receipt is created exclusively only
when every check has completed; an existing receipt is never overwritten.
"""
import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import stat


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture_root", type=Path)
    parser.add_argument("expected_events", type=Path)
    parser.add_argument("expected_case_receipt", type=Path)
    parser.add_argument("source", type=Path)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--expect-count", type=int, required=True)
    args = parser.parse_args()
    require(args.expect_count > 0, "positive explicit expected frame count")
    root = args.capture_root.resolve(strict=True)
    source = args.source.resolve(strict=True)
    require(stat.S_ISREG(source.stat().st_mode), "local regular source required")
    summary = json.loads((root / "summary.json").read_text())
    require(summary["capture_complete"] is True, "complete capture required")
    require(summary["normative_composer"] is False, "source-identity diagnostic scope")
    require(source.stat().st_size == summary["source_bytes"] and
            sha256(source) == summary["source_sha256"], "original source identity")
    start, end = Fraction(summary["window_start"]), Fraction(summary["window_end"])
    require(start < end, "ordered half-open capture window")
    time_base = Fraction(summary["time_base"])
    old = json.loads(args.expected_events.read_text())
    retained = [row for row in old if row.get("kind") == "accepted_source_pair"]
    case = json.loads(args.expected_case_receipt.read_text())
    require(retained == case["accepted_sources"], "independent retained case/event agreement")
    expected = [row for row in retained if start <= Fraction(row["pts"]) < end]
    require(len(expected) == args.expect_count, "exact retained window frame count")
    coded = {row["pts_ticks"]: row for row in old if row.get("kind") == "coded_window_input"}
    require(len(coded) == len(retained), "one retained original coded AU per accepted source")
    rows = [json.loads(line) for line in (root / "events.jsonl").read_text().splitlines()]
    frames = [row for row in rows if row.get("kind") == "accepted_source_pair"]
    require(len(frames) == summary["selected_frames"] == len(expected), "exact capture window frame count")
    fields = ["pts", "duration", "bl_sha256", "el_sha256", "rpu_sha256",
              "bl_width", "bl_height", "el_width", "el_height"]
    results = []
    with (root / "bl.yuv").open("rb") as bl, (root / "el.yuv").open("rb") as el, (root / "rpus.bin").open("rb") as rpu:
        for index, (actual, wanted) in enumerate(zip(frames, expected)):
            require(actual["frame"] == index, "contiguous captured ordinal")
            require(all(actual[key] == wanted[key] for key in fields),
                    f"canonical association mismatch at captured frame {index}")
            for layer, stream in (("bl", bl), ("el", el)):
                require(stream.tell() == actual[f"{layer}_offset"], "contiguous canonical plane offset")
                width, height = actual[f"{layer}_width"], actual[f"{layer}_height"]
                require(width == summary[f"{layer}_width"] and height == summary[f"{layer}_height"],
                        "summary raster agreement")
                size = width * height * 3
                data = stream.read(size)
                require(len(data) == size and hashlib.sha256(data).hexdigest() == wanted[f"{layer}_sha256"],
                        f"raw canonical {layer} digest at captured frame {index}")
            require(rpu.tell() == actual["rpu_offset"], "contiguous fresh RPU offset")
            data = rpu.read(actual["rpu_size"])
            require(len(data) == actual["rpu_size"] and hashlib.sha256(data).hexdigest() == wanted["rpu_sha256"],
                    f"raw RPU digest at captured frame {index}")
            ticks = Fraction(actual["pts"]) / time_base
            require(ticks.denominator == 1 and int(ticks) in coded, "retained original packet timestamp")
            original = coded[int(ticks)]
            require(actual["packet_sha256"] == original["packet_sha256"] and
                    actual["rpu_sha256"] == original["rpu_sha256"] and
                    Fraction(actual["duration"]) == original["duration_ticks"] * time_base,
                    f"independent coded/decoded binding at captured frame {index}")
            results.append({"captured_frame": index, "retained_frame": wanted["frame"],
                            **{key: actual[key] for key in fields}, "packet_sha256": actual["packet_sha256"],
                            "raw_bl_el_rpu_rehashed": True})
        require(all(stream.read(1) == b"" for stream in (bl, el, rpu)), "no unaccounted trailing raw bytes")
    receipt = {"result": "pass", "capture_root": str(root), "capture_summary": summary,
               "expected_events_sha256": sha256(args.expected_events),
               "expected_case_receipt_sha256": sha256(args.expected_case_receipt),
               "verifier_sha256": sha256(Path(__file__)), "canonical_matches": len(results),
               "every_dumped_frame_rehashed": True,
               "canonical_layout": "10-bit little-endian planar Y/U/V; width*2 row bytes, excluding stride padding",
               "frames": results,
               "capture_files": {name: {"bytes": (root / name).stat().st_size, "sha256": sha256(root / name)}
                                 for name in ("summary.json", "events.jsonl", "bl.yuv", "el.yuv", "rpus.bin")},
               "non_output_original_aus": [row for row in rows if row.get("kind") == "coded_before_initial_random_access_not_output"],
               "normative_composer": False}
    with args.receipt.open("x") as output:
        json.dump(receipt, output, indent=2)
        output.write("\n")
    print(json.dumps({"result": "pass", "canonical_matches": len(results), "receipt": str(args.receipt)}))


if __name__ == "__main__":
    main()
