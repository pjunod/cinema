"""Own the Apple build number: one claim, generated copies, per-change notes.

The Apple build number is a single monotonic counter that used to be duplicated
as prose across four documents, so two concurrent Apple branches conflicted on
every one of them and re-conflicted every time ``main`` moved (issue #509).

Two facts shape this module, and they pull in opposite directions:

* The **number** never conflicts. Both branches write the same next value, Git
  sees one identical change, and it merges clean into a tree where that value is
  already taken. That is the silent half of the bug, so every copy of the number
  is generated from ``project.yml`` here rather than hand-edited.
* The **narrative** always conflicts, because every change prepended a sentence
  at the same offset in the same shared documents. That is the loud half, so the
  narrative moved to one file per change, named after the issue rather than the
  build. Two concurrent changes write two different paths and cannot collide.

What is *not* solved, and cannot be here: once a competing Apple change merges,
the counter still has to be re-claimed above the new merge target. This module
makes that one generated diff instead of three prose merges plus six
hand-edited mentions that auto-merged to the wrong value.
"""

from __future__ import annotations

from collections.abc import Callable, Iterable
import dataclasses
from pathlib import Path
import re
import subprocess
import sys

from validation.mobile_versions import REPO_ROOT


NOTES_DIR = "docs/apple-builds"
# Files in the notes directory that are not per-change fragments.
NOTES_NON_FRAGMENTS = frozenset({"README.md", "history-through-build-78.md"})

PROJECT = "clients/apple/project.yml"
APPLE_README = "clients/apple/README.md"
PARITY = "docs/APPLE-CLIENT-PARITY.md"
STATUS = "docs/STATUS.html"

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
NARRATIVE_FREE = (APPLE_README, PARITY)
_NARRATIVE = re.compile(r"\bBuild [1-9]\d*\b")
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


@dataclasses.dataclass(frozen=True)
class Surface:
    """One generated occurrence of the build number.

    ``pattern`` must match exactly once and capture the number in group 1, so a
    rewrite that would silently hit zero or several places fails instead.
    """

    path: str
    pattern: re.Pattern[str]
    label: str


SURFACES: tuple[Surface, ...] = (
    Surface(PROJECT, re.compile(r'(?<=CURRENT_PROJECT_VERSION: ")[1-9]\d*(?=")'),
            "project.yml CURRENT_PROJECT_VERSION"),
    Surface(APPLE_README, re.compile(r"(?<=^> Status: \*\*v0\.2\.7\*\*, build `)[1-9]\d*(?=`)",
                                     re.MULTILINE),
            "Apple README status line"),
    Surface(PARITY, re.compile(r"(?<=^> Status \(\d{4}-\d{2}-\d{2}\): source is v0\.2\.7, "
                               r"Apple build )[1-9]\d*(?=\.)", re.MULTILINE),
            "Apple parity status line"),
    Surface(STATUS, re.compile(r"(?<=· Apple build )[1-9]\d*(?= source, not yet uploaded)"),
            "STATUS.html viewers tile"),
    Surface(STATUS, re.compile(r"(?<=upload Apple build )[1-9]\d*(?= \(release notes:)"),
            "STATUS.html TestFlight upload item"),
)


def _read(root: Path, path: str) -> str:
    return (root / path).read_text(encoding="utf-8")


def current_build(root: Path = REPO_ROOT) -> int:
    contents = _read(root, PROJECT)
    matches = re.findall(r'^\s*CURRENT_PROJECT_VERSION: "([1-9]\d*)"\s*$',
                         contents, re.MULTILINE)
    if not matches or len(set(matches)) != 1:
        raise AppleBuildError(
            f"{PROJECT} must declare one CURRENT_PROJECT_VERSION; found {matches}"
        )
    return int(matches[0])


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


def rewrite(contents: str, surface: Surface, build: int) -> str:
    replaced, count = surface.pattern.subn(str(build), contents)
    if count != 1:
        raise AppleBuildError(
            f"{surface.path}: expected exactly one {surface.label} to rewrite; "
            f"found {count}. The generated occurrence was reworded by hand — "
            "restore it or update validation/apple_build.py deliberately."
        )
    return replaced


def render(read: Callable[[str], str], build: int,
           fragments: Iterable[str] = ()) -> dict[str, str]:
    """The full contents every generated surface should have at ``build``.

    Returns only files whose contents change, so an already-correct tree renders
    empty and the check is a plain equality rather than a diff heuristic.
    """
    rendered: dict[str, str] = {}
    for surface in SURFACES:
        contents = rendered.get(surface.path, read(surface.path))
        rendered[surface.path] = rewrite(contents, surface, build)
    for path in fragments:
        contents = read(path)
        _one(_FRAGMENT_BUILD, contents, path, "`Build:` line")
        rendered[path] = _FRAGMENT_BUILD.sub(f"Build: {build}", contents)
    return {
        path: contents
        for path, contents in rendered.items()
        if contents != read(path)
    }


def validate_notes(read: Callable[[str], str], build: int,
                   fragments: Iterable[str]) -> tuple[str, ...]:
    """Fragment shape, claim bounds, and the shared documents' narrative ban."""
    errors: list[str] = []
    for path in fragments:
        contents = read(path)
        if not _FRAGMENT_TITLE.match(contents):
            errors.append(f"{path} must open with a `# ` title line")
        try:
            claimed = int(_one(_FRAGMENT_BUILD, contents, path, "`Build:` line"))
        except AppleBuildError as exc:
            errors.append(str(exc))
        else:
            if claimed > build:
                errors.append(
                    f"{path} claims Apple build {claimed}; {PROJECT} declares "
                    f"{build}. Run `make apple-build-bump` rather than editing "
                    "the claim by hand."
                )
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
                f"({', '.join(mentions)}). Per-build notes belong in "
                f"{NOTES_DIR}/ as one file per change; prose here at a shared "
                "insertion point is what made every concurrent Apple change "
                "conflict."
            )
    return tuple(errors)


def check_repository(root: Path = REPO_ROOT) -> tuple[str, ...]:
    read = lambda path: _read(root, path)  # noqa: E731 - one-line reader
    build = current_build(root)
    fragments = fragment_paths(root)
    errors = list(validate_notes(read, build, fragments))
    # Generation is the contract: if re-rendering at the declared build changes
    # anything, a generated occurrence was hand-edited and `make
    # apple-build-bump` would produce a surprise diff on someone else's branch.
    try:
        stale = render(read, build)
    except AppleBuildError as exc:
        errors.append(str(exc))
    else:
        for path in sorted(stale):
            errors.append(
                f"{path} does not match the generated form for Apple build "
                f"{build}; run `make apple-build-bump`"
            )
    return tuple(errors)


def _git(root: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", *arguments], cwd=root, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise AppleBuildError(
            f"git {' '.join(arguments)} failed: {result.stderr.strip()}"
        )
    return result.stdout


def branch_fragments(root: Path, merge_target: str) -> tuple[str, ...]:
    """Fragments this branch introduces, committed or not.

    Only these get their `Build:` rewritten. A fragment already on the merge
    target belongs to a build that has shipped, and moving its claim would
    rewrite somebody else's history.
    """
    paths: set[str] = set()
    changed = _git(root, "diff", "--name-only", "-z", "--diff-filter=ACMR",
                   f"{merge_target}...HEAD", "--", NOTES_DIR)
    paths.update(path for path in changed.split("\0") if path)
    status = _git(root, "status", "--porcelain=1", "-z", "--", NOTES_DIR)
    for entry in status.split("\0"):
        if len(entry) > 3:
            paths.add(entry[3:])
    known = set(fragment_paths(root))
    return tuple(sorted(paths & known))


def target_build(root: Path, merge_target: str) -> int:
    """The highest build the merge target claims.

    An unreadable target is a hard failure, not a zero. Claiming is idempotent,
    so a target that silently read as 0 would leave every branch already "ahead"
    and turn the whole tool into a no-op that reports success — the one outcome
    worse than refusing to run, because the branch then merges carrying a number
    `main` has already used.
    """
    try:
        contents = _git(root, "show", f"{merge_target}:{PROJECT}")
    except AppleBuildError as exc:
        raise AppleBuildError(
            f"cannot read {PROJECT} at merge target {merge_target!r}: {exc}. "
            "Fetch it (`git fetch origin`) or name another with --merge-target."
        ) from exc
    matches = re.findall(r'^\s*CURRENT_PROJECT_VERSION: "([1-9]\d*)"\s*$',
                         contents, re.MULTILINE)
    if not matches:
        raise AppleBuildError(
            f"{merge_target}:{PROJECT} declares no CURRENT_PROJECT_VERSION"
        )
    return max(int(value) for value in matches)


def bump(root: Path = REPO_ROOT, *, merge_target: str = "origin/main",
         build: int | None = None) -> tuple[int, tuple[str, ...]]:
    # Resolved first, and unconditionally: it is both the number to beat and the
    # ref `branch_fragments` diffs against, so an unreadable one has to stop the
    # run here with an answerable message rather than surface as a raw git error
    # or, worse, as a silent no-op.
    target = target_build(root, merge_target)
    fragments = branch_fragments(root, merge_target)
    if build is None:
        # Claiming is idempotent. A branch already above the merge target holds
        # a valid claim, so a second run confirms it rather than inflating the
        # counter; re-running is the documented response to `main` moving, and
        # it must not punish anyone who runs it twice.
        held = current_build(root)
        build = held if held > target else target + 1
    changed = render(lambda path: _read(root, path), build, fragments)
    for path, contents in sorted(changed.items()):
        (root / path).write_text(contents, encoding="utf-8")
    return build, tuple(sorted(changed))


def main(argv: tuple[str, ...]) -> int:
    merge_target = "origin/main"
    build: int | None = None
    arguments = list(argv)
    while arguments:
        flag = arguments.pop(0)
        if flag == "--merge-target" and arguments:
            merge_target = arguments.pop(0)
        elif flag == "--build" and arguments:
            build = int(arguments.pop(0))
        else:
            print(f"usage: apple_build [--merge-target REF] [--build N]",
                  file=sys.stderr)
            return 2
    try:
        claimed, changed = bump(merge_target=merge_target, build=build)
    except (AppleBuildError, OSError) as exc:
        print(f"apple build bump failed: {exc}", file=sys.stderr)
        return 1
    if not changed:
        print(f"Apple build {claimed} already claimed everywhere")
        return 0
    print(f"Apple build {claimed} claimed in:")
    for path in changed:
        print(f"  - {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(tuple(sys.argv[1:])))
