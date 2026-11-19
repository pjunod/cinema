"""Compare one admitted authored NP1 window with its retained served F+ baseline.

Run: python m7_pair_compare.py REQUEST.json REQUEST_SHA256 NEW_RESULT.json
Schema 1 request artifacts are absolute {path, sha256} references. See ARTIFACTS
for exact names. The decode receipt contract is specified in validate_candidate.
rate_proposal binds the guarded pre-encode policy and target used by the Rust
author resolver. encode_recipe binds the completed encoder output separately.
Only the new arm and paired error differences are scored. Retained F+ summaries
are the authority for the margin and tail gates. A result is fixture evidence.
"""

import argparse
from fractions import Fraction
import hashlib
import json
import re
from pathlib import Path

import served_refit_eval as baseline

np, fm = baseline.np, baseline.fm
require, sha = baseline.require, baseline.sha
HERE = Path(__file__).resolve().parent
PROTOCOL_SHA = "9fa874faf2ac48c928bf6dbadf86528b7922061179c041e1ea084a5b93db05f3"
ARTIFACTS = {
    "protocol", "baseline_request", "baseline_result", "run_request", "run_terminal", "run_landing",
    "rate_proposal", "encode_recipe", "encoder_log", "roi_map", "encoded", "authored", "author_request", "author_receipt", "author_protocol",
    "decode_receipt", "decoder", "decode_tool", "candidate_planes", "encoder_input_raw",
    "encoded_sps_trace", "authored_sps_trace", "source_rpu_trace", "authored_rpu_trace",
    "encoded_probe", "authored_probe",
}
CODED_FORMAT = {
    "pix_fmt": "yuv420p10le", "width": 3840, "height": 2160,
    "video_full_range_flag": 0, "colour_primaries": 9,
    "transfer_characteristics": 16, "matrix_coeffs": 9,
    "chroma_sample_loc_type_top_field": 2, "chroma_sample_loc_type_bottom_field": 2,
}
FRAME_BYTES = baseline.WIDTH * baseline.HEIGHT * 3


class EncoderNotAdmitted(ValueError):
    """Actual encoder artifacts do not establish the frozen NP1 arm."""


def encoder_check(condition, reason):
    if not condition:
        raise EncoderNotAdmitted(reason)


def read(path):
    return json.loads(path.read_text())


def bind(refs):
    require(set(refs) == ARTIFACTS, "comparison artifact inventory differs from schema 1")
    paths = {}
    for name, ref in refs.items():
        path = Path(ref["path"])
        require(path.is_absolute() and baseline.is_sha(ref["sha256"]), f"{name}: invalid reference")
        require(sha(path) == ref["sha256"], f"{name}: digest mismatch")
        paths[name] = path.resolve()
    return paths


def custody(actual, refs, links, label):
    for field, name in links.items():
        require(actual[field] == refs[name]["sha256"], f"{label}: invalid {field}")


def validate_publication(refs, protocol, extraction):
    """Tie the scored F+ readback to the original HTTP bytes and repaired copy."""
    media_names = ("init.mp4", "seg00000.m4s", "seg00001.m4s", "seg00002.m4s")
    digest = protocol["served_baseline"]["curve_digest"]

    def bound(label):
        ref = refs[label]
        path = Path(ref["path"])
        require(path.is_absolute() and baseline.is_sha(ref["sha256"]) and sha(path) == ref["sha256"],
                f"retained publication reference differs: {label}")
        return path

    witness = read(bound("publication_adoption"))
    original_score = bound("publication_original_score")
    require(witness["retained_baseline_sha256"] == sha(original_score)
            == protocol["served_baseline"]["calibration"][0]["result_sha256"]
            and witness["planned_segments"] == witness["materialized_segments"] == 3
            and witness["producer_failure"] is None and witness["scoring_repeated"] is False,
            "successor adoption witness differs")
    require(read(original_score)["artifacts"]["source"]["sha256"] == extraction["source_sha256"]
            == protocol["source"]["sha256"], "publication source differs")
    objects = witness["byte_identical_objects"]
    require(set(objects) == set(media_names) and objects["init.mp4"] == extraction["init_sha256"],
            "publication init or fragment inventory differs")

    for role in ("original", "successor"):
        prefix = f"publication_{role}_"
        response = read(bound(prefix + "response"))
        status = read(bound(prefix + "status"))
        require(status["id"] == response["session_id"]
                and response["delivered_dynamic_range"] == "dolby_vision"
                and response["delivered_dolby_vision_profile"] == 8
                and status["dolby_vision_refit"]["state"] == "fitted"
                and status["dv_refit_digest"] == digest
                and status["stream_format"]["video_action"] == "copy",
                f"{role} publication is not the frozen fitted-copy session")
        if role == "successor":
            require(status["materialized_segments"] == status["planned_segments"] == 3
                    and status["producer_failed"] is None, "successor did not finish")
        capture = read(bound(prefix + "capture"))["segments"]
        require([row["name"] for row in capture[:3]] == list(media_names[1:]),
                f"{role} HTTP segment order differs")
        require(len(capture) == (4 if role == "original" else 3), f"{role} HTTP inventory differs")
        if role == "original":
            require(capture[3]["name"] == "seg00003.m4s" and capture[3]["error"].startswith("502 "),
                    "original HTTP tail is not the archived failure")
        playlist = bound(prefix + "playlist").read_text().splitlines()
        require('#EXT-X-MAP:URI="init.mp4"' in playlist
                and [line for line in playlist if line and not line.startswith("#")] ==
                    [row["name"] for row in capture], f"{role} playlist differs from HTTP capture")
        whole = hashlib.sha256()
        length = 0
        for index, name in enumerate(media_names):
            path = bound(prefix + name)
            require(path.name == name and refs[prefix + name]["sha256"] == objects[name],
                    f"{role} publication bytes disagree with adoption witness: {name}")
            size = path.stat().st_size
            if index:
                entry = capture[index - 1]
                require(entry["sha256"] == objects[name] and entry["bytes"] == size,
                        f"{role} HTTP receipt differs for {name}")
            with path.open("rb") as stream:
                while chunk := stream.read(1024 * 1024):
                    whole.update(chunk)
            length += size
        require(length == extraction["artifacts"]["served"]["bytes"]
                and whole.hexdigest() == extraction["served_sha256"],
                f"{role} publication assembly differs from exact extraction input")
    return {"curve_digest": digest, "served_sha256": extraction["served_sha256"],
            "init_sha256": extraction["init_sha256"], "http_objects": objects,
            "scope": "Retained fitted-copy acquisition and byte-identical successor"}


def validate_baseline(paths, protocol, run_refs):
    request = read(paths["baseline_request"])
    result = read(paths["baseline_result"])
    eval_window = protocol["evaluation"]["window"]
    require(request["window"] == eval_window == result["window"], "baseline window mismatch")
    require(result["request_sha256"] == sha(paths["baseline_request"])
            and result["artifacts"] == request["artifacts"]
            and result["kind"] == "served_fplus_baseline"
            and result["source_time_base"] == "1/1000"
            and set(result["models"]) == set(fm.MODEL_IDS), "unbound retained baseline result")
    require(result["sampling"] == {"roi": baseline.ROI, "stride": 8, "axis_phases": [0, 1],
                                   "frames": baseline.FRAME_COUNT}, "baseline lattice differs")
    for name, digest in protocol["frozen_operators"].items():
        require(sha(HERE / name) == digest == result["scorers"][name], f"frozen operator differs: {name}")
    for name, digest in result["scorers"].items():
        require(Path(name).name == name and sha(HERE / name) == digest, f"baseline scorer differs: {name}")
    source_paths = baseline.bind_artifacts(request)
    refs = request["artifacts"]
    require(refs["source"]["sha256"] == protocol["source"]["sha256"], "source authority differs")
    events = baseline.read_jsonl(source_paths["events"])
    metadata = baseline.read_jsonl(source_paths["metadata"])
    window = baseline.parse_window(eval_window)
    ticks = baseline.validate_capture(events, metadata, source_paths, refs["source"]["sha256"], window)
    baseline.validate_master(read(source_paths["master_binding"]), source_paths, refs["events"]["sha256"])
    receipt = read(source_paths["served_receipt"])
    curves = baseline.bind_readback(read(source_paths["readback"]), receipt, refs, ticks,
                                   request.get("allow_extra_readback_frames", False), window)
    baseline.verify_extraction(receipt, source_paths, refs, events, curves)
    publication = validate_publication(run_refs, protocol, receipt)
    return result, source_paths, metadata, curves, publication


def validate_encoder(paths, refs, folder, chosen, recipe):
    """Confirm actual x265 settings, effective coded QG, and submitted ROI."""
    encoder_check(paths["encoder_log"] == folder / f"{chosen}-encode.log"
                  and paths["roi_map"] == folder / f"{chosen}-map.json"
                  and Path(recipe["argv"][3]).resolve() == paths["roi_map"]
                  and recipe["map_sha256"] == refs["roi_map"]["sha256"],
                  "encoder log or map does not belong to the selected attempt")
    roi = read(paths["roi_map"])
    encoder_check(roi == recipe["map"], "ROI map differs from the encoder recipe")
    expected = {"mode": "lumazfsp", "grid": 16, "threshold_nits": 1,
                "qp_offset": -6, "qp_range": 63, "flat": "kept", "span_octaves": 0.5,
                "encoded_frames": 48, "packets": 48}
    encoder_check(all(roi.get(key) == value for key, value in expected.items()),
                  "ROI map parameters or coverage differ")
    frames = roi["frames"]
    encoder_check(len(frames) == 48, "ROI map must cover all 48 frames")
    nonzero = 0
    for index, frame in enumerate(frames):
        counts = [frame[key] for key in ("selected_zero_offset_blocks", "selected_blocks", "blocks")]
        encoder_check(all(type(value) is int for value in counts)
                      and frame["frame"] == index and frame["zero_offset_blocks_coded_at"] == "balance"
                      and 0 <= counts[0] <= counts[1] <= counts[2] == (3840 // 16) * (2160 // 16),
                      f"ROI map frame {index} has inconsistent coverage")
        nonzero += counts[1] - counts[0]
    encoder_check(nonzero > 0, "ROI map contains no selected nonzero offsets")

    log = paths["encoder_log"].read_text()
    encoder_check(not re.search(r"\bskipping ROI\b", log, re.IGNORECASE),
                  "FFmpeg reported skipping ROI")
    aq = re.findall(r"AQ: mode / str / qg-size / cu-tree\s*:\s*(\d+)\s*/\s*([0-9.]+)\s*/\s*(\d+)\s*/\s*(\d+)", log)
    encoder_check(len(aq) == 1 and tuple(map(Fraction, aq[0])) == (2, 0, 32, 1),
                  f"x265 actual AQ tuple differs: {aq}")
    tool_lines = "\n".join(line for line in log.splitlines() if "tools:" in line)
    psycho = {}
    for key, want in (("psy-rd", 2), ("rdoq", 2), ("psy-rdoq", 1)):
        values = re.findall(rf"(?<![-\w]){key}=([0-9.]+)(?=\s|$)", tool_lines)
        encoder_check(len(values) == 1 and Fraction(values[0]) == want,
                      f"x265 actual {key} differs: {values}")
        psycho[key] = want

    trace = paths["encoded_sps_trace"].read_text()
    syntax = {}
    for key in ("log2_min_luma_coding_block_size_minus3", "log2_diff_max_min_luma_coding_block_size",
                "cu_qp_delta_enabled_flag", "diff_cu_qp_delta_depth"):
        observed = re.findall(rf"\b{key}\s+[01]+\s*=\s*(\d+)\s*$", trace, re.MULTILINE)
        encoder_check(observed and len(set(observed)) == 1, f"absent or conflicting SPS/PPS {key}")
        syntax[key] = int(observed[0])
    ctu = 3 + syntax["log2_min_luma_coding_block_size_minus3"] + syntax["log2_diff_max_min_luma_coding_block_size"]
    encoder_check(syntax["cu_qp_delta_enabled_flag"] == 1
                  and ctu - syntax["diff_cu_qp_delta_depth"] == 5,
                  "coded QP deltas are disabled or effective QG is not 32")
    return {"aq_mode_strength_qg_cutree": [2, 0, 32, 1], "psy": psycho,
            "effective_qp_group_size": 32, "sps_pps": syntax,
            "selected_nonzero_offset_blocks": nonzero, "roi_skip_warning": False,
            "scope": "Map construction and encoder admission do not prove each coded block used its requested offset."}


def validate_candidate(paths, refs, protocol, result, metadata):
    """Decode receipt schema 1 owns software decoding, trace parsing and AU audit.

    Required kind is m7-authored-decode. It binds all byte inputs via `hashes`
    (see custody call), reports `encoded_sps` and `authored_sps` as CODED_FORMAT
    fields parsed for all active SPS, and gives 48 ordered `frames`. Each frame
    includes the Rust author fields paired below, both VCL hashes, packet size,
    decoded PTS/time base and the decoded raw-frame SHA-256. The typed receipt
    is checked against trace bytes, the Rust author, and decoded frame bytes.
    """
    run = read(paths["run_request"])
    terminal = read(paths["run_terminal"])
    author = read(paths["author_receipt"])
    recipe = read(paths["encode_recipe"])
    decoded = read(paths["decode_receipt"])
    require(terminal["status"] == "predicted_landing"
            and terminal["request_sha256"] == refs["run_request"]["sha256"]
            and terminal["protocol_sha256"] == PROTOCOL_SHA, "no sealed predicted rate landing")
    require(terminal["landing_sha256"] == refs["run_landing"]["sha256"], "landing receipt differs")
    landing = read(paths["run_landing"])
    chosen = landing["chosen"]
    require(isinstance(chosen, str) and re.fullmatch(r"michael5400-m7-eval-np1-L[1-4]", chosen)
            and landing["landed"] is True, "unbound selected attempt")
    folder = Path(run["output_root"]) / "corrected-encodes"
    require(paths["encode_recipe"] == folder / f"{chosen}-recipe.json"
            and paths["encoded"] == folder / f"{chosen}-hdr.mp4", "candidate is not the selected attempt")
    matches = [row for row in landing["attempts"] if row["prefix"] == chosen and row.get("in_band") is True]
    require(len(matches) == 1 and type(matches[0]["b_kbps"]) is int, "selected rate entry differs")
    target = matches[0]["b_kbps"]
    attempt = int(chosen[-1])
    require(paths["rate_proposal"] == folder.parent / f"proposal-{attempt}.json",
            "rate proposal is not the selected attempt")
    proposal = read(paths["rate_proposal"])
    baseline.require_fields(proposal, {"target_kbps": target,
                                        "request_sha256": refs["run_request"]["sha256"]},
                            "selected rate proposal")
    require(proposal["driver_argv"][-4:] == ["--prefix", chosen, "--override", f"b={target}k"],
            "rate proposal argv differs from the selected attempt")
    require(83334 <= target <= 125000
            and recipe["overrides"] == ["x265-params+=cbqpoffs=-9:crqpoffs=-12:aq-strength=0", f"b={target}k"],
            "candidate differs from sealed NP1 override policy")
    for run_name, ref_name in (("protocol", "protocol"), ("served_baseline", "baseline_result")):
        require(run["artifacts"][run_name]["sha256"] == refs[ref_name]["sha256"],
                f"run binding differs: {run_name}")
    require(recipe["status"] == "encoded" and recipe["mode"] == "lumazfsp"
            and recipe["sha256"] == refs["encoded"]["sha256"]
            and recipe["protocol"]["sha256"] == PROTOCOL_SHA, "wrong encoded candidate")
    baseline.require_fields(author, {"schema_version": 1, "status": "validated_profile81_window",
                                       "protocol_sha256": refs["author_protocol"]["sha256"]}, "author receipt")
    runtime = read(paths["author_protocol"])
    require(runtime["comparison_protocol_sha256"] == PROTOCOL_SHA
            and runtime["recipe_sha256"] == refs["rate_proposal"]["sha256"]
            and runtime["input_representation_sha256"] == run["artifacts"]["input"]["sha256"]
            and runtime["author_plan_sha256"] == terminal["author_plan_sha256"],
            "author runtime protocol differs from the frozen comparison or guarded rate proposal")
    author_request = read(paths["author_request"])
    require(author_request["protocol"]["sha256"] == refs["author_protocol"]["sha256"]
            and author_request["encoded_mp4"]["sha256"] == refs["encoded"]["sha256"]
            and author_request["author_plan"]["sha256"] == terminal["author_plan_sha256"],
            "author request differs from its runtime protocol or candidate")
    custody(author, refs, {"request_sha256": "author_request", "encoded_mp4_sha256": "encoded",
                          "output_mp4_sha256": "authored"}, "author")
    require(author["author_plan_sha256"] == terminal["author_plan_sha256"]
            and author["original_source_census_sha256"] == protocol["source"]["presentation_census_sha256"]
            and author["original_source_rpus_sha256"] == result["artifacts"]["rpus"]["sha256"]
            and author["author_harness_source_sha256"] == run["artifacts"]["author_source"]["sha256"],
            "author source custody differs")
    baseline.require_fields(author["metadata"], {"profile": 8, "bl_signal_compatibility_id": 1,
                                                  "rpu_present": True, "bl_present": True,
                                                  "el_present": False}, "authored profile")
    baseline.require_fields(decoded, {"schema": 1, "kind": "m7-authored-decode",
                                       "software_only": True, "decoder_threads": 1}, "decode receipt")
    custody(decoded["hashes"], refs, {name: name for name in (
        "encoded", "authored", "author_receipt", "candidate_planes", "decoder", "decode_tool",
        "encoded_sps_trace", "authored_sps_trace")}, "decode")
    for label in ("encoded", "authored"):
        baseline.require_fields(decoded[f"{label}_sps"], CODED_FORMAT, f"{label} SPS")
        validate_trace(paths[f"{label}_sps_trace"])
    for name in ("source_rpu_trace", "authored_rpu_trace", "encoded_probe", "authored_probe"):
        require(decoded["artifacts"][name]["sha256"] == refs[name]["sha256"],
                f"decode receipt differs: {name}")
    encoded_packets = packet_probe(paths["encoded_probe"], paths["encoded"])
    authored_packets = packet_probe(paths["authored_probe"], paths["authored"])
    original_dm = rpu_color_trace(paths["source_rpu_trace"], 7)
    authored_dm = rpu_color_trace(paths["authored_rpu_trace"], 8)
    package_ref = run["artifacts"]["input_package"]
    package_path = Path(package_ref["path"])
    require(sha(package_path) == package_ref["sha256"], "input package differs from run binding")
    package = read(package_path)
    require(package["raw"]["sha256"] == refs["encoder_input_raw"]["sha256"]
            and len(package["frames"]) == 48, "wrong encoder input for grain proxy")
    author_frames, frames = author["frames"], decoded["frames"]
    require(len(author_frames) == len(frames) == len(metadata) == 48, "incomplete candidate frames")
    with paths["candidate_planes"].open("rb") as stream, paths["encoder_input_raw"].open("rb") as raw:
        for index, (row, signed, observed) in enumerate(zip(frames, author_frames, metadata, strict=True)):
            expected = {"source_ordinal": 192 + index, "source_pts": observed["pts_ticks"],
                        "source_rpu_sha256": observed["rpu_sha256"],
                        "encoded_pts": index * 1001, "encoded_dts": index * 1001,
                        "encoded_duration": 1001}
            packaged = package["frames"][index]
            require(packaged["ordinal"] == expected["source_ordinal"]
                    and packaged["source_pts_ticks"] == expected["source_pts"]
                    and packaged["source_rpu_sha256"] == expected["source_rpu_sha256"]
                    and packaged["source_duration_ticks"] == signed["source_duration"]
                    and Fraction(packaged["encoder_grid_pts_seconds"]) == Fraction(index * 1001, 24000)
                    and packaged["frame_sha256"] == signed["input_frame_sha256"],
                    "author frame differs from packaged original ordinal")
            input_bytes = raw.read(FRAME_BYTES)
            require(len(input_bytes) == FRAME_BYTES and hashlib.sha256(input_bytes).hexdigest()
                    == packaged["frame_sha256"], "encoder raw input frame hash differs")
            baseline.require_fields(signed, expected, "author frame")
            expected.update({key: signed[key] for key in ("authored_rpu_sha256", "encoded_base_payload_sha256",
                                                        "authored_payload_sha256")})
            baseline.require_fields(row, expected, "decoded frame")
            require(Fraction(row["decoded_pts"]) * Fraction(row["decoded_time_base"])
                    == Fraction(index * 1001, 24000), "decoded grid differs")
            require(baseline.is_sha(row["encoded_vcl_sha256"])
                    and row["encoded_vcl_sha256"] == row["authored_vcl_sha256"],
                    "coded picture changed during authoring")
            color = observed["color"]
            require(signed["vp_color"] == original_dm[index] == authored_dm[index] == {
                "ycc_to_rgb_q13": [n for n, _ in color["ycc_to_rgb_matrix"]],
                "ycc_to_rgb_offset_q28": [n for n, _ in color["ycc_to_rgb_offset"]],
                "rgb_to_lms_q14": [n for n, _ in color["rgb_to_lms_matrix"]],
            }, "authored same-frame DM colour differs")
            payload = stream.read(FRAME_BYTES)
            require(len(payload) == FRAME_BYTES and hashlib.sha256(payload).hexdigest()
                    == row["decoded_frame_sha256"], "decoded plane frame hash differs")
            require(encoded_packets[index][1] == signed["encoded_base_payload_sha256"]
                    and authored_packets[index] == (row["authored_packet_bytes"], signed["authored_payload_sha256"])
                    and type(row["authored_packet_bytes"]) is int, "raw packet proof differs")
        require(not stream.read(1) and not raw.read(1), "raw video has extra bytes")
    encoder = validate_encoder(paths, refs, folder, chosen, recipe)
    actual_rate = Fraction(sum(size for size, _ in authored_packets) * 8 * 24000,
                           48 * 1001 * 1_000_000)
    return frames, actual_rate, encoder


def packet_probe(path, media):
    report = read(path)
    streams = report["streams"]
    require(len(streams) == 1 and streams[0]["codec_type"] == "video",
            "packet probe must cover the sole video track")
    stream_info = streams[0]
    require(Fraction(stream_info["time_base"]) == Fraction(1, 24000), "probe time base differs")
    packets = report["packets"]
    require(len(packets) == 48, "probe packet coverage differs")
    observed = []
    filesize = media.stat().st_size
    previous_end = 0
    with media.open("rb") as stream:
        for index, packet in enumerate(packets):
            require(packet["stream_index"] == stream_info["index"]
                    and int(packet["pts"]) == int(packet["dts"]) == index * 1001
                    and int(packet["duration"]) == 1001, "raw packet grid differs")
            position, size = int(packet["pos"]), int(packet["size"])
            require(previous_end <= position and 0 < size <= 16 * 1024 * 1024
                    and position + size <= filesize, "invalid raw packet extent")
            stream.seek(position)
            data = stream.read(size)
            require(len(data) == size, "truncated raw packet")
            observed.append((size, hashlib.sha256(data).hexdigest()))
            previous_end = position + size
    return observed


def validate_trace(path):
    """Check the advertised raw fields within every reported SPS."""
    text = path.read_text()
    blocks = re.split(r"(?m)^.*?\] Sequence Parameter Set\s*$", text)[1:]
    require(blocks, "SPS trace contains no sequence parameter set")
    expected = {
        "chroma_format_idc": 1, "bit_depth_luma_minus8": 2, "bit_depth_chroma_minus8": 2,
        "pic_width_in_luma_samples": 3840, "pic_height_in_luma_samples": 2160,
        "conformance_window_flag": 0, "vui_parameters_present_flag": 1,
        "video_signal_type_present_flag": 1, "colour_description_present_flag": 1,
        "chroma_loc_info_present_flag": 1, "video_full_range_flag": 0, "colour_primaries": 9,
        "transfer_characteristics": 16, "matrix_coefficients": 9,
        "chroma_sample_loc_type_top_field": 2, "chroma_sample_loc_type_bottom_field": 2,
    }
    for index, block in enumerate(blocks):
        block = re.split(r"(?m)^.*?\] (?:Picture Parameter Set|Slice Segment Header|Packet:)", block)[0]
        for key, value in expected.items():
            hits = re.findall(rf"\b{key}\s+[01]+\s*=\s*(\d+)\s*$", block, flags=re.MULTILINE)
            require(hits == [str(value)], f"SPS trace differs: block {index}, {key}")


def rpu_color_trace(path, profile):
    payload = read(path)
    require(isinstance(payload, list) and len(payload) == 48, "RPU colour trace coverage differs")
    colors = []
    for frame in payload:
        require(frame["dovi_profile"] == profile, "RPU colour trace profile differs")
        if profile == 8:
            require(frame["header"]["disable_residual_flag"] is True, "authored RPU enables residual")
        dm = frame["vdr_dm_data"]
        colors.append({
            "ycc_to_rgb_q13": [dm[f"ycc_to_rgb_coef{i}"] for i in range(9)],
            "ycc_to_rgb_offset_q28": [dm[f"ycc_to_rgb_offset{i}"] for i in range(3)],
            "rgb_to_lms_q14": [dm[f"rgb_to_lms_coef{i}"] for i in range(9)],
        })
    return colors


def paired(values):
    return {**fm.vm.stats(values), "min": float(values.min()),
            "candidate_closer_count": int(np.count_nonzero(values > 0)),
            "candidate_worse_count": int(np.count_nonzero(values < 0)),
            "ties": int(np.count_nonzero(values == 0))}


def with_empty_counts(candidate, baseline_stats):
    require(candidate["full"]["count"] == baseline_stats["full"]["count"], "baseline sample count differs")
    for label, summary in candidate["bands"].items():
        require(summary["count"] == baseline_stats["bands"][label]["count"], "baseline mask count differs")


def score(source, metadata, curves, result, planes_path, frames, bands):
    count, w, h = baseline.FRAME_COUNT, baseline.WIDTH, baseline.HEIGHT
    bl = np.memmap(source["bl"], dtype="<u2", mode="r").reshape(count, -1)
    el = np.memmap(source["el"], dtype="<u2", mode="r").reshape(count, -1)
    candidate = np.memmap(planes_path, dtype="<u2", mode="r").reshape(count, -1)
    xs = baseline.lattice(baseline.ROI[0], baseline.ROI[2])
    ys = baseline.lattice(baseline.ROI[1], baseline.ROI[3])
    cx, cy = np.minimum((xs + 1) // 2, w // 2 - 1), np.minimum((ys + 1) // 2, h // 2 - 1)
    color = baseline.fixture_color(metadata)
    bounds = [hi for _, hi in bands if hi is not None]
    models = {}
    for model in fm.MODEL_IDS:
        retained = result["models"][model]
        require(len(retained["frames"]) == count, "baseline frame coverage differs")
        shape = (count, len(xs) * len(ys))
        errors, advantages = np.empty(shape), np.empty(shape)
        masks = np.empty(shape, dtype=np.uint8)
        rows = []
        for index, (curve, frame, old) in enumerate(zip(curves, frames, retained["frames"], strict=True)):
            require(curve["source_pts"] == old["source_pts"] == frame["source_pts"]
                    and curve["rpu_sha256"] == old["rpu_sha256"], "paired F+ frame binding differs")
            base, residual = baseline.sample_frame(bl, el, index, xs, ys)
            reference = fm.model_rgb(model, base, residual, *color)
            y, u, v = fm.planes(candidate, w, h, index)
            codes = np.stack((y[np.ix_(ys, xs)], u[np.ix_(cy, cx)], v[np.ix_(cy, cx)]), axis=-1).reshape(-1, 3)
            require(int(codes.max()) <= 1023, "decoded candidate codes exceed ten bits")
            rgb = fm.model_rgb(model, codes.astype(np.int64), None, *color)
            errors[index] = fm.vm.delta(reference, rgb)
            plus = fm.model_rgb(model, base, baseline.centered_reshape(curve, base), *color)
            advantages[index] = fm.vm.delta(reference, plus) - errors[index]
            luminance = 10000 * fm.eotf(reference.astype(np.float64) / 65535) @ fm.Y_WEIGHTS
            masks[index] = np.searchsorted(bounds, luminance, side="right")
            np1 = baseline.summarize(errors[index], masks[index], bands)
            retained_frame = old["arms"]["served_Fplus"]
            with_empty_counts(np1, retained_frame)
            rows.append({"source_ordinal": frame["source_ordinal"], "source_pts": frame["source_pts"],
                         "encoded_pts": frame["encoded_pts"], "encoded_time_base": "1/24000",
                         "served_rpu_sha256": curve["rpu_sha256"], "authored_rpu_sha256": frame["authored_rpu_sha256"],
                         "served_Fplus": retained_frame, "NP1": np1, "paired_Fplus_minus_NP1": paired(advantages[index])})
        np1 = baseline.summarize(errors, masks, bands)
        fplus = retained["pooled"]["served_Fplus"]
        with_empty_counts(np1, fplus)
        models[model] = {"frames": rows, "NP1": np1, "served_Fplus": fplus,
                         "paired_Fplus_minus_NP1": paired(advantages)}
    return models


def acceptance(models, minimum):
    report = {}
    shadow, dark = "[0,0.1)", "[0.1,1)"
    for model, entry in models.items():
        np1, plus = entry["NP1"], entry["served_Fplus"]
        gain = plus["full"]["mean"] - np1["full"]["mean"]
        checks = {"mean_gain_at_least_margin": gain >= minimum}
        for band, metrics in ((shadow, ("mean", "p99")), (dark, ("p99",))):
            for metric in metrics:
                left, right = np1["bands"][band], plus["bands"][band]
                checks[f"pooled_{band}_{metric}_nonincrease"] = (
                    left[metric] <= right[metric] if left["count"] and right["count"] else None)
        frame_shadow = {arm: [row[arm]["bands"][shadow] for row in entry["frames"]]
                        for arm in ("NP1", "served_Fplus")}
        worst = {arm: max((row["p99"] for row in rows if row["count"]), default=None)
                 for arm, rows in frame_shadow.items()}
        checks["largest_frame_shadow_p99_nonincrease"] = (
            worst["NP1"] <= worst["served_Fplus"] if all(
                all(row["count"] for row in rows) for rows in frame_shadow.values()) else None)
        report[model] = {"window_and_pooled_mean_gain": gain, "largest_frame_shadow_p99": worst,
                         "checks": checks}
    outcomes = [value for item in report.values() for value in item["checks"].values()]
    status = "refused" if False in outcomes else "inconclusive" if None in outcomes else "satisfied"
    return status, report


def grain(input_path, candidate_path):
    # Same report-only 16x16 input PQ luma mask and 3x3 energy as grain_ratio.py.
    w, h = baseline.WIDTH, baseline.HEIGHT
    inputs = [np.memmap(path, dtype="<u2", mode="r").reshape(48, -1)
              for path in (input_path, candidate_path)]
    rows, total_input, total_candidate = [], 0.0, 0.0
    for frame in range(48):
        source, decoded = (raw[frame, :w*h].reshape(h, w).astype(np.float64) for raw in inputs)
        block = source.reshape(h//16, 16, w//16, 16).mean(axis=(1, 3))
        nits = 10000 * fm.eotf(np.clip((block - 64) / 876, 0, 1))
        mask = ((nits >= 1) & (nits < 100)).repeat(16, axis=0).repeat(16, axis=1)
        energies = []
        for luma in (source, decoded):
            pad = np.pad(luma, 1, mode="edge")
            low = sum(pad[y:y+h, x:x+w] for y in range(3) for x in range(3)) / 9
            energies.append(float(np.square((luma - low)[mask]).sum()))
        a, b = energies
        total_input += a
        total_candidate += b
        rows.append({"source_ordinal": 192 + frame, "pixels": int(mask.sum()),
                     "decoded_over_input": b / a if a else None})
    return {"scope": "report-only high-pass luma energy, not an acceptance metric",
            "frames": rows, "pooled_ratio": total_candidate / total_input if total_input else None}


def evaluate(request, expected):
    require(sha(request) == expected, "comparison request SHA-256 differs")
    body = read(request)
    baseline.require_fields(body, {"schema": 1}, "comparison request")
    refs = body["artifacts"]
    paths = bind(refs)
    require(refs["protocol"]["sha256"] == PROTOCOL_SHA, "unexpected protocol identity")
    protocol = read(paths["protocol"])
    run_refs = read(paths["run_request"])["artifacts"]
    scorer_sha = sha(Path(__file__))
    require(run_refs["paired_scorer"]["sha256"] == scorer_sha, "paired scorer differs from pre-encode seal")
    require(run_refs["decode_collector"]["sha256"] == refs["decode_tool"]["sha256"],
            "decoder collector differs from pre-encode seal")
    result, source, metadata, curves, publication = validate_baseline(paths, protocol, run_refs)
    frames, rate, encoder = validate_candidate(paths, refs, protocol, result, metadata)
    output = {"schema": 1, "kind": "m7_np1_served_fplus_pair", "request_sha256": expected,
              "artifacts": refs, "scorer_sha256": sha(Path(__file__)), "window": result["window"],
              "sampling": result["sampling"], "served_publication": publication, "encoder_admission": encoder,
              "authored_video_packet_mbps": float(rate),
              "authored_video_packet_mbps_exact": str(rate),
              "scope": "offline single-fixture experimental comparison"}
    if not Fraction("128.5") <= rate <= Fraction("130.0"):
        return {**output, "status": "inconclusive", "reason": "actual authored rate is outside the frozen band"}
    models = score(source, metadata, curves, result, paths["candidate_planes"], frames,
                   protocol["evaluation"]["reference_luminance_bands_nits"])
    status, gates = acceptance(models, protocol["acceptance"]["minimum_mean_delta_itp_gain"])
    return {**output, "status": status, "gates": gates, "models": models,
            "grain_proxy": grain(paths["encoder_input_raw"], paths["candidate_planes"])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("request", type=Path)
    parser.add_argument("request_sha256")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    require(not args.output.exists(), "output already exists")
    try:
        result = evaluate(args.request, args.request_sha256)
    except EncoderNotAdmitted as error:
        result = {"schema": 1, "status": "inconclusive", "request_sha256": args.request_sha256,
                  "reason": str(error), "stage": "encoder_admission"}
    except (ValueError, KeyError, TypeError, OSError) as error:
        result = {"schema": 1, "status": "refused", "request_sha256": args.request_sha256,
                  "reason": str(error)}
    with args.output.open("x") as output:
        json.dump(result, output, indent=2, allow_nan=False)
        output.write("\n")
    return 0 if result["status"] == "satisfied" else 1


if __name__ == "__main__":
    raise SystemExit(main())
