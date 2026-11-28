"""`scripts/fold`: fragment folders fold into the pages every pull request used to edit."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import sys
from pathlib import Path
import subprocess
import tempfile
import unittest
import unittest.mock

ROOT = Path(__file__).resolve().parents[2]


def _load(name: str, path: str):
    loader = importlib.machinery.SourceFileLoader(name, str(ROOT / path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    # Registered before it runs: a dataclass in it looks its module up
    # in sys.modules (Python 3.12 fails on an unregistered one).
    sys.modules[spec.name] = module
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

    def test_a_pull_request_may_not_edit_status_md_or_add_ledger_rows(self):
        base = self.head()
        if subprocess.run(["git", "cat-file", "-e", f"{base}:scripts/fold"], cwd=ROOT).returncode:
            self.skipTest("HEAD predates scripts/fold; the cutover pull request writes STATUS.md")
        row = '[[fixes]]\nid = "x"\n'
        texts = {
            "STATUS.md": ("old page\n", "hand-edited page\n"),
            "tests/client-fixes.toml": ("version = 1\n" + row, "version = 1\n" + row + row),
            "Cargo.toml": ("", ""),
        }
        original = self.PAPERWORK._at

        def at(revision, path):
            if path in texts:
                return texts[path][0 if revision is not None else 1]
            return original(revision, path)

        self.PAPERWORK._at = at
        self.addCleanup(setattr, self.PAPERWORK, "_at", original)
        refused = self.PAPERWORK.check_folded_pages(
            base, ["STATUS.md", "tests/client-fixes.toml"], "fix: something")
        self.assertFalse(refused.ok)
        self.assertIn("generated from status.d/", refused.detail)
        self.assertIn("gained 1 [[fixes]] row", refused.detail)
        # A fold's title and paths are not enough: it must be exactly the fold of its base.
        hand_fold = self.PAPERWORK.check_folded_pages(
            base, ["STATUS.md", "tests/client-fixes.toml"], "chore(fold): fold fragment folders")
        self.assertFalse(hand_fold.ok)
        self.assertIn("the fold of", hand_fold.detail)
        # The title alone does not exempt a pull request that also changes code.
        self.assertFalse(self.PAPERWORK.check_folded_pages(
            base, ["STATUS.md", "crates/plurxd/src/main.rs"], "chore(fold): sneaky").ok)
        # Editing an existing ledger row (a re-anchor) is allowed.
        texts["tests/client-fixes.toml"] = ("version = 1\n" + row, "version = 1\n" + row.replace("x", "y"))
        self.assertTrue(self.PAPERWORK.check_folded_pages(base, ["tests/client-fixes.toml"], "fix: x").ok)

    def test_only_a_raised_workspace_version_in_the_own_diff_is_a_release_cut(self):
        base = self.head()
        cargo = '[workspace.package]\nversion = "{}"\n'
        original = self.PAPERWORK._at
        versions = {"before": "0.8.0", "after": "0.8.0"}

        def at(revision, path):
            if path == "Cargo.toml":
                return cargo.format(versions["after" if revision is None else "before"])
            return original(revision, path)

        self.PAPERWORK._at = at
        self.addCleanup(setattr, self.PAPERWORK, "_at", original)
        self.assertFalse(self.PAPERWORK._is_release_cut(base, ["Cargo.toml"]))
        versions["after"] = "0.9.0"
        self.assertTrue(self.PAPERWORK._is_release_cut(base, ["Cargo.toml"]))
        self.assertFalse(self.PAPERWORK._is_release_cut(base, ["CHANGELOG.md"]))
        versions["after"] = "0.7.9"
        self.assertFalse(self.PAPERWORK._is_release_cut(base, ["Cargo.toml"]))


class ClientFixesFoldCase(unittest.TestCase):
    def test_rows_move_into_the_ledger_past_the_bound_or_at_a_release(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "tests/client-fixes.d").mkdir(parents=True)
        (root / "tests/client-fixes.toml").write_text(
            'version = 1\nenforce_after = "abcdef1"\n\n[[fixes]]\nid = "old"\n', encoding="utf-8")
        (root / "tests/client-fixes.d/a.toml").write_text('[[fixes]]\nid = "new"\n', encoding="utf-8")
        text, absorbed = FOLD.render_client_fixes(root, everything=False)
        self.assertEqual(absorbed, [])
        text, absorbed = FOLD.render_client_fixes(root, everything=True)
        self.assertEqual(absorbed, ["tests/client-fixes.d/a.toml"])
        self.assertLess(text.index('id = "new"'), text.index('id = "old"'))
        self.assertTrue(text.startswith('version = 1\nenforce_after = "abcdef1"\n'))
        for index in range(FOLD.CLIENT_FIXES_BOUND):
            (root / f"tests/client-fixes.d/b{index:02}.toml").write_text(
                f'[[fixes]]\nid = "b{index}"\n', encoding="utf-8")
        _, absorbed = FOLD.render_client_fixes(root, everything=False)
        self.assertEqual(len(absorbed), FOLD.CLIENT_FIXES_BOUND + 1)

class FoldTree(unittest.TestCase):
    """A committed tree with one status entry and a client-fix folder past its bound."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = root = Path(temporary.name)
        (root / "status.d").mkdir()
        (root / "status.d/2026-11-01-base.md").write_text("- **2026-11-01 · Base:** x.\n", encoding="utf-8")
        (root / "tests/client-fixes.d").mkdir(parents=True)
        (root / "tests/client-fixes.toml").write_text(
            'version = 1\nenforce_after = "0"\n\n[[fixes]]\nid = "old"\nsource = "a"\n', encoding="utf-8")
        for number in range(FOLD.CLIENT_FIXES_BOUND + 1):
            (root / f"tests/client-fixes.d/row-{number:02}.toml").write_text(
                f'# row {number}\n[[fixes]]\nid = "row-{number:02}"\nsource = "s{number}"\n', encoding="utf-8")
        (root / "STATUS.md").write_text(FOLD.render_status(root), encoding="utf-8")
        for args in (("init", "-q", "-b", "main"), ("config", "user.name", "Fold Test"),
                     ("config", "user.email", "fold@example.invalid"), ("add", "-A"),
                     ("commit", "-qm", "base")):
            subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)

    def fold(self) -> list[str]:
        pages, removed = FOLD.changes(self.root)
        for page, text in pages.items():
            (self.root / page).write_text(text, encoding="utf-8")
        for path in removed:
            (self.root / path).unlink()
        return [*pages, *removed]


class VerifyFoldCase(FoldTree):
    """`verify`: the one check a fold lands on -- exactly a fold of its base, nothing lost."""

    def test_a_real_fold_verifies(self):
        paths = self.fold()
        self.assertIn("tests/client-fixes.toml", paths)
        self.assertEqual(FOLD.verify(self.root, "HEAD", paths), [])

    def test_a_fold_that_drops_or_alters_a_row_is_refused(self):
        paths = self.fold()
        ledger = self.root / "tests/client-fixes.toml"
        ledger.write_text(ledger.read_text(encoding="utf-8").replace('source = "s3"', 'source = "s33"'),
                          encoding="utf-8")
        problems = FOLD.verify(self.root, "HEAD", paths)
        self.assertEqual(problems, ["tests/client-fixes.toml is not the fold of HEAD's folder"])

    def test_an_incomplete_fold_is_refused(self):
        (self.root / "tests/client-fixes.d/row-00.toml").unlink()
        problems = FOLD.verify(self.root, "HEAD", ["tests/client-fixes.d/row-00.toml"])
        self.assertIn("tests/client-fixes.toml: the fold of HEAD changes it, but this does not", problems)

    def test_removing_a_status_entry_or_touching_another_file_is_refused(self):
        paths = self.fold()
        (self.root / "status.d/2026-11-01-base.md").unlink()
        (self.root / "STATUS.md").write_text(FOLD.render_status(self.root), encoding="utf-8")
        (self.root / "README.md").write_text("x\n", encoding="utf-8")
        problems = FOLD.verify(self.root, "HEAD", [*paths, "status.d/2026-11-01-base.md", "README.md"])
        self.assertTrue(any(problem.startswith("status.d/2026-11-01-base.md") for problem in problems))
        self.assertTrue(any(problem.startswith("README.md") for problem in problems))

    def test_a_fold_that_adds_an_entry_of_its_own_is_refused(self):
        paths = self.fold()
        (self.root / "status.d/2026-11-02-new.md").write_text("- **2026-11-02 · New:** y.\n", encoding="utf-8")
        (self.root / "STATUS.md").write_text(FOLD.render_status(self.root), encoding="utf-8")
        problems = FOLD.verify(self.root, "HEAD", [*paths, "status.d/2026-11-02-new.md"])
        self.assertIn("status.d/2026-11-02-new.md: changed, but the fold of HEAD does not change it", problems)

    def test_a_release_style_fold_of_everything_verifies(self):
        for path in sorted((self.root / "tests/client-fixes.d").glob("*.toml"))[1:]:
            path.unlink()
        subprocess.run(["git", "commit", "-qam", "under the bound"], cwd=self.root, check=True)
        self.assertEqual(FOLD.changes(self.root), ({}, []))
        pages, removed = FOLD.changes(self.root, everything=True)
        for page, text in pages.items():
            (self.root / page).write_text(text, encoding="utf-8")
        for path in removed:
            (self.root / path).unlink()
        self.assertEqual(FOLD.verify(self.root, "HEAD", [*pages, *removed]), [])
        # The same result is not the fold of an older base with more fragments.
        self.assertNotEqual(FOLD.verify(self.root, "HEAD~1", [*pages, *removed]), [])

    def test_a_page_with_other_line_endings_is_not_the_fold(self):
        paths = self.fold()
        page = self.root / "STATUS.md"
        page.write_bytes(page.read_bytes().replace(b"\n", b"\r\n"))
        problems = FOLD.verify(self.root, "HEAD", sorted({*paths, "STATUS.md"}))
        self.assertTrue(any(problem.startswith("STATUS.md") for problem in problems), problems)

    def test_a_fragment_with_a_non_ascii_name_folds_and_verifies(self):
        (self.root / "tests/client-fixes.d/row-00.toml").rename(self.root / "tests/client-fixes.d/é.toml")
        subprocess.run(["git", "add", "-A"], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "rename"], cwd=self.root, check=True)
        self.assertEqual(FOLD.verify(self.root, "HEAD", self.fold()), [])


class PublishFoldCase(FoldTree):
    """`publish` lands only a verified fold, merged at the head it verified."""

    def publish(self, paths: list[str]) -> tuple[int, list[tuple[str, str, dict | None]]]:
        remote = self.root.parent / f"{self.root.name}-forge" / "noirr" / "plurx.git"
        remote.parent.mkdir(parents=True)
        subprocess.run(["git", "init", "-q", "--bare", str(remote)], check=True)
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(remote.parent.parent)], check=True))
        calls = []

        def api(server, token, method, path, payload=None):
            calls.append((method, path, payload))
            if method == "GET":
                raise FOLD.ForgeStatus(404, "no pull request")
            return {"number": 7} if path.endswith("/pulls") else None

        environment = {"FOLD_BOT_TOKEN": "test-token", "GITHUB_REPOSITORY": "noirr/plurx",
                       "GITHUB_SERVER_URL": remote.parent.parent.as_uri()}
        original = FOLD._api
        FOLD._api = api
        try:
            with unittest.mock.patch.dict("os.environ", environment):
                return FOLD.publish(self.root, paths), calls
        finally:
            FOLD._api = original

    def test_a_verified_fold_merges_pinned_to_its_head(self):
        code, calls = self.publish(self.fold())
        self.assertEqual(code, 0)
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.root, check=True,
                              capture_output=True, text=True).stdout.strip()
        merge = calls[-1]
        self.assertTrue(merge[1].endswith("/pulls/7/merge"))
        self.assertEqual(merge[2]["head_commit_id"], head)
        self.assertNotIn("merge_when_checks_succeed", merge[2])
        self.assertIn("Changelog: none", merge[2]["MergeMessageField"])

    def test_without_the_bot_token_the_fold_fails_closed(self):
        paths = self.fold()
        head = lambda: subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.root, check=True,  # noqa: E731
                                      capture_output=True, text=True).stdout
        before = head()
        environment = {"FOLD_BOT_TOKEN": "", "GITHUB_SERVER_URL": "x", "GITHUB_REPOSITORY": "noirr/plurx"}
        with unittest.mock.patch.dict("os.environ", environment), \
                unittest.mock.patch.object(FOLD, "_api", side_effect=AssertionError("called the forge")):
            self.assertEqual(FOLD.publish(self.root, paths), 2)
        self.assertEqual(head(), before, "nothing is committed without the token")

    def test_a_fold_that_does_not_verify_is_never_pushed(self):
        paths = self.fold()
        ledger = self.root / "tests/client-fixes.toml"
        ledger.write_text(ledger.read_text(encoding="utf-8") + '\n[[fixes]]\nid = "extra"\n', encoding="utf-8")
        code, calls = self.publish(paths)
        self.assertEqual(code, 1)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
