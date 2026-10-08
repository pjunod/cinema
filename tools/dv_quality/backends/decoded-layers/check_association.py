"""Known synthetic fixture association, not a general media conformance checker."""
from fractions import Fraction
import hashlib
import json
import re
from pathlib import Path
import sys

class AssociationError(ValueError):
    pass

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def events(path):

    def refuse(value):
        raise AssociationError('nonfinite JSON constant')
    return [json.loads(line, parse_constant=refuse) for line in Path(path).read_text().splitlines()]

def inspect(root, bframes, mode):
    root = Path(root)
    folder = root / f'b{bframes}-{mode}'
    status = (folder / 'decode-status.txt').read_text()
    diagnostics = (folder / 'decode.stderr').read_text()
    if status != '0\n':
        normalized = re.sub('^\\[hevc @ 0x[0-9a-f]+\\] ', '', diagnostics, flags=re.MULTILINE)
        poc = 1 if (bframes, mode) == (0, 'swapped-el') else 2
        expected = f'Could not find ref with POC {poc}\nError constructing the frame RPS.\nError parsing NAL unit #1.\nControlled decode refusal: HEVC packet decode send\n'
        if mode in ('missing-el', 'swapped-el') and status == '1\n' and (normalized == expected):
            raise AssociationError('decoder-reference-chain-failure; association stage incomplete')
        raise AssociationError('uncontrolled decoder exit/diagnostic')
    if diagnostics:
        raise AssociationError('decoder diagnostic prevents accepted known fixture')
    records = events(folder / 'decode.jsonl')
    probe = json.loads((root / f'bl-b{bframes}.ffprobe.json').read_text())
    el_probe = json.loads((root / f'el-b{bframes}.ffprobe.json').read_text())
    for source in (probe, el_probe):
        stream = source['streams'][0]
        if stream['profile'] != 'Main 10' or stream['width'] != 64 or stream['height'] != 64 or (stream['pix_fmt'] != 'yuv420p10le'):
            raise AssociationError('encoded profile/pixel domain differs')
    time_base = Fraction(probe['streams'][0]['time_base'])
    packets = [entry for entry in probe['packets_and_frames'] if entry['type'] == 'packet']
    display = [entry for entry in probe['packets_and_frames'] if entry['type'] == 'frame']
    if len(packets) != 6 or len(display) != 6:
        raise AssociationError('six-source-frame fixture incomplete')
    source_packets = [Fraction(packet['pts']) * time_base for packet in packets]
    source_display = {Fraction(frame['pts']) * time_base: i for i, frame in enumerate(display)}
    if len(source_display) != 6:
        raise AssociationError('duplicate source PTS')
    demux = [record for record in records if record['kind'] == 'demux_packet']
    if len(demux) != 6 or [Fraction(entry['pts']) for entry in demux] != source_packets:
        raise AssociationError('container PTS altered or missing')
    for packet, source in zip(demux, packets):
        for key in ('dts', 'duration'):
            expected = None if key not in source else Fraction(source[key]) * time_base
            actual = None if packet[key] is None else Fraction(packet[key])
            if actual != expected:
                raise AssociationError('container DTS/duration altered')
    for layer in ('bl', 'el'):
        split = [record for record in records if record['kind'] == 'split_packet' and record['layer'] == layer]
        if len(split) != 6:
            raise AssociationError('missing layer access unit')
        for packet, expected in zip(split, demux):
            if any((packet.get(key) != expected.get(key) for key in ['pts', 'dts', 'duration'])):
                raise AssociationError('split changed actual timing')
    decoded = {}
    for layer in ('bl', 'el'):
        rows = [record for record in records if record['kind'] == 'decoded_frame' and record['layer'] == layer]
        pts = [Fraction(row['pts']) for row in rows if row['pts'] is not None]
        if len(rows) != 6 or len(set(pts)) != 6 or pts != sorted(source_display):
            raise AssociationError('missing/duplicate/unmatched decoded layer PTS')
        decoded[layer] = {Fraction(row['pts']): row for row in rows}
    definitions = json.loads((root / 'source-definition.json').read_text())
    wanted = {(row['layer'], row['logical_source_frame']): row['source_sha256'] for row in definitions['frames']}
    pairs = []
    for pts, source in sorted(source_display.items()):
        bl = decoded['bl'][pts]
        el = decoded['el'][pts]
        if bl['duration'] != el['duration'] or Fraction(bl['duration']) <= 0 or bl['duration'] != demux[source_packets.index(pts)]['duration'] or (bl['best_effort_pts'] != bl['pts']) or (el['best_effort_pts'] != el['pts']):
            raise AssociationError('decoded layer duration mismatch')
        bl_path = folder / f"frames/bl-frame-{bl['display_emission']:03d}.yuv420p10le"
        el_path = folder / f"frames/el-frame-{el['display_emission']:03d}.yuv420p10le"
        if sha(bl_path) != wanted['bl', source] or sha(el_path) != wanted['el', source]:
            raise AssociationError('decoded pixels disagree with independent native-code source')
        if bl['decoder_rpu_present'] is not True or bl['decoder_metadata_present'] is not True:
            raise AssociationError('fresh per-frame RPU absent; cached metadata is insufficient')
        rpu_path = folder / f"frames/bl-frame-{bl['display_emission']:03d}.rpu.nal"
        if sha(rpu_path) != sha(root / f'rpu-tags/p7-frame{source}.nal'):
            raise AssociationError('decoded RPU disagrees with known frame-associated fixture')
        if bl['l1_max'] != 4095 - source or bl['l1_average'] != 2048 - source:
            raise AssociationError('parsed L1 source tag mismatch')
        pairs.append({'source_frame': source, 'coded_arrival': source_packets.index(pts), 'display_emission': bl['display_emission'], 'pts': bl['pts'], 'duration': bl['duration'], 'bl_sha256': sha(bl_path), 'el_sha256': sha(el_path), 'rpu_sha256': sha(rpu_path), 'bl_path': str(bl_path.relative_to(root)), 'el_path': str(el_path.relative_to(root)), 'rpu_path': str(rpu_path.relative_to(root)), 'parsed_l1': {'max': bl['l1_max'], 'average': bl['l1_average']}})
    return {'accepted': True, 'scope': 'synthetic fixture decode/display association only', 'bframes': bframes, 'container_time_base': str(time_base), 'source_container_sha256': sha(root / f'bl-b{bframes}.mkv'), 'compound_container_sha256': sha(folder / 'compound.mkv'), 'decode_trace_sha256': sha(folder / 'decode.jsonl'), 'source_definition_sha256': sha(root / 'source-definition.json'), 'pairs': pairs, 'full_fel_qualified': False}
EXPECTED_NEGATIVES = {'missing-el': ('decoder', 'decoder-reference-chain-failure; association stage incomplete'), 'swapped-el': ('decoder', 'decoder-reference-chain-failure; association stage incomplete'), 'swapped-rpu': ('association', 'decoded RPU disagrees with known frame-associated fixture'), 'missing-rpu': ('association', 'fresh per-frame RPU absent; cached metadata is insufficient')}

def assess(root, bframes, mode):
    """An unrelated failure is a failed test, never a passing negative control."""
    try:
        result = inspect(root, bframes, mode)
    except AssociationError as error:
        if mode == 'normal':
            raise
        stage, expected = EXPECTED_NEGATIVES[mode]
        if str(error) != expected:
            raise AssociationError(f'unexpected {mode} failure: {error}') from error
        return {'accepted': False, 'bframes': bframes, 'mode': mode, 'failure_stage': stage, 'reason': str(error), 'full_fel_qualified': False}
    if mode != 'normal':
        raise AssociationError('negative fixture unexpectedly admitted')
    return result
if __name__ == '__main__':
    root = Path(sys.argv[1])
    results = [assess(root, bframes, mode) for bframes in (0, 2) for mode in ('normal', 'missing-el', 'swapped-el', 'swapped-rpu', 'missing-rpu')]
    print(json.dumps({'scope': 'known synthetic encoded fixtures', 'results': results}, indent=2, allow_nan=False))
