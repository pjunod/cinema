"""Exact synthetic Profile 8 reuse evidence checks, not general DV validation."""
import hashlib
import json
from pathlib import Path
import sys

PTS = [0, 40, 110, 140, 230, 300]
DURATION = [40, 70, 30, 90, 70, 41]
CODED = [0, 2, 1, 3, 5, 4]
FRAME_BYTES = 64 * 64 * 3
INVALID_DATA = -1094995529
EXPECTED_INPUTS = {'bl-vfr-b2.mkv': '6af152b438835b23205313ca3c9e7e55d4af3de5ac686af20fd0eb09181f4df2', 'bl.yuv420p10le': '286c629dc58dd3a1cc6c0605f58389f4356346aaa80bf519d90fe7f0fe6d0113', 'ffmpeg-source.tar.gz': 'fb1931fd4eb29297ee1c1017a24f800c4d8fbea35b4f2aaeb28308a48a9149b4', 'dovi-source.tar.gz': '57d13f03d04a7a1b3d7dcb139b976dc3748c745c1720d4b6e44b61e5ef67b69b'}


def refuse(reason):
    raise ValueError(reason)


def reject_number(value):
    refuse("noninteger/nonfinite numeric evidence")


def load(path):
    return json.loads(Path(path).read_text(), parse_float=reject_number,
                      parse_constant=reject_number)


def digest(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        refuse("regular artifact required")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def exact(actual, expected, label):
    # JSON spelling preserves bool/int distinctions that Python equality loses.
    if json.dumps(actual, sort_keys=True, allow_nan=False) != json.dumps(expected, sort_keys=True, allow_nan=False):
        refuse(label)


def lines(path):
    return [json.loads(line, parse_float=reject_number, parse_constant=reject_number)
            for line in Path(path).read_text().splitlines()]


def fraction(value):
    return f"{value}/1000"


def curve(coefficients):
    return {"pivots": [0, 1023], "pieces": 1, "method": 0, "order": 1,
            "coefficients": coefficients}


def metadata(picture, epoch, emission, missing=False):
    actual_tag = 0 if missing else picture
    coefficients = [0, 8388608, 0] if picture < 3 else [524288, 6291456, 0]
    return {"kind": "resolved_metadata", "epoch": epoch, "display_emission": emission,
            "source_min_pq": actual_tag, "denom": 23, "mapping_id": 0,
            "curves": [curve(coefficients), curve([0, 8388608, 0]), curve([0, 8388608, 0])]}


def decoded(picture, epoch, emission, missing=False):
    tag = 0 if missing else picture
    return {"kind": "decoded_frame", "layer": "bl", "epoch": epoch, "context_id": 0,
            "display_emission": emission, "pts": fraction(PTS[picture]),
            "best_effort_pts": fraction(PTS[picture]),
            "pkt_dts": fraction([0, 40, 40, 140][picture]) if picture < 4 else None,
            "duration": fraction(DURATION[picture]), "picture_type": "IBPIBP"[picture],
            "decoder_rpu_present": not missing, "decoder_metadata_present": True,
            "l1_min": 0, "l1_max": 4095 - tag, "l1_average": 2048 - tag}


def timing(phase):
    return {"kind": "stream_timing_context", "phase": phase, "time_base": "1/1000",
            "r_frame_rate": "25/1", "avg_frame_rate": "25/1", "codec_framerate": "24/1"}


def expected_events(mode):
    events = [
        {"kind": "environment", "ffmpeg_version": "9.0.1", "libavcodec_version": 4129125},
        {"kind": "muxed_configuration", "profile": 8, "compression": 1,
         "compatibility": 6, "rpu": 1, "bl": 1, "el": 0},
        timing("initial_open"),
        {"kind": "epoch_start", "epoch": 0, "context_id": 0, "fresh_contexts": True},
    ]

    def packet(arrival, picture, epoch, dts):
        row = {"kind": "demux_packet", "epoch": epoch, "coded_arrival": arrival,
               "pts": fraction(PTS[picture]), "dts": fraction(dts) if dts is not None else None,
               "duration": fraction(DURATION[picture]), "keyframe": picture in (0, 3)}
        events.append(row)
        split = {key: value for key, value in row.items() if key != "keyframe"}
        split.update(kind="split_packet", layer="bl")
        events.append(split)

    def frame(picture, epoch, emission):
        missing = mode == "missing-rpu" and picture == 2
        events.extend([metadata(picture, epoch, emission, missing), decoded(picture, epoch, emission, missing)])

    if mode == "seek-reset":
        packet(0, 0, 0, None)
        packet(1, 2, 0, None)
        events.extend([
            {"kind": "read_boundary", "epoch": 0, "packets": 2, "eof": False},
            timing("after_seek"),
            {"kind": "epoch_boundary", "from": 0, "to": 1, "reason": "actual_demux_seek",
             "seek_result": 0, "target": "200/1000", "reset_applied": True,
             "bsf_flush_calls": 1, "decoder_flush_calls": 1},
        ])
        packet(0, 3, 1, None)
        packet(1, 5, 1, None)
        packet(2, 4, 1, 140)
        frame(3, 1, 0)
        events.append({"kind": "read_boundary", "epoch": 1, "packets": 3, "eof": True})
        frame(4, 1, 1)
        frame(5, 1, 2)
        events.append({"kind": "drain", "epoch": 1, "bl_frames": 3})
    else:
        for arrival, picture in enumerate(CODED):
            packet(arrival, picture, 0, [None, None, 0, 40, 110, 140][arrival])
            if arrival == 2:
                frame(0, 0, 0)
            elif arrival == 3:
                frame(1, 0, 1)
                frame(2, 0, 2)
            elif arrival == 5:
                frame(3, 0, 3)
        events.append({"kind": "read_boundary", "epoch": 0, "packets": 6, "eof": True})
        frame(4, 0, 4)
        frame(5, 0, 5)
        events.append({"kind": "drain", "epoch": 0, "bl_frames": 6})
    return events


def inspect_execution(root, stage, mode):
    root = Path(root)
    if stage == "decode":
        path = root / mode / "execution.json"
        input_name = "missing-rpu.mkv" if mode == "missing-rpu" else "normal.mkv"
        argv = ["decode_reuse", input_name, mode, "seek-reset" if mode == "seek-reset" else "normal"]
        consumed_names = {"decode_reuse", input_name}
        epoch = 1 if mode == "seek-reset" else 0
        count = 3 if epoch else 6
        output_names = {f"{mode}/events.jsonl", f"{mode}/decoder.stderr", f"{mode}/decode-status.txt"}
        for emission in range(count):
            stem = f"{mode}/epoch-{epoch}-bl-frame-{emission:03d}"
            output_names.add(stem + ".yuv420p10le")
            if not (mode == "missing-rpu" and emission == 2):
                output_names.add(stem + ".rpu.nal")
    else:
        path = root / f"cache-{mode}.execution.json"
        seed = "normal/epoch-0-bl-frame-000.rpu.rbsp"
        reuse = "normal/epoch-0-bl-frame-001.rpu.rbsp"
        argv = ["cache_probe", "normal.mkv", seed, reuse, mode]
        consumed_names = {"cache_probe", "normal.mkv", seed, reuse}
        output_names = {f"cache-{mode}.jsonl", f"cache-{mode}.stderr", f"cache-{mode}.status"}
    record = load(path)
    exact(sorted(record), sorted({"schema", "stage", "mode", "argv", "process_returncode", "shell_status",
                        "consumed_before", "consumed_after", "outputs"}), "execution receipt fields")
    for key, expected in (("schema", 1), ("stage", stage), ("mode", mode), ("argv", argv),
                          ("process_returncode", 0), ("shell_status", 0)):
        exact(record[key], expected, "execution invocation/status mismatch")
    exact(sorted(record["consumed_before"]), sorted(consumed_names), "execution input/tool coverage mismatch")
    exact(record["consumed_before"], record["consumed_after"], "consumed bytes changed during execution")
    exact(sorted(record["outputs"]), sorted(output_names), "execution output coverage mismatch")
    for binding in (record["consumed_before"], record["outputs"]):
        for name, checksum in binding.items():
            if digest(root / name) != checksum:
                refuse("execution input/tool/output identity changed")
    return digest(path)


def inspect_decoded(root, mode):
    root = Path(root)
    directory = root / mode
    if (directory / "decode-status.txt").read_text() != "0\n":
        refuse("decode process did not exit cleanly")
    execution_sha = inspect_execution(root, "decode", mode)
    exact(lines(directory / "events.jsonl"), expected_events(mode), "ordered decode lifecycle/typed metadata mismatch")
    if (directory / "decoder.stderr").read_text():
        refuse("unexpected decoder diagnostic")
    source = (root / "bl.yuv420p10le").read_bytes()
    if len(source) != FRAME_BYTES * 6:
        refuse("bounded independent source size")
    epoch = 1 if mode == "seek-reset" else 0
    pictures = range(3, 6) if epoch else range(6)
    rows = []
    for emission, picture in enumerate(pictures):
        stem = directory / f"epoch-{epoch}-bl-frame-{emission:03d}"
        native = stem.with_suffix(".yuv420p10le")
        if native.read_bytes() != source[picture * FRAME_BYTES:(picture + 1) * FRAME_BYTES]:
            refuse("native decoded picture mismatch")
        if mode == "missing-rpu" and picture == 2:
            if stem.with_suffix(".rpu.nal").exists():
                refuse("omitted raw RPU unexpectedly exists")
            continue
        raw = stem.with_suffix(".rpu.nal")
        emitted = root / f"normal.mkv.picture-{picture}.nal"
        if raw.read_bytes() != emitted.read_bytes():
            refuse("actual emitted/decoder raw RPU mismatch")
        parsed_path = stem.with_suffix(".rpu.json")
        parsed = load(parsed_path)
        header = parsed["header"]
        exact({key: header[key] for key in (
            "reserved_zero_3bits", "use_prev_vdr_rpu_flag", "prev_vdr_rpu_id",
            "disable_residual_flag", "vdr_seq_info_present_flag")},
            {"reserved_zero_3bits": 0, "use_prev_vdr_rpu_flag": picture not in (0, 3),
             "prev_vdr_rpu_id": 0, "disable_residual_flag": True, "vdr_seq_info_present_flag": True},
            "actual compressed header syntax mismatch")
        exact(parsed["vdr_dm_data"]["source_min_pq"], picture, "fresh static DM tag mismatch")
        exact(parsed.get("rpu_data_mapping") is None, picture not in (0, 3), "full/reused mapping presence mismatch")
        # Bind the resolved frame to the same-epoch key's actual full seed, including seek preroll.
        seed_picture = 0 if picture < 3 else 3
        rows.append({"picture": picture, "epoch": epoch, "pts": fraction(PTS[picture]),
                     "duration": fraction(DURATION[picture]), "reuse": picture not in (0, 3),
                     "seed_picture": seed_picture, "seed_sha256": digest(root / f"normal.mkv.picture-{seed_picture}.nal"),
                     "native_sha256": digest(native), "raw_rpu_sha256": digest(raw),
                     "parsed_rpu_sha256": digest(parsed_path), "rbsp_sha256": digest(stem.with_suffix(".rpu.rbsp")),
                     "events_sha256": digest(directory / "events.jsonl"),
                     "preroll": mode == "seek-reset" and picture == 3})
    return {"mode": mode, "accepted": mode != "missing-rpu",
            "execution_sha256": execution_sha,
            "reason": "actual raw RPU missing; cached metadata is not explicit reuse" if mode == "missing-rpu" else "explicit reuse resolved from same-epoch full seed",
            "frames": rows, "container_sha256": digest(root / ("missing-rpu.mkv" if mode == "missing-rpu" else "normal.mkv"))}


def cache_state(phase, result=0, profile=8, compression=1, present=False, tag=-1):
    return {"kind": "cache_state", "phase": phase, "result": result,
            "profile": profile, "compression": compression, "mapping_present": present,
            "cache_zero_present": present, "color_present": present,
            "luma": [0, 8388608] if present else [-1, -1], "source_min_pq": tag}


def inspect_cache(root, mode):
    root = Path(root)
    if (root / f"cache-{mode}.status").read_text() != "0\n":
        refuse("cache process did not exit cleanly")
    expected = [cache_state("initial")]
    if mode != "cold":
        expected.append(cache_state("full_seed", present=True, tag=0))
    diagnostic = ""
    if mode == "flush":
        expected.append(cache_state("after_flush_before_parse"))
    elif mode == "wrong-compression":
        expected.append(cache_state("wrong_compression_before_parse", compression=0, present=True, tag=0))
    elif mode == "no-reset":
        expected.append(cache_state("declared_new_epoch_without_reset", present=True, tag=0))
    if mode in ("cold", "flush"):
        expected.append(cache_state("reuse_parse", INVALID_DATA, profile=0, compression=0))
        diagnostic = "Unknown previous RPU ID: 0\n"
    elif mode == "wrong-compression":
        expected.append(cache_state("reuse_parse", INVALID_DATA, compression=0, present=True, tag=0))
        diagnostic = "Uncompressed RPUs should not have use_prev_vdr_rpu=1\n"
    else:
        expected.append(cache_state("reuse_parse", present=True, tag=1))
    execution_sha = inspect_execution(root, "cache", mode)
    exact(lines(root / f"cache-{mode}.jsonl"), expected, "ordered typed cache state/result mismatch")
    if (root / f"cache-{mode}.stderr").read_text() != diagnostic:
        refuse("cache refusal diagnostic mismatch")
    return {"mode": mode, "parser_result": expected[-1]["result"],
            "epoch_acceptance": mode == "same", "execution_sha256": execution_sha, "events_sha256": digest(root / f"cache-{mode}.jsonl"),
            "diagnostic_sha256": digest(root / f"cache-{mode}.stderr")}


def inspect(root):
    root = Path(root)
    lock = load(root / "input-lock.json")
    exact(lock, EXPECTED_INPUTS, "runtime input lock differs from source-bound identities")
    for name, checksum in EXPECTED_INPUTS.items():
        if digest(root / name) != checksum:
            refuse("independent input identity changed")
    provenance = load(root / "parsed-provenance.json")
    expected_keys = {str(raw.relative_to(root)) for mode in ("normal", "missing-rpu", "seek-reset")
                     for raw in (root / mode).glob("*.rpu.nal")} | {"inspector"}
    if set(provenance) != expected_keys:
        refuse("parsed provenance coverage mismatch")
    for raw, binding in provenance.items():
        expected_files = ({"dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/target/release/examples/m0_inspect_reuse"}
                          if raw == "inspector" else {raw, str(Path(raw).with_suffix(".json")), str(Path(raw).with_suffix(".rbsp"))})
        if set(binding) != expected_files:
            refuse("parsed provenance file coverage mismatch")
        for name, checksum in binding.items():
            if digest(root / name) != checksum:
                refuse("parsed output/raw input provenance changed")
    decoded_rows = [inspect_decoded(root, mode) for mode in ("normal", "seek-reset", "missing-rpu")]
    cache_rows = [inspect_cache(root, mode) for mode in ("same", "cold", "flush", "wrong-compression", "no-reset")]
    if (root / "p7-refusal.status").read_text() != "1\n" or (root / "p7-refusal.mkv").exists():
        refuse("P7 compression must refuse before output")
    exact(lines(root / "p7-refusal.jsonl"), [{"kind": "compressor_init", "status": -22, "requested_profile": 7}], "P7 exact failure stage")
    diagnostic = (root / "p7-refusal.stderr").read_text()
    if "Invalid compression level 1 for Dolby Vision profile 7." not in diagnostic or not diagnostic.endswith("Controlled mux refusal: compression initialization\n"):
        refuse("P7 exact refusal diagnostic")
    return {"schema": 1, "scope": "synthetic Profile 8 limited mapping reuse; no P7/FEL/conformance qualification",
            "decoded": decoded_rows, "cache": cache_rows, "p7_compression": "controlled refusal"}


if __name__ == "__main__":
    print(json.dumps(inspect(Path(sys.argv[1])), indent=2, allow_nan=False))
