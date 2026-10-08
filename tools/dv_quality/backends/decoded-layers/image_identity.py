"""Validate a prerequisite replay's local Linux ARM64 Docker image identity."""
import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

def read_identity(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise ValueError('image-id.txt must be a regular file')
    payload = path.read_bytes()
    if len(payload) > 72:
        raise ValueError('image identity exceeds digest length')
    value = payload.decode('ascii')
    if not re.fullmatch('sha256:[0-9a-f]{64}\\n?', value):
        raise ValueError('image identity must be a lowercase SHA256 digest')
    return value.rstrip('\n')

def inspect_identity(path):
    image_id = read_identity(path)
    result = subprocess.run(['docker', 'image', 'inspect', image_id, '--format', '{{.Id}}|{{.Os}}|{{.Architecture}}'], check=True, capture_output=True, text=True)
    if result.stdout.strip() != f'{image_id}|linux|arm64':
        raise ValueError('local image identity/platform differs from prerequisite')
    return image_id

def record_identity(scratch, stage, expected):
    scratch = Path(scratch)
    actual = read_identity(scratch / 'image-id.txt')
    if actual != expected:
        raise ValueError('propagated identity differs from the executed image')
    receipt = {'schema': 1, 'stage': stage, 'image_id': actual, 'platform': 'linux/arm64', 'status': 'passed', 'dependency_resolution': 'prior replay image; live apt/pip may drift'}
    library = scratch / 'prefix/lib/aarch64-linux-gnu/libplacebo.so.374'
    if library.is_file():
        receipt['libplacebo_library_sha256'] = hashlib.sha256(library.read_bytes()).hexdigest()
    with (scratch / f'{stage}-build-identity.json').open('x') as output:
        output.write(json.dumps(receipt, indent=2, allow_nan=False) + '\n')
if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    read = commands.add_parser('read')
    read.add_argument('path', type=Path)
    record = commands.add_parser('record')
    record.add_argument('scratch', type=Path)
    record.add_argument('stage', choices=('parsed', 'cross', 'encode', 'chroma'))
    record.add_argument('expected')
    args = parser.parse_args()
    if args.command == 'read':
        print(inspect_identity(args.path))
    else:
        record_identity(args.scratch, args.stage, args.expected)
