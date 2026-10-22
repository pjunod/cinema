"""Numerical QC against source-bound official software-rendered RGB16 references.

No vendor executable is invoked, inspected or copied. The existing acquisition
packet supplies reference images/metadata/true-exit records. Public libdovi
imports each XML into actual serialized RPUs. A pinned public FFmpeg decoder
reads TIFF without a color transform. The SOP-only candidate remains unaccepted.
Run only in the coordinator's reviewed qualification batch.
"""
import argparse
import json
from pathlib import Path
import struct
import subprocess

from trim_software_metrics import (acquisition, bind, comparison, differences,
    metrics, non_l8_rpu_identity, non_l8_xml_identity, packet_file, rgb16, rgba32, sha, xml_companion_l2, xml_contract, xyz, XYZ)


def command(argv, root, name):
    result = subprocess.run([str(a) for a in argv], capture_output=True, timeout=300, check=False)
    stdout, stderr = root / (name + '.stdout'), root / (name + '.stderr')
    stdout.write_bytes(result.stdout); stderr.write_bytes(result.stderr)
    record = {'argv': [str(a) for a in argv], 'exit': result.returncode,
              'binary': bind(argv[0]), 'stdout': bind(stdout), 'stderr': bind(stderr)}
    if result.returncode:
        raise RuntimeError(f'{name} exited {result.returncode}; retained raw logs')
    return record, result.stdout


def decode(decoder, probe, source, output, width, height, commands, name):
    record, raw = command([probe, '-v', 'error', '-select_streams', 'v:0',
        '-show_entries', 'stream=codec_name,pix_fmt,width,height', '-of', 'json', source], output, name + '-probe')
    commands.append(record)
    streams = json.loads(raw)['streams']
    if len(streams) != 1 or streams[0].get('codec_name') != 'tiff' or (
            streams[0].get('width'), streams[0].get('height')) != (width, height) or streams[0].get('pix_fmt') not in ('rgb48le', 'rgb48be'):
        raise ValueError('reference/source must decode as exact TIFF RGB16 raster')
    decoded = output / (name + '.rgb48le')
    record, _ = command([decoder, '-v', 'error', '-nostdin', '-i', source,
        '-map', '0:v:0', '-frames:v', '1', '-pix_fmt', 'rgb48le', '-c:v', 'rawvideo',
        '-f', 'rawvideo', '-n', decoded], output, name + '-decode')
    commands.append(record)
    return decoded, rgb16(decoded.read_bytes(), width, height)


def app_args(record, app):
    argv = record['argv']; at = argv.index('--entrypoint')
    if argv[at + 1] != '/vendor/' + app:
        raise ValueError('unexpected acquisition application')
    return argv[at + 3:]


def flag(args, name, expected):
    if args.count(name) != 1 or args[args.index(name) + 1] != expected:
        raise ValueError('acquisition format/selection changed: ' + name)


def reference_case(root, result, transfer, case):
    name = case['name']
    if not name or any(c not in 'abcdefghijklmnopqrstuvwxyz0123456789-' for c in name):
        raise ValueError('unsafe case name')
    if (case['source_frame_index'], case['source_pts'], case['target_id'],
        case['target_peak_nits'], case['target_floor_nits'], case['primaries']) != (
            0, [0, 1], 255, 1000, .005, 'BT2020'):
        raise ValueError('matched source frame/target changed')
    metadata_name = f'cases/{name}/metadata.xml'
    image_name = f'cases/{name}/output/frame_000000.tif'
    xml = packet_file(root, metadata_name, transfer[metadata_name])
    image = packet_file(root, image_name, transfer[image_name])
    if sha(xml) != case['metadata_sha256'] or sha(image) != case['output_sha256']:
        raise ValueError('case identity does not match reference result')
    commands = {c['name']: c for c in result['commands']}
    render = app_args(commands[name + '-render'], 'cm_offline')
    for field, value in {'-m': '/work/' + metadata_name, '--targ': '255', '-f': '0',
                         '--output-format': 'pq rgb computer bt2020'}.items():
        flag(render, field, value)
    if render[-2:] != ['/baseline/inputs/frame_######.tif', f'/work/cases/{name}/output/frame_######.tif']:
        raise ValueError('vendor source/output frame patterns changed')
    flag(app_args(commands[name + '-validate'], 'metafier'), '--validate', '/work/' + metadata_name)
    target_record = commands[name + '-target']
    target_args = app_args(target_record, 'cm_offline')
    flag(target_args, '-m', '/work/' + metadata_name); flag(target_args, '--targ-info', '255')
    info_name = f'logs/{name}-target.stderr'
    info = packet_file(root, info_name, transfer[info_name]).read_text()
    if 'Application Type: ALL' not in info or 'pq(0.005,1000) rgb computer bt2020' not in info or 'Bit Depth: 16bit' not in info:
        raise ValueError('documented target-info format changed')
    frozen = xml_contract(xml, case['l8_vector'])
    return xml, image, frozen


def authoring_scope(reference, cases, reference_root, parent_root):
    scope = reference.get('metadata_delta_scope', 'L8-plus-officially-regenerated-companion-L2')
    if scope == 'L8-plus-officially-regenerated-companion-L2':
        return ('Level2', 'Level8'), None, None
    if scope != 'strict-l8-only-authored-xml' or parent_root is None:
        raise ValueError('strict authored XML requires its original parent reference packet')
    parent_root = parent_root.resolve(strict=True)
    parent, transfer = acquisition(parent_root)
    if reference['parent_reference_result_sha256'] != sha(parent_root/'RESULT.json') or reference['baseline_result_sha256'] != parent['baseline_result_sha256']:
        raise ValueError('authored XML parent/source acquisition binding mismatch')
    parents = {c['name']: c for c in parent['cases']}
    records = reference['metadata_authoring']
    if len(records) != len(cases) or len({r['name'] for r in records}) != len(cases):
        raise ValueError('complete unique metadata-authoring records required')
    records = {r['name']: r for r in records}
    if set(records) != {c['name'] for c in cases}:
        raise ValueError('authoring case coverage mismatch')
    baseline_name = 'cases/zero/metadata.xml'
    baseline = packet_file(parent_root, baseline_name, transfer[baseline_name])
    fixed = non_l8_xml_identity(baseline)
    for case in cases:
        name = case['name']; record = records[name]
        donor_name = f'cases/{name}/metadata.xml'
        donor = packet_file(parent_root, donor_name, transfer[donor_name])
        output = reference_root/f'cases/{name}/metadata.xml'
        if (record['baseline_xml_sha256'], record['donor_xml_sha256'],
            record['output_xml_sha256'], record['replaced_l8_nodes']) != (
                sha(baseline), sha(donor), sha(output), 4) or parents[name]['l8_vector'] != case['l8_vector']:
            raise ValueError('metadata-authoring input/output/vector binding mismatch')
        if non_l8_xml_identity(output) != fixed:
            raise ValueError('authored variant changed non-L8 original baseline XML')
    return ('Level8',), {'parent_reference_acquisition': bind(parent_root/'RESULT.json'),
        'records': list(records.values()), 'method': 'original zero XML; donor L8 only; fixed non-L8 verified'}, fixed


def parsed_block(parsed, level):
    dm = parsed['vdr_dm_data']
    return [b[level] for version in ('cmv29_metadata', 'cmv40_metadata')
            for b in (dm.get(version) or {}).get('ext_metadata_blocks', []) if level in b]


def check_selected(event, parsed):
    blocks = [b for b in parsed_block(parsed, 'Level8') if b['target_display_index'] == 255]
    targets = [b for b in parsed_block(parsed, 'Level10') if b['target_display_index'] == 255]
    if len(blocks) != 1 or len(targets) != 1:
        raise ValueError('actual parsed RPU must contain custom L8/L10 target255')
    block, target = blocks[0], targets[0]
    words = event['trim_words']
    expected = [block[k] for k in ('trim_slope', 'trim_offset', 'trim_power',
                                   'trim_chroma_weight', 'trim_saturation_gain', 'ms_weight')]
    if words[1:4] != [8, target['target_max_pq'], 255] or words[5:11] != expected:
        raise ValueError('candidate did not select the converted actual RPU trim')


def patch_means(pixels, width, x, y, size=16):
    return [sum(pixels[row*width+col][channel] for row in range(y, y+size)
                for col in range(x, x+size)) / (size*size) for channel in range(3)]


def observe(args):
    baseline_root, reference_root = args.baseline_packet.resolve(), args.reference_packet.resolve()
    baseline, bt = acquisition(baseline_root); reference, rt = acquisition(reference_root)
    if reference['baseline_result_sha256'] != sha(baseline_root / 'RESULT.json'):
        raise ValueError('reference does not bind this source acquisition')
    analyze = {c['name']: c for c in baseline['commands']}['analyze']
    aa = app_args(analyze, 'cm_analyze')
    flag(aa, '--source-format', 'pq rgb computer bt2020'); flag(aa, '--frame-rate', '24')
    if aa[-2:] != ['/work/inputs/frame_######.tif', '/work/metadata.xml']:
        raise ValueError('analyzed source patterns changed')
    manifest = json.loads(packet_file(baseline_root, 'INPUT-MANIFEST.json', bt['INPUT-MANIFEST.json']).read_text())
    if (manifest['kind'], manifest['width'], manifest['height'], manifest['frames'],
        manifest['fps'], manifest['source_frame_start'], manifest['mastering_peak_nits'],
        manifest['mastering_primaries']) != ('self-owned-engineered-PQ-BT2020-full-range-RGB16',
            256, 128, 3, [24, 1], 0, 4000, 'BT2020'):
        raise ValueError('only the frozen acquired engineered fixture is supported')
    width, height = 256, 128
    sources = [packet_file(baseline_root, f'inputs/frame_{i:06}.tif',
                          bt[f'inputs/frame_{i:06}.tif']) for i in range(3)]
    if any(sha(p) != manifest['source_sha256'] for p in sources):
        raise ValueError('source frames no longer match the acquired identical frames')
    cases = reference['cases']
    if not 2 <= len(cases) <= 16 or len({c['name'] for c in cases}) != len(cases):
        raise ValueError('nonzero unique baseline and trim cases required')
    zero = [c for c in cases if c['name'] == 'zero' and c['l8_vector'] == [0]*6]
    if len(zero) != 1:
        raise ValueError('exactly one zero-L8 baseline required')
    cases = zero + [c for c in cases if c['name'] != 'zero']
    frozen = [reference_case(reference_root, reference, rt, c) for c in cases]
    if any(contract != frozen[0][2] for _, _, contract in frozen):
        raise ValueError('source/target/version/shot contract varies across cases')
    varying_levels, authored, original_fixed = authoring_scope(reference, cases, reference_root,
        getattr(args, 'parent_reference_packet', None))
    xml_baseline = non_l8_xml_identity(frozen[0][0], varying_levels)
    if any(non_l8_xml_identity(xml, varying_levels) != xml_baseline for xml, _, _ in frozen):
        raise ValueError('metadata beyond explicitly recorded L8/companion-L2 changed in XML')
    binaries = {k: Path(getattr(args, k)).resolve(strict=True)
                for k in ('converter', 'decoder', 'probe', 'candidate')}
    output = args.output.resolve(); output.mkdir()
    commands = []
    source_decoded, pixels = decode(binaries['decoder'], binaries['probe'], sources[0],
                                     output, width, height, commands, 'source-frame0')
    native_input = output / 'source-frame0.rgba32f'
    native_input.write_bytes(b''.join(struct.pack('<ffff', *p, 1) for p in pixels))
    conversions, references, association = [], [], []
    argv = [binaries['candidate'], 'bt2020-pq-target-1000', '50', '4000',
            str(width), str(height), 'inferred-raw-sop-v1', output / 'candidate']
    for case, (xml, image, contract) in zip(cases, frozen, strict=True):
        name = case['name']; converted = output / (name + '-rpu')
        record, _ = command([binaries['converter'], converted, 'xml-reference', xml,
                             str(width), str(height)], output, name + '-convert')
        commands.append(record)
        conversion_path = converted / 'conversion.json'
        conversion = json.loads(conversion_path.read_text())
        frames = conversion['frames']
        if conversion['schema'] != 'public-libdovi-xml-reference-v1' or conversion['library_revision'] != '83e1fdad6dcd5995556235946e7c5c0f9010d5a1' or (
                conversion['width'], conversion['height']) != (width, height) or [f['source_frame_index'] for f in frames] != [0, 1, 2] or any(
                    f['shot_id'] != contract['shot_id'] or f['shot_start'] != 0 or
                    f['edit_offset'] != i for i, f in enumerate(frames)):
            raise ValueError('public XML conversion frame association mismatch')
        frame = frames[0]
        nal = (converted / frame['nal']).resolve(strict=True)
        parsed_path = (converted / frame['parsed_rpu']).resolve(strict=True)
        if not nal.is_relative_to(converted) or not parsed_path.is_relative_to(converted):
            raise ValueError('conversion artifact escaped attempt')
        parsed = json.loads(parsed_path.read_text()); conversions.append(parsed)
        argv.extend([nal, native_input])
        decoded, rgb = decode(binaries['decoder'], binaries['probe'], image, output,
                              width, height, commands, name + '-reference')
        references.append(rgb)
        association.append({'case': name, 'source_frame_index': 0, 'pts': [0, 1],
            'duration': [1, 24], 'xml_l8_vector': case['l8_vector'],
            'source_tiff': bind(sources[0]), 'source_decoded_rgb16': bind(source_decoded),
            'source_imported_rgba32f': bind(native_input), 'official_xml': bind(xml),
            'public_conversion_receipt': bind(conversion_path),
            'public_imported_config': bind(converted / 'imported-config.json'),
            'actual_rpu': bind(nal), 'roundtripped_rpu_json': bind(parsed_path),
            'xml_companion_l2': xml_companion_l2(xml),
            'actual_rpu_companion_l2': parsed_block(parsed, 'Level2'),
            'reference_tiff': bind(image), 'reference_decoded_rgb16': bind(decoded)})
    rpu_baseline = non_l8_rpu_identity(conversions[0], varying_levels)
    if any(non_l8_rpu_identity(parsed, varying_levels) != rpu_baseline for parsed in conversions):
        raise ValueError('metadata beyond explicitly recorded L8/companion-L2 changed in actual RPU')
    record, raw = command(argv, output, 'candidate'); commands.append(record)
    events = [json.loads(line) for line in raw.splitlines()]
    rows = [e for e in events if e.get('kind') == 'trim_candidate_frame']
    if [e['frame'] for e in rows] != list(range(len(cases))) or any(
            e['acceptance'] != 'unaccepted' or e['creative_trims_applied'] is not False or
            e['scope'] != 'rgb-sop-only' for e in rows):
        raise ValueError('candidate coverage or provenance mismatch')
    candidate_rgb = []
    for i, (event, parsed) in enumerate(zip(rows, conversions, strict=True)):
        check_selected(event, parsed)
        path = output / 'candidate' / f'frame-{i:03}.rgba32f'
        candidate_rgb.append(rgba32(path.read_bytes(), width, height))
        association[i].update(candidate_output=bind(path), candidate_event=event)
    candidate_xyz, reference_xyz = [xyz(p) for p in candidate_rgb], [xyz(p) for p in references]
    comparisons, patches = [], []
    for i, case in enumerate(cases):
        comparisons.append({'case': case['name'],
            'normalized_pq': comparison(candidate_rgb[i], references[i], candidate_rgb[0], references[0]),
            'linear_xyz_cd_m2': comparison(candidate_xyz[i], reference_xyz[i], candidate_xyz[0], reference_xyz[0])})
        for y in range(0, height, 16):
            for x in range(0, width, 16):
                row = {'case': case['name'], 'rectangle': [x, y, 16, 16]}
                for domain, arrays in [('pq', (candidate_rgb, references)),
                                       ('xyz_cd_m2', (candidate_xyz, reference_xyz))]:
                    c, r = arrays
                    row[domain] = {'candidate': patch_means(c[i], width, x, y),
                        'reference': patch_means(r[i], width, x, y),
                        'candidate_zero': patch_means(c[0], width, x, y),
                        'reference_zero': patch_means(r[0], width, x, y)}
                patches.append(row)
    report = {'schema': 'e-official-software-reference-observations-v1',
        'kind': 'official-dolby-software-reference-numerical-qc',
        'status': 'observations-only-not-acceptance', 'creative_trims_applied': False,
        'engineered_source': True, 'physical_device_or_photometry': False,
        'frozen_contract': frozen[0][2], 'rgb_to_xyz_matrix': XYZ,
        'frozen_metadata_except_levels': varying_levels,
        'frozen_metadata_xml_sha256': xml_baseline, 'frozen_metadata_rpu_sha256': rpu_baseline,
        'metadata_delta_scope': reference.get('metadata_delta_scope', 'L8-plus-officially-regenerated-companion-L2'),
        'metadata_authoring': authored, 'original_fixed_xml_sha256': original_fixed,
        'companion_l2_changed_cases': [a['case'] for a in association
            if a['actual_rpu_companion_l2'] != association[0]['actual_rpu_companion_l2']],
        'reference_quantization_step_normalized_pq': 1/65535,
        'reference_acquisition': bind(reference_root / 'RESULT.json'),
        'source_acquisition': bind(baseline_root / 'RESULT.json'),
        'source_archive': bind(args.source_archive), 'dependency_manifest': bind(args.dependency_manifest),
        'adapter': bind(__file__), 'metric_source': bind(Path(__file__).with_name('trim_software_metrics.py')),
        'pq_reference_source': bind(Path(__file__).with_name('annex_b2.py')),
        'binaries': {k: bind(p) for k, p in binaries.items()}, 'commands': commands,
        'matched_cases': len(cases), 'pixels_per_case': width*height, 'associations': association,
        'zero_baseline_error': {'normalized_pq': metrics(differences(candidate_rgb[0], references[0])),
                                'linear_xyz_cd_m2': metrics(differences(candidate_xyz[0], reference_xyz[0]))},
        'comparisons': comparisons, 'patches': patches,
        'limitations': ['SOP-only candidate; chroma/saturation/L3/detail/extended controls unapplied',
            'zero-reference has other nonneutral instructions; baseline error is separate from metadata delta',
            ('Metafier regenerated L2; recorded delta is not a causally isolated L8-only effect'
             if authored is None else 'L8-only authored XML input variation; finite cases do not establish an algorithm'),
            'delta comparison does not remove nonlinear interactions with baseline instructions',
            'supported public XML importer supplies actual RPU mapping, not Dolby algorithm equivalence',
            'source is decoded self-owned master RGB, not proof of encoded-DV reconstruction',
            'finite engineered software comparisons do not establish physical optics or full Dolby equivalence',
            'no fitting of weights, calibration curve or proprietary algorithm from these controls']}
    (output / 'observations.json').write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ('baseline-packet', 'reference-packet', 'converter', 'decoder', 'probe',
                'candidate', 'source-archive', 'dependency-manifest', 'output'):
        parser.add_argument('--' + key, required=True, type=Path)
    parser.add_argument('--parent-reference-packet', type=Path,
        help='original mixed packet, required for strict L8-only authored variants')
    report = observe(parser.parse_args())
    print(f"{report['status']}: {report['matched_cases']} software-reference cases")
