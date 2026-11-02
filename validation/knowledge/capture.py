"""Read diff identities without executing PR text or changing the checkout."""
from __future__ import annotations

from pathlib import Path
import re

from .model import KnowledgeError, safe_relative
from .snapshot import SnapshotChanged, git


def resolve_revision(root: Path, value: str) -> str:
    if not isinstance(value, str) or not value or len(value) > 1024:
        raise KnowledgeError('invalid base revision')
    resolved = git(root, 'rev-parse', '--verify', '--end-of-options', value + '^{commit}').decode().strip()
    if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', resolved):
        raise KnowledgeError('base revision is not an available commit')
    return resolved


def changed_paths(root: Path, indexed, *, base=None, staged=False, worktree=False) -> tuple[str, ...]:
    """Select staged paths or the full proposed worktree diff, including renames.

    The graph remains explicitly identified as worktree knowledge. This function
    selects change paths; it does not attest an index-only execution or source tree.
    """
    if staged and (base is not None or worktree):
        raise KnowledgeError('staged and base/worktree diff modes are exclusive')
    if not indexed.snapshot.verify(root):
        raise SnapshotChanged('snapshot-changed before diff selection')
    arguments = ['diff', '--name-only', '-z', '--no-renames']
    if staged:
        arguments.append('--cached')
    elif base is not None:
        arguments.append(resolve_revision(root, base))
    else:
        arguments.append(indexed.snapshot.head)
    raw = git(root, *arguments, '--')
    paths = {safe_relative(p.decode('utf-8')) for p in raw.split(b'\0') if p}
    if not staged:
        # Only inputs already selected by the reviewed snapshot may contribute
        # untracked change paths. No directory-wide untracked content ingestion.
        paths.update(indexed.snapshot.untracked)
    if not indexed.snapshot.verify(root):
        raise SnapshotChanged('snapshot-changed during diff selection')
    return tuple(sorted(paths))
