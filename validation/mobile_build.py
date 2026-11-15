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

The signing controller reads the number from the two manifests of the commit
it is given, so the release job gives it a *stamp commit*: the release commit
plus one child whose only change is the number written into those manifests
(`stamp_commit`). `scripts/ship-physical` writes the same number into its
disposable worktree (`stamp_tree`), and `make android-release` passes it to
Gradle as `-Pplurx.buildNumber`.
"""

from __future__ import annotations

import argparse
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
) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args], text=True, input=data,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=None if env is None else {**os.environ, **env},
    )
    if result.returncode != 0:
        raise MobileBuildError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout if raw else result.stdout.strip()


def build_number(repo: Path, commit: str = "HEAD") -> int:
    """The store build number of `commit`: how many commits it contains."""

    if _git(repo, "rev-parse", "--is-shallow-repository") != "false":
        raise MobileBuildError(
            f"{repo} is a shallow clone, so its commit count is not the build number; "
            "fetch the full history (`git fetch --unshallow`)"
        )
    number = int(_git(repo, "rev-list", "--count", commit))
    floor = max(LAST_CLAIMED.values())
    if number <= floor:
        raise MobileBuildError(
            f"{commit} contains {number} commits, not above the last store build "
            f"({floor}); this is not the plurx history the stores have builds from"
        )
    if number > GOOGLE_PLAY_CEILING:
        raise MobileBuildError("the build number exceeds Google Play's 2100000000 ceiling")
    return number


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


def stamp_tree(root: Path, build: int) -> None:
    """Write `build` into a disposable checkout's manifests.

    For a throwaway release worktree only: the result is a modified tree, and
    committing it to a branch is what `validation.mobile_versions` refuses.
    """

    android, apple = stamp_manifests(
        (root / ANDROID).read_text(encoding="utf-8"),
        (root / APPLE).read_text(encoding="utf-8"),
        build,
    )
    (root / ANDROID).write_text(android, encoding="utf-8")
    (root / APPLE).write_text(apple, encoding="utf-8")


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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    commands = parser.add_subparsers(dest="command", required=True)
    number = commands.add_parser("number", help="print the build number of a commit")
    number.add_argument("--repo", type=Path, default=REPO_ROOT)
    number.add_argument("--commit", default="HEAD")
    tree = commands.add_parser(
        "stamp-tree", help="write a commit's build number into a disposable checkout"
    )
    tree.add_argument("root", type=Path)
    tree.add_argument("--commit", default="HEAD")
    args = parser.parse_args(argv)
    try:
        if args.command == "number":
            print(build_number(args.repo, args.commit))
        else:
            build = build_number(args.root, args.commit)
            stamp_tree(args.root, build)
            print(build)
    except (MobileBuildError, OSError) as exc:
        print(f"mobile build: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
