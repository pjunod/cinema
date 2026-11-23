"""`scripts/fold`: fragment folders fold into the pages every pull request used to edit."""

from __future__ import annotations

import importlib.machinery
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def _load(name: str, path: str):
    loader = importlib.machinery.SourceFileLoader(name, str(ROOT / path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


FOLD = _load("fold_script", "scripts/fold")


class StatusFoldCase(unittest.TestCase):
    def tree(self, entries: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "status.d").mkdir()
        for name, text in entries.items():
            (root / "status.d" / name).write_text(text, encoding="utf-8")
        return root

    def test_entries_render_newest_first_bullets_before_sections(self):
        root = self.tree({
            "2026-11-01-old.md": "- **2026-11-01 · Old:** done.\n",
            "2026-11-20-new.md": "- **2026-11-20 · New:** going.\n",
            "2026-11-10-note.md": "## 2026-11-10 · A longer note\n\nParagraph.\n",
            "INDEX.md": "## Older efforts — where each one now lives\n",
            "README.md": "# not an entry\n",
        })
        page = FOLD.render_status(root)
        self.assertIn("**Updated:** 2026-11-20", page)
        self.assertLess(page.index("New:"), page.index("Old:"))
        self.assertLess(page.index("Old:"), page.index("## 2026-11-10"))
        self.assertTrue(page.rstrip().endswith("## Older efforts — where each one now lives"))
        self.assertNotIn("not an entry", page)

    def test_a_badly_named_or_shaped_entry_is_refused(self):
        for entries in ({"notes.md": "- x\n"}, {"2026-11-01-x.md": "plain text\n"}):
            with self.subTest(entries=entries), self.assertRaises(FOLD.FoldError):
                FOLD.status_entries(self.tree(entries))

    def test_two_branches_adding_entries_merge_without_conflict(self):
        """The property the folder exists for: concurrent entries never collide."""

        root = self.tree({"2026-11-01-base.md": "- **2026-11-01 · Base:** x.\n"})

        def git(*args: str) -> subprocess.CompletedProcess[str]:
            return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True)

        for args in (("init", "-q", "-b", "main"), ("config", "user.name", "Fold Test"),
                     ("config", "user.email", "fold@example.invalid"), ("add", "-A"),
                     ("commit", "-qm", "base")):
            git(*args)
        for branch, day in (("a", "02"), ("b", "03")):
            git("checkout", "-q", "-b", branch, "main")
            (root / f"status.d/2026-11-{day}-{branch}.md").write_text(
                f"- **2026-11-{day} · {branch}:** y.\n", encoding="utf-8")
            git("add", "-A")
            git("commit", "-qm", branch)
        git("checkout", "-q", "main")
        self.assertEqual(git("merge", "-q", "--no-ff", "-m", "a", "a").returncode, 0)
        merged = git("merge", "-q", "--no-ff", "-m", "b", "b")
        self.assertEqual(merged.returncode, 0, merged.stdout + merged.stderr)
        self.assertEqual(len(FOLD.status_entries(root)), 3)


class PaperworkFoldCase(unittest.TestCase):
    PAPERWORK = _load("paperwork_script", "scripts/paperwork")

    def head(self) -> str:
        return subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, check=True,
                              capture_output=True, text=True).stdout.strip()

    def test_a_pull_request_may_not_edit_status_md(self):
        base = self.head()
        if subprocess.run(["git", "cat-file", "-e", f"{base}:scripts/fold"], cwd=ROOT).returncode:
            self.skipTest("HEAD predates scripts/fold; the cutover pull request writes STATUS.md")
        refused = self.PAPERWORK.check_folded_pages(base, ["STATUS.md"], "fix: something")
        self.assertFalse(refused.ok)
        self.assertIn("generated from status.d/", refused.detail)
        self.assertTrue(self.PAPERWORK.check_folded_pages(
            base, ["STATUS.md"], "chore(fold): fold fragment folders").ok)
        self.assertTrue(self.PAPERWORK.check_folded_pages(base, ["status.d/x.md"], "fix: x").ok)


if __name__ == "__main__":
    unittest.main()
