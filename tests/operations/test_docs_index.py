"""The docs index has to stay true, or it is worse than no index.

docs/README.md is the map of docs/. Three properties keep it usable:
every Markdown file under docs/ is listed, every link on the page
resolves, and nothing anywhere in the repo points at a docs/ path that
does not exist. A file that is moved or added without touching the index
fails here rather than being found by someone six weeks later.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
INDEX = ROOT / "docs" / "README.md"

# Directories under docs/ whose contents are records rather than prose:
# per-build notes, generated evidence, and image assets. They are named on
# the index as directories, so listing every file would only add noise.
UNLISTED_DIRS = {"apple-builds", "evidence", "img", "mockups", "archive/retro-2026-08-09"}

# docs/ paths referenced in the repo that do not exist here. Each is prose in
# an older document naming a companion that was never written, a reference to
# another repository's docs/ that happens to share the path shape, or a
# synthetic test fixture. Fix by writing the doc or correcting the reference,
# not by widening this set. Note that the macOS working copy is
# case-insensitive and CI is not, so a lowercase path can resolve locally and
# fail on Linux — this list is judged by the Linux answer.
KNOWN_ABSENT = {
    "docs/FRAGMENT-INDEX-QUEUE-REPAIR-HANDOFF.md",   # named by crates/plurx-core/src/store/placeholder_census.rs
    # clients/EBOOK-READER-PLAN.md §M6 names three files in *Curator's* docs/,
    # not this repo's. The path shape is identical, so the sweep sees them.
    "docs/settings.md",
    "docs/integration.md",
    "docs/plan-integration.md",
    "docs/base.md", "docs/current.md", "docs/plan.md",  # tests/validation/test_history.py fixtures
    "docs/NOTES.md",                                 # tests/validation/test_mobile_versions.py fixture
}

BINARY_SUFFIXES = {
    ".png", ".jpg", ".jpeg", ".gif", ".pdf", ".zip", ".ipa", ".apk",
    ".mp4", ".ico", ".woff", ".woff2", ".ttf", ".jar", ".keystore",
}

LINK = re.compile(r'(?:\]\(|\]:[ \t]*|href="|src=")([^)"\s#]+)')
DOCS_PATH = re.compile(r"(?<![\w/.-])(docs/[A-Za-z0-9_.\-/]*\.(?:md|html))")


def tracked_files() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files"], cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE
    ).stdout
    return [line for line in out.splitlines() if line]


def readable(paths: list[str]) -> list[tuple[str, str]]:
    for path in paths:
        if Path(path).suffix.lower() in BINARY_SUFFIXES:
            continue
        try:
            yield path, (ROOT / path).read_text(encoding="utf-8")
        except (UnicodeDecodeError, FileNotFoundError):
            continue


class DocsIndexTest(unittest.TestCase):
    def test_every_docs_markdown_file_is_listed(self) -> None:
        index = INDEX.read_text(encoding="utf-8")
        missing = []
        for path in tracked_files():
            if not path.startswith("docs/") or not path.endswith((".md", ".html")):
                continue
            relative = path[len("docs/") :]
            if relative == "README.md":
                continue
            if any(
                relative.startswith(f"{unlisted}/") for unlisted in UNLISTED_DIRS
            ):
                continue
            if relative not in index:
                missing.append(path)
        self.assertEqual(
            [],
            sorted(missing),
            "these documents are not listed in docs/README.md; add a row "
            "saying what question each one answers",
        )

    def test_every_index_link_resolves(self) -> None:
        broken = []
        for target in sorted(set(LINK.findall(INDEX.read_text(encoding="utf-8")))):
            if re.match(r"^(https?:|mailto:|/|#)", target):
                continue
            if not (INDEX.parent / target).exists():
                broken.append(target)
        self.assertEqual([], broken, "docs/README.md links to paths that do not exist")

    def test_no_reference_points_at_a_missing_docs_path(self) -> None:
        dangling: dict[str, list[str]] = {}
        for path, text in readable(tracked_files()):
            for target in sorted(set(DOCS_PATH.findall(text))):
                if target in KNOWN_ABSENT or (ROOT / target).exists():
                    continue
                dangling.setdefault(path, []).append(target)
        self.assertEqual(
            {},
            dangling,
            "these files reference docs/ paths that do not exist — a document "
            "was moved or renamed without updating what points at it",
        )

    def test_relative_links_between_documents_resolve(self) -> None:
        broken: dict[str, list[str]] = {}
        for path, text in readable(tracked_files()):
            if not path.startswith("docs/"):
                continue
            base = (ROOT / path).parent
            for target in sorted(set(LINK.findall(text))):
                if re.match(r"^(https?:|mailto:|/|\{|data:)", target):
                    continue
                if target.startswith("docs/"):
                    continue  # covered by the absolute check above
                candidate = (base / target).resolve()
                if candidate.exists():
                    continue
                # Only judge targets that look like paths to repo files;
                # prose inside link syntax is not a link.
                if Path(target).suffix in {".md", ".html", ".toml", ".json"}:
                    broken.setdefault(path, []).append(target)
        self.assertEqual({}, broken, "relative links between documents do not resolve")


if __name__ == "__main__":
    unittest.main()
