"""Public color math and strict artifact/XML boundaries for software-reference QC.

RGB chromaticities/D65: ITU-R BT.2020-2 table 3. RGB->XYZ is derived from
those primaries, not fitted to any vendor output. PQ: ST2084 (annex_b2.py).
No proprietary algorithm is inferred here. All errors remain observations.
"""
import hashlib
import json
import math
from pathlib import Path
import struct
import xml.etree.ElementTree as ET

from annex_b2 import eotf

PRIMARIES = {'Red': [0.708, 0.292], 'Green': [0.17, 0.797], 'Blue': [0.131, 0.046]}
WHITE = [0.3127, 0.329]


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def bind(path):
    path = Path(path).resolve(strict=True)
    return {'path': str(path), 'sha256': sha(path), 'size': path.stat().st_size}


MAX_PACKET_FILES = 2048
MAX_PACKET_FILE_BYTES = 8 * 1024 * 1024
MAX_PACKET_BYTES = 64 * 1024 * 1024
MAX_TRANSFER_MANIFEST_BYTES = 2 * 1024 * 1024
# These outer transport records are intentionally not self-bound by acquisition
# RESULT.json. No image, XML, input manifest or numerical-QC input is exempt.
TRANSPORT_ONLY = {'RESULT.json', 'OUTER-RESULT.json', 'outer.stdout', 'outer.stderr'}


def packet_record(relative, record):
    if not isinstance(relative, str) or not relative or Path(relative).is_absolute() or '..' in Path(relative).parts:
        raise ValueError('packet artifact path must be relative and contained')
    if not isinstance(record, dict) or type(record.get('size')) is not int or not (
            0 <= record['size'] <= MAX_PACKET_FILE_BYTES):
        raise ValueError('packet artifact size exceeds bounded record')
    digest = record.get('sha256')
    if not isinstance(digest, str) or len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest):
        raise ValueError('packet artifact requires exact SHA256 record')


def packet_file(root, relative, record):
    packet_record(relative, record)
    root = Path(root).resolve(strict=True)
    path = (root / relative).resolve(strict=True)
    if not path.is_relative_to(root) or not path.is_file() or (
            path.stat().st_size != record['size'] or sha(path) != record['sha256']):
        raise ValueError('packet artifact identity/path mismatch')
    return path


def acquisition(root):
    root = Path(root).resolve(strict=True)
    manifest_path = (root / 'TRANSFER-SHA256.json').resolve(strict=True)
    if not manifest_path.is_relative_to(root) or not manifest_path.is_file() or not (
            0 < manifest_path.stat().st_size <= MAX_TRANSFER_MANIFEST_BYTES):
        raise ValueError('transfer manifest exceeds bounded path/size')
    transfer = json.loads(manifest_path.read_text())
    if not isinstance(transfer, dict) or not 2 <= len(transfer) <= MAX_PACKET_FILES or not {
            'RESULT.json', 'OUTER-RESULT.json'}.issubset(transfer):
        raise ValueError('transfer coverage requires RESULT.json and OUTER-RESULT.json')
    for name, record in transfer.items():
        packet_record(name, record)
    if sum(record['size'] for record in transfer.values()) > MAX_PACKET_BYTES:
        raise ValueError('transfer packet exceeds bounded total size')
    for name, record in transfer.items():
        packet_file(root, name, record)
    result_path = packet_file(root, 'RESULT.json', transfer['RESULT.json'])
    result = json.loads(result_path.read_text())
    receipt_files = result.get('files')
    if not isinstance(receipt_files, dict) or not 1 <= len(receipt_files) <= MAX_PACKET_FILES:
        raise ValueError('immutable acquisition receipt file records required')
    if not set(receipt_files).issubset(transfer) or not set(transfer).difference(TRANSPORT_ONLY).issubset(receipt_files):
        raise ValueError('transfer coverage must include every acquisition artifact receipt')
    for name, record in receipt_files.items():
        packet_record(name, record)
        if (record['size'], record['sha256']) != (transfer[name]['size'], transfer[name]['sha256']):
            raise ValueError('immutable acquisition receipt disagrees with transfer artifact identity')
        # The transfer may have been recomputed over replacement pixels. The
        # acquisition's immutable records are the authority for original bytes.
        packet_file(root, name, record)
    if result['status'] != 'pass' or json.loads((root / 'OUTER-RESULT.json').read_text()) != {'exit_code': 0}:
        raise ValueError('reference acquisition did not finish successfully')
    names = [c['name'] for c in result['commands']]
    if not names or len(names) != len(set(names)) or any(
            c['exit_code'] != 0 or len(c['binary_sha256']) != 64 or not c['argv']
            for c in result['commands']):
        raise ValueError('missing true successful reference command records')
    return result, transfer


def one(node, path):
    items = node.findall(path)
    if len(items) != 1:
        raise ValueError('XML requires exactly one ' + path)
    return items[0]


def text(node, path):
    return (one(node, path).text or '').strip()


def chromaticities(node):
    p = one(node, './{*}Primaries')
    for name, expected in PRIMARIES.items():
        if [float(x) for x in text(p, './{*}' + name).split()] != expected:
            raise ValueError('reference primaries changed')
    if [float(x) for x in text(node, './{*}WhitePoint').split()] != WHITE:
        raise ValueError('reference white changed')


def xml_contract(path, vector):
    # The supported importer supplies SOP semantics. This preflight only freezes
    # the documented source/target/frame contract and the XML control vector.
    root = ET.parse(path).getroot()
    if text(root, './{*}Version') != '4.0.2':
        raise ValueError('only the acquired XML4.0.2 packet is supported')
    track = one(root, './/{*}Video/{*}Track')
    if text(track, './{*}EditRate') != '24 1':
        raise ValueError('source frame rate changed')
    encoding = one(track, './{*}ColorEncoding'); chromaticities(encoding)
    for name, value in {'Encoding': 'pq', 'ColorSpace': 'rgb', 'SignalRange': 'computer',
                        'PeakBrightness': '10000', 'MinimumBrightness': '0'}.items():
        if text(encoding, './{*}' + name) != value:
            raise ValueError('source encoding changed')
    master = one(track, './/{*}DVGlobalData/{*}MasteringDisplay'); chromaticities(master)
    if float(text(master, './{*}PeakBrightness')) != 4000 or float(text(master, './{*}MinimumBrightness')) != .005:
        raise ValueError('source mastering endpoints changed')
    targets = [t for t in track.findall('.//{*}DVGlobalData/{*}TargetDisplay')
               if text(t, './{*}ID') == '255']
    if len(targets) != 1:
        raise ValueError('unique custom target255 required')
    target = targets[0]; chromaticities(target)
    if float(text(target, './{*}PeakBrightness')) != 1000 or float(text(target, './{*}MinimumBrightness')) != .005 or text(target, './{*}EOTF') != 'pq':
        raise ValueError('target endpoints/EOTF changed')
    shot = one(track, './{*}Shot')
    if text(shot, './{*}Record/{*}In') != '0' or text(shot, './{*}Record/{*}Duration') != '3':
        raise ValueError('source-frame association changed')
    edits = shot.findall('./{*}Frame')
    if sorted(int(text(e, './{*}EditOffset')) for e in edits) != [0, 1, 2]:
        raise ValueError('complete exact frame edit association required')
    levels = shot.findall('.//{*}Level8')
    if len(levels) != 4:
        raise ValueError('shot plus three frame L8 instructions required')
    for level in levels:
        if text(level, './{*}TID') != '255' or [float(v) for v in text(level, './{*}L8Trim').split()] != vector:
            raise ValueError('XML vector/target does not match reference case')
        for name, expected in {'MidContrastBias': '0', 'HighlightClipping': '0',
                               'SaturationVectorField': '0 0 0 0 0 0',
                               'HueVectorField': '0 0 0 0 0 0'}.items():
            if text(level, './{*}' + name) != expected:
                raise ValueError('extra XML trim controls changed')
    return {'xml_version': '4.0.2', 'source_edit_rate': [24, 1], 'source_frames': 3,
            'source_encoding': 'pq rgb computer bt2020', 'source_encoding_peak': 10000,
            'source_mastering_peak_nits': 4000, 'source_mastering_floor_nits': .005,
            'target_id': 255, 'target_peak_nits': 1000, 'target_floor_nits': .005,
            'target_encoding': 'pq rgb computer bt2020', 'reference_bit_depth': 16,
            'application_type': 'ALL', 'primaries': PRIMARIES, 'white': WHITE,
            'shot_id': text(shot, './{*}UniqueID')}


def non_l8_xml_identity(path, varying_levels=('Level8',)):
    track = one(ET.parse(path).getroot(), './/{*}Video/{*}Track')
    def canonical(node):
        return (node.tag, sorted(node.attrib.items()), (node.text or '').strip(),
                [canonical(child) for child in node if child.tag.split('}')[-1] not in varying_levels])
    return hashlib.sha256(json.dumps(canonical(track), separators=(',', ':')).encode()).hexdigest()


def non_l8_rpu_identity(parsed, varying_levels=('Level8',)):
    value = json.loads(json.dumps(parsed))
    # CRC changes with the intentional L8 edit; the remaining RPU representation
    # must stay frozen. Callers explicitly declare companion varying levels;
    # the official acquisition also regenerates L2, which is reported separately.
    value.pop('rpu_data_crc32', None)
    for version in ('cmv29_metadata', 'cmv40_metadata'):
        metadata = value['vdr_dm_data'].get(version)
        if metadata:
            metadata['ext_metadata_blocks'] = [b for b in metadata['ext_metadata_blocks'] if not any(level in b for level in varying_levels)]
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def xml_companion_l2(path):
    shot = one(ET.parse(path).getroot(), './/{*}Video/{*}Track/{*}Shot')
    return [{'target_id': text(level, './{*}TID'),
             'xml_trim': [float(v) for v in text(level, './{*}Trim').split()]}
            for level in shot.findall('.//{*}Level2')]


def rgb16(data, width, height):
    if len(data) != width * height * 6:
        raise ValueError('decoded RGB16 raster length mismatch')
    return [tuple(v / 65535 for v in pixel) for pixel in struct.iter_unpack('<HHH', data)]


def rgba32(data, width, height):
    if len(data) != width * height * 16:
        raise ValueError('candidate float raster length mismatch')
    pixels = list(struct.iter_unpack('<ffff', data))
    if any(not math.isfinite(v) or not 0 <= v <= 1 for p in pixels for v in p):
        raise ValueError('candidate contains invalid normalized PQ')
    return [p[:3] for p in pixels]


def solve3(matrix, rhs):
    rows = [list(row) + [value] for row, value in zip(matrix, rhs, strict=True)]
    for col in range(3):
        pivot = max(range(col, 3), key=lambda i: abs(rows[i][col]))
        rows[col], rows[pivot] = rows[pivot], rows[col]
        divisor = rows[col][col]
        if not divisor:
            raise ValueError('singular primary matrix')
        rows[col] = [v / divisor for v in rows[col]]
        for other in range(3):
            if other != col:
                weight = rows[other][col]
                rows[other] = [a - weight*b for a, b in zip(rows[other], rows[col], strict=True)]
    return [row[3] for row in rows]


def xyz_matrix():
    cols = [[x/y, 1, (1-x-y)/y] for x, y in PRIMARIES.values()]
    matrix = list(map(list, zip(*cols, strict=True)))
    x, y = WHITE
    scale = solve3(matrix, [x/y, 1, (1-x-y)/y])
    return [[value * scale[i] for i, value in enumerate(row)] for row in matrix]


XYZ = xyz_matrix()


def xyz(pixels):
    return [tuple(math.fsum(a*b for a, b in zip(row, [eotf(v) for v in pixel], strict=True))
                  for row in XYZ) for pixel in pixels]


def differences(a, b):
    return [tuple(x-y for x, y in zip(p, q, strict=True)) for p, q in zip(a, b, strict=True)]


def metrics(errors):
    if not errors:
        raise ValueError('nonzero matched sample coverage required')
    channels = list(zip(*errors, strict=True))
    return {'pixels': len(errors), 'channels': [{'mean_signed': math.fsum(c)/len(c),
        'mean_absolute': math.fsum(abs(v) for v in c)/len(c),
        'rms': math.sqrt(math.fsum(v*v for v in c)/len(c)),
        'maximum_absolute': max(abs(v) for v in c)} for c in channels]}


def comparison(candidate, reference, candidate_zero, reference_zero):
    if len({len(x) for x in (candidate, reference, candidate_zero, reference_zero)}) != 1:
        raise ValueError('matched rasters must have identical coverage')
    return {'absolute_error': metrics(differences(candidate, reference)),
            'candidate_trim_delta': metrics(differences(candidate, candidate_zero)),
            'reference_trim_delta': metrics(differences(reference, reference_zero)),
            'trim_delta_error': metrics(differences(differences(candidate, candidate_zero),
                                                   differences(reference, reference_zero)))}
