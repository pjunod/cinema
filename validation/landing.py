"""Land one pull request on `main` through the Forgejo API, and audit a landing.

`make history-check` reads a corrective pull request's `Regression-Test:`
lines from the landing commit's message (docs/VALIDATION.md, "The boundary,
and what replaces a fragment past it"). The merge train copies them. The raw
API does not: `POST …/pulls/<N>/merge` without `MergeMessageField` lands the
title alone, and a landing commit cannot be amended, so every such merge
costs a permanent row in `validation/merge-errata.toml` (#1596 and #1599 on
2026-11-14, eleven more on 2026-11-15).

Two entry points share one definition of a landing message:

* `scripts/land <pr> --head <sha>` is the landing path for a merge made with
  the train off. It refuses a pull request that is not ready, resolves every
  line in the tree the merge will produce, merges with the lines in the
  message, then reads the landed commit back and checks them.
* `python3 -m validation.landing check` runs on every push to `main`
  (`.github/workflows/landing-check.yml`) and fails, naming the pull request,
  when a landing lacks the lines its description carries. It is the same
  question `make history-check` asks in `ci.yml`, asked when the landing
  happens instead of at the next manual dispatch.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
from typing import Callable
import urllib.error
import urllib.parse
import urllib.request

from validation.history import (
    CORRECTIVE_RE,
    REGRESSION_TRAILER,
    HistoryError,
    _excuse,
    _git,
    git_tree_reader,
    landing_commit_title,
    load_merge_ledger,
    parse_regression_fields,
    resolve_regression_test,
)
from validation.regression_field import corrective_subjects, landing_lines
from validation.runner import REPO_ROOT


TARGET = "main"
# Forgejo's own work-in-progress prefixes; either keeps a pull request a draft.
DRAFT_RE = re.compile(r"^\s*(?:wip:|\[wip\])", re.IGNORECASE)
# Forgejo answers 405 with this text while it is still computing whether the
# pull request merges cleanly. The merge train waits on the same answer.
RETRY_LATER = re.compile(r"try again later", re.IGNORECASE)
FULL_SHA_RE = re.compile(r"[0-9a-f]{40}")
MERGE_RETRIES = 12
MERGE_WAIT = 5.0
READBACK_RETRIES = 5
READBACK_WAIT = 2.0


class LandRefused(Exception):
    """Nothing was merged, and the reason is the pull request's to fix."""


class LandConflict(LandRefused):
    """Nothing was merged: the pull request does not merge cleanly into main."""


class LandedWrong(Exception):
    """The merge happened and the landing commit is not what was asked for."""


class ForgeError(Exception):
    def __init__(self, status: int, body: str):
        super().__init__(f"HTTP {status}")
        self.status = status
        self.body = body


class Forge:
    """The four Forgejo calls a landing needs, for one repository."""

    def __init__(self, url: str, repo: str, token: str | None,
                 opener: Callable = urllib.request.urlopen):
        self.url = url.rstrip("/")
        self.repo = repo
        self._token = token
        self._open = opener

    def _call(self, method: str, path: str, payload: dict | None = None) -> object:
        headers = {"Accept": "application/json"}
        if self._token:
            headers["Authorization"] = f"token {self._token}"
        data = None
        if payload is not None:
            data = json.dumps(payload).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            f"{self.url}/api/v1/repos/{self.repo}{path}", data=data, headers=headers, method=method
        )
        try:
            with self._open(request, timeout=30) as response:
                raw = response.read()
        except urllib.error.HTTPError as error:
            raise ForgeError(error.code, error.read().decode("utf-8", "replace")) from None
        return json.loads(raw) if raw.strip() else None

    def pull(self, number: int) -> dict:
        return self._call("GET", f"/pulls/{number}")

    def merge(self, number: int, head: str, title: str, message: str) -> None:
        self._call("POST", f"/pulls/{number}/merge", {
            "Do": "merge",
            "MergeTitleField": title,
            "MergeMessageField": message,
            "head_commit_id": head,
        })

    def commit_message(self, sha: str) -> str:
        return self._call("GET", f"/git/commits/{sha}")["commit"]["message"]

    def fetch(self, root: Path, number: int) -> tuple[str, str]:
        """Bring main and the pull request's head into `root`; their SHAs.

        The token travels in the environment, never in an argument list or a
        stored remote URL.
        """

        env = os.environ.copy()
        if self._token:
            env.update({
                "GIT_CONFIG_COUNT": "1",
                "GIT_CONFIG_KEY_0": "http.extraHeader",
                "GIT_CONFIG_VALUE_0": f"Authorization: token {self._token}",
            })
        env["GIT_TERMINAL_PROMPT"] = "0"
        fetched = subprocess.run(
            ["git", "-C", str(root), "fetch", "--quiet", "--no-tags", f"{self.url}/{self.repo}.git",
             f"+refs/heads/{TARGET}:refs/land/{TARGET}",
             f"+refs/pull/{number}/head:refs/land/pull-{number}"],
            env=env, capture_output=True, text=True, check=False,
        )
        if fetched.returncode != 0:
            raise HistoryError(f"cannot fetch {TARGET} and pull request #{number} from the forge "
                               f"(git fetch exit {fetched.returncode})")
        return (
            _git(root, "rev-parse", f"refs/land/{TARGET}").strip(),
            _git(root, "rev-parse", f"refs/land/pull-{number}").strip(),
        )


def _writes_merge_trees(root: Path) -> bool:
    """`git merge-tree --write-tree` arrived in Git 2.38."""

    found = re.search(r"(\d+)\.(\d+)", _git(root, "version"))
    return bool(found) and (int(found.group(1)), int(found.group(2))) >= (2, 38)


def classic_merge_tree(root: Path, main: str, head: str) -> str | None:
    """The merge's tree from a three-way index merge, for a Git older than 2.38.

    `read-tree -m` and `git-merge-one-file` in a throwaway index and work
    tree: the checkout is not touched. It knows one merge base and no renames,
    so None means "this could not build it", not "it conflicts".
    """

    bases = _git(root, "merge-base", "--all", main, head).split()
    if len(bases) != 1:
        return None
    git_dir = _git(root, "rev-parse", "--absolute-git-dir").strip()
    with tempfile.TemporaryDirectory() as scratch:
        work = Path(scratch) / "work"
        work.mkdir()
        env = {**os.environ, "GIT_DIR": git_dir, "GIT_WORK_TREE": str(work),
               "GIT_INDEX_FILE": str(Path(scratch) / "index")}
        for command in (
            ("read-tree", "-i", "-m", "--aggressive", bases[0], main, head),
            ("merge-index", "-o", "-q", "git-merge-one-file", "-a"),
            ("write-tree",),
        ):
            done = subprocess.run(("git", *command), cwd=work, env=env, capture_output=True,
                                  text=True, check=False)
            if done.returncode != 0:
                return None
        tree = done.stdout.strip()
    return tree if FULL_SHA_RE.fullmatch(tree) else None


def merge_tree(root: Path, main: str, head: str) -> tuple[str | None, bool]:
    """(the tree merging `head` into `main` produces, whether None proves a conflict)."""

    if _writes_merge_trees(root):
        try:
            tree = _git(root, "merge-tree", "--write-tree", main, head).split("\n", 1)[0].strip()
        except HistoryError:
            return None, True
        return (tree if FULL_SHA_RE.fullmatch(tree) else None), True
    return classic_merge_tree(root, main, head), False


def landing_message(title: str, number: int, branch: str, fields: tuple[str, ...]) -> tuple[str, str]:
    """Forgejo's merge subject for this pull request, and the trailer body."""

    subject = f"Merge pull request '{title}' (#{number}) from {branch} into {TARGET}"
    return subject, "\n".join(landing_lines(fields))


def missing_trailers(message: str, trailers: tuple[str, ...]) -> tuple[str, ...]:
    carried = {line.strip() for line in message.splitlines()}
    return tuple(line for line in trailers if line not in carried)


def prepare(pull: dict, number: int, head: str, root: Path,
            fetch: Callable[[int], tuple[str, str]],
            say: Callable[[str], None] = print) -> tuple[str, str, tuple[str, ...]]:
    """Every refusal that can be made before the merge; (subject, message, trailers)."""

    title = str(pull.get("title") or "")
    if pull.get("merged"):
        raise LandRefused(f"#{number} is already merged as {pull.get('merge_commit_sha') or 'an unknown commit'}")
    if pull.get("state") != "open":
        raise LandRefused(f"#{number} is {pull.get('state') or 'not open'}, not open")
    base = (pull.get("base") or {}).get("ref")
    if base != TARGET:
        raise LandRefused(f"#{number} targets {base!r}, and scripts/land only lands on {TARGET}")
    if DRAFT_RE.match(title):
        raise LandRefused(f"#{number} is still a draft ({title!r}); take the WIP prefix off its title "
                          f"when it is ready to merge")
    forge_head = (pull.get("head") or {}).get("sha") or ""
    if forge_head != head:
        raise LandRefused(f"#{number} head is {forge_head or 'unknown'}, not the --head {head} you reviewed; "
                          f"look at what was pushed, then run again with the new head")
    main_sha, fetched_head = fetch(number)
    if fetched_head != head:
        raise LandRefused(f"#{number} fetched as {fetched_head}, not the --head {head}; it was pushed to "
                          f"while landing")
    remedy = f"merge {TARGET} into the branch, push, and run again with the new head"
    tree, proven = merge_tree(root, main_sha, head)
    where = "the tree the merge will produce"
    if tree is None and proven:
        raise LandConflict(f"#{number} conflicts with {TARGET} {main_sha[:9]} (git merge-tree could not "
                           f"build a clean merge of {head[:9]}); {remedy}")
    if tree is None and pull.get("mergeable") is False:
        raise LandConflict(f"#{number} conflicts with {TARGET} {main_sha[:9]}: the forge reports it not "
                           f"mergeable and this Git (older than 2.38) could not build the merge of "
                           f"{head[:9]} either; {remedy}. If the forge was only still checking, run the "
                           f"same command again")
    if tree is None:
        # Older Git, a merge it cannot reproduce, and a forge that calls it
        # mergeable: the forge decides conflicts at the merge, and the lines
        # are judged where the fast lane judges them in the same position.
        tree, where = head, "the pull request head"
        say(f"warning: this Git is older than 2.38 and could not build the merge tree; judging the "
            f"{REGRESSION_TRAILER} lines against the pull request head {head[:9]} instead")

    fields = parse_regression_fields(str(pull.get("body") or ""))
    read = git_tree_reader(root, tree)
    problems = [
        problem
        for problem in (resolve_regression_test(value, read, where) for value in fields)
        if problem is not None
    ]
    if not fields:
        needs = title if CORRECTIVE_RE.match(title) else None
        if needs is None:
            subjects = corrective_subjects(root, main_sha, head)
            needs = subjects[0] if subjects else None
        if needs is not None:
            raise LandRefused(f"#{number} is corrective and its description carries no "
                              f"{REGRESSION_TRAILER}: <path>::<test> line: {needs}")
    if problems:
        raise LandRefused(f"#{number} has {REGRESSION_TRAILER} lines that do not resolve in {where}:\n"
                          + "\n".join(f"  - {problem}" for problem in problems))
    subject, message = landing_message(title, number, str((pull.get("head") or {}).get("ref") or ""), fields)
    return subject, message, landing_lines(fields)


def land(forge, number: int, head: str, root: Path = REPO_ROOT, *,
         fetch: Callable[[int], tuple[str, str]] | None = None,
         dry_run: bool = False, retries: int = MERGE_RETRIES, wait: float = MERGE_WAIT,
         sleep: Callable[[float], None] = time.sleep,
         say: Callable[[str], None] = print) -> str | None:
    """Land `number` at `head`; the landing commit's SHA (None for a dry run)."""

    fetch = fetch or (lambda pr: forge.fetch(root, pr))
    subject, message, trailers = prepare(forge.pull(number), number, head, root, fetch, say)
    say(subject)
    for line in trailers:
        say(line)
    if dry_run:
        say(f"dry run: #{number} would land with the message above; nothing was merged")
        return None

    for attempt in range(retries + 1):
        try:
            forge.merge(number, head, subject, message)
            break
        except ForgeError as error:
            if error.status == 405 and RETRY_LATER.search(error.body):
                if attempt == retries:
                    raise LandRefused(
                        f"the forge is still computing whether #{number} merges (HTTP 405 'try again "
                        f"later' on {retries + 1} attempts over {int(retries * wait)}s); nothing was "
                        f"merged; run the same command again"
                    ) from None
                say(f"forge is still computing mergeability of #{number}; waiting {wait:g}s "
                    f"({attempt + 1}/{retries})")
                sleep(wait)
                continue
            if error.status in (405, 409, 422):
                detail = _forge_message(error.body)
                if re.search(r"conflict", detail, re.IGNORECASE):
                    raise LandConflict(f"the forge refused to merge #{number} (HTTP {error.status}: "
                                       f"{detail}); it no longer merges cleanly into {TARGET}") from None
                raise LandRefused(f"the forge refused to merge #{number} (HTTP {error.status}: "
                                  f"{detail}); nothing was merged") from None
            raise

    pull: dict = {}
    for attempt in range(READBACK_RETRIES + 1):
        pull = forge.pull(number) or {}
        if pull.get("merged") and pull.get("merge_commit_sha"):
            break
        if attempt < READBACK_RETRIES:
            sleep(READBACK_WAIT)
    sha = str(pull.get("merge_commit_sha") or "")
    if not pull.get("merged") or not sha:
        raise LandedWrong(f"the forge accepted the merge of #{number} and does not report a merge "
                          f"commit; look at {TARGET} before doing anything else")
    landed = forge.commit_message(sha)
    wrong = []
    if landed.splitlines()[:1] != [subject]:
        wrong.append(f"its subject is {(landed.splitlines() or [''])[0]!r}, not {subject!r}")
    wrong.extend(f"it lacks `{line}`" for line in missing_trailers(landed, trailers))
    if wrong:
        raise LandedWrong(
            f"#{number} landed as {sha} and the landing commit is not the one asked for:\n"
            + "\n".join(f"  - {problem}" for problem in wrong)
            + f"\n{TARGET}'s history-check will fail on it; it needs an [[errata]] row in "
              f"validation/merge-errata.toml"
        )
    say(f"landed #{number} as {sha} with {len(trailers)} {REGRESSION_TRAILER} line"
        f"{'' if len(trailers) == 1 else 's'}")
    return sha


def _forge_message(body: str) -> str:
    try:
        parsed = json.loads(body)
    except ValueError:
        parsed = None
    text = parsed.get("message") if isinstance(parsed, dict) else None
    return " ".join(str(text or body).split())[:300] or "no message"


# -- after the fact: every landing in a push to main -------------------------------


def audit_landing(root: Path, sha: str, body_of: Callable[[int], str | None]) -> list[str]:
    """What is wrong with one first-parent commit of main as a landing, if anything.

    `body_of` returns the pull request's description, or None when the forge
    cannot be asked; the commit's own message and tree are then all there is.
    """

    subject = _git(root, "show", "-s", "--format=%s", sha).strip()
    landed = landing_commit_title(subject)
    if landed is None:
        return []
    title, pull = landed
    message = _git(root, "show", "-s", "--format=%B", sha)
    carried = parse_regression_fields(message)
    read = git_tree_reader(root, sha)
    where = f"#{pull} landed as {sha[:9]}"
    errors = [
        f"{where}: {problem}"
        for problem in (resolve_regression_test(value, read, "its own tree") for value in carried)
        if problem is not None
    ]
    body = body_of(int(pull))
    if body is not None:
        lacking = missing_trailers(message, landing_lines(parse_regression_fields(body)))
        if lacking:
            errors.append(f"{where} without {len(lacking)} of the {REGRESSION_TRAILER} lines its "
                          f"description carries: " + "; ".join(lacking))
    if not carried:
        parents = _git(root, "show", "-s", "--format=%P", sha).split()
        brought = (
            [line for line in _git(root, "log", "--no-merges", "--format=%s",
                                   f"{parents[0]}..{sha}", "--").splitlines() if CORRECTIVE_RE.match(line)]
            if len(parents) > 1 else []
        )
        if CORRECTIVE_RE.match(title):
            errors.append(f"{where} with no {REGRESSION_TRAILER} line, and its title is corrective: {title}")
        elif brought:
            errors.append(f"{where} with no {REGRESSION_TRAILER} line, and it brings in "
                          f"{len(brought)} corrective commit{'' if len(brought) == 1 else 's'}: {brought[0]}")
    return errors


def audit_push(root: Path, before: str, after: str,
               body_of: Callable[[int], str | None]) -> list[str]:
    """Every landing the push `before..after` put on main, audited.

    A landing an errata row already records is not reported again.
    """

    ledger = load_merge_ledger(root / "validation" / "merge-errata.toml")
    known = bool(before) and set(before) != {"0"}
    if known:
        try:
            _git(root, "rev-parse", "--verify", f"{before}^{{commit}}")
        except HistoryError:
            known = False
    commits = (
        _git(root, "rev-list", "--first-parent", f"{before}..{after}", "--").split()
        if known else [_git(root, "rev-parse", "--verify", f"{after}^{{commit}}").strip()]
    )
    errors: list[str] = []
    for sha in reversed(commits):
        if _excuse(ledger, sha) is None:
            errors.extend(audit_landing(root, sha, body_of))
    return errors


# -- command lines ------------------------------------------------------------------


def _origin(root: Path) -> tuple[str, str] | None:
    """(forge URL, owner/repo) from the checkout's `origin`, without any credentials in it."""

    try:
        remote = _git(root, "remote", "get-url", "origin").strip()
    except HistoryError:
        return None
    parts = urllib.parse.urlsplit(remote)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        return None
    host = parts.hostname + (f":{parts.port}" if parts.port else "")
    repo = parts.path.strip("/").removesuffix(".git")
    return (f"{parts.scheme}://{host}", repo) if repo.count("/") == 1 else None


def _token(root: Path, token_file: Path | None) -> str | None:
    if os.environ.get("FORGEJO_TOKEN"):
        return os.environ["FORGEJO_TOKEN"].strip()
    if token_file is None:
        config = root / "scripts" / "ci-failures.fleet.json"
        if config.is_file():
            named = json.loads(config.read_text(encoding="utf-8")).get("token_file")
            token_file = Path(os.path.expanduser(named)) if named else None
    return token_file.read_text(encoding="utf-8").strip() if token_file else None


def main_land(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="scripts/land",
        description="Land a pull request on main with its Regression-Test lines in the landing "
                    "commit. The landing path when MERGE_TRAIN is off; never the raw merge API.",
    )
    parser.add_argument("pr", type=int, help="pull request number")
    parser.add_argument("--head", required=True, help="the full head SHA you reviewed and tested")
    parser.add_argument("--dry-run", action="store_true",
                        help="make every check and print the landing message; merge nothing")
    parser.add_argument("--forge-url", default=os.environ.get("FORGEJO_URL"),
                        help="forge base URL (default: $FORGEJO_URL, else the origin remote)")
    parser.add_argument("--repo", default=os.environ.get("FORGEJO_REPO"),
                        help="owner/name (default: $FORGEJO_REPO, else the origin remote)")
    parser.add_argument("--token-file", type=Path,
                        help="file holding the API token (default: $FORGEJO_TOKEN, else token_file "
                             "in scripts/ci-failures.fleet.json)")
    parser.add_argument("--retries", type=int, default=MERGE_RETRIES,
                        help="how many times to wait on 'try again later' (default %(default)s)")
    parser.add_argument("--wait", type=float, default=MERGE_WAIT,
                        help="seconds between those attempts (default %(default)s)")
    parser.add_argument("--root", type=Path, default=REPO_ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)

    if not FULL_SHA_RE.fullmatch(args.head):
        print("land: --head must be the full 40-character commit SHA", file=sys.stderr)
        return 2
    origin = _origin(args.root)
    url = args.forge_url or (origin[0] if origin else None)
    repo = args.repo or (origin[1] if origin else None)
    if not url or not repo:
        print("land: pass --forge-url and --repo (the origin remote is not an http(s) forge URL)",
              file=sys.stderr)
        return 2
    try:
        token = _token(args.root, args.token_file)
    except OSError as error:
        print(f"land: cannot read the token file: {error.strerror}", file=sys.stderr)
        return 2
    if not token:
        print("land: set $FORGEJO_TOKEN or pass --token-file", file=sys.stderr)
        return 2
    try:
        land(Forge(url, repo, token), args.pr, args.head, args.root,
             dry_run=args.dry_run, retries=args.retries, wait=args.wait)
    except LandConflict as conflict:
        print(f"land: CONFLICT, nothing merged: {conflict}", file=sys.stderr)
        return 1
    except LandRefused as refusal:
        print(f"land: refused, nothing merged: {refusal}", file=sys.stderr)
        return 1
    except LandedWrong as wrong:
        print(f"land: MERGED, but: {wrong}", file=sys.stderr)
        return 3
    except ForgeError as error:
        print(f"land: the forge answered HTTP {error.status}: {_forge_message(error.body)}", file=sys.stderr)
        return 2
    except (HistoryError, urllib.error.URLError) as error:
        print(f"land: {error}", file=sys.stderr)
        return 2
    return 0


def main_check(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m validation.landing check",
        description="Fail when a landing pushed to main lacks its pull request's Regression-Test lines.",
    )
    parser.add_argument("--before", default=os.environ.get("LANDING_BEFORE", ""),
                        help="main before the push (default: $LANDING_BEFORE)")
    parser.add_argument("--after", default=os.environ.get("LANDING_SHA") or "HEAD",
                        help="main after the push (default: $LANDING_SHA, else HEAD)")
    parser.add_argument("--root", type=Path, default=REPO_ROOT)
    args = parser.parse_args(argv)

    api, repo = os.environ.get("GITHUB_API_URL"), os.environ.get("GITHUB_REPOSITORY")
    forge = Forge(api.removesuffix("/api/v1"), repo, os.environ.get("GITHUB_TOKEN")) if api and repo else None
    unread: list[int] = []

    def body_of(number: int) -> str | None:
        if forge is None:
            return None
        try:
            return str(forge.pull(number).get("body") or "")
        except (ForgeError, urllib.error.URLError, ValueError, AttributeError):
            unread.append(number)
            return None

    try:
        errors = audit_push(args.root, args.before, args.after, body_of)
    except HistoryError as error:
        print(f"landing check error: {error}", file=sys.stderr)
        return 2
    if forge is None:
        print("landing check: no forge API in this environment; judged each landing by its own "
              "message and tree only")
    for number in unread:
        print(f"landing check: could not read #{number}'s description from the forge; judged its "
              f"landing by its own message and tree only")
    if errors:
        for error in errors:
            print(f"::error::{error.splitlines()[0]}")
        print("LANDING WITHOUT ITS REGRESSION-TEST LINES:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        print(
            "A landing commit cannot be amended. Record each one with an [[errata]] row in\n"
            "validation/merge-errata.toml (python3 -m validation.regression_field --body-file <body>\n"
            "--rev <landing sha> --landing-lines prints the lines it should have carried), and land\n"
            "with scripts/land, never the raw merge API: AGENTS.md \"Landing on main\".",
            file=sys.stderr,
        )
        return 1
    print(f"landing check ok: {args.before[:9] or 'no base'}..{args.after[:9]}")
    return 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["check"]:
        raise SystemExit(main_check(sys.argv[2:]))
    raise SystemExit(main_land(sys.argv[1:]))
