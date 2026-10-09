"""Focused real decoded EOF/AAC checks; no qualification or throughput claim."""
import json
import os
from fractions import Fraction
from pathlib import Path
import subprocess
import sys


def test_video_track_duration_eof_with_audio(helper, source, work):
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_streams", "-show_frames",
        "-select_streams", "v:0", "-of", "json", str(source)]))
    stream = probe["streams"][0]
    assert stream["codec_name"] == "hevc" and stream["start_pts"] == 0
    assert "duration" not in stream
    tag = stream["tags"]["DURATION"]
    all_streams = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_streams", "-of", "json", str(source)]))["streams"]
    assert any(s["codec_name"] == "aac" for s in all_streams)
    last = probe["frames"][-1]
    tick = Fraction(stream["time_base"])
    start = int(last["pts"]) * tick
    end = start + int(last["pkt_duration"]) * tick
    rational = lambda value: f"{value.numerator}/{value.denominator}"
    raw = source.read_bytes()
    assert raw.count(tag.encode()) == 1, "unique selected-video duration tag"
    work.mkdir(exist_ok=False)
    results = []
    cases = [("fel", raw, False, None), ("base", raw, True, None)]
    at = raw.index(tag.encode())
    key = raw.rfind(b"DURATION", 0, at)
    assert key >= 0
    missing = raw[:key] + b"XURATION" + raw[key + 8:]
    cases += [("missing", missing, False, "source EOF extent unavailable or inconsistent")]
    for name, replacement, reason in [
        ("malformed", "x" + tag[1:], "invalid declared video DURATION extent"),
        ("invalid-minute", "00:99:00.000000000", "invalid declared video DURATION extent"),
        ("mismatch", "00:00:00.001000000", "source EOF disagrees with declared video DURATION extent")]:
        # Equal-size mutations keep unrelated EBML lengths and all coded AUs intact.
        assert len(replacement) == len(tag)
        cases.append((name, raw.replace(tag.encode(), replacement.encode(), 1), False, reason))
    for name, payload, base, reason in cases:
        directory = work / name
        directory.mkdir()
        path = directory / "source.mkv"
        path.write_bytes(payload)
        with path.open("rb") as src, (directory / "out.nut").open("xb") as out:
            args = [str(helper), f"/proc/self/fd/{src.fileno()}", ".", "0", "64",
                str(stream["width"]), str(stream["height"]), "0", "0", "0",
                "bt2020-pq-master-clip", f"/proc/self/fd/{out.fileno()}",
                rational(start), rational(end), "512"]
            if base:
                args.append("base-rpu")
            result = subprocess.run(args, cwd=directory, env=os.environ,
                pass_fds=(src.fileno(), out.fileno()), capture_output=True, timeout=30)
        (directory / "renderer.jsonl").write_bytes(result.stdout)
        (directory / "stderr").write_bytes(result.stderr)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        complete = [e for e in events if e["kind"] == "window_complete"]
        if reason:
            assert result.returncode == 1 and reason in result.stderr.decode(), (name, result.stderr)
            assert not complete
        else:
            assert result.returncode == 0, (name, result.stderr)
            assert len(complete) == 1 and complete[0]["source_eof_observed"]
            assert complete[0]["frames"] == 1
        results.append({"case": name, "exit": result.returncode, "reason": reason})
    (work / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results))


if __name__ == "__main__":
    test_video_track_duration_eof_with_audio(*(Path(arg).resolve() for arg in sys.argv[1:]))
