"""Check a pull request's `Regression-Test:` fields before it can merge.

The field is §3.1 of docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md:

    Regression-Test: crates/plurxd/src/transcode.rs::the_producer_releases_on_late_arrival

A repository-relative path and a test name, never a SHA, so rebasing a branch
cannot invalidate it — which is the whole reason it replaces the per-commit
short-SHA receipts in `validation/regressions.d/`.

There are two carriers because there are two questions. *Pre-merge* — here —
the PR description carries it and this module checks it against the tree the
merge will produce. *At merge*, the landing commit's message carries the same
lines and `validation.history.audit_merge_regressions` checks them against
that commit's own immutable tree. Both go through one resolver,
`validation.history.resolve_regression_test`, so they cannot disagree about
what resolves. This module is what makes a typo fixable: once a landing
commit exists it cannot be amended, and the only remaining remedy is a
permanent row in `validation/merge-errata.toml`.

**Which tree.** The plan assumed the fast lane's `pull_request` checkout is
the merge candidate. On this Forgejo it is not: the checkout step runs
`git checkout refs/remotes/pull/<N>/head`, the branch head (task 11298, PR
#480, 2026-09-24). So when the lane supplies the base and head revisions this
module builds the merge candidate's tree itself with `git merge-tree
--write-tree` and judges the fields against that; only when that cannot be
built (a conflicted merge, or a Git older than 2.38) does it fall back to the
checked-out head, and it says so.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import sys


from validation.history import (
    CORRECTIVE_RE,
    REGRESSION_TRAILER,
    HistoryError,
    TreeReader,
    _git,
    git_tree_reader,
    load_merge_ledger,
    parse_regression_fields,
    resolve_regression_test,
    worktree_reader,
)
from validation.runner import REPO_ROOT


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
    return parse_regression_fields(body)


def check_field(
    root: Path,
    value: str,
    read: TreeReader | None = None,
    where: str = "this tree",
) -> tuple[str | None, str | None]:
    """(error, warning) for one field value, against one tree.

    `read` defaults to the checkout on disk, which is what a local run wants;
    the lane passes the merge candidate's tree.
    """

    problem = resolve_regression_test(value, read or worktree_reader(root), where)
    if problem is not None:
        return problem, None
    path = value.partition("::")[0].strip()
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


def merge_candidate_tree(root: Path, base: str, head: str) -> str | None:
    """The tree merging `head` into `base` would produce, or None.

    `git merge-tree --write-tree` exits 1 on a conflicted merge and is
    missing before Git 2.38; either way there is no clean candidate to judge.
    """

    try:
        output = _git(root, "merge-tree", "--write-tree", base, head)
    except HistoryError:
        return None
    tree = output.split("\n", 1)[0].strip()
    return tree if re.fullmatch(r"[0-9a-f]{40,64}", tree) else None


def corrective_subjects(root: Path, base: str, head: str) -> tuple[str, ...]:
    log = _git(root, "log", "--no-merges", "--format=%s", f"{base}..{head}", "--")
    return tuple(
        subject
        for subject in log.splitlines()
        if CORRECTIVE_RE.match(subject)
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Check a pull request's Regression-Test fields against the tree it merges to."
    )
    parser.add_argument("--root", type=Path, default=REPO_ROOT)
    parser.add_argument("--body", help="the pull request description")
    parser.add_argument("--body-file", type=Path, help="read the description from a file")
    parser.add_argument("--title", help="the pull request title")
    parser.add_argument("--base", help="base revision of the pull request's range")
    parser.add_argument("--head", help="head revision of the pull request's range")
    parser.add_argument(
        "--rev",
        help="judge the fields in this commit or tree rather than the merge "
        "candidate (with --base/--head) or the checkout on disk (without)",
    )
    parser.add_argument(
        "--landing-lines",
        action="store_true",
        help="print only the checked Regression-Test lines, for the landing "
        "commit's message; exits 1 if any does not resolve",
    )
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
    say = (lambda *_args, **_kwargs: None) if args.landing_lines else print

    if args.rev:
        read, where = git_tree_reader(root, args.rev), f"revision {args.rev}"
    elif base and head:
        checkout = _git(root, "rev-parse", "HEAD").strip()
        say(
            f"checkout HEAD {checkout[:12]}; pull request head {head[:12]} — "
            + (
                "the lane checked out the branch head, not a merge candidate"
                if checkout.startswith(head) or head.startswith(checkout)
                else "the checkout is not the pull request head"
            )
        )
        tree = merge_candidate_tree(root, base, head)
        if tree is not None:
            read, where = git_tree_reader(root, tree), "the merge candidate"
            say(f"judging against the merge candidate of {base[:12]} and {head[:12]}: tree {tree[:12]}")
        else:
            read, where = git_tree_reader(root, head), "the pull request head"
            say(
                "warning: no clean merge candidate could be built "
                "(a conflicted merge, or Git older than 2.38); judging "
                "against the pull request head instead"
            )
    else:
        read, where = worktree_reader(root), "this tree"

    # §3.2's second mitigation: a reviewer should see `feat(` on a pull
    # request whose body describes a repair, so print the prefix beside the
    # field check rather than hoping someone reads the title.
    if title:
        prefix = title.split(":", 1)[0] if ":" in title else title.split(" ", 1)[0]
        judged = "corrective" if CORRECTIVE_RE.match(title) else "not corrective"
        say(f"pull request subject prefix: {prefix!r} — {judged} under §3.2")

    fields = parse_fields(body)
    errors: list[str] = []
    for value in fields:
        error, warning = check_field(root, value, read, where)
        if error:
            errors.append(error)
        elif warning:
            say(f"warning: {warning}")
        else:
            say(f"ok: {value}")

    if not fields:
        needs: str | None = None
        if title and CORRECTIVE_RE.match(title):
            needs = title
        elif base and head:
            try:
                subjects = corrective_subjects(root, base, head)
            except HistoryError as exc:
                say(f"warning: cannot read the pull request range: {exc}")
                subjects = ()
            needs = subjects[0] if subjects else None
        if needs:
            errors.append(
                f"this pull request is corrective and carries no "
                f"{REGRESSION_TRAILER}: field: {needs}"
            )

    if errors:
        print(f"{REGRESSION_TRAILER} field check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        try:
            phase_b = load_merge_ledger(root / "validation" / "merge-errata.toml").enforce_after
        except HistoryError:
            phase_b = None
        if not phase_b and not args.landing_lines:
            print(
                "  (phase A: no boundary is set in validation/merge-errata.toml, "
                "so the fast lane reports this and does not block on it)",
                file=sys.stderr,
            )
        return 1
    if args.landing_lines:
        for value in fields:
            print(f"{REGRESSION_TRAILER}: {value}")
        return 0
    if not fields:
        print(f"no {REGRESSION_TRAILER}: field, and nothing corrective that needs one")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
