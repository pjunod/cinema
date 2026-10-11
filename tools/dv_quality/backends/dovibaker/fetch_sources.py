"""Fetch public source-only archives; no checkout history or credentials."""
import hashlib
import io
import json
from pathlib import Path
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent
SOURCE_NAMES = {
    'erazortt/DoViBaker': 'DoViBaker',
    'quietvoid/dovi_tool': 'dovi_tool',
    'sekrit-twc/timecube': 'timecube',
    'sekrit-twc/graphengine': 'graphengine',
}


def fetch(pin, source_dir):
    name = SOURCE_NAMES[pin['repo']]
    destination = source_dir / name
    if destination.exists():
        raise RuntimeError(f'Refusing existing source extraction: {name}')
    url = f"https://codeload.github.com/{pin['repo']}/tar.gz/{pin['sha']}"
    with urllib.request.urlopen(url, timeout=60) as response:
        archive = response.read()
    if hashlib.sha256(archive).hexdigest() != pin['archive_sha256']:
        raise ValueError(f'Archive hash mismatch: {name}')
    with tarfile.open(fileobj=io.BytesIO(archive), mode='r:gz') as archive_file:
        top = archive_file.getmembers()[0].name.split('/')[0]
        archive_file.extractall(source_dir, filter='data')
    (source_dir / top).rename(destination)


def prepare():
    source_dir = ROOT / 'src'
    source_dir.mkdir(exist_ok=True)
    pins = json.loads((ROOT / 'dependency-pins.json').read_text())
    if {pin['repo'] for pin in pins} != set(SOURCE_NAMES):
        raise ValueError('Unexpected public source dependency set')
    for pin in pins:
        fetch(pin, source_dir)
    for name, parent in [
        ('dovi_tool', 'DoViBaker'),
        ('timecube', 'DoViBaker'),
        ('graphengine', 'timecube'),
    ]:
        link = source_dir / parent / name
        if link.exists():
            link.rmdir()
        link.symlink_to('../' + name, target_is_directory=True)
    # Recorded build-only workspace change; generated lock pins the changed graph.
    cargo = source_dir / 'dovi_tool/Cargo.toml'
    cargo.write_text(cargo.read_text() + '\n[workspace]\nmembers = ["dolby_vision"]\n')
    (source_dir / 'dovi_tool/Cargo.lock').write_bytes((ROOT / 'resolved-Cargo.lock').read_bytes())
    examples = source_dir / 'dovi_tool/dolby_vision/examples'
    examples.mkdir(exist_ok=True)
    (examples / 'm0_generate.rs').write_bytes((ROOT / 'm0_generate.rs').read_bytes())


if __name__ == '__main__':
    prepare()
