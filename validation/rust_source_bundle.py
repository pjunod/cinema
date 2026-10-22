"""Git-native source identity without a Git repository on the execution host.

Only export() invokes Git. identity() verifies payload bytes, modes, recursive
Git tree ordering and the original raw commit object. This is source integrity,
not CI authentication/adoption or SHA256 archive transport evidence. Extra
outputs are allowed only beneath a real root target/ directory; even there
.git names are forbidden. Python bytecode/cache directories are not exempt.
Symlink payloads bind the link bytes; external referent bytes gain no authority.
"""
from __future__ import annotations

import base64
import binascii
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess

HEX = re.compile(r'[0-9a-f]{40}')
MODES = {'100644', '100755', '120000'}
FIELDS = {'schema', 'commit', 'tree', 'raw_commit_base64', 'entries'}


class BundleError(ValueError):
    """Source payload cannot establish the exported Git identity."""


def _require(condition, message):
    if not condition:
        raise BundleError(message)


def _object(kind, content):
    return hashlib.sha1(kind.encode() + b' ' + str(len(content)).encode() + b'\0' + content).hexdigest()


def _path(value):
    _require(isinstance(value, str) and value and '\\' not in value and '\0' not in value,
             'invalid source path')
    parts = value.split('/')
    _require(all(part and part not in ('.', '..') and part.casefold() != '.git' for part in parts),
             'source traversal or .git path is forbidden')
    try:
        value.encode('utf-8')
    except UnicodeEncodeError as error:
        raise BundleError('source path must be UTF-8') from error
    return parts


def _entries(manifest):
    _require(isinstance(manifest, dict) and set(manifest) == FIELDS and
             type(manifest['schema']) is int and manifest['schema'] == 1, 'invalid source manifest schema')
    for key in ('commit', 'tree'):
        _require(isinstance(manifest[key], str) and HEX.fullmatch(manifest[key]), 'invalid Git ' + key)
    entries = manifest['entries']
    _require(isinstance(entries, list), 'source entries must be a list')
    seen, nodes, directories = set(), {}, set()
    for entry in entries:
        _require(isinstance(entry, dict) and set(entry) == {'path', 'mode', 'blob'}, 'invalid source entry')
        parts = _path(entry['path'])
        _require(entry['path'] not in seen, 'duplicate source path')
        seen.add(entry['path'])
        _require(entry['mode'] in MODES, 'unsupported source type or gitlink')
        _require(isinstance(entry['blob'], str) and HEX.fullmatch(entry['blob']), 'invalid Git blob identity')
        node = nodes
        for index, part in enumerate(parts[:-1]):
            directories.add('/'.join(parts[:index + 1]))
            _require(part not in node or isinstance(node[part], dict), 'file/directory source collision')
            node = node.setdefault(part, {})
        _require(parts[-1] not in node, 'file/directory source collision')
        node[parts[-1]] = (entry['mode'], entry['blob'])
    return entries, nodes, directories


def _tree(nodes):
    # Git compares directory names with an implicit trailing '/', not the
    # plain filename ordering used by an ordinary sorted directory listing.
    ordered = sorted(nodes, key=lambda name: name.encode() + (b'/' if isinstance(nodes[name], dict) else b''))
    content = bytearray()
    for name in ordered:
        node = nodes[name]
        mode, digest = ('40000', _tree(node)) if isinstance(node, dict) else node
        content.extend(mode.encode() + b' ' + name.encode() + b'\0' + bytes.fromhex(digest))
    return _object('tree', bytes(content))


def _payload(root, entry):
    parts = _path(entry['path'])
    parent = root
    for part in parts[:-1]:
        parent = parent / part
        _require(stat.S_ISDIR(parent.lstat().st_mode), 'source parent is not a real directory')
    path = parent / parts[-1]
    before = path.lstat()
    if entry['mode'] == '120000':
        _require(stat.S_ISLNK(before.st_mode), 'expected a source symlink')
        digest = _object('blob', os.readlink(os.fsencode(path)))
    else:
        _require(stat.S_ISREG(before.st_mode), 'expected a regular source file')
        expected_exec = 0o111 if entry['mode'] == '100755' else 0
        _require(before.st_mode & 0o111 == expected_exec, 'source executable mode drift')
        # NOFOLLOW refuses replacement of a validated file with a symlink.
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, 'rb') as stream:
            opened = os.fstat(stream.fileno())
            _require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino), 'source replaced while opening')
            hasher = hashlib.sha1(b'blob ' + str(opened.st_size).encode() + b'\0')
            size = 0
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                size += len(chunk)
                hasher.update(chunk)
            _require(size == opened.st_size, 'source size changed while hashing')
            digest = hasher.hexdigest()
    after = path.lstat()
    _require((before.st_dev, before.st_ino, before.st_mode, before.st_size, before.st_mtime_ns) ==
             (after.st_dev, after.st_ino, after.st_mode, after.st_size, after.st_mtime_ns),
             'source changed during verification')
    _require(digest == entry['blob'], 'source blob bytes differ: ' + entry['path'])


def _scan(root, entries, directories, *, local_git=False):
    expected = {row['path'] for row in entries}
    observed = set()
    def unreadable(_error):
        raise BundleError('cannot inventory source directory')
    for parent, children, files in os.walk(root, followlinks=False, onerror=unreadable):
        base = Path(parent).relative_to(root)
        for name in [*children, *files]:
            relative = (base / name).as_posix()
            if name.casefold() == '.git':
                if local_git and relative == '.git':
                    if name in children:
                        children.remove(name)
                    continue
                raise BundleError('.git content is forbidden in a source bundle')
            path = Path(parent) / name
            if relative in expected:
                observed.add(relative)
            generated = relative.startswith('target/')
            if relative == 'target' and relative not in expected and relative not in directories:
                _require(stat.S_ISDIR(path.lstat().st_mode), 'generated target must be a real directory')
                continue
            if generated:
                continue  # Walk directory names to reject hidden .git, never follow links.
            if stat.S_ISDIR(path.lstat().st_mode):
                _require(relative in directories, 'unexpected source directory: ' + relative)
            else:
                _require(relative in expected, 'unexpected source file: ' + relative)
    _require(observed == expected, 'actual directory-entry paths differ from the source tree')


def _verify(root, manifest, *, local_git=False):
    root = Path(root).resolve()
    _require(root.is_dir(), 'source root is unavailable')
    try:
        entries, nodes, directories = _entries(manifest)
        tree = _tree(nodes)
        _require(tree == manifest['tree'], 'reconstructed Git tree differs')
        raw = base64.b64decode(manifest['raw_commit_base64'], validate=True)
        _require(_object('commit', raw) == manifest['commit'], 'raw Git commit identity differs')
        headers, separator, _message = raw.partition(b'\n\n')
        lines = headers.split(b'\n')
        _require(separator and lines and lines[0] == b'tree ' + tree.encode() and
                 sum(line.startswith(b'tree ') for line in lines) == 1, 'raw commit tree differs or is ambiguous')
        for entry in entries:
            _payload(root, entry)
        _scan(root, entries, directories, local_git=local_git)
    except (OSError, TypeError, KeyError, UnicodeError, binascii.Error) as error:
        raise BundleError('source evidence is missing or malformed') from error
    return {'commit': manifest['commit'], 'tree': tree, 'tracked_clean': True}


def export(root):
    """Export only a clean SHA1 Git HEAD's immutable commit and tree entries."""
    root = Path(root).resolve()
    def git(*args):
        return subprocess.check_output(['git', '-C', str(root), *args], stderr=subprocess.PIPE)
    _require(git('rev-parse', '--show-object-format').strip() == b'sha1', 'only Git-native SHA1 repositories are supported')
    _require(not git('status', '--porcelain', '--untracked-files=all').strip(), 'source checkout is not clean')
    commit = git('rev-parse', 'HEAD').decode().strip()
    tree = git('rev-parse', 'HEAD^{tree}').decode().strip()
    raw = git('cat-file', 'commit', commit)
    entries = []
    for record in git('ls-tree', '-r', '-z', commit).split(b'\0'):
        if not record:
            continue
        header, path = record.split(b'\t', 1)
        mode, kind, blob = header.decode('ascii').split()
        _require(kind == 'blob', 'gitlinks and unsupported source types are forbidden')
        entries.append({'path': path.decode('utf-8'), 'mode': mode, 'blob': blob})
    manifest = dict(schema=1, commit=commit, tree=tree,
                    raw_commit_base64=base64.b64encode(raw).decode('ascii'), entries=entries)
    _verify(root, manifest, local_git=True)
    return manifest


def _pairs(pairs):
    value = {}
    for key, item in pairs:
        _require(key not in value, 'duplicate JSON manifest key')
        value[key] = item
    return value


def identity(root, manifest_path):
    """Verify an outside-source manifest and payload without invoking Git."""
    root, manifest_path = Path(root).resolve(), Path(manifest_path).resolve()
    _require(not manifest_path.is_relative_to(root), 'source manifest must be outside the payload')
    try:
        manifest = json.loads(manifest_path.read_text(), object_pairs_hook=_pairs)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise BundleError('source manifest is unavailable or invalid JSON') from error
    return _verify(root, manifest)
