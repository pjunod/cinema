"""Assign the store build number at release, from the commit being released.

The Apple build number and the Android versionCode used to be claimed in the
tree: every pull request that touched a client wrote "the next number" into
`project.yml` or `build.gradle.kts` and into the status lines that quoted it.
Two open client pull requests always claimed the same number, so the second
to merge had to re-claim, and each re-claim was another edit to the same
lines (validation/apple_build.py recorded this as the part it could not
solve).

No branch claims a number now. The tree carries `DEVELOPMENT_BUILD` in both
manifests, which is what a local build is, and nothing edits it. A store
build is numbered when it is built, by the commit it is built from:

    build number = the count of commits reachable from the release commit

Every commit `main` advances to has strictly more ancestors than the one
before it, so the number rises with every release without anyone recording
the last one, and building the same commit twice gives the same number. Both
platforms use the one number.

A commit that is not on `main` is numbered by the `main` commit it branched
from, so a branch build never outranks the `main` build that follows it and a
device that ran it still upgrades. Such a build is for devices only: nothing
uploads it to a store (`require_main`).

**Every build path gets its tree from here.** The signing controller, the
Ansible plays and `scripts/ship-physical` read the number from the two
manifests of the checkout they build, so they are all handed a *release
source* (`release_source`): a pinned repository whose `main` is the release
commit plus one child that writes the number into those manifests, or a
checkout of it (`release_checkout`). `make android-release`, which builds the
working tree in a container, passes the same number as `-PplurxBuildNumber`.
A path that builds anything else builds development build 1, and every
upload and install step refuses that (`require_stamped`).

The release job also keeps a ledger of what it published, per platform
(`record_published`), and refuses a number at or below the last one
(`require_above_published`): a rewritten `main` would otherwise be uploaded
under a number the store already holds.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

REPO_ROOT = Path(__file__).resolve().parents[1]
ANDROID = "clients/android/app/build.gradle.kts"
APPLE = "clients/apple/project.yml"
# What both manifests declare in the tree. A build made from an unstamped
# checkout is a development build and says so.
DEVELOPMENT_BUILD = 1
# The last numbers claimed by hand, on the stores when numbering moved here.
# Every assigned number must clear both; the commit count did so by a factor
# of fifty on the day of the change, and this refuses a history (a shallow
# clone, a rewritten main) that would not.
LAST_CLAIMED = {"apple": 301, "android": 207}
GOOGLE_PLAY_CEILING = 2_100_000_000
MAIN = "origin/main"
PIN = "pinned-source.git"
LEDGER = "cinema-published-builds.jsonl"
# Which hand-claimed floor each release platform inherits.
PLATFORM_FLOOR = {"ios": "apple", "tvos": "apple", "android": "android"}

_ANDROID_LINE = re.compile(r"(?m)^(\s*versionCode\s*=\s*)(\d+)(\s*)$")
_APPLE_LINE = re.compile(r'(?m)^(\s*CURRENT_PROJECT_VERSION:\s*")(\d+)("\s*)$')
_STAMP_IDENTITY = {
    "GIT_AUTHOR_NAME": "plurx release stamp",
    "GIT_AUTHOR_EMAIL": "release-stamp@plurx.invalid",
    "GIT_COMMITTER_NAME": "plurx release stamp",
    "GIT_COMMITTER_EMAIL": "release-stamp@plurx.invalid",
}


class MobileBuildError(ValueError):
    """A build number cannot be assigned or stamped, and why."""


def _git(
    repo: Path,
    *args: str,
    env: dict[str, str] | None = None,
    data: str | None = None,
    raw: bool = False,
    check: bool = True,
    status: bool = False,
) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args], text=True, input=data,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=None if env is None else {**os.environ, **env},
    )
    if status:
        return str(result.returncode)
    if result.returncode != 0:
        if not check:
            return ""
        raise MobileBuildError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout if raw else result.stdout.strip()


def on_main(repo: Path, commit: str = "HEAD", main: str = MAIN) -> bool:
    """Whether `commit` is `main` or one of its ancestors."""

    if _git(repo, "rev-parse", "--verify", "--quiet", f"{main}^{{commit}}", check=False) == "":
        raise MobileBuildError(
            f"{repo} has no {main}, so a build cannot be numbered against it; "
            "run `git fetch origin main`"
        )
    return _git(repo, "merge-base", "--is-ancestor", commit, main, status=True) == "0"


def build_number(repo: Path, commit: str = "HEAD", main: str = MAIN) -> int:
    """The build number of `commit`.

    On `main`: how many commits the commit contains. Off it: the number of
    the `main` commit it branched from, so a branch build sorts with its base
    and below every later `main` build.
    """

    if _git(repo, "rev-parse", "--is-shallow-repository") != "false":
        raise MobileBuildError(
            f"{repo} is a shallow clone, so its commit count is not the build number; "
            "fetch the full history (`git fetch --unshallow`)"
        )
    counted = commit
    if not on_main(repo, commit, main):
        counted = _git(repo, "merge-base", commit, main, check=False)
        if not counted:
            raise MobileBuildError(f"{commit} shares no history with {main}")
    number = int(_git(repo, "rev-list", "--count", counted))
    floor = max(LAST_CLAIMED.values())
    if number <= floor:
        raise MobileBuildError(
            f"{counted} contains {number} commits, not above the last store build "
            f"({floor}); this is not the plurx history the stores have builds from"
        )
    if number > GOOGLE_PLAY_CEILING:
        raise MobileBuildError("the build number exceeds Google Play's 2100000000 ceiling")
    return number


def require_main(repo: Path, commit: str, main: str = MAIN) -> None:
    """Refuse a store upload of a commit that is not on `main`.

    A branch commit shares its number with the `main` commit it branched
    from, and a store holds one build per number.
    """

    if not on_main(repo, commit, main):
        raise MobileBuildError(
            f"{commit} is not on {main}: a store build is built from main. Land the "
            "change on main (a hotfix goes next with the `train:solo` label) and the "
            "push publishes it; a branch build is for devices only"
        )


def declared_builds(root: Path) -> dict[str, int]:
    """The build number each manifest of a checkout declares."""

    def one(pattern: re.Pattern[str], path: str) -> int:
        found = pattern.findall((root / path).read_text(encoding="utf-8"))
        if len(found) != 1:
            raise MobileBuildError(
                f"{path}: expected exactly one build-number line, found {len(found)}"
            )
        return int(found[0][1])

    return {"android": one(_ANDROID_LINE, ANDROID), "apple": one(_APPLE_LINE, APPLE)}


def require_stamped(root: Path) -> dict[str, int]:
    """Refuse a checkout that still carries the development build.

    Every upload and install step calls this (or its Ansible twin in
    `noirr_mobile_metadata`) before it builds: an unstamped tree would be
    uploaded as build 1, or refused by a device as a downgrade.
    """

    builds = declared_builds(root)
    if DEVELOPMENT_BUILD in builds.values() or len(set(builds.values())) != 1:
        raise MobileBuildError(
            f"{root} is not a release source: its manifests declare {builds}. A build "
            "that is uploaded or installed comes from `python3 -m validation.mobile_build "
            "checkout` (scripts/ship, scripts/ship-physical and the Ansible plays call "
            "it), never from a working checkout"
        )
    return builds


def stamp_manifests(android: str, apple: str, build: int) -> tuple[str, str]:
    """Both manifests with `build` in place of the development number."""

    def put(pattern: re.Pattern[str], text: str, path: str) -> str:
        found = pattern.findall(text)
        if len(found) != 1:
            raise MobileBuildError(
                f"{path}: expected exactly one build-number line, found {len(found)}"
            )
        if int(found[0][1]) != DEVELOPMENT_BUILD:
            raise MobileBuildError(
                f"{path} declares build {found[0][1]}, not the development build "
                f"{DEVELOPMENT_BUILD}; it is already stamped or was edited by hand"
            )
        return pattern.sub(rf"\g<1>{build}\g<3>", text)

    return put(_ANDROID_LINE, android, ANDROID), put(_APPLE_LINE, apple, APPLE)


def stamp_commit(repo: Path, commit: str, build: int) -> str:
    """A child of `commit` in `repo` whose manifests carry `build`.

    Written with plumbing, so `repo` may be bare and no checkout is touched.
    The author, committer and dates are fixed by `commit`, so stamping the
    same commit with the same number always yields the same sha: a retried
    release job builds the commit the first attempt built.
    """

    android, apple = stamp_manifests(
        _git(repo, "show", f"{commit}:{ANDROID}", raw=True),
        _git(repo, "show", f"{commit}:{APPLE}", raw=True),
        build,
    )
    when = _git(repo, "show", "-s", "--format=%cI", commit)
    with tempfile.TemporaryDirectory() as temporary:
        env = {"GIT_INDEX_FILE": str(Path(temporary) / "index")}
        _git(repo, "read-tree", commit, env=env)
        for path, text in ((ANDROID, android), (APPLE, apple)):
            blob = _git(repo, "hash-object", "-w", "--stdin", data=text)
            mode = _git(repo, "ls-tree", commit, "--", path).split()[0]
            _git(repo, "update-index", "--cacheinfo", f"{mode},{blob},{path}", env=env)
        tree = _git(repo, "write-tree", env=env)
    return _git(
        repo, "commit-tree", tree, "-p", commit, "-m",
        f"release stamp: build {build} of {commit}\n\n"
        "Written by validation/mobile_build.py for the signing controller.\n"
        "Never pushed: the build number is assigned at release, not in the tree.",
        env={**_STAMP_IDENTITY, "GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when},
    )


def release_source(source: Path, state: Path, commit: str, build: int) -> tuple[Path, str]:
    """A bare repository whose `main` is `commit` stamped as `build`.

    The one place a build tree comes from. The controller fetches
    `origin/main` before building and again before publishing, and refuses
    when the two differ; pointed at the live checkout that comparison fails
    whenever anything merges during a build. The pin's remote is itself, so
    both fetches see the commit chosen here; objects come from `source`.

    The controller also reads the build number from the manifests of the
    commit it checks out and refuses a platform whose client paths changed
    after the number last did. `main` here is the stamp commit, the newest
    change to both numbers. Returns the pin and the stamp commit.
    """

    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    pin = state / PIN
    if not (pin / "HEAD").is_file():
        subprocess.run(["git", "init", "--bare", "--quiet", str(pin)], check=True)
    common = Path(_git(source, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    (pin / "objects" / "info").mkdir(parents=True, exist_ok=True)
    (pin / "objects" / "info" / "alternates").write_text(f"{common / 'objects'}\n")
    # The pin owns only its stamp commits. Drop the refs an earlier run left so
    # a replaced or rewritten source clone cannot leave one naming objects it
    # no longer has.
    for ref in _git(pin, "for-each-ref", "--format=%(refname)", "refs/remotes").splitlines():
        _git(pin, "update-ref", "-d", ref)
    _git(pin, "worktree", "prune")
    _git(pin, "config", "remote.origin.url", str(pin))
    stamped = stamp_commit(pin, commit, build)
    _git(pin, "update-ref", "refs/heads/main", stamped)
    return pin, stamped


def release_checkout(source: Path, state: Path, commit: str) -> tuple[Path, int, str]:
    """A checkout of `commit` stamped with its build number, for a local build.

    Returns the checkout, the number and the stamp commit. The directory is
    named after the stamp, so the same commit reuses its build output and a
    different one never inherits it. Tracked drift is discarded.
    """

    commit = _git(source, "rev-parse", "--verify", f"{commit}^{{commit}}")
    build = build_number(source, commit)
    pin, stamped = release_source(source, state, commit, build)
    checkout = state / "checkouts" / f"plurx-{stamped[:12]}"
    if not (checkout / ".git").exists():
        checkout.parent.mkdir(parents=True, exist_ok=True)
        _git(pin, "worktree", "add", "--detach", str(checkout), stamped)
    if _git(checkout, "rev-parse", "HEAD") != stamped:
        raise MobileBuildError(
            f"{checkout} is not at {stamped}; remove it with "
            f"`git -C {pin} worktree remove --force {checkout}` and run again"
        )
    _git(checkout, "reset", "--hard", "--quiet", stamped)
    require_stamped(checkout)
    return checkout, build, stamped


def published(state: Path) -> dict[str, dict]:
    """The highest build this controller published, per platform."""

    ledger = state / LEDGER
    latest: dict[str, dict] = {}
    if not ledger.is_file():
        return latest
    for number, line in enumerate(ledger.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
            platform, build = row["platform"], int(row["build"])
        except (ValueError, KeyError, TypeError) as exc:
            raise MobileBuildError(f"{ledger} line {number} is not a publication record") from exc
        if platform not in latest or build > int(latest[platform]["build"]):
            latest[platform] = row
    return latest


def require_above_published(state: Path, platforms: list[str], build: int) -> None:
    """Refuse a number at or below the last one published for a platform.

    The commit count only rises while `main` is never rewritten. This is the
    record that holds when it is: a store rejects a number it has seen only
    after the whole build and upload, and Play accepts a lower one silently
    into a track nobody is offered.
    """

    latest = published(state)
    for platform in platforms:
        floor = LAST_CLAIMED[PLATFORM_FLOOR[platform]]
        source = "the last hand-claimed build"
        if platform in latest:
            floor, source = int(latest[platform]["build"]), (
                f"published from {str(latest[platform].get('source_commit', '?'))[:12]} "
                f"at {latest[platform].get('at', '?')}"
            )
        if build <= floor:
            raise MobileBuildError(
                f"build {build} is not above {platform} build {floor} ({source}); "
                "the same or a lower number cannot be published again. If main was "
                "rewritten, land a commit that raises its commit count past that build"
            )


def record_published(
    state: Path, platforms: list[str], build: int, source_commit: str, stamp_commit: str
) -> None:
    """Append what was just published: number, source commit and stamp commit."""

    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    at = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    with (state / LEDGER).open("a", encoding="utf-8") as ledger:
        for platform in platforms:
            ledger.write(json.dumps({
                "platform": platform, "build": build, "source_commit": source_commit,
                "stamp_commit": stamp_commit, "at": at,
            }, sort_keys=True) + "\n")


def default_state() -> Path:
    return Path(
        os.environ.get("PLURX_RELEASE_SOURCE_DIR", "~/.local/state/noirr/mobile-release-source")
    ).expanduser()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    commands = parser.add_subparsers(dest="command", required=True)
    number = commands.add_parser("number", help="print the build number of a commit")
    number.add_argument("--repo", type=Path, default=REPO_ROOT)
    number.add_argument("--commit", default="HEAD")
    checkout = commands.add_parser(
        "checkout",
        help="print the path of a checkout of a commit stamped with its build number",
    )
    checkout.add_argument("--repo", type=Path, default=REPO_ROOT)
    checkout.add_argument("--commit", default=MAIN)
    checkout.add_argument("--state", type=Path, default=None)
    checkout.add_argument("--fetch", action="store_true", help="fetch origin/main first")
    checkout.add_argument(
        "--store", action="store_true", help="refuse a commit that is not on main"
    )
    source = commands.add_parser(
        "release-source",
        help="print the path of the pinned repository whose main is origin/main, stamped",
    )
    source.add_argument("--repo", type=Path, default=REPO_ROOT)
    source.add_argument("--state", type=Path, default=None)
    stamped = commands.add_parser(
        "require-stamped", help="refuse a checkout that carries the development build"
    )
    stamped.add_argument("root", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "number":
            print(build_number(args.repo, args.commit))
        elif args.command == "require-stamped":
            builds = require_stamped(args.root)
            print(builds["android"])
        else:
            state = args.state or default_state()
            if args.command == "release-source" or args.fetch:
                _git(args.repo, "fetch", "--quiet", "origin",
                     "+refs/heads/main:refs/remotes/origin/main")
            if args.command == "release-source":
                commit = _git(args.repo, "rev-parse", MAIN)
                pin, _ = release_source(args.repo, state, commit, build_number(args.repo, commit))
                print(pin)
            else:
                if args.store:
                    require_main(args.repo, args.commit)
                path, _, _ = release_checkout(args.repo, state, args.commit)
                print(path)
    except (MobileBuildError, OSError, subprocess.CalledProcessError) as exc:
        print(f"mobile build: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
