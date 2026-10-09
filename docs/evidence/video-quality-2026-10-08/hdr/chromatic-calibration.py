#!/usr/bin/env python3
"""Supplement the neutral calibration with analytical BT.2020 color swatches."""
import argparse
import array
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import sys

SWATCHES = [
    ("neutral", (1, 1, 1)), ("red", (1, 0, 0)), ("green", (0, 1, 0)),
    ("blue", (0, 0, 1)), ("cyan", (0, 1, 1)), ("magenta", (1, 0, 1)),
    ("orange", (1, .35, .02)), ("warm", (1, .62, .42)),
]


def signal(case, calibration):
    """Analytical encoded BT.2020NC; the warm proxy is not real skin."""
    width, height = calibration.WIDTH, calibration.HEIGHT
    if (width, height) != (512, 128):
        raise ValueError("chromatic layout requires the original 512x128 geometry")
    peak = 4000 if case == "pq-4000" else 1000
    yuv = []
    for row in range(height):
        for x in range(width):
            _, rgb = SWATCHES[x // 64]
            exposure = .001 if row < 43 else .1 if row < 86 else 1
            if case == "hlg":
                encoded = [(calibration.hlg_code(v * exposure) - 64) / 876 for v in rgb]
            else:
                encoded = [(calibration.pq_code(v * exposure * peak) - 64) / 876 for v in rgb]
            red, green, blue = encoded
            luma = .2627 * red + .678 * green + .0593 * blue
            yuv.append((round(64 + 876 * luma),
                        round(512 + 896 * (blue - luma) / 1.8814),
                        round(512 + 896 * (red - luma) / 1.4746)))
    raw = array.array("H", [p[0] for p in yuv])
    for channel in (1, 2):
        raw.extend(round(sum(yuv[(y + dy) * width + x + dx][channel]
                             for dy in (0, 1) for dx in (0, 1)) / 4)
                   for y in range(0, height, 2) for x in range(0, width, 2))
    if sys.byteorder != "little":
        raw.byteswap()
    return raw.tobytes()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ffmpeg", type=Path, required=True)
    parser.add_argument("--ffprobe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--tool-prefix-json", default="[]")
    args = parser.parse_args()
    prefix = json.loads(args.tool_prefix_json)
    if not isinstance(prefix, list) or any(not isinstance(x, str) or not x for x in prefix):
        parser.error("tool prefix must be an argv string array")
    root = Path(__file__).resolve().parents[4]
    loader = importlib.machinery.SourceFileLoader("calibration", str(root / "scripts/tone-map-calibration"))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    calibration = importlib.util.module_from_spec(spec)
    loader.exec_module(calibration)
    calibration.authored_frame = lambda case: signal(case, calibration)
    report = calibration.calibrate(
        args.ffmpeg if prefix else args.ffmpeg.resolve(),
        args.ffprobe if prefix else args.ffprobe.resolve(),
        args.output.resolve(), prefix=prefix,
    )
    report["kind"] = "authored-chromatic-tone-map-comparison"
    report["authored_signal"] = {
        "matrix": "BT2020NC coefficients Kr=.2627 Kb=.0593", "swatches": SWATCHES,
        "row_relative_exposures": [.001, .1, 1],
        "source": "Analytical PQ inverse EOTF or HLG OETF; the warm proxy is not genuine skin",
        "chroma": "2x2 arithmetic average in encoded YUV; limited ten-bit",
    }
    report["limitations"] = [
        "Tool hashes and versions bind this screening; not full fleet qualification",
        "Historical graph is not independent ground truth",
        "Synthetic warm proxy does not establish real skin fidelity or artistic grading",
        "No physical-display qualification",
        "Filter math only: the reused harness omits production HDR side-data deletion; not an SDR grade reference",
        "Saturated BT2020 primaries exceed BT709 gamut; clipping is measured, not assumed avoidable",
    ]
    (args.output.resolve() / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"completed": report["completed"], "cases": [x["name"] for x in report["cases"]]}))


if __name__ == "__main__":
    main()
