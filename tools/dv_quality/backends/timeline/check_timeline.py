"""Known synthetic timeline invariants; not a general playback validity adapter."""
from fractions import Fraction
import hashlib
import json
import math
from pathlib import Path
import sys
import inspect_matroska


class TimelineError(ValueError):
    pass


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def finite_float(text):
    value = float(text)
    if not math.isfinite(value):
        raise TimelineError('nonfinite or overflow JSON number')
    return value


def read_json(path):
    def refuse(value):
        raise TimelineError('nonfinite JSON constant')
    return json.loads(Path(path).read_text(), parse_constant=refuse, parse_float=finite_float)


def events(path):
    return [read_json_text(line) for line in Path(path).read_text().splitlines()]


def read_json_text(text):
    def refuse(value):
        raise TimelineError('nonfinite JSON constant')
    return json.loads(text, parse_constant=refuse, parse_float=finite_float)


def require(ok, reason):
    if not ok:
        raise TimelineError(reason)


def container(root, name, declared_ms):
    parsed = inspect_matroska.inspect(root / name)
    require(parsed['timestamp_scale_ns'] == 1000000, 'fixture timestamp scale differs')
    require(sorted(block['pts_ticks'] for block in parsed['blocks']) == declared_ms,
            'stored container PTS differ from authored timeline')
    require(parsed['default_duration_ns'] == [40000000], 'stored presentation interval unspecified')
    intervals = {value: (declared_ms[i + 1] - value if i + 1 < len(declared_ms) else 41)
                 for i, value in enumerate(declared_ms)}
    for block in parsed['blocks']:
        duration = block['block_duration_ticks']
        expected = intervals[block['pts_ticks']]
        if expected == 40:
            require(duration is None, 'authored40ms default duration differs')
        else:
            require(duration == expected, 'stored BlockDuration differs from actual PTS gap')
    return parsed, intervals


def source_probe(root, name, declared, evidence):
    data = read_json(root / name)
    stream = data['streams'][0]
    require(stream['profile'] == 'Main 10' and stream['pix_fmt'] == 'yuv420p10le' and
            stream['width'] == 64 and stream['height'] == 64 and stream['time_base'] == '1/1000',
            'actual encoded source domain differs')
    packets = [row for row in data['packets_and_frames'] if row['type'] == 'packet']
    display = [row for row in data['packets_and_frames'] if row['type'] == 'frame']
    require([row['pts'] for row in display] == declared, 'actual encoder display PTS differ')
    require(len(packets) == len(declared), 'actual encoder packet count differs')
    actual_path = root / (name.removesuffix('.ffprobe.json') + '.mkv')
    actual = inspect_matroska.inspect(actual_path)
    require([b['pts_ticks'] for b in actual['blocks']] == [p['pts'] for p in packets] and
            actual['timestamp_scale_ns'] == 1000000, 'encoder probe differs from actual EBML PTS')
    evidence[name] = {'probe_sha256': sha(root / name), 'container': actual}
    return packets


def observe_known_frames(root, folder, rows, wanted):
    by_sha = {layer: {digest: source for (kind, source), digest in wanted.items() if kind == layer}
              for layer in ('bl', 'el')}
    rpu_by_sha = {sha(root / f'rpu-tags/p7-frame{i}.nal'): i for i in range(6)}
    observed = []
    for row in rows:
        if row['kind'] != 'decoded_frame':
            continue
        epoch, layer, emission = row['epoch'], row['layer'], row['display_emission']
        require(layer in ('bl', 'el') and type(epoch) is int and type(emission) is int,
                'unknown decoded frame labels')
        artifact = folder / f'frames/epoch-{epoch}-{layer}-frame-{emission:03d}.yuv420p10le'
        digest = sha(artifact)
        require(digest in by_sha[layer], 'unknown decoded native picture identity')
        require(Fraction(row['duration']) > 0 and row['best_effort_pts'] == row['pts'],
                'invalid actual decoded timing')
        item = {'epoch': epoch, 'layer': layer, 'pts': row['pts'], 'duration': row['duration'],
                'display_emission': emission, 'actual_source_picture': by_sha[layer][digest],
                'pixels_sha256': digest}
        if layer == 'bl':
            require(row['decoder_rpu_present'] is True and row['decoder_metadata_present'] is True,
                    'fresh RPU evidence absent; legitimate reuse not covered')
            metadata = folder / f'frames/epoch-{epoch}-bl-frame-{emission:03d}.rpu.nal'
            rpu_hash = sha(metadata)
            require(rpu_hash in rpu_by_sha, 'unknown decoded raw RPU identity')
            rpu_source = rpu_by_sha[rpu_hash]
            require(row['l1_min'] == 0 and row['l1_max'] == 4095 - rpu_source and row['l1_average'] == 2048 - rpu_source,
                    'parsed metadata disagrees with actual raw RPU')
            item.update(rpu_sha256=rpu_hash, actual_rpu_source_picture=rpu_source)
        else:
            require(row['decoder_rpu_present'] is False and row['decoder_metadata_present'] is False,
                    'unexpected EL metadata side data')
        observed.append(item)
    return observed


def validate_trace(rows, case):
    """Recognize the actual bounded driver lifecycle, not just event membership."""
    rational = lambda value: type(value) is str and Fraction(value).denominator > 0
    integer = lambda value: type(value) is int
    boolean = lambda value: type(value) is bool
    text = lambda value: type(value) is str
    optional_time = lambda value: value is None or rational(value)
    schemas = {
        'environment': {'ffmpeg_version': text, 'libavcodec_version': integer},
        'stream_timing_context': {key: text if key == 'phase' else rational
                                  for key in ('phase', 'time_base', 'r_frame_rate', 'avg_frame_rate', 'codec_framerate')},
        'epoch_start': {'epoch': integer, 'context_id': integer, 'fresh_contexts': boolean},
        'demux_packet': {'epoch': integer, 'coded_arrival': integer, 'pts': rational,
                         'dts': optional_time, 'duration': rational, 'keyframe': boolean},
        'split_packet': {'epoch': integer, 'layer': text, 'coded_arrival': integer,
                         'pts': rational, 'dts': optional_time, 'duration': rational},
        'decoded_frame': {'epoch': integer, 'layer': text, 'context_id': integer, 'display_emission': integer,
                          'pts': rational, 'best_effort_pts': rational, 'pkt_dts': optional_time,
                          'duration': rational, 'picture_type': text, 'decoder_rpu_present': boolean,
                          'decoder_metadata_present': boolean, 'l1_min': integer, 'l1_max': integer, 'l1_average': integer},
        'read_boundary': {'epoch': integer, 'packets': integer, 'eof': boolean},
        'drain': {'epoch': integer, 'bl_frames': integer, 'el_frames': integer},
    }
    for row in rows:
        require(type(row) is dict and type(row.get('kind')) is str, 'invalid trace event type')
        kind = row['kind']
        if kind == 'decoded_frame':
            require(row.get('picture_type') in ('I', 'P', 'B'), 'unknown decoded picture type')
        if kind == 'epoch_boundary':
            schema = {'from': integer, 'to': integer, 'reason': text, 'reset_applied': boolean}
            if row.get('reason') == 'actual_demux_seek':
                schema.update(seek_result=integer, target=rational, bsf_flush_calls=integer, decoder_flush_calls=integer)
            else:
                schema.update(fresh_contexts=boolean, context_id=integer)
        else:
            require(kind in schemas, 'unknown trace event type')
            schema = schemas[kind]
        require(set(row) == {'kind', *schema}, 'trace event fields differ')
        require(all(check(row[key]) for key, check in schema.items()), 'trace event field type differs')
    position = 0
    counts = {}

    def take(kind, **fields):
        nonlocal position
        require(position < len(rows) and rows[position]['kind'] == kind and
                all(rows[position][key] == value for key, value in fields.items()),
                'illegal lifecycle event order or identity')
        row = rows[position]
        position += 1
        return row

    def frames(epoch, layer):
        nonlocal position
        key = epoch, layer
        while position < len(rows) and rows[position]['kind'] == 'decoded_frame':
            row = rows[position]
            if row['layer'] != layer:
                break
            take('decoded_frame', epoch=epoch, layer=layer, display_emission=counts.get(key, 0))
            counts[key] = counts.get(key, 0) + 1
            require(counts[key] <= 6, 'decoded lifecycle frame cap exceeded')

    def read(epoch, packets, eof):
        for arrival in range(packets):
            take('demux_packet', epoch=epoch, coded_arrival=arrival)
            for layer in ('bl', 'el'):
                take('split_packet', epoch=epoch, layer=layer, coded_arrival=arrival)
                frames(epoch, layer)
        take('read_boundary', epoch=epoch, packets=packets, eof=eof)

    def drain(epoch):
        for layer in ('bl', 'el'):
            frames(epoch, layer)
        take('drain', epoch=epoch, bl_frames=counts.get((epoch, 'bl'), 0),
             el_frames=counts.get((epoch, 'el'), 0))

    take('environment')
    take('stream_timing_context', phase='initial_open')
    take('epoch_start', epoch=0, context_id=0, fresh_contexts=True)
    if case.startswith('seek-') or case == 'implicit-seek':
        read(0, 2, False)
        take('stream_timing_context', phase='after_seek')
        take('epoch_boundary', **{'from': 0, 'to': 1, 'reason': 'actual_demux_seek'})
        read(1, 3, True)
        drain(1)
    elif case.startswith('normal-') or case == 'swapped-rpu':
        read(0, 6, True)
        drain(0)
    else:
        read(0, 3, True)
        if case != 'epochs-no-reset':
            drain(0)
        take('stream_timing_context', phase='second_segment_open')
        take('epoch_boundary', **{'from': 0, 'to': 1, 'reason': 'new_independent_segment_timestamp_reset'})
        read(1, 3, True)
        drain(1)
    require(position == len(rows), 'duplicate or trailing lifecycle event')


def inspect_case(root, case):
    root = Path(root)
    folder = root / case
    require((folder / 'decode-status.txt').read_text() == '0\n' and
            not (folder / 'decode.stderr').read_text(), 'unexpected decoder exit/diagnostic')
    rows = events(folder / 'decode.jsonl')
    validate_trace(rows, case)
    allowed = {'environment', 'epoch_start', 'stream_timing_context', 'demux_packet', 'split_packet',
               'decoded_frame', 'read_boundary', 'epoch_boundary', 'drain'}
    require(all(row['kind'] in allowed for row in rows), 'unknown decoder evidence event')
    starts = [row for row in rows if row['kind'] == 'epoch_start']
    require(starts == [{'kind': 'epoch_start', 'epoch': 0, 'context_id': 0, 'fresh_contexts': True}],
            'initial decoder epoch identity differs')
    for layer in ('bl', 'el'):
        demux = [row for row in rows if row['kind'] == 'demux_packet']
        split = [row for row in rows if row['kind'] == 'split_packet' and row['layer'] == layer]
        require(len(split) == len(demux) and all(all(a[key] == b[key] for key in ('epoch', 'pts', 'dts', 'duration'))
                    for a, b in zip(split, demux)), 'split actual timing differs')
    require(rows[0]['kind'] == 'environment', 'decoder environment absent')
    definition = read_json(root / 'source-definition.json')
    wanted = {(item['layer'], item['source_picture']): item['sha256'] for item in definition['frames']}
    observed = observe_known_frames(root, folder, rows, wanted)
    require(definition['vfr_pts_ms'] == [0, 40, 110, 140, 230, 300] and
            definition['segment_pts_ms'] == [0, 40, 110] and definition['terminal_interval_ms'] == 41,
            'declared synthetic timeline differs')
    encoder_evidence = {}
    full_pts = definition['vfr_pts_ms']
    segment_pts = definition['segment_pts_ms']
    normal = case.startswith('normal-') or case == 'swapped-rpu'
    seek = case.startswith('seek-') or case == 'implicit-seek'
    bframes = 0 if case == 'normal-b0' else 2
    expected_inputs = {}
    contexts = {}
    if normal or seek:
        name = 'compound-implicit.mkv' if case == 'implicit-seek' else (
            'compound-swapped-rpu.mkv' if case == 'swapped-rpu' else f'compound-vfr-b{bframes}.mkv')
        packets = source_probe(root, f'bl-vfr-b{bframes}.ffprobe.json', full_pts, encoder_evidence)
        other = source_probe(root, f'el-vfr-b{bframes}.ffprobe.json', full_pts, encoder_evidence)
        parsed, intervals = container(root, name, full_pts)
        require([p['pts'] for p in packets] == [p['pts'] for p in other], 'encoder layer timing differs')
        require([b['pts_ticks'] for b in parsed['blocks']] == [p['pts'] for p in packets],
                'compound coded order differs from actual encoder order')
        if normal:
            expected_inputs[0] = [(p['pts'], intervals[p['pts']]) for p in packets]
            contexts[0] = parsed
            epoch_sources = {0: {Fraction(value, 1000): i for i, value in enumerate(full_pts)}}
        else:
            expected_inputs[0] = [(p['pts'], intervals[p['pts']]) for p in packets[:2]]
            expected_inputs[1] = [(p['pts'], intervals[p['pts']]) for p in packets if p['pts'] >= 140]
            contexts[0] = contexts[1] = parsed
            boundary = [row for row in rows if row['kind'] == 'epoch_boundary']
            require(len(boundary) == 1 and boundary[0]['reason'] == 'actual_demux_seek' and
                    type(boundary[0]['seek_result']) is int and boundary[0]['seek_result'] == 0 and
                    boundary[0]['from'] == 0 and boundary[0]['to'] == 1 and boundary[0]['target'] == '200/1000',
                    'actual seek evidence differs')
            require(boundary[0]['reset_applied'] is True and boundary[0]['bsf_flush_calls'] == 2 and
                    type(boundary[0]['bsf_flush_calls']) is int and type(boundary[0]['decoder_flush_calls']) is int and
                    boundary[0]['decoder_flush_calls'] == 2, 'explicit epoch reset absent')
            require(expected_inputs[1][0][0] == 140, 'known seek keyframe differs')
            epoch_sources = {0: {}, 1: {Fraction(value, 1000): i for i, value in enumerate(full_pts) if value >= 140}}
    else:
        for epoch in (0, 1):
            name = 'compound-cross-epoch-el.mkv' if case == 'cross-epoch-el' and epoch == 1 else f'compound-segment{epoch}.mkv'
            parsed, intervals = container(root, name, segment_pts)
            packets = source_probe(root, f'bl-segment{epoch}.ffprobe.json', segment_pts, encoder_evidence)
            el_source = 0 if case == 'cross-epoch-el' and epoch == 1 else epoch
            other = source_probe(root, f'el-segment{el_source}.ffprobe.json', segment_pts, encoder_evidence)
            require([p['pts'] for p in packets] == [p['pts'] for p in other], 'encoder layer timing differs')
            require([b['pts_ticks'] for b in parsed['blocks']] == [p['pts'] for p in packets],
                    'compound coded order differs from actual encoder order')
            expected_inputs[epoch] = [(p['pts'], intervals[p['pts']]) for p in packets]
            contexts[epoch] = parsed
        boundary = [row for row in rows if row['kind'] == 'epoch_boundary']
        require(len(boundary) == 1 and boundary[0]['reason'] == 'new_independent_segment_timestamp_reset' and
                boundary[0]['from'] == 0 and boundary[0]['to'] == 1,
                'discontinuity boundary absent')
        require(boundary[0]['reset_applied'] is True and boundary[0]['fresh_contexts'] is True and
                boundary[0]['context_id'] == 1, 'explicit epoch reset absent')
        epoch_sources = {epoch: {Fraction(value, 1000): 3 * epoch + i for i, value in enumerate(segment_pts)}
                         for epoch in (0, 1)}
    expected_reads = ([{'kind': 'read_boundary', 'epoch': 0, 'packets': 2, 'eof': False},
                       {'kind': 'read_boundary', 'epoch': 1, 'packets': 3, 'eof': True}] if seek else
                      [{'kind': 'read_boundary', 'epoch': 0, 'packets': 6, 'eof': True}] if normal else
                      [{'kind': 'read_boundary', 'epoch': epoch, 'packets': 3, 'eof': True} for epoch in (0, 1)])
    require([row for row in rows if row['kind'] == 'read_boundary'] == expected_reads,
            'actual reader boundary differs')
    for row in rows:
        if row['kind'] in ('demux_packet', 'split_packet', 'decoded_frame', 'drain', 'read_boundary'):
            require(row['epoch'] in expected_inputs, 'unexpected timeline epoch')
    for epoch, expected in expected_inputs.items():
        packets = [row for row in rows if row['kind'] == 'demux_packet' and row['epoch'] == epoch]
        require([(Fraction(p['pts']), Fraction(p['duration'])) for p in packets] ==
                [(Fraction(pts, 1000), Fraction(duration, 1000)) for pts, duration in expected],
                'actual demux PTS/duration differ from stored authored intervals')
        for layer in ('bl', 'el'):
            split = [row for row in rows if row['kind'] == 'split_packet' and row['epoch'] == epoch and row['layer'] == layer]
            require(len(split) == len(packets), 'split count differs')
            require(all(all(a[key] == b[key] for key in ('pts', 'dts', 'duration')) for a, b in zip(split, packets)),
                    'split actual timing differs')
        if seek and epoch == 1:
            require(packets[0]['keyframe'] is True, 'seek did not start at actual keyframe')
    if normal:
        require(not any(row['kind'] == 'epoch_boundary' for row in rows), 'unexpected normal epoch boundary')
    accepted = []
    for epoch, source_by_pts in epoch_sources.items():
        layers = {}
        for layer in ('bl', 'el'):
            frames = [row for row in rows if row['kind'] == 'decoded_frame' and row['epoch'] == epoch and row['layer'] == layer]
            require([Fraction(frame['pts']) for frame in frames] == sorted(source_by_pts),
                    'decoded frame timeline/epoch differs')
            require([frame['display_emission'] for frame in frames] == list(range(len(frames))),
                    'decoded emission identity differs')
            layers[layer] = {Fraction(frame['pts']): frame for frame in frames}
        for pts, source in sorted(source_by_pts.items()):
            bl, el = layers['bl'][pts], layers['el'][pts]
            expected_duration = dict(expected_inputs[epoch])[int(pts * 1000)]
            pair = {'epoch': epoch, 'source_picture': source, 'pts': bl['pts'], 'duration': bl['duration']}
            for layer, frame in [('bl', bl), ('el', el)]:
                require(Fraction(frame['duration']) == Fraction(expected_duration, 1000) and frame['best_effort_pts'] == frame['pts'],
                        'decoded duration/PTS differs from actual demux')
                require(frame['context_id'] == (epoch if not normal and not seek else 0),
                        'decoder context identity crosses epoch')
                pixels = folder / f"frames/epoch-{epoch}-{layer}-frame-{frame['display_emission']:03d}.yuv420p10le"
                require(sha(pixels) == wanted[(layer, source)], f'decoded {layer.upper()} identity disagrees with epoch source')
                pair[f'{layer}_sha256'] = sha(pixels)
            require(bl['decoder_rpu_present'] is True and bl['decoder_metadata_present'] is True,
                    'fresh RPU evidence absent; legitimate reuse not covered')
            metadata = folder / f"frames/epoch-{epoch}-bl-frame-{bl['display_emission']:03d}.rpu.nal"
            require(sha(metadata) == sha(root / f'rpu-tags/p7-frame{source}.nal') and
                    bl['l1_max'] == 4095 - source and bl['l1_average'] == 2048 - source,
                    'decoded RPU identity disagrees with epoch source')
            pair['rpu_sha256'] = sha(metadata)
            pair['presentation_role'] = 'preroll' if seek and pts < Fraction(200, 1000) else 'selected-display'
            accepted.append(pair)
    return {'case': case, 'accepted': True, 'pairs': accepted,
            'source_definition_sha256': sha(root / 'source-definition.json'),
            'decode_trace_sha256': sha(folder / 'decode.jsonl'),
            'container_evidence': contexts, 'encoder_evidence': encoder_evidence,
            'observed_frames': observed, 'full_playback_qualified': False}


EXPECTED_NEGATIVES = {
    'seek-no-reset': 'explicit epoch reset absent',
    'epochs-no-reset': 'explicit epoch reset absent',
    'cross-epoch-el': 'decoded EL identity disagrees with epoch source',
    'swapped-rpu': 'decoded RPU identity disagrees with epoch source',
    'implicit-seek': 'stored presentation interval unspecified',
}


def assess(root, case):
    try:
        result = inspect_case(root, case)
    except TimelineError as error:
        if case not in EXPECTED_NEGATIVES or str(error) != EXPECTED_NEGATIVES[case]:
            raise
        folder = Path(root) / case
        definition = read_json(Path(root) / 'source-definition.json')
        wanted = {(item['layer'], item['source_picture']): item['sha256'] for item in definition['frames']}
        observed = observe_known_frames(Path(root), folder, events(folder / 'decode.jsonl'), wanted)
        return {'case': case, 'accepted': False, 'stage': 'fixture-association',
                'observed_frames': observed,
                'reason': str(error), 'decode_trace_sha256': sha(Path(root) / case / 'decode.jsonl'),
                'full_playback_qualified': False}
    require(case not in EXPECTED_NEGATIVES, 'negative timeline unexpectedly admitted')
    return result


if __name__ == '__main__':
    root = Path(sys.argv[1])
    cases = ['normal-b0', 'normal-b2', 'seek-reset', 'epochs-reset', *EXPECTED_NEGATIVES]
    print(json.dumps({'scope': 'bounded synthetic timelines only',
                      'results': [assess(root, case) for case in cases]}, indent=2, allow_nan=False))
