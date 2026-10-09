#!/usr/bin/env python3
"""Bounded, source-local C2 recipe qualification; emits numerical evidence only.

The private manifest names three existing tagged SDR source files and exact
production encoder exports. This does not mutate daemon settings or exercise
its durable job/cache owners. All source clips and score files remain under a
fresh private directory on the source node and are removed in finally.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1024 * 1024):
            h.update(chunk)
    return h.hexdigest()


def metrics(path, count):
    rows = json.loads(Path(path).read_text())["frames"]
    if len(rows) != count or [x["frameNum"] for x in rows] != list(range(count)):
        raise ValueError("scorer frame identity mismatch")
    values = [x["metrics"]["vmaf"] for x in rows]
    if not all(isinstance(x, (int, float)) and math.isfinite(x) and 0 <= x <= 100 for x in values):
        raise ValueError("invalid VMAF")
    return {"mean": sum(values) / len(values), "p10": sorted(values)[math.ceil(len(values) / 10) - 1], "frames": count}


def run(manifest, output):
    m = json.loads(Path(manifest).read_text())
    if len(m["sources"]) != 3:
        raise ValueError("exactly three preselected anonymous source identities required")
    root = Path(tempfile.mkdtemp(prefix="plurx-content-qualification-", dir="/var/tmp"))
    deadline = time.monotonic() + 600
    container = None
    report = {"schema_version": 1, "scope": "source_local_real_title_recipe_measurement", "production_job_integration": False,
              "source_revision": m["source_revision"], "resource_limits": {"cpu_threads": 2, "wall_seconds": 600, "scratch_bytes": 1073741824},
              "script_sha256": sha(__file__), "cases": [], "cleanup": {},
              "limits": ["60-second copied excerpts, not whole-film guarantee", "isolated current shipping encoder/scorer; no durable cache/offline owner invocation", "single encode timing per recipe; not fleet throughput", "no private media exported"]}
    def idle():
        text = subprocess.check_output(["curl", "-fsS", "--max-time", "5", "http://127.0.0.1:32400/metrics"], text=True)
        counts = [float(x.split()[-1]) for x in text.splitlines() if x.startswith("plurx_transcode_sessions_active")]
        if not counts or any(counts):
            raise RuntimeError("active or unknown viewers")
    def invoke(argv, timeout=90, capture=True):
        if time.monotonic() >= deadline:
            raise TimeoutError("campaign budget")
        if sum(x.stat().st_size for x in root.iterdir() if x.is_file()) > 1073741824:
            raise RuntimeError("scratch cap")
        idle()
        t = time.monotonic()
        p = subprocess.run(["docker", "exec", "-w", str(root), container] + argv, stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
                           stderr=subprocess.PIPE, timeout=min(timeout, max(1, deadline - t)))
        if p.returncode:
            raise RuntimeError("media command failed: " + p.stderr.decode(errors="replace")[-1000:])
        return p.stdout, time.monotonic() - t
    try:
        idle()
        image = subprocess.check_output(["docker", "inspect", "plurxd", "--format", "{{.Image}}"], text=True).strip()
        report["image"] = image
        mounts = ["--mount", f"type=bind,src={root},dst={root}"]
        for i, source in enumerate(m["sources"]):
            mounts += ["--mount", f"type=bind,src={source['path']},dst=/source-{i},readonly"]
        container = subprocess.check_output(["docker", "run", "--detach", "--user", f"{os.getuid()}:{os.getgid()}", "--network", "none", "--cpus", "2", "--memory", "2g", "--pids-limit", "128"] + mounts + ["--entrypoint", "/bin/sleep", image, "660"], text=True).strip()
        encoder = "/usr/lib/jellyfin-ffmpeg/ffmpeg"
        probe = "/usr/lib/jellyfin-ffmpeg/ffprobe"
        scorer = "/usr/local/lib/plurx/vmaf-ffmpeg"
        report["tools"] = {k: invoke([v, "-version"])[0].decode().splitlines()[0] for k, v in [("encoder", encoder), ("probe", probe), ("scorer", scorer)]}
        common = [encoder, "-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-threads", "1", "-filter_threads", "1", "-filter_complex_threads", "1"]
        colors = ["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]
        recipes = {"vbr": m["exports"][0]["modes"]["vbr"]["encoder_args"]}
        recipes.update({f"q{x['quality']}": x["modes"]["qvbr"]["encoder_args"] for x in m["exports"]})
        report["recipes"] = recipes
        for i, source in enumerate(m["sources"]):
            before = os.stat(source["path"])
            clip = root / "source.mkv"
            invoke(common + ["-ss", "120", "-i", f"/source-{i}", "-t", "60", "-map", "0:v:0", "-an", "-c", "copy", "-map_metadata", "-1", "-map_chapters", "-1", str(clip)])
            document = json.loads(invoke([probe, "-v", "error", "-select_streams", "v:0", "-show_streams", "-show_format", "-of", "json", str(clip)])[0])
            v = document["streams"][0]
            if any(v.get(k) != "bt709" for k in ["color_space", "color_transfer", "color_primaries"]) or v.get("color_range") != "tv" or v.get("sample_aspect_ratio") != "1:1" or v.get("pix_fmt") != "yuv420p" or v.get("field_order") != "progressive":
                raise ValueError("source metadata outside C2 contract")
            numerator, denominator = map(int, v["avg_frame_rate"].split("/")); fps = numerator / denominator
            frames = int(fps * 2)
            height = 720; width = int(v["width"] * height / v["height"]) // 2 * 2
            case = {"identity": source["identity"], "excerpt_sha256": sha(clip), "excerpt_bytes": clip.stat().st_size, "offset_seconds": 120,
                    "requested_excerpt_seconds": 60, "source_probe": {k: v.get(k) for k in ["codec_name", "width", "height", "pix_fmt", "color_range", "color_space", "color_transfer", "color_primaries", "field_order", "sample_aspect_ratio", "avg_frame_rate"]}, "windows": []}
            duration = float(document["format"]["duration"])
            ref, encoded, score = [root / x for x in ["reference.mkv", "encoded.mkv", "scores.json"]]
            def encode_score(reference, mode, count):
                _, elapsed = invoke(common + ["-i", str(reference), "-map", "0:v:0", "-an", "-sn", "-dn"] + recipes[mode] + ["-force_key_frames", "expr:gte(t,n_forced*2)"] + colors + ["-f", "matroska", str(encoded)])
                result = {"bytes": encoded.stat().st_size, "seconds": elapsed, "sha256": sha(encoded)}
                graph = "[0:v]setpts=PTS-STARTPTS[d];[1:v]setpts=PTS-STARTPTS[r];[d][r]libvmaf=model=version=vmaf_v0.6.1:n_threads=1:n_subsample=1:log_fmt=json:log_path=scores.json:shortest=1:repeatlast=0"
                invoke([scorer, "-nostdin", "-hide_banner", "-loglevel", "error", "-threads", "1", "-filter_complex_threads", "1", "-i", str(encoded), "-threads", "1", "-i", str(reference), "-lavfi", graph, "-an", "-f", "null", "-"])
                result.update(metrics(score, count)); encoded.unlink(); score.unlink()
                return result
            for n in range(3):
                start = (n + .5) * duration / 3 - 1
                invoke(common + ["-ss", f"{start:.9f}", "-i", str(clip), "-map", "0:v:0", "-an", "-vf", f"scale={width}:{height},fps={fps},format=yuv420p,setsar=1,setpts=PTS-STARTPTS", "-frames:v", str(frames), "-c:v", "ffv1", "-threads", "1"] + colors + [str(ref)])
                row = {"start_seconds": start, "frames": frames, "modes": {mode: encode_score(ref, mode, frames) for mode in recipes}}
                case["windows"].append(row); ref.unlink()
            candidate_results = {}
            for mode in ["q18", "q23", "q28"]:
                reasons = []
                for n, window in enumerate(case["windows"]):
                    base, current = window["modes"]["vbr"], window["modes"][mode]
                    if current["mean"] < 93: reasons.append(f"window_{n}:floor")
                    for metric in ["mean", "p10"]:
                        if current[metric] < base[metric]: reasons.append(f"window_{n}:{metric}")
                savings = 1 - sum(w["modes"][mode]["bytes"] for w in case["windows"]) / sum(w["modes"]["vbr"]["bytes"] for w in case["windows"])
                ratio = sum(w["modes"][mode]["seconds"] for w in case["windows"]) / sum(w["modes"]["vbr"]["seconds"] for w in case["windows"])
                if savings < .10: reasons.append("bytes")
                if ratio > 1.10: reasons.append("time")
                candidate_results[mode] = {"accepted": not reasons, "reasons": reasons, "byte_savings": savings, "encode_time_ratio": ratio}
            case["sample_decision"] = candidate_results
            # A deterministic 12s holdout disjoint from all three 2s samples.
            count = int(fps * 12)
            invoke(common + ["-ss", "17", "-i", str(clip), "-map", "0:v:0", "-an", "-vf", f"scale={width}:{height},fps={fps},format=yuv420p,setsar=1,setpts=PTS-STARTPTS", "-frames:v", str(count), "-c:v", "ffv1", "-threads", "1"] + colors + [str(ref)])
            accepted = [k for k, v in candidate_results.items() if v["accepted"]]
            chosen = max(accepted, key=lambda k: candidate_results[k]["byte_savings"]) if accepted else "vbr"
            case["chosen"] = chosen
            case["holdout"] = {"start_seconds": 17, "duration_seconds": 12, "modes": {"vbr": encode_score(ref, "vbr", count)}}
            if chosen != "vbr": case["holdout"]["modes"][chosen] = encode_score(ref, chosen, count)
            ref.unlink(); clip.unlink()
            after = os.stat(source["path"])
            if (before.st_size, before.st_mtime_ns, before.st_ino) != (after.st_size, after.st_mtime_ns, after.st_ino): raise RuntimeError("source changed")
            case["source_stat_stable"] = True
            report["cases"].append(case)
            Path(output).write_text(json.dumps(report, indent=2) + "\n")
        report["status"] = "measured"
    except Exception as e:
        report["status"] = "incomplete"
        report["error"] = type(e).__name__ + ": " + str(e)
    finally:
        if container:
            p = subprocess.run(["docker", "rm", "-fv", container], capture_output=True, timeout=15)
            report["cleanup"]["container_removed"] = p.returncode == 0
        shutil.rmtree(root)
        report["cleanup"]["private_media_removed"] = not root.exists()
        Path(output).write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--output", required=True)
    a = parser.parse_args()
    result = run(a.manifest, a.output)
    print(json.dumps({"status": result["status"], "cases": len(result["cases"]), "cleanup": result["cleanup"]}))
    raise SystemExit(0 if result["status"] == "measured" else 1)
