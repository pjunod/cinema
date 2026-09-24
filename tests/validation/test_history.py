from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
import tempfile
import textwrap
import unittest

from validation.history import (
    CORRECTIVE_RE,
    ISSUE_RE,
    HistoryError,
    _audit_tips,
    _write_report,
    audit_history,
    landing_commit_title,
    load_coverage,
    load_merge_ledger,
    verify_migration_fidelity,
)
from validation.runner import load_catalog


CATALOG = """
version = 1

[settings]
profiles = ["commit"]
always_checks = ["baseline"]

[[checks]]
id = "baseline"
title = "Baseline"
command = "true"
profiles = ["commit"]

[[points]]
id = "app"
title = "Application"
contract = "The app works."
paths = ["src/**", "crates/**", "clients/**", "tests/**", "docs/**"]
checks = ["baseline"]
"""


class RepositoryFixture(unittest.TestCase):
    """The fixture-repository harness, shared by every case in this file.

    A base class with no tests of its own: subclassing a case that has tests
    would re-run them under every subclass's name.
    """

    def repository(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(["git", "config", "user.name", "History Test"], cwd=root, check=True)
        subprocess.run(
            ["git", "config", "user.email", "history@example.invalid"], cwd=root, check=True
        )
        (root / "src").mkdir()
        (root / "crates").mkdir()
        (root / "tests").mkdir()
        (root / "docs").mkdir()
        catalog_path = root / "points.toml"
        catalog_path.write_text(CATALOG, encoding="utf-8")
        coverage_dir = root / "regressions.d"
        coverage_dir.mkdir()
        return root, load_catalog(catalog_path), coverage_dir

    @staticmethod
    def write_coverage(coverage: Path, name: str, body: str) -> Path:
        """Write one `[[coverage]]` fragment, the way an author adds a mapping."""

        path = coverage / name
        path.write_text(
            "version = 1\n\n[[coverage]]\n" + textwrap.dedent(body).lstrip(),
            encoding="utf-8",
        )
        return path

    @staticmethod
    def commit(root: Path, subject: str):
        subprocess.run(["git", "add", "-A"], cwd=root, check=True)
        subprocess.run(["git", "commit", "-qm", subject], cwd=root, check=True)
        return subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE
        ).stdout.strip()


class HistoryAuditCase(RepositoryFixture):
    def test_fix_with_a_direct_regression_test_needs_no_ledger_entry(self):
        root, catalog, coverage = self.repository()
        (root / "src/app.rs").write_text("pub fn answer() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/app.rs").write_text(
            "pub fn answer() -> u8 { 2 }\n\n#[test]\nfn fixed_answer() { assert_eq!(answer(), 2); }\n",
            encoding="utf-8",
        )
        self.commit(root, "fix: return the corrected answer")

        report = audit_history(root, catalog, coverage)

        self.assertEqual(report.errors, ())
        self.assertEqual(report.mapped_count, 0)
        self.assertEqual(report.ignored_count, 0)
        self.assertEqual(report.direct_count, 1)

    def test_fix_without_a_test_must_name_current_check_evidence(self):
        root, catalog, coverage = self.repository()
        (root / "src/app.rs").write_text("pub fn answer() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/app.rs").write_text("pub fn answer() -> u8 { 2 }\n", encoding="utf-8")
        sha = self.commit(root, "fix: return the corrected answer")

        missing = audit_history(root, catalog, coverage)
        self.assertTrue(any(sha[:8] in error for error in missing.errors))

        self.write_coverage(
            coverage,
            f"{sha[:8]}-app.toml",
            f"""
            commits = ["{sha[:8]}"]
            points = ["app"]
            checks = ["baseline"]
            reason = "The current baseline exercises this generated behavior."
            """,
        )
        covered = audit_history(root, catalog, coverage)
        self.assertEqual(covered.errors, ())

    def test_a_merge_in_progress_audits_both_parents(self):
        """The hook runs before the merge commit exists, so it must see both sides.

        `git log` from HEAD sees only the first parent, but the merged tree
        already carries the second parent's coverage fragments. Auditing from
        HEAD alone therefore reports every one of those mappings as describing a
        commit that does not exist -- refusing a merge in which nothing is
        wrong, and leaving no way to merge into an effort branch at all short of
        skipping the hook. The audited population is the history the resulting
        commit will actually have.
        """

        root, catalog, coverage = self.repository()
        (root / "src/app.rs").write_text("pub fn answer() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        base = subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"],
            cwd=root, check=True, text=True, stdout=subprocess.PIPE,
        ).stdout.strip()

        def corrective(branch: str, value: str, module: str) -> str:
            subprocess.run(["git", "checkout", "-q", "-b", branch, base], cwd=root, check=True)
            # The other branch's fragment was the only file in `regressions.d`,
            # and git does not track an empty directory, so checking out the
            # base takes the directory with it.
            coverage.mkdir(parents=True, exist_ok=True)
            (root / "src" / module).write_text(
                "pub fn answer() -> u8 { %s }\n" % value, encoding="utf-8"
            )
            sha = self.commit(root, "fix: return the corrected answer from %s" % module)
            self.write_coverage(
                coverage,
                f"{sha[:8]}-app.toml",
                f"""
                commits = ["{sha[:8]}"]
                points = ["app"]
                checks = ["baseline"]
                reason = "The current baseline exercises this generated behavior."
                """,
            )
            # Two commits, the way a mapping is really made: the fix, then the
            # fragment naming it. So the tip the merge records is the mapping
            # commit, not the fix.
            return sha, self.commit(root, "validation: map %s" % sha[:8])

        corrective("ours", "2", "app.rs")
        theirs, theirs_tip = corrective("theirs", "3", "other.rs")

        subprocess.run(["git", "checkout", "-q", "ours"], cwd=root, check=True)
        merge = subprocess.run(
            ["git", "merge", "--no-commit", "--no-ff", "-q", "theirs"],
            cwd=root, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        self.assertEqual(merge.returncode, 0, merge.stdout)
        self.assertTrue((root / ".git/MERGE_HEAD").is_file(), "expected a merge in progress")

        self.assertEqual(_audit_tips(root), ("HEAD", theirs_tip))
        self.assertEqual(audit_history(root, catalog, coverage).errors, ())

        # The same tree, audited from HEAD alone, is what the hook was doing:
        # the other parent's mapping describes a commit it cannot see.
        (root / ".git/MERGE_HEAD").unlink()
        self.assertEqual(_audit_tips(root), ("HEAD",))
        one_sided = audit_history(root, catalog, coverage).errors
        self.assertTrue(
            any(f"{theirs[:8]} matches 0 audited commits" in error for error in one_sided),
            one_sided,
        )

    def test_explicit_ledger_survives_a_non_corrective_squash_title(self):
        root, catalog, coverage = self.repository()
        (root / "src/app.rs").write_text(
            "pub fn imported() -> bool { true }\n", encoding="utf-8"
        )
        sha = self.commit(root, "Import state into a fresh target (#119)")
        self.write_coverage(
            coverage,
            f"{sha[:8]}-app.toml",
            f"""
            commits = ["{sha[:8]}"]
            points = ["app"]
            checks = ["baseline"]
            reason = "The current import contract exercises the squash-merged behavior."
            """,
        )

        report = audit_history(root, catalog, coverage)

        self.assertEqual(report.errors, ())
        self.assertEqual(report.mapped_count, 1)

    def test_client_anchor_survives_a_non_corrective_squash_title(self):
        root, catalog, coverage = self.repository()
        (root / "clients").mkdir()
        (root / "clients/app.swift").write_text(
            "func restoredPolicy() {}\n", encoding="utf-8"
        )
        (root / "tests/client.swift").write_text(
            "func testRestoredPolicy() {}\n", encoding="utf-8"
        )
        subject = "Persist saved client policy (#120)"
        self.assertIsNone(ISSUE_RE.search(subject))
        sha = self.commit(root, subject)
        (root / "tests/client-fixes.toml").write_text(
            textwrap.dedent(
                f"""
                version = 1

                [[fixes]]
                id = "client.restored-policy"
                commits = ["{sha[:8]}"]
                source = "clients/app.swift"
                source_anchor = "restoredPolicy"
                test = "tests/client.swift"
                test_anchor = "testRestoredPolicy"
                """
            ),
            encoding="utf-8",
        )

        report = audit_history(root, catalog, coverage)

        self.assertEqual(report.errors, ())
        self.assertEqual(report.anchored_count, 1)

    def test_commit_prefixes_must_be_stable_lowercase_shas(self):
        root, catalog, coverage = self.repository()
        (root / "src/app.rs").write_text(
            "pub fn answer() -> u8 { 1 }\n", encoding="utf-8"
        )
        self.commit(root, "feat: seed")
        for prefix in ("", "a", "123456", "ABCDEF0", "123456g", "a" * 41):
            with self.subTest(prefix=prefix):
                self.write_coverage(
                    coverage,
                    "candidate-app.toml",
                    f"""
                    commits = ["{prefix}"]
                    points = ["app"]
                    checks = ["baseline"]
                    reason = "Invalid prefixes must fail before history inspection."
                    """,
                )
                with self.assertRaises(HistoryError) as raised:
                    audit_history(root, catalog, coverage)
                self.assertEqual(
                    str(raised.exception),
                    "candidate-app.toml commits must contain Git SHA prefixes",
                )

    def test_one_commit_cannot_use_two_client_anchor_prefixes(self):
        root, catalog, coverage = self.repository()
        (root / "clients").mkdir()
        (root / "clients/app.swift").write_text(
            "func restoredPolicy() {}\n", encoding="utf-8"
        )
        (root / "tests/client.swift").write_text(
            "func testRestoredPolicy() {}\n", encoding="utf-8"
        )
        sha = self.commit(root, "fix(client): restore saved policy")
        rows = []
        for identifier, prefix in (("short", sha[:8]), ("long", sha[:12])):
            rows.append(
                textwrap.dedent(
                    f"""
                    [[fixes]]
                    id = "client.{identifier}"
                    commits = ["{prefix}"]
                    source = "clients/app.swift"
                    source_anchor = "restoredPolicy"
                    test = "tests/client.swift"
                    test_anchor = "testRestoredPolicy"
                    """
                )
            )
        (root / "tests/client-fixes.toml").write_text(
            "version = 1\n" + "".join(rows), encoding="utf-8"
        )

        report = audit_history(root, catalog, coverage)

        self.assertTrue(any("anchored more than once" in error for error in report.errors))

    def test_unknown_checks_and_duplicate_commit_coverage_fail_loudly(self):
        root, catalog, coverage = self.repository()
        (root / "src/app.rs").write_text("pub fn answer() -> u8 { 1 }\n", encoding="utf-8")
        sha = self.commit(root, "fix: seed the corrected answer")
        self.write_coverage(
            coverage,
            f"{sha[:8]}-first.toml",
            f"""
            commits = ["{sha[:8]}"]
            points = ["app"]
            checks = ["missing"]
            reason = "First claim."
            """,
        )
        self.write_coverage(
            coverage,
            f"{sha[:8]}-second.toml",
            f"""
            commits = ["{sha[:8]}"]
            points = ["app"]
            checks = ["baseline"]
            reason = "Duplicate claim."
            """,
        )

        report = audit_history(root, catalog, coverage)

        self.assertTrue(any("unknown check missing" in error for error in report.errors))
        self.assertTrue(any("covered more than once" in error for error in report.errors))

    def test_documentation_correction_can_be_explicitly_ignored(self):
        root, catalog, coverage = self.repository()
        (root / "docs/plan.md").write_text("Correct explanation.\n", encoding="utf-8")
        sha = self.commit(root, "docs: correct the explanation")
        self.write_coverage(
            coverage,
            f"{sha[:8]}-non-runtime.toml",
            f"""
            commits = ["{sha[:8]}"]
            reason = "Documentation only; no executable behavior changed."
            ignore = true
            """,
        )

        report = audit_history(root, catalog, coverage)

        self.assertEqual(report.errors, ())
        self.assertEqual(report.mapped_count, 0)
        self.assertEqual(report.ignored_count, 1)

    def test_runtime_fix_cannot_hide_behind_an_unrelated_test_edit(self):
        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text(
            "pub fn answer() -> u8 { 1 }\n", encoding="utf-8"
        )
        base = self.commit(root, "feat: seed runtime")
        (root / "crates/app.rs").write_text(
            "pub fn answer() -> u8 { 2 }\n", encoding="utf-8"
        )
        (root / "tests/unrelated.rs").write_text(
            "#[test]\nfn unrelated() { assert_eq!(1, 1); }\n", encoding="utf-8"
        )
        (root / "tests/client-fixes.toml").write_text(
            textwrap.dedent(
                f"""
                version = 1
                enforce_after = "{base[:8]}"
                """
            ),
            encoding="utf-8",
        )
        sha = self.commit(root, "Address runtime review feedback")

        report = audit_history(root, catalog, coverage)

        self.assertTrue(
            any(
                sha[:8] in error and "needs an explicit" in error
                for error in report.errors
            ),
            report.errors,
        )

    @staticmethod
    def git(root: Path, *args: str) -> str:
        env = os.environ.copy()
        for name in (
            "GIT_COMMON_DIR",
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_PREFIX",
            "GIT_SHALLOW_FILE",
            "GIT_WORK_TREE",
        ):
            env.pop(name, None)
        return subprocess.run(
            ["git", *args], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=env,
        ).stdout.strip()

    def pending_merge(self, root: Path, *, client: bool = False):
        (root / "docs/base.md").write_text("seed\n", encoding="utf-8")
        base = self.commit(root, "feat: seed")
        self.git(root, "checkout", "-qb", "incoming")
        source = root / ("clients/app.swift" if client else "crates/app.rs")
        source.parent.mkdir(exist_ok=True)
        source.write_text("corrected behavior\n", encoding="utf-8")
        (root / "tests/unrelated.rs").write_text(
            "#[test]\nfn unrelated() {}\n", encoding="utf-8"
        )
        incoming = self.commit(root, "fix: retain corrected behavior")
        self.git(root, "checkout", "-qb", "current", base)
        (root / "docs/current.md").write_text("current\n", encoding="utf-8")
        self.commit(root, "docs: describe current branch")
        self.git(root, "merge", "--no-commit", "--no-ff", "incoming")
        return base, incoming

    def map_commit(self, coverage: Path, sha: str):
        self.write_coverage(
            coverage, f"{sha[:8]}-app.toml",
            f'''commits = ["{sha[:8]}"]
points = ["app"]
checks = ["baseline"]
reason = "Focused current regression evidence covers the incoming behavior."
''',
        )

    def test_pending_merge_accepts_mapping_only_for_its_actual_parents(self):
        root, catalog, coverage = self.repository()
        base, incoming = self.pending_merge(root)
        self.map_commit(coverage, incoming)
        (root / "tests/client-fixes.toml").write_text(
            f'version = 1\nenforce_after = "{base}"\n', encoding="utf-8"
        )
        report = audit_history(root, catalog, coverage)
        self.assertEqual(report.errors, ())
        self.assertEqual(report.covered_by_entry, (incoming,))

        # Once the merge is aborted, a merely existing branch is not evidence.
        self.git(root, "merge", "--abort")
        report = audit_history(root, catalog, coverage)
        self.assertTrue(any("matches 0 audited commits" in error for error in report.errors))

    def test_pending_merge_enforces_incoming_runtime_mapping(self):
        root, catalog, coverage = self.repository()
        base, incoming = self.pending_merge(root)
        (root / "tests/client-fixes.toml").write_text(
            f'version = 1\nenforce_after = "{base}"\n', encoding="utf-8"
        )
        report = audit_history(root, catalog, coverage)
        self.assertTrue(any(
            incoming[:8] in error and "needs an explicit" in error
            for error in report.errors
        ), report.errors)

    def test_pending_merge_enforces_incoming_client_anchor(self):
        root, catalog, coverage = self.repository()
        base, incoming = self.pending_merge(root, client=True)
        (root / "tests/client-fixes.toml").write_text(
            f'version = 1\nenforce_after = "{base}"\n', encoding="utf-8"
        )
        report = audit_history(root, catalog, coverage)
        self.assertTrue(any(
            incoming[:8] in error and "needs a tests/client-fixes.toml anchor row" in error
            for error in report.errors
        ), report.errors)

    def test_pending_merge_in_linked_worktree_uses_worktree_local_heads(self):
        root, catalog, coverage = self.repository()
        _, incoming = self.pending_merge(root)
        self.git(root, "merge", "--abort")
        with tempfile.TemporaryDirectory() as directory:
            linked = Path(directory) / "linked"
            self.git(root, "worktree", "add", "-b", "linked", str(linked), "HEAD")
            self.git(linked, "merge", "--no-commit", "--no-ff", "incoming")
            self.map_commit(coverage, incoming)
            self.assertEqual(audit_history(linked, catalog, coverage).errors, ())
            self.assertTrue(any(
                "matches 0 audited commits" in error
                for error in audit_history(root, catalog, coverage).errors
            ))

    def test_pending_merge_rejects_malformed_or_non_commit_heads(self):
        root, catalog, coverage = self.repository()
        self.pending_merge(root)
        merge_path = root / self.git(root, "rev-parse", "--git-path", "MERGE_HEAD")
        tree = self.git(root, "rev-parse", "HEAD^{tree}")
        for invalid in ("", "HEAD\n", "--all\n", "a" * 40 + "\n", tree + "\n", "\xff\n"):
            with self.subTest(invalid=invalid):
                merge_path.write_text(invalid, encoding="utf-8")
                with self.assertRaises(HistoryError):
                    audit_history(root, catalog, coverage)

    def test_pending_octopus_merge_audits_every_parent_once(self):
        root, catalog, coverage = self.repository()
        base, first = self.pending_merge(root)
        self.git(root, "merge", "--abort")
        self.git(root, "checkout", "-qb", "second", base)
        (root / "src/second.rs").write_text("second correction\n", encoding="utf-8")
        second = self.commit(root, "fix: retain second behavior")
        self.git(root, "checkout", "current")
        self.git(root, "merge", "--no-commit", "--no-ff", "incoming", "second")
        self.map_commit(coverage, first)
        self.map_commit(coverage, second)
        report = audit_history(root, catalog, coverage)
        self.assertEqual(report.errors, ())
        self.assertEqual(set(report.covered_by_entry), {first, second})
        self.assertEqual(len(report.issues), 2)


class CoverageDirectoryCase(unittest.TestCase):
    """The regression ledger must survive concurrent corrective changes.

    A shared append-only file made every merge to `main` conflict every other
    open pull request that carried a mapping, and clearing that conflict with a
    rebase stranded the reviewer's exact-SHA approval. One entry per file makes
    the collision impossible instead of merely rare.
    """

    @staticmethod
    def git(root: Path, *arguments: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["git", *arguments], cwd=root, check=True, text=True, stdout=subprocess.PIPE
        )

    @staticmethod
    def fragment(root: Path, name: str, commit: str, reason: str) -> None:
        (root / "validation/regressions.d" / name).write_text(
            textwrap.dedent(
                f"""
                version = 1

                [[coverage]]
                commits = ["{commit}"]
                points = ["app"]
                checks = ["baseline"]
                reason = "{reason}"
                """
            ).lstrip(),
            encoding="utf-8",
        )

    def repository(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.git(root, "init", "-q", "-b", "main")
        self.git(root, "config", "user.name", "Ledger Test")
        self.git(root, "config", "user.email", "ledger@example.invalid")
        (root / "validation/regressions.d").mkdir(parents=True)
        self.fragment(root, "0000aa11-seed.toml", "0000aa11", "Seeded before either branch.")
        self.git(root, "add", "-A")
        self.git(root, "commit", "-qm", "seed the ledger")
        return root

    def branch_adding(self, root: Path, name: str, fragment: str, commit: str) -> None:
        self.git(root, "checkout", "-q", "-b", name, "main")
        self.fragment(root, fragment, commit, f"Added independently on {name}.")
        self.git(root, "add", "-A")
        self.git(root, "commit", "-qm", f"fix: map {commit}")

    def merged(self, root: Path, name: str, first: str, second: str) -> tuple[str, ...]:
        self.git(root, "checkout", "-q", "-b", name, "main")
        for branch in (first, second):
            merge = subprocess.run(
                ["git", "merge", "--no-edit", "-q", branch],
                cwd=root,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
            )
            self.assertEqual(
                merge.returncode,
                0,
                f"merging {branch} into {name} conflicted:\n{merge.stdout}",
            )
        entries = load_coverage(root / "validation/regressions.d")
        return tuple(entry.commits[0] for entry in entries)

    def test_independent_mappings_merge_cleanly_in_either_order(self):
        root = self.repository()
        self.branch_adding(root, "alpha", "1111bb22-app.toml", "1111bb22")
        self.branch_adding(root, "beta", "2222cc33-app.toml", "2222cc33")

        expected = ("0000aa11", "1111bb22", "2222cc33")
        self.assertEqual(self.merged(root, "alpha-first", "alpha", "beta"), expected)
        self.assertEqual(self.merged(root, "beta-first", "beta", "alpha"), expected)

    def test_a_shared_append_only_ledger_is_what_conflicted(self):
        root = self.repository()
        shared = root / "validation/shared.toml"
        shared.write_text("version = 1\n", encoding="utf-8")
        self.git(root, "add", "-A")
        self.git(root, "commit", "-qm", "seed a shared tail")
        for branch, commit in (("gamma", "3333dd44"), ("delta", "4444ee55")):
            self.git(root, "checkout", "-q", "-b", branch, "main")
            shared.write_text(
                shared.read_text(encoding="utf-8")
                + f'\n[[coverage]]\ncommits = ["{commit}"]\n',
                encoding="utf-8",
            )
            self.git(root, "add", "-A")
            self.git(root, "commit", "-qm", f"fix: map {commit}")

        self.git(root, "checkout", "-q", "gamma")
        merge = subprocess.run(
            ["git", "merge", "--no-edit", "-q", "delta"],
            cwd=root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )

        self.assertNotEqual(merge.returncode, 0)
        self.assertIn("<<<<<<<", shared.read_text(encoding="utf-8"))

    def test_a_fragment_must_be_named_after_its_first_mapped_commit(self):
        root = self.repository()
        self.fragment(root, "unrelated-name.toml", "5555ff66", "Named after nothing.")

        with self.assertRaises(HistoryError) as raised:
            load_coverage(root / "validation/regressions.d")

        self.assertIn("5555ff66", str(raised.exception))

    def test_a_fragment_holds_exactly_one_entry(self):
        root = self.repository()
        path = root / "validation/regressions.d/6666aa77-app.toml"
        self.fragment(root, path.name, "6666aa77", "First.")
        path.write_text(
            path.read_text(encoding="utf-8")
            + '\n[[coverage]]\ncommits = ["7777bb88"]\nreason = "Second."\nignore = true\n',
            encoding="utf-8",
        )

        with self.assertRaises(HistoryError) as raised:
            load_coverage(root / "validation/regressions.d")

        self.assertIn("exactly one", str(raised.exception))

    def test_a_reintroduced_shared_ledger_is_refused(self):
        root = self.repository()
        (root / "validation/regressions.toml").write_text(
            "version = 1\n", encoding="utf-8"
        )

        with self.assertRaises(HistoryError) as raised:
            load_coverage(root / "validation/regressions.d")

        self.assertIn("no longer the regression ledger", str(raised.exception))

    def test_migration_fidelity_catches_a_dropped_multi_commit_member(self):
        root = self.repository()
        legacy = root / "legacy-regressions.toml"
        legacy.write_text(
            textwrap.dedent(
                """
                version = 1

                [[coverage]]
                commits = ["0000aa11", "1111bb22"]
                points = ["app"]
                checks = ["baseline"]
                reason = "Seeded before either branch."
                """
            ).lstrip(),
            encoding="utf-8",
        )

        with self.assertRaises(HistoryError) as raised:
            verify_migration_fidelity(legacy, root / "validation/regressions.d")
        self.assertIn("1 missing and 1 extra", str(raised.exception))

        (root / "validation/regressions.d/0000aa11-seed.toml").write_text(
            textwrap.dedent(
                """
                version = 1

                [[coverage]]
                commits = ["0000aa11", "1111bb22"]
                points = ["app"]
                checks = ["baseline"]
                reason = "Seeded before either branch."
                """
            ).lstrip(),
            encoding="utf-8",
        )
        verify_migration_fidelity(legacy, root / "validation/regressions.d")


class CorrectiveBoundaryCase(RepositoryFixture):
    """The freeze of §3.1/§3.2, proved on both sides of the boundary.

    Narrowing the corrective rule weakens the audit this repository has lived
    under, so the narrowing is bounded by a commit rather than applied to the
    whole history. Everything at or before the boundary keeps the rule it was
    written under and the `regressions.d/` fragments that answer it;
    everything past it is judged by `CORRECTIVE_RE` and answers with a
    `Regression-Test:` trailer on its landing commit. Both halves of that
    sentence need a test, because either one failing silently is the whole
    audit going quiet.
    """

    def ledger(self, root: Path, enforce_after=None, errata=()) -> Path:
        path = root / "merge-errata.toml"
        lines = ["version = 1", ""]
        if enforce_after:
            lines.append(f'enforce_after = "{enforce_after}"')
        if errata:
            for commit, reason in errata:
                lines.append("")
                lines.append("[[errata]]")
                lines.append(f'commit = "{commit}"')
                lines.append(f'reason = "{reason}"')
        else:
            lines.append("errata = []")
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return path

    @staticmethod
    def commit_with_trailers(root: Path, subject: str, *trailers: str) -> str:
        subprocess.run(["git", "add", "-A"], cwd=root, check=True)
        message = subject + "\n\n" + "\n".join(trailers) + "\n"
        subprocess.run(["git", "commit", "-q", "-m", message], cwd=root, check=True)
        return subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()

    @staticmethod
    def branch(root: Path) -> str:
        return subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=root, check=True,
            text=True, stdout=subprocess.PIPE,
        ).stdout.strip()

    def test_the_boundary_chooses_which_rule_judges_a_commit(self):
        """A commit before the boundary keeps its requirement; one after does not.

        `chore: keep the reader bounded` is corrective under `ISSUE_RE` -- the
        body-word alternatives match "keep" and "bound" -- and is not
        corrective under `CORRECTIVE_RE`. One such commit sits on each side of
        the boundary, so a single audit says which rule judged which commit.
        """

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")

        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        before = self.commit(root, "chore: keep the reader bounded")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        (root / "crates/other.rs").write_text("pub fn b() -> u8 { 1 }\n", encoding="utf-8")
        after = self.commit(root, "chore: keep the writer bounded")

        self.assertTrue(ISSUE_RE.search("chore: keep the reader bounded"))
        self.assertIsNone(CORRECTIVE_RE.match("chore: keep the writer bounded"))

        frozen = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        self.assertTrue(
            any(before[:8] in error for error in frozen.errors),
            f"the pre-boundary commit lost its requirement: {frozen.errors}",
        )
        self.assertFalse(
            any(after[:8] in error for error in frozen.errors),
            f"the post-boundary commit was judged by the legacy rule: {frozen.errors}",
        )
        judged = {issue.sha for issue in frozen.issues}
        self.assertIn(before, judged)
        self.assertNotIn(
            after, judged, "the post-boundary commit was judged by the legacy rule"
        )

        # Control: with no boundary the repository is in phase A and both
        # commits are judged by the legacy rule, exactly as before this change.
        phase_a = audit_history(root, catalog, coverage, merge_ledger_path=root / "absent.toml")
        self.assertTrue(any(before[:8] in error for error in phase_a.errors))
        self.assertTrue(any(after[:8] in error for error in phase_a.errors))

    def merge(self, root: Path, topic: str, message: str) -> str:
        subprocess.run(
            ["git", "merge", "--no-ff", "-q", "-m", message, topic], cwd=root, check=True
        )
        return subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()

    def test_a_fix_past_the_boundary_awaits_its_landing_then_needs_its_line(self):
        """The narrow rule has to bite, or the freeze is just an amnesty.

        Past the boundary a `fix(` commit is corrective. Until it lands it is
        on a pull request branch, where the description carries its field and
        the fast lane's field check judges it, so the history audit reports
        it as awaiting its landing rather than failing a branch whose author
        cannot yet write a landing commit. Once it lands, the landing commit
        must carry a line that resolves.
        """

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)
        ledger = self.ledger(root, boundary)

        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        (root / "tests/app_test.rs").write_text(
            "#[test]\nfn the_reader_stops_stalling() { assert!(true); }\n",
            encoding="utf-8",
        )
        fix = self.commit(root, "fix(app): stop the reader stalling")

        on_branch = audit_history(root, catalog, coverage, merge_ledger_path=ledger)
        self.assertIn(fix, {issue.sha for issue in on_branch.issues})
        self.assertEqual(on_branch.pending, (fix,))
        self.assertEqual(on_branch.errors, ())

        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        bare = self.merge(
            root, "topic",
            "Merge pull request 'fix(app): stop the reader stalling' (#3) from topic into main",
        )
        landed_bare = audit_history(root, catalog, coverage, merge_ledger_path=ledger)
        self.assertEqual(landed_bare.pending, ())
        self.assertTrue(
            any(fix[:8] in error and bare[:8] in error for error in landed_bare.errors),
            f"a fix( that landed with no line escaped the audit: {landed_bare.errors}",
        )

        subprocess.run(["git", "reset", "-q", "--hard", "HEAD^"], cwd=root, check=True)
        self.merge(
            root, "topic",
            "Merge pull request 'fix(app): stop the reader stalling' (#3) from topic into main\n\n"
            "Regression-Test: tests/app_test.rs::the_reader_stops_stalling\n",
        )
        landed = audit_history(root, catalog, coverage, merge_ledger_path=ledger)
        self.assertEqual(landed.errors, ())
        self.assertEqual(landed.covered_by_trailer, (fix,))

    def test_a_runtime_fix_past_the_boundary_needs_no_fragment(self):
        """Phase B must not demand the evidence it forbids.

        Past the client-fix boundary a corrective commit under `crates/`
        needs "an explicit regressions.d mapping" -- and past the merge
        boundary a regressions.d mapping is refused. Without this the two
        rules together would make every runtime fix unlandable. The landing
        commit's `Regression-Test:` line is the explicit mapping now.
        """

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        seed = self.commit(root, "feat: seed")
        (root / "tests/client-fixes.toml").write_text(
            f'version = 1\nenforce_after = "{seed[:8]}"\nfixes = []\n', encoding="utf-8"
        )
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)

        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "crates/app.rs").write_text(
            "pub fn a() -> u8 { 2 }\n#[test]\nfn a_is_two() { assert_eq!(a(), 2); }\n",
            encoding="utf-8",
        )
        fix = self.commit(root, "fix(app): return two")
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        self.merge(
            root, "topic",
            "Merge pull request 'fix(app): return two' (#4) from topic into main\n\n"
            "Regression-Test: crates/app.rs::a_is_two\n",
        )

        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        self.assertEqual(report.errors, ())
        self.assertEqual(report.covered_by_trailer, (fix,))

    def test_a_fix_that_reached_main_outside_a_landing_commit_is_an_error(self):
        """A direct push has no landing commit to carry its line.

        It is caught once a later landing commit has it as an ancestor, and
        an erratum naming the commit is the only way past it.
        """

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        pushed = self.commit(root, "fix(app): pushed straight to main")

        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "src/note.txt").write_text("later\n", encoding="utf-8")
        self.commit(root, "docs: a later change")
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        self.merge(
            root, "topic",
            "Merge pull request 'docs: a later change' (#5) from topic into main",
        )

        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        outside = [error for error in report.errors if pushed[:8] in error]
        self.assertTrue(outside, f"a direct push escaped the audit: {report.errors}")
        self.assertIn("outside any landing commit", outside[0])

        excused = audit_history(
            root, catalog, coverage,
            merge_ledger_path=self.ledger(
                root, boundary, errata=((pushed[:12], "pushed without a PR"),)
            ),
        )
        self.assertEqual(excused.errors, ())

    def test_an_integration_merge_carries_the_line_for_the_plan_it_brings(self):
        """`Merge plan/X (#N) at <sha>` is a landing commit too.

        Plans reach `main` through an integration branch: each plan PR is
        merged into it as `Merge plan/X (#N) at <sha>`, and the integration PR
        then lands. The line may sit on either merge.
        """

        self.assertEqual(
            landing_commit_title(
                "Merge plan/C-08 (#461) at 7218b96ce92c059ce4e9cea0d0538cbfc770c012"
            ),
            ("plan/C-08", "461"),
        )
        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)
        subprocess.run(["git", "checkout", "-q", "-b", "integ"], cwd=root, check=True)
        subprocess.run(["git", "checkout", "-q", "-b", "plan/X"], cwd=root, check=True)
        (root / "tests/app_test.rs").write_text(
            "#[test]\nfn the_plan_holds() { assert!(true); }\n", encoding="utf-8"
        )
        fix = self.commit(root, "fix(app): the plan's repair")
        subprocess.run(["git", "checkout", "-q", "integ"], cwd=root, check=True)
        self.merge(
            root, "plan/X",
            f"Merge plan/X (#9) at {fix}\n\nRegression-Test: tests/app_test.rs::the_plan_holds\n",
        )
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        self.merge(
            root, "integ",
            "Merge pull request 'Integration wave' (#10) from integ into main",
        )
        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        self.assertEqual(report.errors, ())
        self.assertEqual(report.covered_by_trailer, (fix,))
        self.assertEqual({row.pull for row in report.merges}, {"9", "10"})

    def test_a_landing_line_is_read_wherever_the_message_carries_it(self):
        """The same parse as the pull request description.

        A merge tool may put the description's lines above its own trailer
        paragraph; Git's trailer parser would see only the last paragraph and
        call a correctly carried line absent.
        """

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)
        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "tests/app_test.rs").write_text(
            "#[test]\nfn it_holds() { assert!(true); }\n", encoding="utf-8"
        )
        fix = self.commit(root, "fix(app): hold")
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        self.merge(
            root, "topic",
            "Merge pull request 'fix(app): hold' (#6) from topic into main\n\n"
            "Regression-Test: tests/app_test.rs::it_holds\n\n"
            "Reviewed-on: http://forge.invalid/pulls/6\n",
        )
        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        self.assertEqual(report.errors, ())
        self.assertEqual(report.covered_by_trailer, (fix,))

    def test_the_freeze_refuses_a_later_commit_added_to_an_old_fragment(self):
        """Growing an existing fragment is growing the ledger too."""

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        early = self.commit(root, "fix(app): the first stall")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 3 }\n", encoding="utf-8")
        late = self.commit(root, "fix(app): the second stall")
        self.write_coverage(
            coverage,
            f"{early[:8]}-app.toml",
            f"""
            commits = ["{early[:8]}", "{late[:8]}"]
            points = ["app"]
            checks = ["baseline"]
            reason = "The baseline exercises both."
            """,
        )
        self.commit(root, "validation: map both stalls")
        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        frozen = [error for error in report.errors if "frozen boundary" in error]
        self.assertEqual(len(frozen), 1, report.errors)
        self.assertIn(late[:8], frozen[0])

    def test_the_report_carries_the_landing_commits_it_audited(self):
        """M3's acceptance: `jq '.merges | length'` counts the landing commits.

        Generated into target/, never tracked: landing a pull request adds no
        file to the repository.
        """

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)
        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "tests/app_test.rs").write_text(
            "#[test]\nfn it_holds() { assert!(true); }\n", encoding="utf-8"
        )
        fix = self.commit(root, "fix(app): hold")
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        landing = self.merge(
            root, "topic",
            "Merge pull request 'fix(app): hold' (#6) from topic into main\n\n"
            "Regression-Test: tests/app_test.rs::it_holds\n",
        )
        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        out = root / "target" / "history.json"
        _write_report(out, report)
        payload = json.loads(out.read_text(encoding="utf-8"))
        self.assertEqual(len(payload["merges"]), 1)
        self.assertEqual(payload["merges"][0]["landing"], landing)
        self.assertEqual(payload["merges"][0]["tests"], ["tests/app_test.rs::it_holds"])
        self.assertTrue(payload["merges"][0]["resolved"])
        self.assertEqual(payload["summary"]["landing_trailer"], 1)
        coverage_of = {row["commit"]: row["coverage"] for row in payload["commits"]}
        self.assertEqual(coverage_of[fix], "landing-trailer")

    def test_an_erratum_that_names_no_commit_past_the_boundary_is_an_error(self):
        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        seed = self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        report = audit_history(
            root, catalog, coverage,
            merge_ledger_path=self.ledger(root, boundary, errata=((seed[:12], "wrong"),)),
        )
        self.assertTrue(
            any("errata row" in error and seed[:12] in error for error in report.errors),
            report.errors,
        )

    def test_a_fragment_for_a_commit_past_the_boundary_is_refused(self):
        """Phase B: the directory is frozen, and the message says what replaces it."""

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        early = self.commit(root, "fix(app): stop the first stall")
        self.write_coverage(
            coverage,
            f"{early[:8]}-app.toml",
            f"""
            commits = ["{early[:8]}"]
            points = ["app"]
            checks = ["baseline"]
            reason = "The baseline exercises this behaviour."
            """,
        )
        boundary = self.commit(root, "validation: map the first stall")
        (root / "crates/other.rs").write_text("pub fn b() -> u8 { 2 }\n", encoding="utf-8")
        late = self.commit(root, "fix(app): stop the second stall")
        self.write_coverage(
            coverage,
            f"{late[:8]}-other.toml",
            f"""
            commits = ["{late[:8]}"]
            points = ["app"]
            checks = ["baseline"]
            reason = "The baseline exercises this behaviour too."
            """,
        )
        self.commit(root, "validation: map the second stall")

        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        frozen = [error for error in report.errors if "frozen boundary" in error]
        self.assertTrue(frozen, f"the freeze did not fire: {report.errors}")
        self.assertIn(late[:8], frozen[0])
        self.assertIn("Regression-Test", frozen[0])
        # The pre-boundary fragment is untouched: it is the only evidence that
        # range will ever have.
        self.assertFalse(
            any(early[:8] in error and "frozen boundary" in error for error in report.errors),
            f"the freeze reached back past the boundary: {report.errors}",
        )

    def test_a_corrective_landing_commit_past_the_boundary_needs_a_trailer(self):
        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)

        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        self.commit(root, "fix(app): stop the stall")
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        subprocess.run(
            ["git", "merge", "--no-ff", "-q", "-m",
             "Merge pull request 'fix(app): stop the stall' (#7) from topic into main",
             "topic"],
            cwd=root, check=True,
        )
        landing = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()

        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        missing = [
            error for error in report.errors
            if landing[:8] in error and "Regression-Test" in error
        ]
        self.assertTrue(missing, f"the merge audit did not fire: {report.errors}")
        self.assertIn("(#7)", missing[0])
        row = next(merge for merge in report.merges if merge.landing == landing)
        self.assertTrue(row.corrective)
        self.assertFalse(row.resolved)
        self.assertEqual(row.tests, ())

    def test_a_trailer_is_judged_against_the_landing_commits_own_tree(self):
        """The exact-tree binding, which is the reason the field is not a SHA.

        The test the trailer names is deleted after the merge. The trailer
        still resolves, because the tree it is judged against is the landing
        commit's own and that tree is immutable. An audit that read today's
        worktree would call this a broken trailer, and a rebase or a rename
        would then invalidate evidence that was true when it was recorded.
        """

        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)

        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        (root / "tests/app_test.rs").write_text(
            "#[test]\nfn the_reader_stops_stalling() { assert!(true); }\n",
            encoding="utf-8",
        )
        self.commit(root, "fix(app): stop the stall")
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        subprocess.run(
            ["git", "merge", "--no-ff", "-q", "-m",
             "Merge pull request 'fix(app): stop the stall' (#8) from topic into main\n\n"
             "Regression-Test: tests/app_test.rs::the_reader_stops_stalling\n",
             "topic"],
            cwd=root, check=True,
        )
        landing = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()

        ledger = self.ledger(root, boundary)
        report = audit_history(root, catalog, coverage, merge_ledger_path=ledger)
        row = next(merge for merge in report.merges if merge.landing == landing)
        self.assertEqual(row.tests, ("tests/app_test.rs::the_reader_stops_stalling",))
        self.assertTrue(row.resolved, report.errors)
        self.assertEqual(
            [error for error in report.errors if landing[:8] in error], []
        )

        (root / "tests/app_test.rs").unlink()
        self.commit(root, "chore: drop the suite")
        after_deletion = audit_history(root, catalog, coverage, merge_ledger_path=ledger)
        still = next(
            merge for merge in after_deletion.merges if merge.landing == landing
        )
        self.assertTrue(
            still.resolved,
            "the trailer was judged against today's worktree, not its own tree",
        )

    def test_a_trailer_naming_a_test_its_tree_does_not_define_is_an_error(self):
        root, catalog, coverage = self.repository()
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        self.commit(root, "feat: seed")
        (root / "src/boundary.txt").write_text("the boundary\n", encoding="utf-8")
        boundary = self.commit(root, "docs: draw the boundary")
        base = self.branch(root)

        subprocess.run(["git", "checkout", "-q", "-b", "topic"], cwd=root, check=True)
        (root / "tests/app_test.rs").write_text(
            "#[test]\nfn the_reader_stops_stalling() { assert!(true); }\n",
            encoding="utf-8",
        )
        self.commit(root, "fix(app): stop the stall")
        subprocess.run(["git", "checkout", "-q", base], cwd=root, check=True)
        subprocess.run(
            ["git", "merge", "--no-ff", "-q", "-m",
             "Merge pull request 'fix(app): stop the stall' (#9) from topic into main\n\n"
             "Regression-Test: tests/app_test.rs::the_reader_stops_stallling\n",
             "topic"],
            cwd=root, check=True,
        )
        landing = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()

        report = audit_history(
            root, catalog, coverage, merge_ledger_path=self.ledger(root, boundary)
        )
        misspelt = [error for error in report.errors if landing[:8] in error]
        self.assertTrue(misspelt, f"the misspelt trailer passed: {report.errors}")
        self.assertIn("does not define as a test", misspelt[0])

        # And the erratum is the only remedy, because the landing commit can
        # never be amended.
        excused = audit_history(
            root,
            catalog,
            coverage,
            merge_ledger_path=self.ledger(
                root, boundary, errata=((landing[:12], "trailer typo, PR #9"),)
            ),
        )
        self.assertEqual([error for error in excused.errors if landing[:8] in error], [])

    def test_both_landing_commit_shapes_are_recognised(self):
        self.assertEqual(
            landing_commit_title(
                "Merge pull request 'fix(app): stop the stall' (#7) from topic into main"
            ),
            ("fix(app): stop the stall", "7"),
        )
        self.assertEqual(
            landing_commit_title("fix(app): stop the stall (#7)"),
            ("fix(app): stop the stall", "7"),
        )
        self.assertIsNone(landing_commit_title("fix(app): stop the stall"))

    def test_the_merge_ledger_refuses_a_malformed_boundary(self):
        root, _catalog, _coverage = self.repository()
        path = root / "merge-errata.toml"
        path.write_text('version = 1\nenforce_after = "not a sha"\n', encoding="utf-8")
        with self.assertRaises(HistoryError) as raised:
            load_merge_ledger(path)
        self.assertIn("enforce_after", str(raised.exception))
        self.assertIsNone(load_merge_ledger(root / "absent.toml").enforce_after)


if __name__ == "__main__":
    unittest.main()
