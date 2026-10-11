"""Bounded EBML timeline inspection for these unlaced one-track fixtures only."""
import hashlib
import json
from pathlib import Path
import sys

MASTERS = {0x18538067, 0x1549A966, 0x1654AE6B, 0xAE, 0x1F43B675, 0xA0}


def vint(data, position, identifier=False):
    if position >= len(data) or data[position] == 0:
        raise ValueError('invalid EBML integer')
    width = 1
    mask = 0x80
    while not data[position] & mask:
        width += 1
        mask >>= 1
    if width > (4 if identifier else 8) or position + width > len(data):
        raise ValueError('invalid EBML integer width')
    value = int.from_bytes(data[position:position + width], 'big')
    if not identifier:
        value &= (1 << (7 * width)) - 1
        if value == (1 << (7 * width)) - 1:
            value = None
    return value, width


def inspect(path):
    data = Path(path).read_bytes()
    if len(data) > 1024 * 1024:
        raise ValueError('bounded fixture file size exceeded')
    result = {'file_sha256': hashlib.sha256(data).hexdigest(), 'timestamp_scale_ns': 1000000,
              'default_duration_ns': [], 'blocks': [], 'scope': 'bounded fixture EBML fields only'}
    count = 0

    def walk(start, end, context, depth=0):
        nonlocal count
        if depth > 6:
            raise ValueError('bounded EBML depth exceeded')
        position = start
        while position < end:
            identifier, id_width = vint(data, position, True)
            size, size_width = vint(data, position + id_width)
            payload = position + id_width + size_width
            finish = end if size is None else payload + size
            if finish > end or finish < payload:
                raise ValueError('EBML element bounds')
            count += 1
            if count > 4096:
                raise ValueError('bounded EBML element count exceeded')
            raw = data[payload:finish]
            if identifier in MASTERS:
                child = dict(context)
                if identifier == 0x1F43B675:
                    child = {'cluster_ticks': None}
                if identifier == 0xA0:
                    child['group_blocks'] = []
                    child['block_duration_ticks'] = None
                    child['duration_payload_offset'] = None
                    child['duration_payload_size'] = None
                walk(payload, finish, child, depth + 1)
                if identifier == 0xA0:
                    for block in child['group_blocks']:
                        block['block_duration_ticks'] = child['block_duration_ticks']
                        block['duration_payload_offset'] = child['duration_payload_offset']
                        block['duration_payload_size'] = child['duration_payload_size']
            elif identifier == 0x2AD7B1:
                result['timestamp_scale_ns'] = int.from_bytes(raw, 'big')
            elif identifier == 0x23E383:
                result['default_duration_ns'].append(int.from_bytes(raw, 'big'))
            elif identifier == 0xE7:
                context['cluster_ticks'] = int.from_bytes(raw, 'big')
            elif identifier == 0x9B:
                context['block_duration_ticks'] = int.from_bytes(raw, 'big')
                context['duration_payload_offset'] = payload
                context['duration_payload_size'] = len(raw)
            elif identifier in (0xA1, 0xA3):
                track, width = vint(raw, 0)
                if track != 1 or len(raw) < width + 3 or raw[width + 2] & 6:
                    raise ValueError('only one-track unlaced fixture blocks supported')
                if context.get('cluster_ticks') is None:
                    raise ValueError('cluster timestamp required')
                relative = int.from_bytes(raw[width:width + 2], 'big', signed=True)
                block = {'pts_ticks': context['cluster_ticks'] + relative,
                         'element': 'SimpleBlock' if identifier == 0xA3 else 'Block',
                         'block_duration_ticks': None, 'payload_sha256': hashlib.sha256(raw[width + 3:]).hexdigest()}
                result['blocks'].append(block)
                if identifier == 0xA1:
                    context['group_blocks'].append(block)
            position = finish

    walk(0, len(data), {})
    return result


if __name__ == '__main__':
    print(json.dumps(inspect(sys.argv[1]), indent=2, allow_nan=False))
