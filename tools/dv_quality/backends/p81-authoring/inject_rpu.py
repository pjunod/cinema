"""Strict synthetic no-reorder AUD control injector; not a general HEVC adapter.
RPU placement follows dovi_tool83e1fdad rpu_injector.rs: after last non-EOS/EOB NAL.
"""
from pathlib import Path
import hashlib
import json
import re
ROOT = Path(__file__).resolve().parent

def nals(data):
    matches = list(re.finditer(b'\x00\x00\x00?\x01', data))
    if not matches or matches[0].start() != 0:
        raise ValueError('expected Annex B')
    return [data[m.end():matches[i+1].start() if i+1 < len(matches) else len(data)]
            for i, m in enumerate(matches)]

def inject(data, rpus, expected_rpu_hashes):
    if len(rpus) != len(expected_rpu_hashes) or any(hashlib.sha256(r).hexdigest() != h for r,h in zip(rpus, expected_rpu_hashes)):
        raise ValueError("RPU identity/order differs from source association")
    units = []
    for nal in nals(data):
        kind = (nal[0] >> 1) & 63
        if kind == 35:
            units.append([])
        if not units:
            raise ValueError('NAL before AUD unsupported')
        if kind in (36, 37, 62, 63):
            raise ValueError('EOS/EOB/existing DV unsupported in this control')
        units[-1].append(nal)
    if len(units) != len(rpus):
        raise ValueError('exact RPU/frame count required')
    result = bytearray()
    for unit, rpu in zip(units, rpus):
        slices = [nal for nal in unit if ((nal[0] >> 1) & 63) <= 31]
        if len(slices) != 1 or ((slices[0][0] >> 1) & 63) not in (1, 19, 20):
            raise ValueError('only one IDR/TRAIL_R slice per access unit supported')
        for nal in unit + [rpu]:
            result.extend(b'\x00\x00\x00\x01' + nal)
    return bytes(result)

if __name__ == '__main__':
    source = (ROOT / 'hdr10-base.hevc').read_bytes()
    definition = json.loads((ROOT / 'source-definition.json').read_text())
    rpus = [(ROOT / f['rpu_path']).read_bytes() for f in definition['frames']]
    expected = [f['rpu_sha256'] for f in definition['frames']]
    result = inject(source, rpus, expected)
    (ROOT / 'candidate.hevc').write_bytes(result)
    types = [((n[0] >> 1) & 63) for n in nals(result)]
    actual = [n for n in nals(result) if ((n[0] >> 1) & 63) == 62]
    assert len(actual) == 4 and actual == rpus
    (ROOT / 'injection-receipt.json').write_text(json.dumps({
        'scope': 'no-reorder synthetic analytic control', 'frames': 4,
        'rpu_count': len(actual), 'rpu_sha256_by_frame': expected,
        'source_definition_sha256': hashlib.sha256((ROOT/'source-definition.json').read_bytes()).hexdigest(),
        'nal_types': types, 'exact_payload_match': True,
        'timestamp_basis': 'No reorder, four IDR/TRAIL_R access units at input 24/1; raw HEVC has no PTS timestamps',
        'p81_conformance': None}, indent=2) + '\n')
