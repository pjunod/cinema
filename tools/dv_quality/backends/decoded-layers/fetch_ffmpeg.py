"""Fetch checksum-verified pinned FFmpeg source; never a checkout or credential."""
import hashlib, shutil, tarfile, urllib.request
from pathlib import Path
import sys
PIN = 'bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa'
SHA = 'fb1931fd4eb29297ee1c1017a24f800c4d8fbea35b4f2aaeb28308a48a9149b4'
root = Path(sys.argv[1]).resolve()
archive = root / 'ffmpeg-source.tar.gz'
with urllib.request.urlopen(f'https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/{PIN}', timeout=60) as response:
    data = response.read(40 * 1024 * 1024 + 1)
if len(data) > 40 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != SHA:
    raise ValueError('FFmpeg source hash/size differs')
archive.write_bytes(data)
with tarfile.open(archive) as source:
    members = source.getmembers()
    if len(members) > 12000 or sum((member.size for member in members)) > 100 * 1024 * 1024:
        raise ValueError('expanded source cap')
    for member in members:
        path = Path(member.name)
        if path.is_absolute() or '..' in path.parts or (not path.parts) or (path.parts[0] != f'FFmpeg-{PIN}') or (not (member.isfile() or member.isdir())):
            raise ValueError('unsafe archive member')
        target = root / path
        if any((parent.is_symlink() for parent in (target, *target.parents) if parent != root.parent)):
            raise ValueError('symlink source destination')
        if member.isdir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with source.extractfile(member) as incoming, target.open('wb') as output:
                shutil.copyfileobj(incoming, output)
            target.chmod(member.mode & 493)
