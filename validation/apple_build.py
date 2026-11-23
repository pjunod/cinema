"""Own the Apple per-change notes and keep build numbers out of them.

The Apple narrative used to live in shared documents at a shared insertion
point, so every two concurrent Apple branches conflicted on it (issue #509).
It moved to one file per change under ``docs/apple-builds/``, named after the
issue rather than the build, so two concurrent changes write two different
paths and cannot collide.

The build number itself is no longer stored anywhere in source: it is derived
at build time by ``scripts/build-number`` from the landing commit's place on
main, so a note cannot know it before it merges. A fragment therefore names
its issue and never a build, and the shared status blockquotes carry no
per-build narrative.
"""

from __future__ import annotations

from collections.abc import Callable, Iterable
from pathlib import Path
import re

from validation.mobile_versions import REPO_ROOT


NOTES_DIR = "docs/apple-builds"
# Files in the notes directory that are not per-change fragments.
NOTES_NON_FRAGMENTS = frozenset({"README.md", "history-through-build-78.md"})

APPLE_README = "clients/apple/README.md"
PARITY = "docs/clients/APPLE-CLIENT-PARITY.md"

# The documents whose *status blockquote* must carry no per-build narrative.
#
# The ban is deliberately scoped to that blockquote rather than the whole file.
# It is where the ledger lived and the only place a new change would prepend
# to. Body prose is left alone on purpose: `APPLE-CLIENT-PARITY.md` explains
# under its own headings how a behaviour came to be ("Build 58 corrects the one
# remaining contradictory diagnostic"), and those sentences are documentation,
# not a growing ledger.
NARRATIVE_FREE = (APPLE_README, PARITY)
_NARRATIVE = re.compile(r"\bBuild [1-9]\d*\b")
_STATUS_BLOCKQUOTE = re.compile(r"^> Status.*?(?=\n(?!>))", re.MULTILINE | re.DOTALL)

_FRAGMENT_BUILD = re.compile(r"^Build:.*$", re.MULTILINE)
_FRAGMENT_ISSUE = re.compile(r"^Issue: #([1-9]\d*)$", re.MULTILINE)
_FRAGMENT_TITLE = re.compile(r"\A# \S.*$", re.MULTILINE)


class AppleBuildError(ValueError):
    """A per-change note or a status blockquote is malformed."""


def status_blockquote(contents: str, path: str) -> str:
    """The leading `> Status` blockquote."""
    matches = _STATUS_BLOCKQUOTE.findall(contents)
    if len(matches) != 1:
        raise AppleBuildError(
            f"{path} must carry exactly one `> Status` blockquote; "
            f"found {len(matches)}"
        )
    return matches[0]


def _read(root: Path, path: str) -> str:
    return (root / path).read_text(encoding="utf-8")


def fragment_paths(root: Path = REPO_ROOT) -> tuple[str, ...]:
    directory = root / NOTES_DIR
    if not directory.is_dir():
        return ()
    return tuple(
        sorted(
            f"{NOTES_DIR}/{entry.name}"
            for entry in directory.iterdir()
            if entry.is_file()
            and entry.suffix == ".md"
            and entry.name not in NOTES_NON_FRAGMENTS
        )
    )


def validate_notes(read: Callable[[str], str],
                   fragments: Iterable[str]) -> tuple[str, ...]:
    """Fragment shape and the shared documents' narrative ban."""
    errors: list[str] = []
    for path in fragments:
        contents = read(path)
        if not _FRAGMENT_TITLE.match(contents):
            errors.append(f"{path} must open with a `# ` title line")
        if _FRAGMENT_BUILD.search(contents):
            errors.append(
                f"{path} carries a `Build:` line. Build numbers are derived at build "
                "time by scripts/build-number and known only once the change lands "
                "on main; name the issue and drop the build."
            )
        issues = _FRAGMENT_ISSUE.findall(contents)
        if len(issues) != 1:
            errors.append(
                f"{path} must carry exactly one `Issue: #<number>` line; "
                f"found {len(issues)}"
            )

    for path in NARRATIVE_FREE:
        try:
            blockquote = status_blockquote(read(path), path)
        except AppleBuildError as exc:
            errors.append(str(exc))
            continue
        mentions = sorted(set(_NARRATIVE.findall(blockquote)))
        if mentions:
            errors.append(
                f"{path}'s status blockquote carries per-build narrative "
                f"({', '.join(mentions)}). Per-change notes belong in "
                f"{NOTES_DIR}/ as one file per change; prose here at a shared "
                "insertion point is what made every concurrent Apple change "
                "conflict."
            )
    return tuple(errors)


def check_repository(root: Path = REPO_ROOT) -> tuple[str, ...]:
    return validate_notes(lambda path: _read(root, path), fragment_paths(root))
