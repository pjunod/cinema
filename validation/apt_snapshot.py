"""Prefer HTTPS for pinned Debian snapshot source URLs."""
import argparse
import os
from pathlib import Path
import stat
import sys
import tempfile


def prefer_snapshot_https(source_root=Path('/etc/apt')):
    """Change only snapshot transport in APT's recognized source-list files."""
    source_root = Path(source_root)
    source_parts = source_root / 'sources.list.d'
    if source_root.is_symlink() or source_parts.is_symlink():
        raise ValueError('APT source directories must not be symlinks')
    paths = [source_root / 'sources.list']
    paths += sorted(source_parts.glob('*.list')) + sorted(source_parts.glob('*.sources'))
    changed = 0
    for path in paths:
        if path.is_symlink():
            raise ValueError('APT source file must not be a symlink')
        if not path.exists():
            continue
        if not path.is_file():
            raise ValueError('APT source file must be regular')
        original = path.read_bytes()
        rewritten = original.replace(b'http://snapshot.debian.org/', b'https://snapshot.debian.org/')
        if original == rewritten:
            continue
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=path.parent, prefix='.' + path.name + '.', delete=False) as stream:
                temporary = Path(stream.name)
                stream.write(rewritten)
            temporary.chmod(stat.S_IMODE(path.stat().st_mode))
            os.replace(temporary, path)
            temporary = None
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
        changed += 1
    return changed


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--https-sources-only', action='store_true', required=True)
    parser.parse_args(argv)
    try:
        print('APT snapshot source files switched to HTTPS: ' + str(prefer_snapshot_https()))
        return 0
    except (OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 100


if __name__ == '__main__':
    sys.exit(main())
