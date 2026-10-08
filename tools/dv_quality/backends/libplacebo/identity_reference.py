"""Independent arithmetic reference for a declared identity shader control.

No Dolby bitstream or creative-mapping standard conformance is claimed.
The explicit output directory must be new; this module never writes beside
its source file unless that destination was explicitly supplied by its caller.
"""
import argparse
from decimal import Decimal, ROUND_HALF_EVEN
import hashlib
import json
from pathlib import Path
import struct


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def generate(root):
    root.mkdir(parents=False, exist_ok=False)
    definition = {
        "schema": "identity-composition-control-v1",
        "width": 16,
        "height": 16,
        "frames": 1,
        "domain": "full-range BT2020 PQ RGB",
        "base_pq": "0.25 + 0.02*c + 0.005*x + 0.003*y",
        "el_pq": "0.625 if (x+y+c)%2 else 0.375",
        "offset": "0.5",
        "slope": "0.2",
        "threshold": "0",
        "shape": "identity",
        "nonlinear_matrix": "identity",
        "linear_matrix": "inverse of declared HPE-BT2020 LMS-to-RGB matrix",
        "target_mapping": "none",
        "expected_operation": "base + (el-offset)*slope",
        "scope": "analytic-control; declared identity arithmetic only; no proof of generic FEL or P8.1",
    }
    (root / "source-definition.json").write_text(
        json.dumps(definition, indent=2, allow_nan=False) + "\n")
    for name, residual, shifted in (("nonzero", True, False),
                                    ("base", False, False),
                                    ("shifted", True, True)):
        data = bytearray()
        for y in range(16):
            for x in range(16):
                for component in range(3):
                    value = (Decimal(".25") + Decimal(".02") * component +
                             Decimal(".005") * x + Decimal(".003") * y)
                    if residual:
                        sample_x = (x + 1) % 16 if shifted else x
                        value += (Decimal(".025") if (sample_x + y + component) % 2
                                  else Decimal("-.025"))
                    code = int((value * 65535).quantize(
                        Decimal(1), rounding=ROUND_HALF_EVEN))
                    data.extend(struct.pack("<H", code))
        (root / f"{name}-reference.rgb48le").write_bytes(data)
    evidence = {
        "kind": "independent",
        "scope": "analytic-control",
        "producer": "identity_reference.py using Python Decimal exact decimal arithmetic",
        "method": "Closed-form declared identity BL+signed residual control, independent from executing renderer functions. Output quantization is nearest-even full-range PQ*65535. Does not validate original DV equation, metadata parsing, timing, decoding, chroma resampling, P8.1 compatibility, or creative trims.",
        "input_definition_sha256": sha(root / "source-definition.json"),
        "script_sha256": sha(Path(__file__)),
        "output_format": "rgb48le",
        "artifacts": {f"{name}-reference.rgb48le": sha(root / f"{name}-reference.rgb48le")
                      for name in ("nonzero", "base", "shifted")},
    }
    (root / "reference-method.json").write_text(
        json.dumps(evidence, indent=2, allow_nan=False) + "\n")
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="New output directory with an existing parent")
    arguments = parser.parse_args()
    # Check the lexical path before resolve so an existing symlink cannot be
    # disguised as a new target, then rely on atomic mkdir to reject races.
    if arguments.output.exists() or arguments.output.is_symlink():
        parser.error("output directory already exists")
    result = generate(arguments.output.resolve())
    print(json.dumps(result, indent=2, allow_nan=False))
