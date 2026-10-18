"""Validate a BuildKit /package tar completely, then preserve its filesystem objects."""
import argparse
import math
import os
from pathlib import Path, PurePosixPath
import shutil
import tarfile
import tempfile

# The observed media export contains 38,755 members / 1,428,110,983 expanded bytes.
MAX_MEMBERS = 65536
MAX_EXPANDED = 2 * 1024**3
MAX_ARCHIVE = MAX_EXPANDED + 64 * 1024**2
BLOCK = 512


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def member_path(name, directory=False):
    value = name[:-1] if directory and name.endswith('/') else name
    path = PurePosixPath(value)
    require(value and not path.is_absolute() and str(path) == value
            and '..' not in path.parts and path.parts[0] == 'package'
            and '\\' not in value and ':' not in value
            and all(ord(c) >= 32 and ord(c) != 127 for c in value),
            'Noncanonical or escaping export path')
    return value


def link_target(member):
    value = member.linkname
    path = PurePosixPath(value)
    require(value and not path.is_absolute() and '\\' not in value and ':' not in value
            and str(path) == value and '..' not in path.parts
            and all(ord(c) >= 32 and ord(c) != 127 for c in value), 'Unsafe export link')
    parts = list(PurePosixPath(member.name).parent.parts) if member.issym() else []
    for part in value.split('/'):
        parts.append(part)
    require(parts and parts[0] == 'package', 'Export link leaves package')
    return '/'.join(parts)


def source_identity(stream):
    value = os.fstat(stream.fileno())
    return value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns


def inventory(archive, stream, size):
    """No filesystem writes until every header, graph edge and payload passes."""
    members, total = {}, 0
    for member in archive:
        require(len(members) < MAX_MEMBERS, 'Too many export members')
        name = member_path(member.name, member.isdir())
        require(name not in members and member.type in
                {tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE,
                 tarfile.SYMTYPE, tarfile.LNKTYPE}, 'Duplicate or special export member')
        require(member.size >= 0 and (member.isfile() or member.size == 0)
                and member.mode & ~0o777 == 0 and math.isfinite(member.mtime),
                'Unsupported export size, mode or timestamp')
        require(not member.issym() or member.mode == 0o777,
                'Symlink mode cannot be preserved on the Linux runner')
        require(not member.sparse and not any(k.startswith('GNU.sparse') for k in member.pax_headers),
                'Sparse export member unsupported')
        total += member.size
        require(total <= MAX_EXPANDED and member.offset_data + member.size <= size,
                'Oversized or truncated export')
        member.name = name
        members[name] = member
    require(members and 'package' in members and members['package'].isdir(),
            'Export must contain its package directory')
    for name, member in members.items():
        for parent in PurePosixPath(name).parents:
            if str(parent) != '.':
                require(str(parent) in members and members[str(parent)].isdir(),
                        'Missing directory or non-directory export ancestor')
        if member.issym() or member.islnk():
            target, seen = link_target(member), {name}
            while target in members and (members[target].issym() or members[target].islnk()):
                require(target not in seen and len(seen) < MAX_MEMBERS, 'Cyclic export link')
                seen.add(target)
                target = link_target(members[target])
            require(target in members, 'Unresolved export link')
            if member.islnk():
                require(members[target].isfile() and member.mode == members[target].mode,
                        'Hardlink must resolve to a regular member with identical mode')
        if member.isfile():
            source = archive.extractfile(member)
            require(source is not None, 'Missing export payload')
            with source:
                remaining = member.size
                while remaining:
                    block = source.read(min(1024**2, remaining))
                    require(block, 'Truncated export payload')
                    remaining -= len(block)
    end = max(m.offset_data + ((m.size + BLOCK - 1) // BLOCK) * BLOCK for m in members.values())
    require(size % BLOCK == 0 and size - end >= 2 * BLOCK, 'Missing complete tar end marker')
    stream.seek(end)
    while block := stream.read(1024**2):
        require(not any(block), 'Nonzero trailing tar headers or payload')
    return members


def unpack(source, destination):
    destination = Path(destination).absolute()
    require('..' not in destination.parts and not os.path.lexists(destination)
            and destination.parent.resolve() == destination.parent,
            'Destination must be new with a canonical parent')
    with Path(source).open('rb') as stream:
        original = source_identity(stream)
        size = original[2]
        require(0 < size <= MAX_ARCHIVE, 'Archive outside media export bound')
        with tarfile.open(fileobj=stream, mode='r:') as archive:
            members = inventory(archive, stream, size)
            require(source_identity(stream) == original, 'Archive changed during validation')
            # The private staging directory is created only after full validation.
            with tempfile.TemporaryDirectory(prefix='.media-export-', dir=destination.parent) as temp:
                stage = Path(temp) / 'root'
                stage.mkdir(mode=0o700)
                for name, member in members.items():
                    if member.isdir():
                        (stage / name).mkdir(parents=True, exist_ok=True)
                for name, member in members.items():
                    if member.isfile():
                        with archive.extractfile(member) as src, (stage / name).open('xb') as dst:
                            shutil.copyfileobj(src, dst, 1024**2)
                            require(dst.tell() == member.size, 'Export payload changed during copy')
                        os.chmod(stage / name, member.mode)
                for name, member in members.items():
                    if member.issym():
                        os.symlink(member.linkname, stage / name)
                    elif member.islnk():
                        target = link_target(member)
                        while not members[target].isfile():
                            target = link_target(members[target])
                        os.link(stage / target, stage / name, follow_symlinks=False)
                for name, member in sorted(members.items(), key=lambda item: -len(item[0])):
                    if member.isdir():
                        os.chmod(stage / name, member.mode)
                require(not os.path.lexists(destination), 'Destination appeared during extraction')
                require(source_identity(stream) == original, 'Archive changed during extraction')
                stage.rename(destination)
    return len(members)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    print('Validated media export members:', unpack(args.archive, args.destination))


if __name__ == '__main__':
    main()
