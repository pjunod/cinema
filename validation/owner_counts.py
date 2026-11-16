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

The totals themselves change in one place only: `scripts/release-cut` folds
every recorded difference into its total, keeps each reason as a comment on
the row, and deletes the files (`fold`). `direct_edits` is the rule that
tells that fold from a hand edit: a total may move only by the differences of
the files that went away with it.
"""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
import re
import textwrap
import tomllib

REPO_ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = "tests/rolling-producer-owners.d"
TABLES = ("symbols", "module_symbols", "module_structures")


class OwnerCountError(ValueError):
    """A recorded count change is malformed or names no counted shape."""


def parse_changes(
    fragments: Mapping[str, str], known_ids: frozenset[str], where: str = DIRECTORY
) -> dict[str, list[tuple[str, int, str]]]:
    """`(file, delta, reason)` per shape id, from each fragment's text."""

    changes: dict[str, list[tuple[str, int, str]]] = {}
    for name in sorted(fragments):
        at_file = f"{Path(where).name}/{name}"
        try:
            raw = tomllib.loads(fragments[name])
        except tomllib.TOMLDecodeError as exc:
            raise OwnerCountError(f"cannot read {at_file}: {exc}") from exc
        rows = raw.get("changes")
        if set(raw) != {"changes"} or not isinstance(rows, list) or not rows:
            raise OwnerCountError(f"{at_file} must contain only one or more [[changes]] tables")
        seen: set[str] = set()
        for index, row in enumerate(rows):
            at = f"{at_file} changes[{index}]"
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
            changes.setdefault(identifier, []).append((name, delta, reason.strip()))
    return changes


def read_fragments(directory: Path) -> dict[str, str]:
    if not directory.is_dir():
        return {}
    try:
        return {path.name: path.read_text(encoding="utf-8") for path in sorted(directory.glob("*.toml"))}
    except OSError as exc:
        raise OwnerCountError(f"cannot read {directory}: {exc}") from exc


def load_changes(directory: Path, known_ids: frozenset[str]) -> dict[str, int]:
    """The summed `delta` per shape id across every fragment in `directory`."""

    changes = parse_changes(read_fragments(directory), known_ids, str(directory))
    return {identifier: sum(delta for _, delta, _ in rows) for identifier, rows in changes.items()}


def totals(ledger: str) -> dict[str, int]:
    catalog = tomllib.loads(ledger)
    return {row["id"]: row["expected_occurrences"] for table in TABLES for row in catalog[table]}


def fold(ledger: str, fragments: Mapping[str, str]) -> str:
    """The ledger with every recorded difference folded into its total.

    Each reason is kept as a comment directly above the total it moved, which
    is where a review lived before the per-change files. The caller deletes
    the fragments; `scripts/release-cut` is that caller, so the ledger has one
    writer and is rewritten once per release.
    """

    before = totals(ledger)
    changes = parse_changes(fragments, frozenset(before))
    below_zero = sorted(
        f"{identifier} ({before[identifier]} {sum(d for _, d, _ in rows):+d})"
        for identifier, rows in changes.items()
        if before[identifier] + sum(d for _, d, _ in rows) < 0
    )
    if below_zero:
        raise OwnerCountError(
            "the recorded differences take a total below zero: " + ", ".join(below_zero)
        )
    out: list[str] = []
    current = None
    for line in ledger.split("\n"):
        if line.startswith("[["):
            current = None
        found = re.fullmatch(r'id = "([^"]+)"', line)
        if found:
            current = found.group(1)
        if current in changes and re.fullmatch(r"expected_occurrences = \d+", line):
            for name, delta, reason in changes[current]:
                out.extend(textwrap.wrap(
                    f"{name[: -len('.toml')]} ({delta:+d}): {reason}", width=77,
                    initial_indent="# ", subsequent_indent="# ",
                ))
            line = f"expected_occurrences = {before[current] + sum(d for _, d, _ in changes[current])}"
        out.append(line)
    folded = "\n".join(out)
    expected = {i: before[i] + sum(d for _, d, _ in changes.get(i, ())) for i in before}
    if totals(folded) != expected:
        raise OwnerCountError("could not fold every recorded difference into its total")
    return folded


def direct_edits(
    base_ledger: str, base_fragments: Mapping[str, str], ledger: str, fragment_names: frozenset[str]
) -> tuple[str, ...]:
    """Totals that moved by anything other than a fold of deleted fragments."""

    before, now = totals(base_ledger), totals(ledger)
    gone = {name: text for name, text in base_fragments.items() if name not in fragment_names}
    folded = parse_changes(gone, frozenset(before))
    return tuple(
        f"{identifier}: {before[identifier]} -> {now[identifier]}"
        for identifier in sorted(before.keys() & now.keys())
        if now[identifier] - before[identifier] != sum(d for _, d, _ in folded.get(identifier, ()))
    )


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
