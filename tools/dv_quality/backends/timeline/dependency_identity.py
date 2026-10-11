"""Validate the exact approved FFmpeg prefix and fixture prerequisites."""
import hashlib
import json
from pathlib import Path
import sys

LOCK = Path(__file__).with_name('dependency-lock.json')


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def inventory(root, directory):
    root = Path(root)
    base = root / directory
    if base.is_symlink() or not base.is_dir():
        raise ValueError('dependency directory must be real')
    result = {}
    for path in sorted(base.rglob('*')):
        if path.is_symlink():
            raise ValueError('dependency symlink refused')
        if path.is_file():
            result[path.relative_to(root).as_posix()] = sha(path)
        elif not path.is_dir():
            raise ValueError('dependency special file refused')
    return result


def validate(replay, bundle=None):
    replay = Path(replay)
    lock = json.loads(LOCK.read_text())
    if inventory(replay, 'ffmpeg-prefix') != lock['ffmpeg_prefix']:
        raise ValueError('FFmpeg prefix differs from approved artifact identity')
    if bundle is not None:
        bundle = Path(bundle)
        for name, expected in lock['reviewed_evidence'].items():
            if sha(bundle / name) != expected:
                raise ValueError('approved receipt/inventory identity differs')
        if inventory(bundle, 'rpu-tags') != lock['rpu_tags']:
            raise ValueError('approved fixture identity differs')
        if sha(replay / 'ffmpeg-source.tar.gz') != lock['ffmpeg_archive_sha256']:
            raise ValueError('FFmpeg source archive identity differs')
    else:
        if inventory(replay, 'rpu-tags') != lock['rpu_tags']:
            raise ValueError('copied fixture identity differs')
    return {'status': 'passed', 'dependency_lock_sha256': sha(LOCK),
            'ffmpeg_revision': lock['ffmpeg_revision'],
            'reuse_policy': 'approved static prefix reused after full identity validation; new helpers compiled',
            'reviewed_evidence': lock['reviewed_evidence']}


if __name__ == '__main__':
    if len(sys.argv) not in (2, 3):
        raise SystemExit('usage: dependency_identity.py replay [reviewed-bundle]')
    print(json.dumps(validate(sys.argv[1], sys.argv[2] if len(sys.argv) == 3 else None),
                     indent=2, allow_nan=False))
