"""Per-change client release notes, and status lines that carry no narrative.

Every Apple change used to prepend a sentence at the same offset of three
shared documents, so two concurrent Apple branches conflicted on all of them
(issue #509). The narrative moved to one file per change under
`docs/apple-builds/`, named after the issue: two changes write two paths.

The build number was the other half. Each branch claimed "the next number" in
`project.yml` and in every status line that quoted it, the second branch to
merge had to re-claim, and this module generated those copies so the re-claim
was at least one diff. That is gone rather than generated: the number is
assigned when a build is released (validation/mobile_build.py), no document
quotes it, and a fragment no longer needs a `Build:` line. What is left here
is the shape of a fragment and the rule that the client status blockquotes
stay free of per-build narrative — Android's included, which carried a
paragraph per build until 2026-11-15 (clients/android/BUILD-HISTORY.md).
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
ANDROID_README = "clients/android/README.md"
PARITY = "docs/clients/APPLE-CLIENT-PARITY.md"

# The documents whose *status blockquote* must carry no per-build narrative.
#
# The ban is deliberately scoped to that blockquote rather than the whole file.
# It is where the ledger lived, where the anchored `> Status:` claim the gate
# reads lives, and the only place a new change would prepend to. Body prose is
# left alone on purpose: `APPLE-CLIENT-PARITY.md` explains under its own
# headings how a behaviour came to be ("Build 58 corrects the one remaining
# contradictory diagnostic"), and those sentences are documentation, not a
# growing ledger — banning them would push authors to reword true prose, which
# is the failure mode `data-build-history` exists to avoid on STATUS.html.
NARRATIVE_FREE = (APPLE_README, ANDROID_README, PARITY)
_NARRATIVE = re.compile(r"\bBuild `?[1-9]\d*\b")
_STATUS_BLOCKQUOTE = re.compile(r"^> Status.*?(?=\n(?!>))", re.MULTILINE | re.DOTALL)


def status_blockquote(contents: str, path: str) -> str:
    """The leading `> Status` blockquote, which is the anchored claim's home."""
    matches = _STATUS_BLOCKQUOTE.findall(contents)
    if len(matches) != 1:
        raise AppleBuildError(
            f"{path} must carry exactly one `> Status` blockquote; "
            f"found {len(matches)}"
        )
    return matches[0]

_FRAGMENT_BUILD = re.compile(r"^Build: ([1-9]\d*)$", re.MULTILINE)
_FRAGMENT_ISSUE = re.compile(r"^Issue: #([1-9]\d*)$", re.MULTILINE)
_FRAGMENT_TITLE = re.compile(r"\A# \S.*$", re.MULTILINE)


class AppleBuildError(ValueError):
    """The Apple build claim or a per-change note is malformed."""



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


def _one(pattern: re.Pattern[str], contents: str, path: str, label: str) -> str:
    matches = pattern.findall(contents)
    if len(matches) != 1:
        raise AppleBuildError(
            f"{path} must carry exactly one {label}; found {len(matches)}"
        )
    return matches[0]


def validate_notes(read: Callable[[str], str], fragments: Iterable[str]) -> tuple[str, ...]:
    """Fragment shape, and the shared documents' narrative ban."""
    errors: list[str] = []
    for path in fragments:
        contents = read(path)
        if not _FRAGMENT_TITLE.match(contents):
            errors.append(f"{path} must open with a `# ` title line")
        # A fragment written before 2026-11-15 records the build that shipped
        # it. A new one has no number to record: it is assigned at release.
        if len(_FRAGMENT_BUILD.findall(contents)) > 1:
            errors.append(f"{path} carries more than one `Build:` line")
        try:
            _one(_FRAGMENT_ISSUE, contents, path, "`Issue: #<number>` line")
        except AppleBuildError as exc:
            errors.append(str(exc))

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
                f"({', '.join(mentions)}). An Apple change records itself in "
                f"{NOTES_DIR}/ and any change in changelog.d/, one file each; "
                "prose here at a shared insertion point is what made every "
                "concurrent client change conflict."
            )
    return tuple(errors)


def check_repository(root: Path = REPO_ROOT) -> tuple[str, ...]:
    return validate_notes(lambda path: _read(root, path), fragment_paths(root))
