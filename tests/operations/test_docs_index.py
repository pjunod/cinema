"""The docs index has to stay true, or it is worse than no index.

docs/README.md is the router for docs/: the reference documents at the
root and one row per subject folder, linking that folder's own index,
docs/<area>/README.md. Together they are the map. Four properties keep it
usable: every Markdown file under docs/ is listed on the root page or in its
folder's index, a folder index lists only its own folder, every link on the
pages resolves, and nothing anywhere in the repo points at a docs/ path that
does not exist. A file that is moved or added without touching an index
fails here rather than being found by someone six weeks later.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
DOCS = ROOT / "docs"
INDEX = DOCS / "README.md"

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


def folder_indexes() -> list[Path]:
    """Every subject-folder index: docs/<area>/README.md, one level down."""
    return sorted(DOCS.glob("*/README.md"))


def indexes() -> list[Path]:
    return [INDEX, *folder_indexes()]


def is_listed(relative: str, root_index: str, folder_texts: dict[str, str]) -> bool:
    """A docs/-relative path is listed on the root page, or in its folder's index
    by its path relative to that folder."""
    if relative in root_index:
        return True
    area, _, inside = relative.partition("/")
    if not inside:
        return False
    # Bounded on the left so SOME-PLAN.md does not list PLAN.md.
    return re.search(rf"(?<![\w.-]){re.escape(inside)}", folder_texts.get(area, "")) is not None


TABLE_ROW_LINK = re.compile(r"^\|\s*\[[^\]]*\]\(([^)\s#]+)")


class DocsIndexTest(unittest.TestCase):
    def test_every_docs_markdown_file_is_listed(self) -> None:
        index = INDEX.read_text(encoding="utf-8")
        folders = {
            path.parent.name: path.read_text(encoding="utf-8")
            for path in folder_indexes()
        }
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
            if not is_listed(relative, index, folders):
                missing.append(path)
        self.assertEqual(
            [],
            sorted(missing),
            "these documents are not listed in their folder's index "
            "(docs/<area>/README.md) or, for a root document, in "
            "docs/README.md; add a row saying what question each one answers",
        )

    def test_a_folder_index_lists_only_its_own_folder(self) -> None:
        strays: dict[str, list[str]] = {}
        for path in folder_indexes():
            folder = path.parent.resolve()
            for line in path.read_text(encoding="utf-8").splitlines():
                match = TABLE_ROW_LINK.match(line)
                if match is None or re.match(r"^(https?:|mailto:|/)", match.group(1)):
                    continue
                target = (path.parent / match.group(1)).resolve()
                if target == folder or folder in target.parents:
                    continue
                relative = target.relative_to(DOCS.resolve()).as_posix()
                if any(
                    relative == unlisted or relative.startswith(f"{unlisted}/")
                    for unlisted in UNLISTED_DIRS
                ):
                    continue  # records cited beside the row they belong to
                strays.setdefault(path.relative_to(ROOT).as_posix(), []).append(
                    relative
                )
        self.assertEqual(
            {},
            strays,
            "a folder index has a row for a document in another folder; move the "
            "row to that folder's README.md (or to docs/README.md for a root doc)",
        )

    def test_the_root_index_links_every_folder_index(self) -> None:
        index = INDEX.read_text(encoding="utf-8")
        linked = {
            (DOCS / target).resolve()
            for target in LINK.findall(index)
            if not re.match(r"^(https?:|mailto:|/|#)", target)
        }
        unlinked = [
            path.relative_to(ROOT).as_posix()
            for path in folder_indexes()
            if path.resolve() not in linked
        ]
        self.assertEqual([], unlinked, "docs/README.md must link every folder index")
        listable = sorted(
            {
                path.split("/")[1]
                for path in tracked_files()
                if path.startswith("docs/")
                and path.count("/") >= 2
                and path.endswith((".md", ".html"))
                and path.split("/")[1] not in UNLISTED_DIRS
            }
        )
        without_index = [
            area for area in listable if not (DOCS / area / "README.md").exists()
        ]
        self.assertEqual(
            [], without_index, "every subject folder under docs/ needs a README.md index"
        )

    def test_every_index_link_resolves(self) -> None:
        broken: dict[str, list[str]] = {}
        for path in indexes():
            for target in sorted(set(LINK.findall(path.read_text(encoding="utf-8")))):
                if re.match(r"^(https?:|mailto:|/|#)", target):
                    continue
                if not (path.parent / target).exists():
                    broken.setdefault(path.relative_to(ROOT).as_posix(), []).append(
                        target
                    )
        self.assertEqual({}, broken, "a docs index links to paths that do not exist")

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
