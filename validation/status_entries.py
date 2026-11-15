"""Efforts in flight are one file each under status.d/.

STATUS.md used to be one list every pull request prepended to at the same
anchor line, under a 180-line cap that made each append also move the oldest
section out. Both edits landed on the same lines of the same file, so any two
open pull requests conflicted there. An effort is now a file its owner alone
edits; the page-size limit became a per-file limit, which no merge can break.
"""

from __future__ import annotations

from pathlib import Path
import re

REPO_ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = "status.d"
NON_ENTRIES = frozenset({"README.md"})
MAX_LINES = 30
MAX_BYTES = 2400
_NAME = re.compile(r"\d{4}-\d{2}-\d{2}-[a-z0-9]+(?:-[a-z0-9]+)*\.md")


def entry_paths(root: Path = REPO_ROOT) -> tuple[Path, ...]:
    """Every effort file, oldest first (the name starts with its date)."""

    directory = root / DIRECTORY
    if not directory.is_dir():
        return ()
    return tuple(
        sorted(
            entry
            for entry in directory.iterdir()
            if entry.is_file() and entry.name not in NON_ENTRIES
        )
    )


def title(text: str) -> str:
    first = text.split("\n", 1)[0]
    return first[2:].strip() if first.startswith("# ") else ""


def validate(root: Path = REPO_ROOT) -> tuple[str, ...]:
    errors = []
    for path in entry_paths(root):
        where = f"{DIRECTORY}/{path.name}"
        if not _NAME.fullmatch(path.name):
            errors.append(f"{where}: name an effort <YYYY-MM-DD>-<slug>.md")
            continue
        raw = path.read_bytes()
        text = raw.decode("utf-8")
        if not title(text):
            errors.append(f"{where}: the first line must be `# <Title>`")
        if not text.split("\n", 1)[-1].strip():
            errors.append(f"{where}: say who owns the effort and where it stands")
        if re.search(r"(?m)^#{1,6} ", text.split("\n", 1)[-1]):
            errors.append(f"{where}: one effort per file, so one heading")
        if text.count("\n") > MAX_LINES or len(raw) > MAX_BYTES:
            errors.append(
                f"{where}: {text.count(chr(10))} lines, {len(raw)} bytes; an entry is at "
                f"most {MAX_LINES} lines and {MAX_BYTES} bytes — move the detail to its ledger"
            )
    return tuple(errors)
