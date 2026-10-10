"""Bind copied dependencies to the specifically reviewed prerequisite artifact set."""
import hashlib
import json
from pathlib import Path
import sys
LOCK_PATH = Path(__file__).with_name('prerequisite-lock.json')

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def inventory(root):
    result = {}
    for directory in ('prefix', 'include', 'lib'):
        base = root / directory
        if base.is_symlink() or not base.is_dir():
            raise ValueError('dependency directory must be a real directory')
        for path in sorted(base.rglob('*')):
            name = path.relative_to(root).as_posix()
            if path.is_symlink():
                result[name] = {'symlink': str(path.readlink())}
            elif path.is_file():
                result[name] = {'sha256': sha(path)}
            elif not path.is_dir():
                raise ValueError('dependency special file refused')
    return result

def validate(root, reviewed_bundle=None):
    root = Path(root)
    lock = json.loads(LOCK_PATH.read_text())
    if inventory(root) != lock['copied_artifacts']:
        raise ValueError('copied dependency identity differs from reviewed artifacts')
    if reviewed_bundle is not None:
        reviewed_bundle = Path(reviewed_bundle)
        for name, expected in lock['reviewed_evidence'].items():
            path = reviewed_bundle / name
            if path.is_symlink() or not path.is_file() or sha(path) != expected:
                raise ValueError('reviewed prerequisite evidence identity differs')
        receipt = json.loads((reviewed_bundle / 'receipt.json').read_text())
        source = json.loads((reviewed_bundle / 'source-lock.json').read_text())
        if receipt['libplacebo_revision'] != lock['libplacebo_revision'] or receipt['libdovi_revision'] != lock['libdovi_revision'] or source['libplacebo'] != lock['libplacebo_revision'] or (source['libdovi']['sha'] != lock['libdovi_revision']):
            raise ValueError('reviewed prerequisite source revision differs')
        for name, expected in lock['source_evidence'].items():
            path = root / name
            if path.is_symlink() or not path.is_file() or sha(path) != expected:
                raise ValueError('prerequisite source archive/lock identity differs')
    return {'schema': 1, 'status': 'passed', 'scope': 'reviewed prerequisite artifact identity', 'lock_sha256': sha(LOCK_PATH), 'libplacebo_revision': lock['libplacebo_revision'], 'libdovi_revision': lock['libdovi_revision'], 'reviewed_evidence': lock['reviewed_evidence'], 'copied_artifacts': lock['copied_artifacts'], 'source_evidence': lock['source_evidence'], 'limitation': 'reviewed artifact set only; a different dependency build requires a new reviewed lock'}
if __name__ == '__main__':
    if len(sys.argv) not in (2, 3):
        raise SystemExit('usage: prerequisite_identity.py dependency-root [reviewed-parsed-bundle]')
    result = validate(sys.argv[1], sys.argv[2] if len(sys.argv) == 3 else None)
    print(json.dumps(result, indent=2, allow_nan=False))
