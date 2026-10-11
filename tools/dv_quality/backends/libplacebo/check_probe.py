"""Check declared synthetic arithmetic and write bounded export receipts.

This validates renderer/export mechanics, not encoded P7, RPU parsing, timing,
creative trims, physical playback, or production throughput.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import struct

CASES = ("zero", "nonzero", "omitted", "shifted", "disabled")
STAGES = ("reconstruction", "rendered")
WIDTH = HEIGHT = 16
# Float PQ/matrix roundtrip diagnostic bound; this is not a preregistered
# commercial quality threshold. Integer output permits one quantization code.
FLOAT_ERROR_LIMIT = 1e-5
INTEGER_ERROR_LIMIT = 1


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_float(path):
    data = path.read_bytes()
    if len(data) != WIDTH * HEIGHT * 16:
        raise ValueError(f"Unexpected RGBA32F length: {path}")
    values = struct.unpack("<" + "f" * (WIDTH * HEIGHT * 4), data)
    if not all(math.isfinite(value) for value in values):
        raise ValueError(f"Non-finite RGBA32F component: {path}")
    return values


def read_rgb(path):
    data = path.read_bytes()
    if len(data) != WIDTH * HEIGHT * 6:
        raise ValueError(f"Unexpected RGB48LE length: {path}")
    return struct.unpack("<" + "H" * (WIDTH * HEIGHT * 3), data)


def expected(case, x, y, component):
    value = 0.25 + 0.02 * component + 0.005 * x + 0.003 * y
    if case in ("nonzero", "shifted"):
        sample_x = (x + 1) % WIDTH if case == "shifted" else x
        value += 0.025 if (sample_x + y + component) % 2 else -0.025
    return value


def reject_json_constant(value):
    raise ValueError(f"Non-finite JSON constant: {value}")


def finite_json_float(value):
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"Non-finite JSON number: {value}")
    return number


def strict_json(line):
    return json.loads(line, parse_constant=reject_json_constant,
                      parse_float=finite_json_float)



def check(root):
    observations = [strict_json(line) for line in
                    (root / "probe.jsonl").read_text().splitlines()]
    frames = [row for row in observations if row["kind"] == "frame"]
    if [row["case"] for row in frames] != list(CASES):
        raise ValueError("Missing or reordered synthetic case observations")
    for row in frames:
        if row["render_errors"] or not row["render_ok"]:
            raise ValueError("Renderer failure")
        if row["rpu_parsed"] or row["qualified_fel"]:
            raise ValueError("Synthetic probe overclaims encoded-source acceptance")

    # Validate every float component, including alpha, in supplied BL/EL
    # textures and exported frames. Never quantize or score NaN/Inf pixels.
    for case in CASES:
        for stage in ("bl", "el", *STAGES):
            read_float(root / "outputs" / f"{case}-{stage}.rgba32f")
            read_rgb(root / "outputs" / f"{case}-{stage}.rgb48le")

    results = []
    for stage in STAGES:
        for case in CASES:
            floats = read_float(root / "outputs" / f"{case}-{stage}.rgba32f")
            errors = [abs(floats[4 * (y * WIDTH + x) + component] -
                          expected(case, x, y, component))
                      for y in range(HEIGHT) for x in range(WIDTH)
                      for component in range(3)]
            maximum = max(errors)
            if maximum > FLOAT_ERROR_LIMIT:
                raise ValueError(f"Known-answer failure: {case}/{stage}: {maximum}")
            reference_name = case if case in ("nonzero", "shifted") else "base"
            actual = read_rgb(root / "outputs" / f"{case}-{stage}.rgb48le")
            reference = read_rgb(root / "outputs" /
                                 f"{reference_name}-reference.rgb48le")
            code_error = max(abs(a - b) for a, b in zip(actual, reference))
            if code_error > INTEGER_ERROR_LIMIT:
                raise ValueError(f"Integer known-answer failure: {case}/{stage}")
            results.append({"case": case, "stage": stage,
                            "max_abs_pq_error": maximum,
                            "max_rgb48_code_error": code_error,
                            "known_answer_pass": True})

        base = (root / "outputs" / f"zero-{stage}.rgb48le").read_bytes()
        for case in ("omitted", "disabled"):
            if (root / "outputs" / f"{case}-{stage}.rgb48le").read_bytes() != base:
                raise ValueError(f"No-residual negative control changed base: {case}")
        reference = read_rgb(root / "outputs" / "nonzero-reference.rgb48le")
        for case in ("omitted", "shifted", "disabled"):
            actual = read_rgb(root / "outputs" / f"{case}-{stage}.rgb48le")
            if max(abs(a - b) for a, b in zip(actual, reference)) <= INTEGER_ERROR_LIMIT:
                raise ValueError(f"Negative control was not distinguished: {case}")

    receipt = {
        "schema": "plurx.dv-m0.direct-render-controls.v1",
        "capability": "synthetic structured-metadata EL composition and frame export",
        "scope": "analytic-control",
        "source_definition_sha256": sha(root / "source-definition.json"),
        "probe_source_sha256": sha(root / "fel_export_probe.c"),
        "probe_binary_sha256": sha(root / "fel_export_probe"),
        "library_sha256": sha(root / "prefix/lib/aarch64-linux-gnu/libplacebo.so.374"),
        "reference_method_sha256": sha(root / "reference-method.json"),
        "libplacebo_revision": "0d043c7f6f79cd3687c023454bdacbe615e4d96f",
        "libplacebo_api": 374,
        "observations": frames,
        "results": results,
        "actual_encoded_p7": False,
        "rpu_parser_tested": False,
        "production_qualified": False,
        "unqualified": ["HEVC BL/EL/RPU display timestamp association",
                        "resampling/chroma/half-raster EL",
                        "unsupported or malformed RPU",
                        "creative L2/L8 trims", "P8.1 authoring and HDR10 base",
                        "target remapping and HDR10 metadata/encoding",
                        "hardware decoder or real GPU throughput",
                        "cancellation/queue/resource admission", "physical playback"],
    }
    (root / "render-receipt.json").write_text(json.dumps(receipt, indent=2, allow_nan=False) + "\n")
    return receipt


def manifests(root):
    artifact = root / "prefix/lib/aarch64-linux-gnu/libplacebo.so.374"
    renderer = {"id": "libplacebo-direct-probe",
                "revision": "0d043c7f6f79cd3687c023454bdacbe615e4d96f",
                "artifact_sha256": sha(artifact)}
    target = {"id": "no-map-10000", "peak_nits": 10000,
              "black_nits": 0.005, "gamut": "bt2020",
              "mapping": "none", "parameters": {}}
    source = {"id": "synthetic-identity-composition-control",
              "sha256": sha(root / "source-definition.json")}
    for stage, domain in (("reconstruction", "reconstruction"), ("rendered", "mapped")):
        for role, case in (("candidate", "nonzero"), ("baseline", "omitted"),
                           ("independent-reference", "nonzero-reference"),
                           ("candidate-shifted-control", "shifted")):
            directory = root / "manifests" / stage / role
            directory.mkdir(parents=True, exist_ok=True)
            filename = ("nonzero-reference.rgb48le" if case == "nonzero-reference"
                        else f"{case}-{stage}.rgb48le")
            payload = (root / "outputs" / filename).read_bytes()
            (directory / "frame-000.rgb").write_bytes(payload)
            manifest = {
                "schema": 1,
                "role": "candidate" if role == "candidate-shifted-control" else role,
                "source": source,
                "renderer": renderer,
                "picture": {"width": WIDTH, "height": HEIGHT,
                            "pixel_format": "rgb48le", "range": "full",
                            "primaries": "bt2020", "transfer": "smpte2084",
                            "domain": domain},
                "target": target,
                "frames": [{"path": "frame-000.rgb",
                            "sha256": hashlib.sha256(payload).hexdigest(),
                            "pts": "0/1", "duration": "1/24"}],
            }
            if role == "independent-reference":
                evidence = (root / "reference-method.json").read_bytes()
                (directory / "reference-method.json").write_bytes(evidence)
                manifest["renderer"] = {
                    "id": "independent-declared-identity-decimal-oracle",
                    "revision": sha(root / "reference-method.json"),
                    "artifact_sha256": sha(root / "bundle/identity_reference.py"),
                }
                manifest["provenance"] = {
                    "kind": "independent", "scope": "analytic-control",
                    "producer": "identity_reference.py authored separately from renderer probe",
                    "method": "Exact Decimal closed-form declared identity residual arithmetic; no DV conformance oracle",
                    "evidence": {"path": "reference-method.json",
                                 "sha256": hashlib.sha256(evidence).hexdigest()},
                }
            (directory / "manifest.json").write_text(json.dumps(manifest, indent=2, allow_nan=False) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    arguments = parser.parse_args()
    scratch = arguments.root.resolve()
    receipt = check(scratch)
    manifests(scratch)
    print(json.dumps({"known_answer_checks": len(receipt["results"]),
                      "maximum_float_error": max(row["max_abs_pq_error"]
                                                 for row in receipt["results"]),
                      "qualified_fel": False}, indent=2, allow_nan=False))
