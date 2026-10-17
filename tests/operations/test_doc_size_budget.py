"""Docs stay small enough for an agent to use, and the oversized ones only shrink.

An agent that opens a 700 KB document spends its context on it. So every
tracked Markdown file under docs/ is held to 40 KB, except the documents that
were already larger when the budget was introduced. Those are listed in
tests/operations/doc-size-allowlist.txt, each with its size at the time as a
ceiling. The list is a ratchet:

* an unlisted document over the budget fails: split it, or move the detail to
  a sibling document;
* a listed document over its ceiling fails: trim it or split it;
* a listed document that is now within the budget fails until it is removed
  from the list, so the list cannot hold a slot for a document that got small;
* a listed path that no longer exists fails;
* the documents named in HEAD_REQUIRED must open with an "At a glance" head,
  so an agent can read the head and stop.

Generated evidence (docs/**/evidence/**) and the archive (docs/archive/**) are
records, not reading material, and are outside the budget.
"""

from __future__ import annotations

from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
ALLOWLIST = Path(__file__).resolve().with_name("doc-size-allowlist.txt")

BUDGET_BYTES = 40 * 1024
HEAD_MARKER = "## At a glance"

# The documents that were largest when the budget was introduced. Each one has
# a head, written or already present, so an agent can stop after the first
# screen. Other allowlisted documents gain heads as they are touched.
HEAD_REQUIRED = (
    "docs/playback-control/CONTINUOUS-QUALITY-BUILD.md",
    "docs/features/SHARED-LIBRARIES-IMPLEMENTATION.md",
    "docs/OPERATIONS.md",
    "docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md",
    "docs/DECODER_SELECTION_RECOVERY_STATUS.md",
    "docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md",
    "docs/API.md",
    "docs/streaming/TRANSCODE-DECOMPOSITION-PLAN.md",
    "docs/streaming/MACOS-VIDEO-PROCESSING-EVIDENCE.md",
    "docs/playback-control/PLAYBACK-CONTROL-STATUS.md",
)


def tracked_docs() -> dict[str, int]:
    """Tracked Markdown files under docs/ that the budget applies to, with sizes."""
    out = subprocess.run(
        ["git", "ls-files", "docs"], cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE
    ).stdout
    sizes = {}
    for path in out.splitlines():
        if not path.endswith(".md") or is_exempt(path):
            continue
        file = ROOT / path
        if file.is_file():
            sizes[path] = file.stat().st_size
    return sizes


def is_exempt(path: str) -> bool:
    parts = path.split("/")
    return "evidence" in parts[1:-1] or parts[1:2] == ["archive"]


def read_allowlist() -> dict[str, int]:
    ceilings: dict[str, int] = {}
    for number, raw in enumerate(ALLOWLIST.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 2 or not fields[1].isdigit():
            raise AssertionError(f"{ALLOWLIST.name}:{number}: expected '<path> <max_bytes>', got {raw!r}")
        if fields[0] in ceilings:
            raise AssertionError(f"{ALLOWLIST.name}:{number}: {fields[0]} is listed twice")
        ceilings[fields[0]] = int(fields[1])
    return ceilings


class DocSizeBudget(unittest.TestCase):
    def test_unlisted_docs_fit_the_budget(self):
        allowed = read_allowlist()
        over = sorted(
            (path, size) for path, size in tracked_docs().items()
            if size > BUDGET_BYTES and path not in allowed
        )
        self.assertEqual(
            [], over,
            "These docs are over the 40 KB budget and are not on the shrink-only list. "
            "Split the doc, or move detail to a sibling doc and link it. Do not add to "
            "the allowlist; it only shrinks:\n"
            + "\n".join(f"  {path}: {size} bytes" for path, size in over),
        )

    def test_listed_docs_stay_under_their_ceiling(self):
        sizes = tracked_docs()
        over = sorted(
            (path, sizes[path], ceiling) for path, ceiling in read_allowlist().items()
            if path in sizes and sizes[path] > ceiling
        )
        self.assertEqual(
            [], over,
            "These docs are on the shrink-only list and have grown past their ceiling; "
            "trim it or split it:\n"
            + "\n".join(f"  {path}: {size} bytes, ceiling {ceiling}" for path, size, ceiling in over),
        )

    def test_listed_docs_still_need_their_slot(self):
        sizes = tracked_docs()
        small = sorted(
            (path, sizes[path]) for path in read_allowlist()
            if path in sizes and sizes[path] <= BUDGET_BYTES
        )
        self.assertEqual(
            [], small,
            "These docs are now within the 40 KB budget; remove them from "
            f"{ALLOWLIST.name} so they stay there:\n"
            + "\n".join(f"  {path}: {size} bytes" for path, size in small),
        )

    def test_listed_paths_exist(self):
        sizes = tracked_docs()
        missing = sorted(path for path in read_allowlist() if path not in sizes)
        self.assertEqual(
            [], missing,
            f"These paths in {ALLOWLIST.name} are not tracked docs the budget applies to "
            "(deleted, moved, or an exempt evidence/archive path); remove the lines:\n"
            + "\n".join(f"  {path}" for path in missing),
        )

    def test_largest_docs_open_with_a_head(self):
        headless = []
        for path in HEAD_REQUIRED:
            file = ROOT / path
            text = file.read_text(encoding="utf-8") if file.is_file() else ""
            if f"\n{HEAD_MARKER}" not in text:
                headless.append(path)
        self.assertEqual(
            [], headless,
            f"These docs need a '{HEAD_MARKER}' section near the top (what it is, where it "
            "stands, which heading answers which question):\n"
            + "\n".join(f"  {path}" for path in headless),
        )


if __name__ == "__main__":
    unittest.main()
