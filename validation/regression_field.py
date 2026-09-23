"""Check a pull request's `Regression-Test:` fields before it can merge.

The field is §3.1 of docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md:

    Regression-Test: crates/plurxd/src/transcode.rs::the_producer_releases_on_late_arrival

A repository-relative path and a test name, never a SHA, so rebasing a branch
cannot invalidate it — which is the whole reason it replaces the per-commit
short-SHA receipts in `validation/regressions.d/`.

There are two carriers because there are two questions. *Pre-merge* — here —
the PR description carries it, and the fast lane checks it against the
**merge candidate** it has checked out, because that is the tree the field
will be true of. *At merge*, the landing commit carries the same line as a
trailer and `validation.history.audit_merge_regressions` checks it against
that commit's own immutable tree. This module is what makes a typo fixable:
once a landing commit exists it cannot be amended, and the only remaining
remedy is a permanent row in `validation/merge-errata.toml`.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import sys


from validation.history import CORRECTIVE_RE, REGRESSION_TRAILER, _git
from validation.runner import REPO_ROOT
from validation.test_markers import defines_test


FIELD_RE = re.compile(rf"^\s*{REGRESSION_TRAILER}:\s*(?P<value>\S.*?)\s*$", re.MULTILINE)
_JOB_RE = re.compile(r"^  (?P<name>[A-Za-z0-9_-]+):$", re.MULTILINE)
_NODE_SUITE_RE = re.compile(r"\bnode\s+(?P<path>[\w./-]+\.(?:test\.)?js)\b")
_DISCOVER_RE = re.compile(r"\bunittest\s+discover\s+-s\s+(?P<path>[\w./-]+)")
_MAKE_TARGET_RE = re.compile(r"\bmake\s+(?P<target>[a-z0-9][a-z0-9-]*)\b")


def _job_block(workflow: str, name: str) -> str:
    """The body of one job in a workflow, sliced by indentation.

    Text, not YAML: this module runs inside `preflight` itself and must not
    need a dependency the runner may not have. The slice is exact because
    every job in these workflows is a two-space key and every line inside one
    is indented further.
    """

    start = None
    for match in _JOB_RE.finditer(workflow):
        if match.group("name") == name:
            start = match.end()
            continue
        if start is not None:
            return workflow[start : match.start()]
    if start is None:
        raise LookupError(f"no job named {name!r}")
    return workflow[start:]


def executed_suites(root: Path) -> tuple[tuple[str, ...], tuple[str, ...]]:
    """The suites the fast lane's `preflight` job actually executes.

    Derived from the lane's own step list, never hardcoded, so that the day
    the lane gains a step that runs Rust tests this check starts accepting
    Rust paths without anyone editing this file. `make <target>` is resolved
    one level through the Makefile, which is how `make operations-check`
    becomes `tests/operations`.

    Returns (files, directories).
    """

    lane = (root / ".github/workflows/main-fast-lane.yml").read_text(encoding="utf-8")
    commands = [_job_block(lane, "preflight")]
    makefile = (root / "Makefile").read_text(encoding="utf-8")
    for target in set(_MAKE_TARGET_RE.findall(commands[0])):
        recipe = re.search(
            rf"^{re.escape(target)}:[^\n]*\n((?:\t[^\n]*\n)+)", makefile, re.MULTILINE
        )
        if recipe:
            commands.append(recipe.group(1))
    text = "\n".join(commands)
    files = sorted({match.group("path") for match in _NODE_SUITE_RE.finditer(text)})
    directories = sorted({match.group("path") for match in _DISCOVER_RE.finditer(text)})
    return tuple(files), tuple(directories)


def parse_fields(body: str) -> tuple[str, ...]:
    return tuple(match.group("value") for match in FIELD_RE.finditer(body))


def check_field(root: Path, value: str) -> tuple[str | None, str | None]:
    """(error, warning) for one field value, against the checked-out tree."""

    path, separator, name = value.partition("::")
    if not separator or not path.strip() or not name.strip():
        return f"{REGRESSION_TRAILER} {value!r} is not <path>::<name>", None
    path, name = path.strip(), name.strip()
    target = root / path
    if not target.is_file():
        return f"{REGRESSION_TRAILER} names {path}, which this tree does not carry", None
    try:
        source = target.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        return f"{REGRESSION_TRAILER} cannot read {path}: {exc}", None
    if not defines_test(source, name):
        return (
            f"{REGRESSION_TRAILER} names {name}, which {path} does not define "
            f"as a test (searched {path} for a Node title, or a declaration "
            f"under #[test]/#[tokio::test]/@Test, or a name beginning `test`)",
            None,
        )

    files, directories = executed_suites(root)
    if path in files or any(
        path == directory or path.startswith(f"{directory.rstrip('/')}/")
        for directory in directories
    ):
        return None, None
    # §3.1 step 3, and open question 3 of the plan. The lane's Rust gate is
    # compile-only, so no Rust test is in the executed set and this can only
    # be a warning today. The set above is derived from the lane, so this
    # stops warning on its own once the lane runs the suite.
    return None, (
        f"{REGRESSION_TRAILER} names {path}, which the fast lane's preflight "
        f"does not execute (it runs {', '.join(files + directories)}); the "
        f"field is accepted but nothing on this lane ran it"
    )


def corrective_subjects(root: Path, base: str, head: str) -> tuple[str, ...]:
    log = _git(root, "log", "--no-merges", "--format=%s", f"{base}..{head}", "--")
    return tuple(
        subject
        for subject in log.splitlines()
        if CORRECTIVE_RE.match(subject)
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Check a pull request's Regression-Test fields against this tree."
    )
    parser.add_argument("--root", type=Path, default=REPO_ROOT)
    parser.add_argument("--body", help="the pull request description")
    parser.add_argument("--body-file", type=Path, help="read the description from a file")
    parser.add_argument("--title", help="the pull request title, for the prefix note")
    parser.add_argument("--base", help="base revision of the pull request's range")
    parser.add_argument("--head", help="head revision of the pull request's range")
    args = parser.parse_args(argv)

    root = args.root
    if args.body_file:
        body = args.body_file.read_text(encoding="utf-8")
    elif args.body is not None:
        body = args.body
    else:
        body = os.environ.get("PR_BODY", "")
    title = args.title if args.title is not None else os.environ.get("PR_TITLE", "")
    base = args.base or os.environ.get("PR_BASE_SHA", "")
    head = args.head or os.environ.get("PR_HEAD_SHA", "")

    # §3.2's second mitigation: a reviewer should see `feat(` on a pull
    # request whose body describes a repair, so print the prefix beside the
    # field check rather than hoping someone reads the title.
    if title:
        prefix = title.split(":", 1)[0] if ":" in title else title.split(" ", 1)[0]
        judged = "corrective" if CORRECTIVE_RE.match(title) else "not corrective"
        print(f"pull request subject prefix: {prefix!r} — {judged} under §3.2")

    fields = parse_fields(body)
    errors: list[str] = []
    for value in fields:
        error, warning = check_field(root, value)
        if error:
            errors.append(error)
        elif warning:
            print(f"warning: {warning}")
        else:
            print(f"ok: {value}")

    if not fields and base and head:
        try:
            subjects = corrective_subjects(root, base, head)
        except Exception as exc:  # noqa: BLE001 - reported, never swallowed
            print(f"warning: cannot read the pull request range: {exc}")
            subjects = ()
        if subjects:
            errors.append(
                f"this pull request carries a corrective commit and no "
                f"{REGRESSION_TRAILER}: field: {subjects[0]}"
            )

    if errors:
        print(f"{REGRESSION_TRAILER} field check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    if not fields:
        print(f"no {REGRESSION_TRAILER}: field, and no corrective commit that needs one")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
