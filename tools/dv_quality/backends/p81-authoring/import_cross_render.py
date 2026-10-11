"""Import a bounded GPU evidence inventory into an explicitly new destination.
This copies evidence only. Run check_cross_render.py separately in a validation
root containing the matching authoring inputs and this cross-render directory.
"""
import argparse
import json
from pathlib import Path

BACKENDS = (('p81_export_probe', 'dv-on'), ('hdr10_baseline_probe', 'hdr10'))
STAGES = ('reconstruction', 'rendered')
RGB_BYTES = 64 * 64 * 6
YUV_SEQUENCE_BYTES = 4 * 64 * 64 * 3
TEXT_LIMIT_BYTES = 65536


def inventory():
    """Exactly 33 permitted files; no recursion into build/source artifacts."""
    files = [('four-frame-results.json', 'upstream-results.json', None)]
    for frame in range(4):
        for backend, label in BACKENDS:
            source = f'{backend}/frame-{frame}'
            target = f'frame-{frame}'
            for stage in STAGES:
                files.append((f'{source}/outputs/zero-{stage}.rgb48le',
                              f'{target}/{label}-{stage}.rgb48le', RGB_BYTES))
            files.append((f'{source}/probe.jsonl', f'{target}/{label}-probe.jsonl', None))
            files.append((f'{source}/decoded.yuv420p10le',
                          f'{target}/{label}-decoded-input.yuv420p10le', YUV_SEQUENCE_BYTES))
    return files


def import_evidence(source, destination):
    source = Path(source).resolve(strict=True)
    destination = Path(destination)
    if destination.exists() or destination.is_symlink():
        raise FileExistsError('destination must be new; existing evidence is never overwritten')
    if destination.resolve().is_relative_to(source):
        raise ValueError('destination must be outside the read-only GPU source tree')
    if not source.is_dir():
        raise ValueError('GPU source must be a directory')
    if not destination.parent.is_dir():
        raise FileNotFoundError('create the validation parent directory first')
    payloads = []
    # Validate/read the complete bounded inventory before creating destination.
    for source_name, target_name, expected_length in inventory():
        path = source / source_name
        if path.is_symlink() or not path.is_file():
            raise FileNotFoundError('required regular artifact missing: ' + source_name)
        if expected_length is None:
            if path.stat().st_size > TEXT_LIMIT_BYTES:
                raise ValueError('text artifact exceeds bounded size: ' + source_name)
        elif path.stat().st_size != expected_length:
            raise ValueError('artifact length differs: ' + source_name)
        limit = TEXT_LIMIT_BYTES if expected_length is None else expected_length
        with path.open('rb') as stream:
            data = stream.read(limit + 1)
        if len(data) > limit or (expected_length is not None and len(data) != expected_length):
            raise ValueError('artifact length changed during bounded read: ' + source_name)
        if expected_length is None:
            text = data.decode('utf-8')
            if source_name.endswith('.json'):
                json.loads(text)
            else:
                for line in text.splitlines():
                    if line:
                        json.loads(line)
        payloads.append((target_name, data))
    destination.mkdir(exist_ok=False)
    for target_name, data in payloads:
        target = destination / target_name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return {'copied_files': len(payloads), 'validation': 'not performed; run cross-render checker'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True, type=Path, help='Explicit GPU replay root')
    parser.add_argument('--destination', required=True, type=Path,
                        help='New cross-render directory under a separate validation root')
    args = parser.parse_args()
    print(json.dumps(import_evidence(args.source, args.destination)))


if __name__ == '__main__':
    main()
