"""Two immutable crates.io packages for an opt-in, method-owned comparison."""

from contextlib import contextmanager
import hashlib
import io
from pathlib import Path, PurePosixPath
import tarfile
import tempfile
import urllib.error
import urllib.request

CRATES = {
    'hiqlite': (373845, '8711815c093414290a5fcbc0bf74e1e70e3d6ef37e21735000178d25cee6fcf0'),
    'hiqlite-wal': (33652, '247fc29e082f38fdf25270f6a5d7148284c9710384c3bce21636608dc104fddf'),
}
MAX_MEMBERS = 200
MAX_EXPANDED_BYTES = 2 * 1024 * 1024


class UpstreamFixtureError(RuntimeError):
    """Pinned source is unavailable or invalid; never substitute the fork."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise UpstreamFixtureError('Upstream package redirect refused')


def download_crate(name):
    size, expected = CRATES[name]
    url = f'https://static.crates.io/crates/{name}/{name}-0.14.0.crate'
    try:
        with urllib.request.build_opener(NoRedirect()).open(url, timeout=20) as response:
            raw = response.read(size + 1)
    except (urllib.error.URLError, OSError):
        raise UpstreamFixtureError(f'Pinned upstream {name} 0.14.0 download failed') from None
    if len(raw) != size or hashlib.sha256(raw).hexdigest() != expected:
        raise UpstreamFixtureError(f'Pinned upstream {name} 0.14.0 size/hash mismatch')
    return raw


def extract_crate(raw, name, root):
    """Validate the complete bounded inventory before writing regular files."""
    prefix = f'{name}-0.14.0'
    try:
        with tarfile.open(fileobj=io.BytesIO(raw), mode='r:gz') as archive:
            members, names, total = [], set(), 0
            for member in archive:
                parts = PurePosixPath(member.name).parts
                total += member.size
                if (len(members) >= MAX_MEMBERS or total > MAX_EXPANDED_BYTES
                        or member.size < 0 or not member.isfile() or len(parts) < 2
                        or parts[0] != prefix or '..' in parts
                        or '\\' in member.name or ':' in member.name
                        or str(PurePosixPath(member.name)) != member.name
                        or member.name in names):
                    raise UpstreamFixtureError('Unsafe or oversized upstream package inventory')
                names.add(member.name)
                members.append(member)
            if not members:
                raise UpstreamFixtureError('Empty upstream package')
            for member in members:
                path = root / member.name
                path.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                if source is None:
                    raise UpstreamFixtureError('Upstream package member unavailable')
                with source, path.open('xb') as destination:
                    raw = source.read(member.size + 1)
                    if len(raw) != member.size:
                        raise UpstreamFixtureError('Truncated upstream package member')
                    destination.write(raw)
    except (tarfile.TarError, EOFError, OSError):
        raise UpstreamFixtureError('Pinned upstream package extraction failed') from None


@contextmanager
def pinned_upstream():
    """Own only this temporary directory through method completion or failure."""
    with tempfile.TemporaryDirectory(prefix='plurx-hiqlite-upstream-') as directory:
        root = Path(directory)
        for name in CRATES:
            extract_crate(download_crate(name), name, root)
        yield root
