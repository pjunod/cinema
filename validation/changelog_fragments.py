"""Changelog entries live one file per change, and are assembled at release.

Every pull request used to add its entry at the top of the same `###` list
under `## [Unreleased]` in CHANGELOG.md, so any two open pull requests
conflicted there and conflicted again each time `main` moved. An entry is now
a file of its own under `changelog.d/`: two changes write two paths, which Git
never has to merge. `scripts/release-cut` folds the files into the dated
section and deletes them, so CHANGELOG.md is written once per release, by the
release pull request alone.

A fragment is the text that would have gone under `[Unreleased]`: one or more
`### <Category>` headings, each followed by its entries.

    ### Fixed

    - A leader restart no longer ends Live TV playback.
"""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
import re
import subprocess
import sys

REPO_ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = "changelog.d"
CHANGELOG = "CHANGELOG.md"
UNRELEASED = "## [Unreleased]"
# Keep a Changelog's categories, in the order a release section lists them.
CATEGORIES = ("Added", "Changed", "Deprecated", "Removed", "Fixed", "Security")
NON_FRAGMENTS = frozenset({"README.md"})
_NAME = re.compile(r"[a-z0-9][a-z0-9._-]*\.md")
_HEADING = re.compile(r"(?m)^### (.+)$")


class FragmentError(ValueError):
    """A changelog fragment, or the section it folds into, is malformed."""


def parse(text: str, where: str) -> list[tuple[str, str]]:
    """`(category, entries)` for each `###` block of a fragment or section."""

    headings = list(_HEADING.finditer(text))
    if text[: headings[0].start() if headings else len(text)].strip():
        raise FragmentError(f"{where}: text before the first `### <Category>` heading")
    blocks = []
    for index, heading in enumerate(headings):
        category = heading.group(1).strip()
        if category not in CATEGORIES:
            raise FragmentError(
                f"{where}: `### {category}` is not one of {', '.join(CATEGORIES)}"
            )
        end = headings[index + 1].start() if index + 1 < len(headings) else len(text)
        entries = text[heading.end() : end].strip("\n")
        if not entries.strip():
            raise FragmentError(f"{where}: `### {category}` has no entries")
        if re.search(r"(?m)^#{1,2} ", entries):
            raise FragmentError(f"{where}: a fragment holds `###` headings only")
        blocks.append((category, entries))
    return blocks


def fragment_names(root: Path = REPO_ROOT) -> tuple[str, ...]:
    directory = root / DIRECTORY
    if not directory.is_dir():
        return ()
    return tuple(
        sorted(
            entry.name
            for entry in directory.iterdir()
            if entry.is_file() and entry.name not in NON_FRAGMENTS
        )
    )


def read_fragments(root: Path = REPO_ROOT) -> dict[str, str]:
    return {
        name: (root / DIRECTORY / name).read_text(encoding="utf-8")
        for name in fragment_names(root)
    }


def unreleased_body(changelog: str) -> str:
    """The text between `## [Unreleased]` and the next `## [` heading."""

    if changelog.count(UNRELEASED + "\n") != 1:
        raise FragmentError(f"{CHANGELOG} must carry exactly one '{UNRELEASED}' heading")
    start = changelog.index(UNRELEASED + "\n") + len(UNRELEASED) + 1
    following = re.search(r"(?m)^## \[", changelog[start:])
    return changelog[start : start + following.start() if following else len(changelog)]


def assemble(fragments: Mapping[str, str], unreleased: str = "") -> str:
    """One release section body: every fragment, then what `[Unreleased]` held.

    Fragments are taken in file-name order, so the result does not depend on
    the order their pull requests merged in.
    """

    merged: dict[str, list[str]] = {}
    for name in sorted(fragments):
        for category, entries in parse(fragments[name], f"{DIRECTORY}/{name}"):
            merged.setdefault(category, []).append(entries)
    for category, entries in parse(unreleased, f"{CHANGELOG} [Unreleased]"):
        merged.setdefault(category, []).append(entries)
    return "".join(
        f"### {category}\n\n" + "\n".join(merged[category]) + "\n\n"
        for category in CATEGORIES
        if category in merged
    )


def validate(root: Path = REPO_ROOT) -> tuple[str, ...]:
    errors = []
    for name in fragment_names(root):
        where = f"{DIRECTORY}/{name}"
        if not _NAME.fullmatch(name):
            errors.append(
                f"{where}: name a fragment <slug>.md in lower-case letters, digits, "
                "'.', '-' and '_'"
            )
            continue
        try:
            if not parse((root / DIRECTORY / name).read_text(encoding="utf-8"), where):
                errors.append(f"{where}: no `### <Category>` heading")
        except FragmentError as exc:
            errors.append(str(exc))
    return tuple(errors)


def added_to_unreleased(base_changelog: str, changelog: str) -> tuple[str, ...]:
    """Lines a branch added under `[Unreleased]`, which belong in a fragment.

    Removal is allowed: the release cut empties the section, and an entry may
    be moved out into a fragment.
    """

    remaining: dict[str, int] = {}
    for line in unreleased_body(base_changelog).splitlines():
        remaining[line] = remaining.get(line, 0) + 1
    added = []
    for line in unreleased_body(changelog).splitlines():
        if remaining.get(line, 0):
            remaining[line] -= 1
        elif line.strip():
            added.append(line)
    return tuple(added)


def check_branch(root: Path, base: str) -> tuple[str, ...]:
    """Fragment shape, and no new text under `[Unreleased]` since `base`."""

    errors = list(validate(root))
    shown = subprocess.run(
        ["git", "show", f"{base}:{CHANGELOG}"], cwd=root, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if shown.returncode != 0:
        return (*errors, f"cannot read {CHANGELOG} at {base}: {shown.stderr.strip()}")
    try:
        added = added_to_unreleased(
            shown.stdout, (root / CHANGELOG).read_text(encoding="utf-8")
        )
    except FragmentError as exc:
        return (*errors, str(exc))
    if added:
        errors.append(
            f"{CHANGELOG} gained {len(added)} line(s) under [Unreleased] "
            f"(first: {added[0][:80]!r}). Put the entry in {DIRECTORY}/<slug>.md "
            f"instead ({DIRECTORY}/README.md); the release cut assembles it."
        )
    return tuple(errors)


def main(argv: list[str]) -> int:
    if argv[:1] == ["--preview"]:
        sys.stdout.write(assemble(read_fragments()))
        return 0
    errors = validate()
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
