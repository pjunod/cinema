#!/usr/bin/env python3
"""Census of a library's FEL titles by the vdr_in_max clamp admission.

For every Dolby Vision Profile 7 file with an enhancement layer under the
given roots, read the decoder's RPU metadata at evenly spaced positions and
judge each RPU's NLQ parameters with tools/dv_processing/nlq_clipping.h,
compiled here, so the census applies the helper's own predicates:

- ``pq-bilinear-*`` (no clamp) admits only ``nlq_clip_is_nonbinding``;
- ``pq-ccm001-*`` (clause 5.4.3.2's clamp) admits ``nlq_clip_is_bounded``.

A title counts as refused for the clamp when any sampled RPU fails the
predicate. Sampling reads a frame per position, not the whole file, so a
title whose parameters change between samples can be missed; the result
records the positions read. Titles outside the helper's LINEAR_DZ subset
(denominator 23, 10-bit EL, disable_residual_flag 0) are listed apart,
since they are refused for another reason under either composition.

Usage: fel_clamp_census.py --root DIR [--root DIR ...] --out result.json
       [--redact]   (name titles title-1, title-2, ... instead of by path)
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
NLQ_CLIPPING = HERE.parents[1] / "nlq_clipping.h"
EXTENSIONS = {".mkv", ".mp4", ".m4v", ".m2ts", ".ts"}

JUDGE_C = r"""
#include "nlq_clipping.h"
#include <inttypes.h>
#include <stdio.h>
int main(void) {
  unsigned offset; uint64_t slope, threshold, maximum;
  while (scanf("%u %" SCNu64 " %" SCNu64 " %" SCNu64, &offset, &slope,
               &threshold, &maximum) == 4) {
    uint64_t integer = maximum >> 23, fraction = maximum & ((UINT64_C(1) << 23) - 1);
    printf("%d %d\n",
           offset <= 0xffff && nlq_clip_is_nonbinding((uint16_t)offset, slope, threshold, integer, fraction),
           offset <= 0xffff && nlq_clip_is_bounded((uint16_t)offset, slope, threshold, integer, fraction));
  }
  return 0;
}
"""


def build_judge(work):
    src = work / "judge.c"
    src.write_text(JUDGE_C)
    binary = work / "judge"
    subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                    "-I", str(NLQ_CLIPPING.parent), str(src), "-o", str(binary)], check=True, timeout=60)
    return binary


def ffprobe(args, timeout=120):
    run = subprocess.run(["ffprobe", "-v", "error", *args, "-of", "json"],
                         capture_output=True, text=True, timeout=timeout)
    return json.loads(run.stdout) if run.returncode == 0 and run.stdout else None


def dual_layer_profile7(path):
    info = ffprobe(["-select_streams", "v", "-show_streams", "-show_format", str(path)])
    if not info:
        return None
    for stream in info.get("streams", []):
        for sd in stream.get("side_data_list", []):
            if sd.get("side_data_type") == "DOVI configuration record":
                if sd.get("dv_profile") == 7 and sd.get("el_present_flag") == 1:
                    return {"stream": stream["index"],
                            "duration": float(info.get("format", {}).get("duration") or 0)}
    return None


def rpu_at(path, stream, seconds):
    frames = ffprobe(["-select_streams", str(stream), "-read_intervals", f"{seconds:.3f}%+#1",
                      "-show_frames", "-show_entries", "frame=pts_time:frame_side_data", str(path)],
                     timeout=300)
    for frame in (frames or {}).get("frames", []):
        for sd in frame.get("side_data_list", []):
            if sd.get("side_data_type") == "Dolby Vision Metadata":
                return {"pts_time": frame.get("pts_time"), **sd}
    return None


def census_title(path, found, samples, judge):
    duration = found["duration"]
    positions = [duration * (i + 0.5) / samples for i in range(samples)] if duration > 0 else [0.0]
    rpus, unread = [], []
    for seconds in positions:
        rpu = rpu_at(path, found["stream"], seconds)
        (rpus if rpu else unread).append(rpu if rpu else round(seconds, 3))
    title = {"path": str(path), "duration": duration, "positions_read": len(rpus),
             "positions_unread": unread}
    if not rpus:
        title["verdict"] = "unread"
        return title
    subset = all(r.get("coef_log2_denom") == 23 and r.get("el_bit_depth") == 10 and
                 r.get("disable_residual_flag") == 0 and r.get("nlq_method_idc") == 0 and
                 len(r.get("components", [])) == 3 for r in rpus)
    params = sorted({(c["nlq_offset"], c["linear_deadzone_slope"], c["linear_deadzone_threshold"],
                      c["vdr_in_max"]) for r in rpus for c in r.get("components", [])
                     if "nlq_offset" in c})
    title["nlq_parameters"] = [dict(zip(("offset", "slope", "threshold", "vdr_in_max"), p)) for p in params]
    # MEL: an enhancement layer whose residual is zero (slope and threshold 0).
    title["mel"] = all(p[1] == 0 and p[2] == 0 for p in params)
    if not subset or not params:
        title["verdict"] = "outside the LINEAR_DZ subset"
        return title
    lines = "".join(f"{o} {s} {t} {m}\n" for o, s, t, m in params)
    out = subprocess.run([str(judge)], input=lines, capture_output=True, text=True, check=True,
                         timeout=30).stdout.split("\n")
    judged = [tuple(int(x) for x in line.split()) for line in out if line]
    assert len(judged) == len(params)
    title["refused_pq_bilinear"] = not all(nb for nb, _ in judged)
    title["refused_pq_ccm001"] = not all(b for _, b in judged)
    title["verdict"] = ("refused under both" if title["refused_pq_ccm001"] else
                        "admitted by pq-ccm001 only" if title["refused_pq_bilinear"] else
                        "admitted under both")
    return title


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", action="append", required=True, type=Path)
    parser.add_argument("--samples", type=int, default=12)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--redact", action="store_true",
                        help="name titles by their order, not their path, and omit the roots")
    args = parser.parse_args()
    files = sorted(p for root in args.root for p in root.rglob("*")
                   if p.suffix.lower() in EXTENSIONS and p.is_file() and "@Recycle" not in p.parts)
    with tempfile.TemporaryDirectory() as work:
        judge = build_judge(Path(work))
        titles = []
        for path in files:
            found = dual_layer_profile7(path)
            if found is None:
                continue
            titles.append(census_title(path, found, args.samples, judge))
    counts = {}
    for t in titles:
        counts[t["verdict"]] = counts.get(t["verdict"], 0) + 1
    if args.redact:
        for number, title in enumerate(titles, 1):
            title["path"] = f"title-{number}"
    result = {
        "roots": "redacted" if args.redact else [str(r) for r in args.root],
        "files_scanned": len(files),
        "dual_layer_profile7_titles": len(titles),
        "samples_per_title": args.samples,
        "predicates": "tools/dv_processing/nlq_clipping.h: nlq_clip_is_nonbinding (pq-bilinear-*), "
                      "nlq_clip_is_bounded (pq-ccm001-*)",
        "refused_for_clamp_before": sum(bool(t.get("refused_pq_bilinear")) for t in titles),
        "refused_for_clamp_after": sum(bool(t.get("refused_pq_ccm001")) for t in titles),
        "verdicts": counts,
        "titles": titles,
    }
    args.out.write_text(json.dumps(result, indent=1) + "\n")
    print(json.dumps({k: result[k] for k in ("files_scanned", "dual_layer_profile7_titles",
                                              "refused_for_clamp_before", "refused_for_clamp_after",
                                              "verdicts")}))


if __name__ == "__main__":
    main()
