"""Fetch the exact public libdovi source, without Git history or credentials."""
from pathlib import Path
import hashlib
import io
import tarfile
import urllib.request
ROOT = Path(__file__).resolve().parent
REVISION = '83e1fdad6dcd5995556235946e7c5c0f9010d5a1'
EXPECTED = '57d13f03d04a7a1b3d7dcb139b976dc3748c745c1720d4b6e44b61e5ef67b69b'
URL = 'https://codeload.github.com/quietvoid/dovi_tool/tar.gz/' + REVISION
if (ROOT/'libdovi-source').exists():
    raise ValueError('fresh source directory required')
data = urllib.request.urlopen(URL, timeout=60).read()
if hashlib.sha256(data).hexdigest() != EXPECTED:
    raise ValueError('source archive digest mismatch')
with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
    archive.extractall(ROOT, filter='data')
source = ROOT/('dovi_tool-' + REVISION)
source.rename(ROOT/'libdovi-source')
source = ROOT/'libdovi-source'
with (source/'Cargo.toml').open('a') as f:
    f.write('\n[workspace]\nmembers = ["dolby_vision"]\n')
(source/'Cargo.lock').write_bytes((ROOT/'resolved-Cargo.lock').read_bytes())
(source/'dolby_vision/examples/m0_authoring.rs').write_bytes((ROOT/'generate_frame_rpus.rs').read_bytes())
