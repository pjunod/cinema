"""Exact file and link inventories; excluded build targets are never copied."""
from pathlib import Path
import hashlib
import shutil

CPU_TREES = ('src/dovi_tool', 'src/DoViBaker/include')
CPU_FILES = ('src/DoViBaker/DoViBaker/DoViProcessor.cpp', 'deps/lib/libdovi.a')
GPU_TREES = ('prefix', 'include', 'lib')


def record(path, root):
    if path.is_symlink():
        link = path.readlink()
        if link.is_absolute() or not path.resolve(strict=True).is_relative_to(root.resolve()) or not path.resolve().is_file():
            raise ValueError('dependency link leaves source root')
        return {'symlink':str(link)}
    if not path.is_file():
        raise ValueError('dependency must be a regular file/link')
    return {'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}


def inventory(root, kind):
    root = Path(root)
    trees = CPU_TREES if kind == 'cpu' else GPU_TREES
    result = {}
    for name in trees:
        base = root/name
        if base.is_symlink() or not base.is_dir():
            raise ValueError('dependency directory must be real')
        for path in sorted(base.rglob('*')):
            relative = path.relative_to(root)
            if kind == 'cpu' and relative.parts[:3] == ('src','dovi_tool','target'):
                continue
            if path.is_dir() and not path.is_symlink():
                continue
            result[relative.as_posix()] = record(path,root)
    if kind == 'cpu':
        for name in CPU_FILES:
            result[name] = record(root/name,root)
    return result


def validate(root, kind, expected):
    if inventory(root,kind) != expected:
        raise ValueError('exact dependency inventory differs')


def copy_verified(source, destination, kind, expected):
    validate(source,kind,expected)
    for name, info in expected.items():
        target = destination/name
        target.parent.mkdir(parents=True,exist_ok=True)
        if 'symlink' in info:
            target.symlink_to(info['symlink'])
        else:
            shutil.copyfile(source/name,target)
    # Recheck staged bytes and link targets before any intended source patch.
    validate(destination,kind,expected)
