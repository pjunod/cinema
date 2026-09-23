from __future__ import annotations

import contextlib
import io
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

from validation.regression_field import (
    check_field,
    executed_suites,
    main,
    parse_fields,
)
from validation.runner import REPO_ROOT


class RegressionFieldCase(unittest.TestCase):
    """The pre-merge half of the `Regression-Test:` field.

    This is the check that makes a typo fixable. Once a pull request lands,
    its landing commit cannot be amended and a wrong trailer can only be
    recorded as a permanent erratum -- so every condition here exists to
    refuse the field while the author can still edit the description.
    """

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_a_field_naming_a_test_this_tree_defines_passes(self):
        code, out, _err = self.run_main(
            "--body",
            "Regression-Test: tests/validation/test_regression_field.py"
            "::test_a_field_naming_a_test_this_tree_defines_passes",
        )
        self.assertEqual(code, 0, out)
        self.assertIn("ok:", out)

    def test_a_misspelt_name_fails_and_names_the_file_it_searched(self):
        code, _out, err = self.run_main(
            "--body",
            "Regression-Test: tests/validation/test_regression_field.py::no_such_test_here",
        )
        self.assertEqual(code, 1)
        self.assertIn("no_such_test_here", err)
        self.assertIn("tests/validation/test_regression_field.py", err)

    def test_a_path_absent_from_the_tree_fails(self):
        code, _out, err = self.run_main(
            "--body", "Regression-Test: crates/plurxd/src/not-a-file.rs::a_test"
        )
        self.assertEqual(code, 1)
        self.assertIn("this tree does not carry", err)

    def test_a_value_that_is_not_path_and_name_fails(self):
        code, _out, err = self.run_main("--body", "Regression-Test: just-a-path.rs")
        self.assertEqual(code, 1)
        self.assertIn("<path>::<name>", err)

    def test_a_suite_the_lane_does_not_execute_is_accepted_with_a_warning(self):
        """§3.1 step 3, and the plan's open question 3.

        The fast lane's Rust gate is compile-only, so no Rust test is in the
        executed set and this cannot be an error today without making the
        field unusable for every runtime fix. It is a warning, and the set it
        warns against is derived from the lane, so it stops warning by itself
        the day the lane runs the suite.
        """

        rust = next(
            path
            for path in (REPO_ROOT / "crates" / "plurxd" / "src").rglob("*.rs")
            if "#[test]" in path.read_text(encoding="utf-8", errors="replace")
        )
        source = rust.read_text(encoding="utf-8", errors="replace")
        name = source.split("#[test]", 1)[1].split("fn ", 1)[1].split("(", 1)[0].strip()
        relative = rust.relative_to(REPO_ROOT).as_posix()
        code, out, err = self.run_main("--body", f"Regression-Test: {relative}::{name}")
        self.assertEqual(code, 0, err)
        self.assertIn("warning:", out)
        self.assertIn("does not execute", out)

    def test_a_corrective_commit_with_no_field_fails_naming_the_subject(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Field Test"], cwd=root, check=True)
            subprocess.run(
                ["git", "config", "user.email", "field@example.invalid"],
                cwd=root, check=True,
            )
            (root / "app.rs").write_text("pub fn a() {}\n", encoding="utf-8")
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "feat: seed"], cwd=root, check=True)
            base = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
                stdout=subprocess.PIPE,
            ).stdout.strip()
            (root / "app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)
            subprocess.run(
                ["git", "commit", "-qm", "fix(app): stop the stall"], cwd=root, check=True
            )

            code, _out, err = self.run_main(
                "--root", str(root), "--body", "no field here",
                "--base", base, "--head", "HEAD",
            )
            self.assertEqual(code, 1)
            self.assertIn("fix(app): stop the stall", err)

            # A non-corrective range asks for nothing.
            subprocess.run(["git", "reset", "-q", "--hard", base], cwd=root, check=True)
            (root / "app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)
            subprocess.run(
                ["git", "commit", "-qm", "chore(app): tidy"], cwd=root, check=True
            )
            code, _out, _err = self.run_main(
                "--root", str(root), "--body", "no field here",
                "--base", base, "--head", "HEAD",
            )
            self.assertEqual(code, 0)

    def test_the_executed_set_is_derived_from_the_lane_and_the_makefile(self):
        """Not a hardcoded list.

        A hardcoded set would keep warning about a suite the lane had started
        running, and -- worse -- would keep accepting one it had stopped
        running. This builds a lane and a Makefile that name suites this
        repository does not have, and asserts the derivation follows them.
        """

        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            (root / ".github" / "workflows").mkdir(parents=True)
            (root / ".github" / "workflows" / "main-fast-lane.yml").write_text(
                textwrap.dedent(
                    """\
                    jobs:
                      scope:
                        steps:
                          - run: node tests/not-preflight.test.js
                      preflight:
                        steps:
                          - run: make invented-check
                          - run: node tests/invented/suite.test.js
                      rust_compile:
                        steps:
                          - run: node tests/also-not-preflight.test.js
                    """
                ),
                encoding="utf-8",
            )
            (root / "Makefile").write_text(
                "invented-check:\n\t@python3 -m unittest discover -s tests/invented-dir\n",
                encoding="utf-8",
            )
            self.assertEqual(
                executed_suites(root),
                (("tests/invented/suite.test.js",), ("tests/invented-dir",)),
            )

    def test_the_lane_this_repository_ships_really_runs_what_it_derives(self):
        files, directories = executed_suites(REPO_ROOT)
        self.assertIn("tests/validation", directories)
        self.assertIn("tests/operations", directories)
        self.assertIn("tests/playback/web-policy.test.js", files)
        lane = (
            REPO_ROOT / ".github" / "workflows" / "main-fast-lane.yml"
        ).read_text(encoding="utf-8")
        for path in files:
            self.assertIn(path, lane)

    def test_every_field_line_in_a_body_is_read(self):
        body = textwrap.dedent(
            """\
            Some prose.

            Regression-Test: a/b.rs::one
            Regression-Test: c/d.js::two
            """
        )
        self.assertEqual(parse_fields(body), ("a/b.rs::one", "c/d.js::two"))

    def test_check_field_reports_error_and_warning_separately(self):
        error, warning = check_field(
            REPO_ROOT,
            "tests/validation/test_regression_field.py"
            "::test_every_field_line_in_a_body_is_read",
        )
        self.assertIsNone(error)
        self.assertIsNone(warning)


if __name__ == "__main__":
    unittest.main()
