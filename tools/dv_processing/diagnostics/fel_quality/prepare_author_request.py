#!/usr/bin/env python3
"""Prepare an exact-clock JSON request; ffprobe demux only, no normalization.

RPU list JSON is an ordered array of {"path": ABSOLUTE_NAL_PATH, "sha256": HEX}.
Its hashes should come from the retained reference trace/binding manifest.
The source DV level remains an explicit caller-supplied source fact.
"""
import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def sha(data):
    return hashlib.sha256(data).hexdigest()


def bound_path(text):
    path = Path(text)
    if not path.is_absolute():
        raise ValueError("all media, RPU, request and output paths must be absolute")
    return path


def prepare(args):
    source = bound_path(args.input_mp4)
    output = bound_path(args.output_mp4)
    request_path = bound_path(args.request_json)
    evidence_path = request_path.with_suffix(".ffprobe.json")
    for p in (output, request_path, evidence_path):
        if p.exists():
            raise ValueError("refusing to replace existing output: " + str(p))
    encoded_hash = sha(source.read_bytes())
    if encoded_hash != args.expected_input_sha256.lower():
        raise ValueError("input MP4 SHA256 differs from expected")
    rpu_manifest = bound_path(args.rpu_list)
    rpu_manifest_bytes = rpu_manifest.read_bytes()
    rpus = json.loads(rpu_manifest_bytes)
    if not isinstance(rpus, list) or not 1 <= len(rpus) <= 64:
        raise ValueError("RPU manifest must contain 1..64 ordered entries")
    for item in rpus:
        if not isinstance(item,dict) or set(item) != {"path", "sha256"}:
            raise ValueError("each RPU entry needs exactly path and sha256")
        if not isinstance(item["path"],str) or not isinstance(item["sha256"],str):
            raise ValueError("RPU path and SHA256 must be strings")
        p = bound_path(item["path"])
        if sha(p.read_bytes()) != item["sha256"].lower():
            raise ValueError("RPU file SHA256 differs from manifest: " + str(p))
    ffprobe = shutil.which(args.ffprobe)
    if not ffprobe:
        raise ValueError("ffprobe not found")
    command = [ffprobe, "-v", "error", "-select_streams", "v:0", "-show_streams",
               "-show_packets", "-show_entries",
               "stream=index,codec_name,codec_tag_string,width,height,time_base,r_frame_rate,avg_frame_rate,color_range,color_space,color_transfer,color_primaries:packet=stream_index,pts,dts,duration,flags,size",
               "-of", "json", str(source)]
    completed = subprocess.run(command, capture_output=True, check=True, timeout=60)
    probe = json.loads(completed.stdout)
    streams, packets = probe.get("streams", []), probe.get("packets", [])
    if len(streams) != 1 or len(packets) != len(rpus):
        raise ValueError("expected one selected video stream with exactly one packet per RPU")
    stream = streams[0]
    if stream["codec_name"] != "hevc":
        raise ValueError("expected already encoded HEVC MP4")
    time_base = Fraction(stream["time_base"])
    if time_base.numerator != 1 or not 0 < time_base.denominator <= 2**32-1:
        raise ValueError("expected native MP4 track clock 1/timescale within u32")
    timescale = time_base.denominator
    first = int(packets[0]["pts"])
    duration = int(packets[0]["duration"])
    if not 0 <= first <= 2**64-1 or not 0 < duration <= 2**32-1:
        raise ValueError("start/frame_ticks outside production author's unsigned clock")
    for i, packet in enumerate(packets):
        expected = first + i * duration
        if int(packet["pts"]) != expected or int(packet["dts"]) != expected:
            raise ValueError("nonconsecutive or reordered packet clock; no normalization allowed")
        if int(packet["duration"]) != duration:
            raise ValueError("variable or missing packet duration; no normalization allowed")
    if first + len(packets) * duration > 2**64-1:
        raise ValueError("window end overflows u64")
    raster = [int(stream["width"]), int(stream["height"])]
    for key, supplied, actual in (("timescale",args.timescale,timescale),
                                 ("start",args.start,first),
                                 ("frame_ticks",args.frame_ticks,duration),
                                 ("raster",args.raster,raster)):
        if supplied is not None and supplied != actual:
            raise ValueError(key + " differs from the actual encoded track")
    version = subprocess.run([ffprobe,"-version"],capture_output=True,text=True,
                             check=True,timeout=10).stdout.splitlines()[0]
    # Recheck after probing; the Rust adapter also hashes the bytes it reads.
    if sha(source.read_bytes()) != encoded_hash:
        raise ValueError("input changed during ffprobe")
    evidence = {"schema_version":1,"scope":"ffprobe demux/packet timing only; no media mutation",
                "ffprobe_version":version,"command":command,
                "input_mp4":str(source),"input_sha256":encoded_hash,
                "ordered_rpu_manifest_path":str(rpu_manifest),
                "ordered_rpu_manifest_sha256":sha(rpu_manifest_bytes),
                "derived":{"raster":raster,"timescale":timescale,"start":first,
                           "frame_ticks":duration,"frames":len(packets),
                           "frame_interval_seconds":str(Fraction(duration,timescale))},
                "ffprobe":probe}
    evidence_bytes = (json.dumps(evidence,indent=2)+"\n").encode()
    request = {"schema_version":1,"input_mp4":str(source),"input_sha256":encoded_hash,
               "output_mp4":str(output),"raster":raster,"timescale":timescale,
               "start":first,"frame_ticks":duration,"ordered_rpus":rpus,
               "source_level":args.source_level,
               "grid_evidence":{"path":str(evidence_path),"sha256":sha(evidence_bytes)}}
    with evidence_path.open("xb") as f:
        f.write(evidence_bytes)
    with request_path.open("x") as f:
        json.dump(request,f,indent=2)
        f.write("\n")
    return {"status":"request_prepared","request_json":str(request_path),
            "request_sha256":sha(request_path.read_bytes()),"derived":evidence["derived"]}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-mp4",required=True)
    parser.add_argument("--expected-input-sha256",required=True)
    parser.add_argument("--output-mp4",required=True)
    parser.add_argument("--rpu-list",required=True)
    parser.add_argument("--source-level",type=int,required=True,choices=range(1,10))
    parser.add_argument("--request-json",required=True)
    parser.add_argument("--ffprobe",default="ffprobe")
    parser.add_argument("--timescale",type=int)
    parser.add_argument("--start",type=int)
    parser.add_argument("--frame-ticks",type=int)
    parser.add_argument("--raster",type=int,nargs=2)
    try:
        print(json.dumps(prepare(parser.parse_args()),indent=2))
    except (ValueError,KeyError,OSError,subprocess.SubprocessError) as error:
        print(json.dumps({"status":"request_refused","reason":str(error)},indent=2))
        raise SystemExit(3)


if __name__ == "__main__":
    main()
