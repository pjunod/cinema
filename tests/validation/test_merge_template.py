"""The repository's Forgejo merge-message template, rendered and audited.

Past the phase-B boundary in `validation/merge-errata.toml`, `make
history-check` reads each corrective landing commit's `Regression-Test:`
lines from that commit's own message. Forgejo's built-in merge message is
the title line alone, so a landing that relied on it lost the lines its pull
request carried, and `main` went red until an errata row recorded it (#512,
#519, #522). `.forgejo/default_merge_message/MERGE_TEMPLATE.md` makes the
description part of the default message.

Forgejo 16 (`services/pull/merge.go`, `getMergeMessage` and
`expandDefaultMergeMessage`) reads the template from the default branch's
tip, expands `${Var}` with `os.Expand`, and splits it at its first newline:
line one is the title, the rest the body. Where each half goes differs by
path, and this file pins both:

* The web merge form is prefilled with the title *and* the body, and posts
  both (`routers/web/repo/pull.go`, `MergePullRequest`), so a web merge
  lands the description and every `Regression-Test:` line in it.
* `POST /repos/{owner}/{repo}/pulls/{index}/merge` with no
  `MergeTitleField` takes only the title from the template and **drops its
  body** (`routers/api/v1/repo/pull.go`: `message, _, err =
  GetDefaultMergeMessage(...)`); only `MergeMessageField` adds a body. An
  API merge must therefore still pass the lines, as the promotion helper
  does with `--landing-lines`.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import tempfile
import unittest

from validation.history import (
    CORRECTIVE_RE,
    audit_history,
    landing_commit_title,
    parse_regression_fields,
)
from validation.runner import REPO_ROOT, load_catalog


TEMPLATE = REPO_ROOT / ".forgejo" / "default_merge_message" / "MERGE_TEMPLATE.md"

# The variables Forgejo 16's getMergeMessage defines; any other name expands
# to the empty string, silently.
FORGEJO_VARIABLES = frozenset(
    {
        "BaseRepoOwnerName",
        "BaseRepoName",
        "BaseBranch",
        "HeadRepoOwnerName",
        "HeadRepoName",
        "HeadBranch",
        "PullRequestTitle",
        "PullRequestDescription",
        "PullRequestPosterName",
        "PullRequestIndex",
        "PullRequestReference",
        "ReviewedOn",
        "ReviewedBy",
        "ClosingIssues",
    }
)

_VARIABLE_RE = re.compile(r"\$(?:\{(?P<braced>[^}]*)\}|(?P<bare>[A-Za-z0-9_]+))")

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
paths = ["src/**", "crates/**", "tests/**"]
checks = ["baseline"]
"""


def expand(text: str, variables: dict[str, str]) -> str:
    """Go's `os.Expand` for the forms a template uses: `${Name}` and `$Name`."""

    return _VARIABLE_RE.sub(
        lambda match: variables.get(match.group("braced") or match.group("bare"), ""),
        text,
    )


def render(template: str, variables: dict[str, str]) -> tuple[str, str]:
    """`expandDefaultMergeMessage`: (title, body), with Forgejo's fallbacks.

    An empty first line keeps the built-in title and an empty remainder the
    built-in body (`Reviewed-on:` and `Reviewed-by:`), as Forgejo does.
    """

    default_title = (
        f"Merge pull request '{variables['PullRequestTitle']}' "
        f"({variables['PullRequestReference']}) from {variables['HeadBranch']} "
        f"into {variables['BaseBranch']}"
    )
    default_body = f"{variables['ReviewedOn']}\n{variables['ReviewedBy']}"
    first, newline, rest = template.partition("\n")
    if not newline:
        return expand(template.strip(), variables), default_body
    title = default_title if first == "" else expand(first.strip(), variables)
    body = default_body if rest == "" else expand(rest.rstrip(), variables)
    return title, body


def web_message(title: str, body: str) -> str:
    """What the web merge form posts when it is left as prefilled."""

    body = body.strip()
    return title.strip() + (f"\n\n{body}" if body else "")


def api_message(title: str, message_field: str = "") -> str:
    """The API merge: the template's title, plus `MergeMessageField` if any."""

    field = message_field.strip()
    return title.strip() + (f"\n\n{field}" if field else "")


def pull_request(title: str, index: int, description: str) -> dict[str, str]:
    return {
        "BaseRepoOwnerName": "noirr",
        "BaseRepoName": "plurx",
        "BaseBranch": "main",
        "HeadRepoOwnerName": "noirr",
        "HeadRepoName": "plurx",
        "HeadBranch": "topic",
        "PullRequestTitle": title,
        "PullRequestDescription": description,
        "PullRequestPosterName": "pjunod",
        "PullRequestIndex": str(index),
        "PullRequestReference": f"#{index}",
        "ReviewedOn": f"Reviewed-on: http://forgejo.invalid/noirr/plurx/pulls/{index}",
        "ReviewedBy": "",
        "ClosingIssues": "",
    }


class MergeTemplateCase(unittest.TestCase):
    def template(self) -> str:
        return TEMPLATE.read_text(encoding="utf-8")

    def test_the_template_uses_only_variables_forgejo_defines(self):
        names = {
            match.group("braced") or match.group("bare")
            for match in _VARIABLE_RE.finditer(self.template())
        }
        self.assertIn("PullRequestDescription", names)
        self.assertEqual(names - FORGEJO_VARIABLES, set())

    def test_a_rendered_subject_is_a_landing_commit_the_audit_recognises(self):
        title = "fix(streaming): a late cycle (at #3) cannot trap the producer"
        description = (
            "## Summary\n\nWhat it fixes.\n\n## Regression-Test fields\n\n"
            "Regression-Test: tests/app_test.rs::the_reader_stops_stalling\n"
            "Regression-Test: tests/web/a.test.js::keeps the target\n"
        )
        rendered_title, body = render(self.template(), pull_request(title, 522, description))
        message = web_message(rendered_title, body)
        subject = message.split("\n", 1)[0]

        self.assertEqual(
            subject,
            f"Merge pull request '{title}' (#522) from topic into main",
            "the title line must stay Forgejo's own merge subject",
        )
        self.assertEqual(landing_commit_title(subject), (title, "522"))
        self.assertTrue(CORRECTIVE_RE.match(landing_commit_title(subject)[0]))
        self.assertEqual(message.split("\n", 2)[1], "", "no blank line after the subject")
        self.assertEqual(
            parse_regression_fields(message),
            (
                "tests/app_test.rs::the_reader_stops_stalling",
                "tests/web/a.test.js::keeps the target",
            ),
        )
        self.assertEqual(parse_regression_fields(api_message(rendered_title)), ())

    def repository(self) -> tuple[Path, str]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        git = lambda *args: subprocess.run(  # noqa: E731
            ["git", *args], cwd=root, check=True, text=True, stdout=subprocess.PIPE
        ).stdout.strip()
        git("init", "-q", "-b", "main")
        git("config", "user.name", "Template Test")
        git("config", "user.email", "template@example.invalid")
        for directory in ("src", "crates", "tests", "regressions.d"):
            (root / directory).mkdir()
        (root / "points.toml").write_text(CATALOG, encoding="utf-8")
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 1 }\n", encoding="utf-8")
        git("add", "-A")
        git("commit", "-qm", "feat: seed")
        boundary = git("rev-parse", "HEAD")
        (root / "merge-errata.toml").write_text(
            f'version = 1\nenforce_after = "{boundary}"\nerrata = []\n', encoding="utf-8"
        )
        git("checkout", "-q", "-b", "topic")
        (root / "crates/app.rs").write_text("pub fn a() -> u8 { 2 }\n", encoding="utf-8")
        (root / "tests/app_test.rs").write_text(
            "#[test]\nfn the_reader_stops_stalling() { assert!(true); }\n",
            encoding="utf-8",
        )
        git("add", "-A")
        git("commit", "-qm", "fix(app): stop the stall")
        git("checkout", "-q", "main")
        return root, boundary

    def land(self, message: str):
        root, _boundary = self.repository()
        subprocess.run(
            ["git", "merge", "--no-ff", "-q", "-m", message, "topic"], cwd=root, check=True
        )
        landing = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, check=True, text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()
        report = audit_history(
            root,
            load_catalog(root / "points.toml"),
            root / "regressions.d",
            merge_ledger_path=root / "merge-errata.toml",
        )
        row = next(merge for merge in report.merges if merge.landing == landing)
        return row, [error for error in report.errors if landing[:8] in error]

    def test_a_web_merge_with_the_template_lands_the_descriptions_lines(self):
        description = (
            "Stops the stall.\n\n"
            "Regression-Test: tests/app_test.rs::the_reader_stops_stalling\n\n"
            "🤖 Generated with [Claude Code](https://claude.com/claude-code)\n"
        )
        title, body = render(
            self.template(), pull_request("fix(app): stop the stall", 7, description)
        )
        row, errors = self.land(web_message(title, body))
        self.assertEqual(errors, [])
        self.assertTrue(row.corrective)
        self.assertEqual(row.pull, "7")
        self.assertEqual(row.tests, ("tests/app_test.rs::the_reader_stops_stalling",))
        self.assertTrue(row.resolved)

    def test_an_api_merge_without_a_message_field_still_drops_them(self):
        """The negative control: Forgejo's API ignores the template's body.

        The same pull request merged by `{"Do": "merge"}` alone lands the
        title and nothing else, and the audit fires. Passing the checked
        lines as `MergeMessageField` is what makes an API merge carry them.
        """

        description = "Regression-Test: tests/app_test.rs::the_reader_stops_stalling\n"
        title, _body = render(
            self.template(), pull_request("fix(app): stop the stall", 8, description)
        )
        row, errors = self.land(api_message(title))
        self.assertEqual(row.tests, ())
        self.assertTrue(errors, "a title-only corrective landing passed the audit")
        self.assertIn("carries no Regression-Test", errors[0])

        row, errors = self.land(
            api_message(title, "Regression-Test: tests/app_test.rs::the_reader_stops_stalling")
        )
        self.assertEqual(errors, [])
        self.assertTrue(row.resolved)


if __name__ == "__main__":
    unittest.main()
