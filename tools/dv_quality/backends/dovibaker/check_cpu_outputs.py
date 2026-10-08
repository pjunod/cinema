"""Assert actual CPU output and negative controls; emit a current control receipt."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent
NBYTES = 16 * 16 * 3 * 2


def checked_bytes(path):
    data = path.read_bytes()
    if len(data) != NBYTES:
        raise ValueError(f'{path.name}: expected {NBYTES} bytes, got {len(data)}')
    return data


def verify(actual_dir, reference_dir, negative_path):
    results = {}
    for actual_name, reference_name in [
        ('nonzero', 'nonzero'), ('zero', 'zero'), ('disabled', 'disabled'),
        ('bounded', 'bounded'), ('adapted-base', 'zero'),
    ]:
        actual = checked_bytes(actual_dir / f'{actual_name}.rgb48le')
        expected = checked_bytes(reference_dir / f'{reference_name}.rgb48le')
        av = struct.unpack('<768H', actual)
        ev = struct.unpack('<768H', expected)
        maximum = max(abs(a - e) for a, e in zip(av, ev))
        if maximum:
            raise ValueError(f'{actual_name}: maximum code error {maximum}, expected zero')
        results[actual_name] = {
            'max_code_error': maximum,
            'actual_sha256': hashlib.sha256(actual).hexdigest(),
            'reference_sha256': hashlib.sha256(expected).hexdigest(),
        }
    mutated = checked_bytes(actual_dir / 'nonstandard.rgb48le')
    unmutated = checked_bytes(actual_dir / 'nonzero.rgb48le')
    if mutated != unmutated:
        raise ValueError('Matrix restriction control changed: review backend/source semantics')
    negative = json.loads(negative_path.read_text())
    for name in ('missing_el', 'truncated_rpu'):
        if negative.get(name) != 67:
            raise ValueError(f'{name}: expected rejected exit 67, got {negative.get(name)}')
    for name in ('missing-el', 'malformed'):
        path = actual_dir / f'{name}.rgb48le'
        if path.exists():
            raise ValueError(f'{name}: rejected control unexpectedly published output')
    return {
        'schema': 'm0-baker-cpu-output-check-v1',
        'scope': 'analytic-control; direct CPU processor, no HEVC/physicalDV/P8.1 conformance',
        'result': 'pass',
        'controls': results,
        'negative_exit_codes': negative,
        'matrix_restriction': {
            'accepted_mutated_rpu': True,
            'output_unchanged': True,
            'classification': 'unsupported proof: rgb_to_lms changes ignored',
        },
        'independent_dv_reference': None,
        'p81_conformance': None,
        'reference_fidelity_improvement': None,
    }


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--actual', type=Path, default=ROOT / 'cpu-outputs')
    parser.add_argument('--reference', type=Path, default=ROOT / 'cpu-reference')
    parser.add_argument('--negative', type=Path, default=ROOT / 'negative-controls.json')
    parser.add_argument('--receipt', type=Path, default=ROOT / 'cpu-check-receipt.json')
    args = parser.parse_args()
    receipt = verify(args.actual, args.reference, args.negative)
    args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))
