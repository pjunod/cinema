"""Conservative invalidation of the reviewed affected-core execution boundary."""
import hashlib
import json
from pathlib import Path


def input_digest(root):
    root = Path(root)
    paths = {root / name for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')}
    for directory in ('crates/plurx-core', 'vendor/hiqlite', '.cargo'):
        for path in (root / directory).rglob('*'):
            relative = path.relative_to(root)
            if any(part in ('target', '.git', '__pycache__') for part in relative.parts):
                continue
            if path.is_file() and (path.suffix in ('.rs', '.sql', '.toml', '.json', '.lock')
                                   or path.name == 'Cargo.lock'):
                paths.add(path)
    digest = hashlib.sha256()
    for path in sorted(paths):
        if path.is_symlink() or not path.is_file():
            raise ValueError('reviewed core input is not a regular file: ' + str(path))
        digest.update(str(path.relative_to(root)).encode() + b'\0')
        digest.update(path.read_bytes())
        digest.update(b'\0')
    return {'sha256': digest.hexdigest(), 'input_count': len(paths)}


def compare_scope(root, review_path):
    reviewed = json.loads(Path(review_path).read_text())
    actual = input_digest(root)
    return {'reviewed_sha256': reviewed['sha256'], 'actual_sha256': actual['sha256'],
            'input_count': actual['input_count'],
            'matches': reviewed['sha256'] == actual['sha256']}
