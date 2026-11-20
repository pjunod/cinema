#!/usr/bin/env python3
"""Import a sealed offline M7 experiment without executing any media tools."""

import argparse
import hashlib
import json
import math
import os
import tempfile
from fractions import Fraction
from pathlib import Path

MODELS = ("A", "B", "C", "D")
MAX_JSON_BYTES = 4 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def json_bytes(path):
    require(path.stat().st_size <= MAX_JSON_BYTES, "oversize experiment evidence JSON")
    raw = path.read_bytes()
    return raw, json.loads(raw)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def verify_digest(value):
    require(isinstance(value, str) and len(value) == 64
            and all(c in "0123456789abcdef" for c in value), "invalid SHA-256")
    return value


class Evidence:
    def __init__(self, roots):
        self.roots = tuple(root.resolve(strict=True) for root in roots)
        self.seen = {}
        self.manifest = {}

    def file(self, path, identity, role):
        expected = verify_digest(identity)
        path = Path(path).resolve(strict=True)
        require(any(path.is_relative_to(root) for root in self.roots),
                f"{role}: evidence is outside supplied roots")
        with path.open("rb") as stream:
            before = os.fstat(stream.fileno())
            key = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
            if key not in self.seen:
                self.seen[key] = hashlib.file_digest(stream, "sha256").hexdigest()
            after = os.fstat(stream.fileno())
        require((before.st_size, before.st_mtime_ns, before.st_ctime_ns)
                == (after.st_size, after.st_mtime_ns, after.st_ctime_ns)
                and self.seen[key] == expected, f"{role}: evidence hash changed")
        self.manifest[role] = {"sha256": expected, "bytes": before.st_size}
        return path

    def items(self, bindings, scope):
        require(isinstance(bindings, dict) and 0 < len(bindings) <= 128,
                f"{scope}: invalid artifact manifest")
        return {role: self.file(ref["path"], ref["sha256"], f"{scope}.{role}")
                for role, ref in bindings.items()}


def scores_match(report, margin):
    require(set(report["models"]) == set(MODELS) == set(report["gates"]),
            "incomplete four-model report")
    outcomes = []
    for name in MODELS:
        model = report["models"][name]
        gate = report["gates"][name]
        served, candidate = model["served_Fplus"], model["NP1"]
        mean_gain = served["full"]["mean"] - candidate["full"]["mean"]
        require(math.isfinite(mean_gain) and math.isclose(
            mean_gain, gate["window_and_pooled_mean_gain"], rel_tol=1e-12, abs_tol=1e-14,
        ), "mean gate disagrees with report")
        shadow, dark = "[0,0.1)", "[0.1,1)"
        worst = gate["largest_frame_shadow_p99"]
        checks = {
            "mean_gain_at_least_margin": mean_gain >= margin,
            "pooled_[0,0.1)_mean_nonincrease":
                candidate["bands"][shadow]["mean"] <= served["bands"][shadow]["mean"],
            "pooled_[0,0.1)_p99_nonincrease":
                candidate["bands"][shadow]["p99"] <= served["bands"][shadow]["p99"],
            "pooled_[0.1,1)_p99_nonincrease":
                candidate["bands"][dark]["p99"] <= served["bands"][dark]["p99"],
            "largest_frame_shadow_p99_nonincrease": worst["NP1"] <= worst["served_Fplus"],
        }
        require(checks == gate["checks"], "model gate does not reproduce")
        outcomes.extend(checks.values())
    require(not all(outcomes) and report["status"] == "refused",
            "this import requires a measured terminal rejection")


def windows_match(protocol, report, calibration):
    evaluation = protocol["evaluation"]
    require(evaluation["window"] == report["window"]
            and evaluation["frames"] == report["sampling"]["frames"],
            "evaluation window changed after freeze")
    planned = protocol["served_baseline"]["calibration"]
    require(0 < len(planned) == len(calibration) <= 8, "incomplete calibration receipts")
    spans = []
    for window in [*planned, evaluation]:
        first, last = window["ordinals"]
        start, end = map(Fraction, window["window"])
        require(0 <= first <= last and start < end, "invalid frozen window")
        spans.append((first, last, start, end))
    spans.sort()
    require(all(x[1] < y[0] and x[3] <= y[2] for x, y in zip(spans, spans[1:])),
            "calibration overlaps evaluation")
    return planned


def import_experiment(args):
    seal_raw, seal = json_bytes(args.seal)
    report_raw, report = json_bytes(args.report)
    request_raw, request = json_bytes(args.request)
    evidence = Evidence(args.artifact_root)
    require(report["kind"] == "m7_np1_served_fplus_pair" and report["schema"] == 1
            and report["request_sha256"] == sha256(request_raw)
            and report["artifacts"] == request["artifacts"], "report/request mismatch")
    artifacts = evidence.items(report["artifacts"], "comparison")
    protocol_raw, protocol = json_bytes(artifacts["protocol"])
    protocol_sha = verify_digest(seal["sha256"])
    require(protocol_sha == sha256(protocol_raw) and protocol["schema"] == 1
            and protocol["status"] == "frozen-before-candidate"
            and seal["state"].startswith("Frozen before candidate"),
            "the protocol freeze does not bind the scored experiment")
    require(artifacts["protocol"] == Path(seal["protocol_path"]).resolve(strict=True),
            "seal names another protocol")
    source = verify_digest(protocol["source"]["sha256"])
    evidence.file(args.source, source, "source.title")
    margin = protocol["acceptance"]["minimum_mean_delta_itp_gain"]
    require(isinstance(margin, (int, float)) and math.isfinite(margin) and margin >= 0,
            "invalid frozen margin")
    scores_match(report, margin)

    baseline_raw, baseline = json_bytes(artifacts["baseline_result"])
    _, baseline_request = json_bytes(artifacts["baseline_request"])
    require(baseline["request_sha256"] == sha256(artifacts["baseline_request"].read_bytes())
            and baseline["artifacts"] == baseline_request["artifacts"], "baseline provenance changed")
    evidence.items(baseline["artifacts"], "evaluation_baseline")
    planned = windows_match(protocol, report, args.calibration_result)
    for i, (window, path) in enumerate(zip(planned, args.calibration_result)):
        raw, calibrated = json_bytes(path)
        require(sha256(raw) == window["result_sha256"]
                and calibrated["window"] == window["window"]
                and calibrated["artifacts"]["readback"]["sha256"] == window["readback_sha256"]
                and calibrated["artifacts"]["served_receipt"]["sha256"] == window["served_receipt_sha256"],
                "calibration result does not match the frozen exposure")
        evidence.file(path, window["result_sha256"], f"calibration.{i}.result")
        evidence.items(calibrated["artifacts"], f"calibration.{i}")

    run_raw, run = json_bytes(artifacts["run_request"])
    _, terminal = json_bytes(artifacts["run_terminal"])
    _, author = json_bytes(artifacts["author_receipt"])
    _, authoring_protocol = json_bytes(artifacts["author_protocol"])
    run_assets = evidence.items(run["artifacts"], "frozen_run")
    for index, lib in enumerate(run["encoder_closure"]):
        evidence.file(lib["path"], lib["sha256"], f"encoder_closure.{index}")
    require(0 < len(run["encoder_closure"]) <= 128
            and terminal["request_sha256"] == sha256(run_raw)
            and terminal["protocol_sha256"] == protocol_sha
            and terminal["landing_sha256"] == report["artifacts"]["run_landing"]["sha256"]
            and terminal["land_rate_exit"] == 0
            and 0 < terminal["accepted_attempts"] <= protocol["rate_admission"]["maximum_attempts"]
            and author["status"] == "validated_profile81_window"
            and author["encoded_mp4_sha256"] == report["artifacts"]["encoded"]["sha256"]
            and author["output_mp4_sha256"] == report["artifacts"]["authored"]["sha256"]
            and author["author_plan_sha256"] == terminal["author_plan_sha256"]
            and author["protocol"] == authoring_protocol
            and authoring_protocol["comparison_protocol_sha256"] == protocol_sha
            and authoring_protocol["source_sha256"] == source
            and run_assets["paired_scorer"].is_file()
            and run["artifacts"]["paired_scorer"]["sha256"] == report["scorer_sha256"],
            "run, authoring, and scorer do not name the same experiment")
    curve = verify_digest(protocol["served_baseline"]["curve_digest"])
    require(curve == report["served_publication"]["curve_digest"]
            and baseline["window"] == report["window"], "serving baseline changed")
    lower, upper = (Fraction(str(value))
                    for value in protocol["rate_admission"]["experimental_band_mbps_inclusive"])
    require(lower <= Fraction(report["authored_video_packet_mbps_exact"]) <= upper,
            "authored rate was outside the frozen band")

    manifest = {
        "schema": 1, "source_sha256": source, "protocol_sha256": protocol_sha,
        "report_sha256": sha256(report_raw), "request_sha256": sha256(request_raw),
        "disposition": "refused", "origin": "historical_offline", "frozen_margin": margin,
        "scorer_sha256": verify_digest(report["scorer_sha256"]),
        "helper_sha256": verify_digest(run["artifacts"]["export_helper"]["sha256"]),
        "encoder_sha256": verify_digest(authoring_protocol["encoder_sha256"]),
        "fit_table_sha256": curve,
        "authored_sha256": verify_digest(author["output_mp4_sha256"]),
        "candidate_attempts": terminal["accepted_attempts"],
        "model_gain": {name: report["gates"][name]["window_and_pooled_mean_gain"]
                       for name in MODELS},
    }
    measurements = json.dumps(evidence.manifest, sort_keys=True, separators=(",", ":")).encode()
    manifest["evidence_sha256"] = sha256(measurements)
    files = {
        "manifest.json": json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode(),
        "frozen-protocol.json": protocol_raw, "comparison-result.json": report_raw,
        "comparison-request.json": request_raw, "freeze-seal.json": seal_raw,
        "verified-artifacts.json": measurements,
        "run-request.json": run_raw, "evaluation-baseline.json": baseline_raw,
    }
    parent = args.data_dir.resolve(strict=True) / "dv-m7" / "experiments" / source
    parent.mkdir(parents=True, exist_ok=True)
    destination = parent / protocol_sha
    with tempfile.TemporaryDirectory(prefix=".m7-import-", dir=parent) as temporary:
        stage = Path(temporary)
        for name, data in files.items():
            with (stage / name).open("xb") as out:
                out.write(data)
                out.flush()
                os.fsync(out.fileno())
        sync_directory(stage)
        try:
            stage.rename(destination)
        except OSError:
            require(destination.is_dir() and not destination.is_symlink()
                    and all((destination / name).is_file()
                            and (destination / name).read_bytes() == content
                            for name, content in files.items()),
                    "an immutable experiment already exists with different evidence")
        sync_directory(parent)
    for proof_path in args.loop_proof:
        import_repetition(args.data_dir, proof_path, source, protocol_sha, evidence)
    return manifest


def import_repetition(data_dir, proof_path, source, protocol, evidence):
    raw, proof = json_bytes(proof_path)
    sha = verify_digest(proof["fixture_sha256"])
    repeat = proof["repeat_count"]
    require(proof["source_sha256"] == source and sha != source
            and proof["packet_bytes_exact"] is True and proof["verdict"] == "pass"
            and isinstance(repeat, int) and repeat > 1
            and proof["original_access_units"] > 0
            and proof["access_units"] == proof["original_access_units"] * repeat
            and proof["presentation_unique"] == proof["access_units"],
            "invalid physical repetition custody")
    evidence.file(proof["destination"], sha, "physical_repetition." + sha)
    root = data_dir.resolve(strict=True) / "dv-m7" / "repetitions" / sha
    root.mkdir(parents=True, exist_ok=True)
    files = {
        "manifest.json": json.dumps({
            "schema": 1, "fixture_sha256": sha, "source_sha256": source,
            "protocol_sha256": protocol, "repeat_count": repeat,
            "proof_sha256": sha256(raw),
        }, sort_keys=True, separators=(",", ":")).encode(),
        "fixture-provenance.json": raw,
    }
    for name, data in files.items():
        dst = root / name
        with tempfile.NamedTemporaryFile(dir=root) as staging:
            staging.write(data)
            staging.flush()
            os.fsync(staging.fileno())
            try:
                os.link(staging.name, dst)
            except FileExistsError:
                require(not dst.is_symlink() and dst.read_bytes() == data,
                        "immutable repetition receipt differs")
    sync_directory(root)
    sync_directory(root.parent)


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["data-dir", "seal", "request", "report", "source"]:
        parser.add_argument(f"--{name}", required=True, type=Path)
    for name in ["artifact-root", "calibration-result"]:
        parser.add_argument(f"--{name}", required=True, type=Path, action="append")
    parser.add_argument("--loop-proof", default=[], type=Path, action="append")
    args = parser.parse_args()
    try:
        manifest = import_experiment(args)
    except (ValueError, KeyError, TypeError, OSError) as error:
        parser.exit(1, f"M7 import refused: {error}\n")
    print(json.dumps(manifest, sort_keys=True))


if __name__ == "__main__":
    main()
