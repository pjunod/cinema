"""Check actual parser-roundtripped authored RPUs against source fixture blocks."""
from pathlib import Path
import hashlib
import json
import sys

LEVELS = {'Level2', 'Level3', 'Level4', 'Level5', 'Level8'}

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def blocks(parsed):
    dm = parsed['vdr_dm_data']
    return [block for version in ('cmv29_metadata', 'cmv40_metadata')
            for block in dm[version]['ext_metadata_blocks']
            if next(iter(block)) in LEVELS]

def test_preserved_display_metadata(root):
    rows = [json.loads(line) for line in (root / 'valid/output/author.stdout').read_text().splitlines()]
    assert len(rows) == 3
    seen = set()
    findings = []
    anchors = set()
    for row in rows:
        index = row['renderer_frame']
        assert type(index) is int and index in range(3) and index not in seen
        seen.add(index)
        source_nal = root / f'valid/output/rpus/frame-{index:03d}.nal'
        adapted_nal = root / f'adapted/frame-{index:03d}.nal'
        assert digest(source_nal) == row['source_rpu_sha256']
        assert digest(adapted_nal) == row['adapted_rpu_sha256']
        matches = [p for p in (root / 'valid/rpus').glob('*.nal') if digest(p) == digest(source_nal)]
        assert matches
        original = json.loads(matches[0].with_suffix('.json').read_text())
        adapted = json.loads(adapted_nal.with_suffix('.json').read_text())
        expected = blocks(original)
        observed = blocks(adapted)
        assert json.dumps(expected, sort_keys=True) == json.dumps(observed, sort_keys=True)
        l4 = next(b['Level4'] for b in expected if 'Level4' in b)
        assert l4['anchor_pq'] > 0 and l4['anchor_power'] > 0
        anchors.add((l4['anchor_pq'], l4['anchor_power']))
        counts = {level: sum(level in block for block in expected) for level in LEVELS}
        assert counts == {'Level2': 2, 'Level3': 1, 'Level4': 1, 'Level5': 1, 'Level8': 1}
        assert next(b['Level3'] for b in expected if 'Level3' in b)['avg_pq_offset'] == 1571
        l8 = next(b['Level8'] for b in expected if 'Level8' in b)
        assert l8['length'] == 10 and l8['trim_slope'] == 2100
        assert adapted['dovi_profile'] == 8 and adapted['header']['disable_residual_flag'] is True
        assert adapted['header']['use_prev_vdr_rpu_flag'] is False
        findings.append({'renderer_frame': index, 'source_rpu_sha256': digest(source_nal),
                         'adapted_rpu_sha256': digest(adapted_nal), 'retained_block_counts': counts})
    assert len(anchors) == 3
    return {'status': 'actual-parser-retained-metadata-pass', 'frames': findings,
            'scope': 'synthetic preserved instructions; creative trims are not applied to HDR10 master'}

if __name__ == '__main__':
    root = Path(sys.argv[1])
    result = test_preserved_display_metadata(root)
    (root / 'metadata-results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(result['status'])
