"""F+ statistics on real P7 FEL media with edited RPUs; no GPU.

Derives three titles from one P7 FEL source with dovi_tool (editor,
inject-rpu) and mkvmerge, then runs segment_decode_render --refit-stats on
the source and on each:
- the source: scenes start at dovi_tool's scene cuts, at the presentation
  times of those frames (packet pts sorted), and the run ends at EOF with
  every decoded frame;
- l5: frames 20-50 get a 100/100 active area inside the first scene, whose
  sample counts follow the per-frame lattice;
- cut: a scene cut on a frame that the coded order reorders (its decode
  index differs from its presentation index) starts a scene at that frame's
  presentation time;
- bad: an invalid active area at frame 200 refuses the run as content
  (exit 3, REFIT_EXIT_CONTENT) after the scenes already complete were
  written.

Usage: python3 test_refit_title_edits.py HELPER SOURCE.mkv WORK_DIR
(HELPER may be a wrapper that sets the helper's library path.)
"""
import json
import subprocess
import sys
from pathlib import Path


def run(helper, source):
    p = subprocess.run([str(helper), "--refit-stats", str(source), "0"], capture_output=True)
    return p.returncode, [json.loads(l) for l in p.stdout.decode().splitlines()], p.stderr.decode()


def axis(start, end, phase):
    return len(range(start + phase, end, 8))


def derive(source, work, name, edit):
    (work / f"{name}.json").write_text(json.dumps(edit))
    subprocess.run(["dovi_tool", "editor", "-i", work / "rpu.bin", "-j", work / f"{name}.json",
                    "-o", work / f"rpu-{name}.bin"], check=True, capture_output=True)
    subprocess.run(["dovi_tool", "inject-rpu", "-i", work / "src.hevc", "--rpu-in",
                    work / f"rpu-{name}.bin", "-o", work / f"{name}.hevc"], check=True, capture_output=True)
    subprocess.run(["mkvmerge", "-q", "-o", work / f"{name}.mkv", "--default-duration", "0:24000/1001p",
                    work / f"{name}.hevc"], check=True)
    (work / f"{name}.hevc").unlink()
    return work / f"{name}.mkv"


def display_pts(path):
    out = subprocess.check_output(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
                                   "packet=pts", "-of", "csv=p=0", str(path)], text=True)
    decode = [int(l.split(",")[0]) for l in out.split()]
    return decode, sorted(decode)


def test_refit_title_edits(helper, source, work):
    work.mkdir(exist_ok=False)
    subprocess.run(["mkvextract", str(source), "tracks", f"0:{work / 'src.hevc'}"], check=True, capture_output=True)
    subprocess.run(["dovi_tool", "extract-rpu", "-i", work / "src.hevc", "-o", work / "rpu.bin"],
                   check=True, capture_output=True)
    subprocess.run(["dovi_tool", "export", "-i", work / "rpu.bin", "-d", f"scenes={work / 'scenes.txt'}"],
                   check=True, capture_output=True)
    cuts = [int(x) for x in (work / "scenes.txt").read_text().split()]

    code, lines, err = run(helper, source)
    assert code == 0, err
    scenes, summary = lines[:-1], lines[-1]
    decode, shown = display_pts(source)
    assert summary["frames"] == len(decode) and summary["rpu_scenes"] == len(cuts)
    # Every scene of this source is long, so the pooled scenes are the RPU's own.
    assert [s["first_frame"] for s in scenes] == cuts
    assert [s["first_pts"] for s in scenes] == [shown[c] for c in cuts]
    assert any(decode[i] > decode[i + 1] for i in range(len(decode) - 1)), "coded order reorders"
    assert summary["end_pts"] > shown[-1] and summary["fit_samples"] == sum(sum(s["fit"]["ycnt"]) for s in scenes)

    l5 = derive(source, work, "l5", {"active_area": {"crop": False, "presets": [
        {"id": 1, "left": 0, "right": 0, "top": 100, "bottom": 100}], "edits": {"20-50": 1}}})
    code, lines, err = run(helper, l5)
    assert code == 0, err
    first = lines[0]
    assert first["first_frame"] == 0 and first["frames"] == cuts[1]
    # The source's own L5 for the other frames, read back from its first frame.
    dump = subprocess.run([str(helper), "--dump-composed", str(source), "0", "0", "0"], capture_output=True).stdout
    area = json.loads(dump.split(b"\n")[1])["active_area"]
    w, h = 3840, 2160
    own = [axis(area[0], w - area[1], p) * axis(area[2], h - area[3], p) for p in (0, 4)]
    edited = [axis(0, w, p) * axis(100, h - 100, p) for p in (0, 4)]
    for k, lattice in enumerate(("fit", "held_out")):
        want = 31 * edited[k] + (cuts[1] - 31) * own[k]
        assert sum(first[lattice]["ycnt"]) == want, (lattice, sum(first[lattice]["ycnt"]), want)

    cut_at = next(k for k in range(cuts[2] + 24, cuts[3] - 24) if decode.index(shown[k]) != k)
    cut = derive(source, work, "cut", {"scene_cuts": {f"{cut_at}-{cut_at}": True}})
    code, lines, err = run(helper, cut)
    assert code == 0, err
    _, cut_shown = display_pts(cut)
    starts = {s["first_frame"]: s["first_pts"] for s in lines[:-1]}
    assert starts.get(cut_at) == cut_shown[cut_at], (cut_at, starts)

    bad = derive(source, work, "bad", {"active_area": {"crop": False, "presets": [
        {"id": 2, "left": 2000, "right": 2000, "top": 0, "bottom": 0}], "edits": {"200-200": 2}}})
    code, lines, err = run(helper, bad)
    assert code == 3 and err.strip().endswith("invalid active area metadata"), (code, err)
    assert len(err.strip().splitlines()) == 1, err
    assert lines and all(l["kind"] == "refit_scene" for l in lines)
    assert lines[-1]["first_frame"] + lines[-1]["frames"] <= 200
    for name in ("l5", "cut", "bad"):
        (work / f"{name}.mkv").unlink()
    (work / "src.hevc").unlink()
    print(json.dumps({"refit_title_edits": "pass", "frames": summary["frames"], "cuts": cuts,
                      "reordered_cut": cut_at, "bad_partial_scenes": len(lines)}))


if __name__ == "__main__":
    test_refit_title_edits(*(Path(arg).resolve() for arg in sys.argv[1:]))
