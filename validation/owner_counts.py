"""Reviewed occurrence counts that change one file per change.

`tests/playback/rolling-producer-owners.toml` pins how many times each task,
timer, process and legacy-owner shape occurs, as one `expected_occurrences`
total per shape. Every change that added a spawn or a timer anywhere in the
scanned roots edited the same total, so two open pull requests conflicted on
that line — or, worse, both wrote the same next number, merged cleanly, and
left `main` one short.

A change now records its own difference in a file of its own under
`tests/rolling-producer-owners.d/`:

    [[changes]]
    id = "namespaced-task-spawn"
    delta = 1
    reason = "One joined test-only releaser task in <test>; awaited by the test."

The reviewed count for a shape is its `expected_occurrences` plus every
recorded `delta`. Two changes write two files, and their differences add up
on `main` whichever merges first. The review an edit to the total used to
carry in a comment is the `reason`, which is required.
"""

from __future__ import annotations

from pathlib import Path
import tomllib

REPO_ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = "tests/rolling-producer-owners.d"
TABLES = ("symbols", "module_symbols", "module_structures")


class OwnerCountError(ValueError):
    """A recorded count change is malformed or names no counted shape."""


def load_changes(directory: Path, known_ids: frozenset[str]) -> dict[str, int]:
    """The summed `delta` per shape id across every fragment in `directory`."""

    totals: dict[str, int] = {}
    if not directory.is_dir():
        return totals
    for path in sorted(directory.glob("*.toml")):
        where = f"{directory.name}/{path.name}"
        try:
            raw = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as exc:
            raise OwnerCountError(f"cannot read {where}: {exc}") from exc
        rows = raw.get("changes")
        if set(raw) != {"changes"} or not isinstance(rows, list) or not rows:
            raise OwnerCountError(f"{where} must contain only one or more [[changes]] tables")
        seen: set[str] = set()
        for index, row in enumerate(rows):
            at = f"{where} changes[{index}]"
            if not isinstance(row, dict) or set(row) != {"id", "delta", "reason"}:
                raise OwnerCountError(f"{at} must contain exactly id, delta and reason")
            identifier, delta, reason = row["id"], row["delta"], row["reason"]
            if identifier not in known_ids:
                raise OwnerCountError(f"{at} names unknown shape id {identifier!r}")
            if identifier in seen:
                raise OwnerCountError(f"{at} repeats {identifier!r}; one row per shape per file")
            seen.add(identifier)
            if isinstance(delta, bool) or not isinstance(delta, int) or delta == 0:
                raise OwnerCountError(f"{at} delta must be a nonzero integer")
            if not isinstance(reason, str) or not reason.strip():
                raise OwnerCountError(f"{at} needs the ownership review as its reason")
            totals[identifier] = totals.get(identifier, 0) + delta
    return totals


def reviewed_counts(catalog: dict, directory: Path) -> dict[str, int]:
    """Each counted shape's reviewed total: its base plus the recorded changes."""

    base = {row["id"]: row["expected_occurrences"] for table in TABLES for row in catalog[table]}
    changes = load_changes(directory, frozenset(base))
    return {identifier: count + changes.get(identifier, 0) for identifier, count in base.items()}


def how_to_record(identifier: str, expected: int, actual: int) -> str:
    return (
        f"record the reviewed difference in {DIRECTORY}/<slug>.toml:\n"
        f'[[changes]]\nid = "{identifier}"\ndelta = {actual - expected}\n'
        'reason = "<what was added or removed, and what owns it>"'
    )
