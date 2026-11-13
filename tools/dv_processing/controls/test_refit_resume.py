"""F+ statistics: resumed runs, thread counts and refusal classes; no GPU.

On one real Profile 7 FEL source, segment_decode_render --refit-stats:
- with --threads 1, 2 and the default 4 writes the same lines;
- with --resume FIRST FIRST_PTS at the start of every pooled scene after the
  first seeks there and writes exactly the uninterrupted run's scenes from
  that one on; the scenes before it plus the resumed run's rebuild the
  uninterrupted run's summary (rpu_scene_frames carries the RPU scene
  lengths the median needs);
- refuses a resume point that is not a frame's presentation time with
  exit 1;
- refuses a content problem (an --area that leaves no picture) with exit 3
  (REFIT_EXIT_CONTENT) and an unreadable source with exit 1, each with
  exactly one stderr line.

Usage: python3 test_refit_resume.py HELPER SOURCE.mkv
(HELPER may be a wrapper that sets the helper's library path.)
"""
import json
import statistics
import subprocess
import sys
import time


def run(helper, source, *extra):
    started = time.monotonic()
    p = subprocess.run([str(helper), "--refit-stats", str(source), "0", *extra], capture_output=True)
    lines = [json.loads(line) for line in p.stdout.decode().splitlines()]
    return p.returncode, lines, p.stderr.decode(), time.monotonic() - started


def samples(stats):
    return sum(stats["ycnt"])


def summary_of(scenes):
    """The summary an uninterrupted run writes, from its scenes alone."""
    lengths = [n for s in scenes for n in s["rpu_scene_frames"]]
    median = statistics.median(lengths)
    return {"kind": "refit_summary", "frames": sum(s["frames"] for s in scenes),
            "scenes": len(scenes), "rpu_scenes": len(lengths), "median_scene_frames": median,
            "degenerate": median < 24, "fit_samples": sum(samples(s["fit"]) for s in scenes),
            "held_out_samples": sum(samples(s["held_out"]) for s in scenes),
            "first_pts": scenes[0]["first_pts"], "end_pts": scenes[-1]["end_pts"],
            "time_base": scenes[0]["time_base"]}


def test_refit_resume(helper, source):
    code, full, err, whole = run(helper, source)
    assert code == 0 and not err, (code, err)
    scenes, summary = full[:-1], full[-1]
    assert summary["kind"] == "refit_summary" and len(scenes) == summary["scenes"] >= 2
    assert summary_of(scenes) == summary, (summary_of(scenes), summary)
    for s in scenes:
        assert sum(s["rpu_scene_frames"]) == s["frames"], s["first_frame"]
    for threads in ("1", "2"):
        code, lines, err, _ = run(helper, source, "--threads", threads)
        assert code == 0 and lines == full, f"--threads {threads} changed the output"
    timings = {}
    for index in range(1, len(scenes)):
        at = scenes[index]
        code, lines, err, took = run(helper, source, "--resume", str(at["first_frame"]),
                                     str(at["first_pts"]))
        assert code == 0 and not err, (at["first_frame"], code, err)
        resumed, tail = lines[:-1], lines[-1]
        assert resumed == scenes[index:], f"--resume {at['first_frame']}: scenes differ"
        assert tail == summary_of(resumed), at["first_frame"]
        assert summary_of(scenes[:index] + resumed) == summary
        timings[at["first_frame"]] = round(took, 2)
    code, lines, err, _ = run(helper, source, "--resume", str(scenes[1]["first_frame"]),
                              str(scenes[1]["first_pts"] + 1))
    assert code == 1 and len(err.strip().splitlines()) == 1, (code, err)
    code, lines, err, _ = run(helper, source, "--area", "4000", "4000", "0", "0")
    assert code == 3 and len(err.strip().splitlines()) == 1, (code, err)
    code, lines, err, _ = run(helper, "/nonexistent/source.mkv")
    assert code == 1 and len(err.strip().splitlines()) == 1 and not lines, (code, err)
    print(json.dumps({"scenes": len(scenes), "frames": summary["frames"],
                      "full_seconds": round(whole, 2), "resume_seconds": timings}))


if __name__ == "__main__":
    test_refit_resume(sys.argv[1], sys.argv[2])
