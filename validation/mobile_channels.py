"""Select post-merge mobile releases and invoke the private signing controller."""

from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from validation.mobile_versions import (
    ANDROID_RELEASE_FILES,
    ANDROID_RELEASE_PATHS,
    APPLE_RELEASE_FILES,
    APPLE_RELEASE_PATHS,
)


# The signing controller's release guard (noirr_mobile_release_guard, kind
# "plurx") refuses a platform whose client paths changed after its last
# build-number update. Mirror its layout here so the workflow skips such a
# platform with a warning instead of starting a build the guard will refuse.
# Keep this table in step with the controller's LAYOUTS["plurx"].
_README = r"(^|/)README(?:\.[^/]*)?$"
GUARD_LAYOUT = {
    "apple": ("clients/apple/project.yml", "CURRENT_PROJECT_VERSION", "clients/apple",
              (r"(^|/)Tests?/", _README)),
    "android": ("clients/android/app/build.gradle.kts", "versionCode", "clients/android",
                (r"(^|/)src/(?:test|androidTest)/", _README)),
}


def _guard_group(platform: str) -> str:
    return "android" if platform == "android" else "apple"


def unbumped_changes(root: Path, head: str, platform: str) -> list[str]:
    """Client paths changed on main since this platform's build number landed.

    The bump is found along main's first-parent history and merge commits are
    diffed against their first parent, so a pull request whose bump commit is
    older than its later source commits still counts as bumped when it merges:
    the merge is the moment both reached main together. An empty list means
    the platform is releasable at ``head``.
    """
    metadata, pattern, client, ignored = GUARD_LAYOUT[_guard_group(platform)]
    bump = git(root, "log", "-1", "--first-parent", "--diff-merges=first-parent", "--no-patch",
               "--format=%H", "-G", pattern, head, "--", metadata)
    if not bump:
        return [metadata]
    changed = git(root, "diff", "--no-renames", "--name-only", "-z", bump, head, "--", client)
    return [path for path in changed.rstrip("\0").split("\0")
            if path and not any(re.search(rule, path) for rule in ignored)]


def releasable_platforms(root: Path, head: str, platforms: list[str]) -> list[str]:
    """Drop platforms the controller guard would refuse, saying why on stderr."""
    ready = []
    reasons: dict[str, list[str]] = {}
    for platform in platforms:
        group = _guard_group(platform)
        if group not in reasons:
            reasons[group] = unbumped_changes(root, head, platform)
        if reasons[group]:
            continue
        ready.append(platform)
    for group, paths in reasons.items():
        if paths:
            shown = ", ".join(paths[:5]) + (f" and {len(paths) - 5} more" if len(paths) > 5 else "")
            print(f"mobile channels: skipping Cinema {group}: changed after its last build-number "
                  f"update ({shown}); the next build-number bump on main publishes it.",
                  file=sys.stderr, flush=True)
    return ready


def pinned_source(source: Path, state: Path, commit: str) -> Path:
    """A bare repository whose ``main`` stays at ``commit`` for the whole build.

    The controller fetches ``origin/main`` before building and again before
    publishing, and refuses when the two differ. Pointed at the live checkout
    that comparison fails whenever anything merges during a build, which on
    this repository is most builds. Its remote is itself, so both fetches see
    the commit this job chose; objects come from the source checkout.
    """
    pin = state / "pinned-source.git"
    if not (pin / "HEAD").is_file():
        subprocess.run(["git", "init", "--bare", "--quiet", str(pin)], check=True)
    common = Path(git(source, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    (pin / "objects" / "info").mkdir(parents=True, exist_ok=True)
    (pin / "objects" / "info" / "alternates").write_text(f"{common / 'objects'}\n")
    # The pin owns no objects. Drop what an earlier run left so a replaced or
    # rewritten source clone cannot leave refs naming objects it no longer has.
    for ref in git(pin, "for-each-ref", "--format=%(refname)", "refs/remotes").splitlines():
        subprocess.run(["git", "-C", str(pin), "update-ref", "-d", ref], check=True)
    subprocess.run(["git", "-C", str(pin), "worktree", "prune"], check=True)
    subprocess.run(["git", "-C", str(pin), "config", "remote.origin.url", str(pin)], check=True)
    subprocess.run(["git", "-C", str(pin), "update-ref", "refs/heads/main", commit], check=True)
    return pin


def require_first_parent_guard(ansible: Path) -> None:
    """Refuse a controller whose guard would reject what this job selects."""
    guard = ansible / "library" / "noirr_mobile_release_guard.py"
    if guard.is_file() and "--first-parent" not in guard.read_text():
        raise ValueError(f"{guard} predates the first-parent bump rule; install a controller version "
                         "whose guard uses --first-parent --diff-merges=first-parent")


def platforms_for_paths(paths: tuple[str, ...]) -> list[str]:
    platforms = []
    if any(p in APPLE_RELEASE_FILES or p.startswith(APPLE_RELEASE_PATHS) for p in paths):
        platforms.extend(("ios", "tvos"))
    if any(p in ANDROID_RELEASE_FILES or p.startswith(ANDROID_RELEASE_PATHS) for p in paths):
        platforms.append("android")
    return platforms


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def changed_platforms(root: Path, base: str, head: str, *, initial_push: bool = False) -> list[str]:
    for revision in (base, head):
        if not re.fullmatch(r"[0-9a-f]{40}", revision):
            raise ValueError("push revisions must be full commit SHAs")
    if base == "0" * 40 and not initial_push:
        raise ValueError("recovery requires a real ancestor commit; the zero SHA is only valid for an initial push")
    if base != "0" * 40:
        subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", base, head], check=True)
    # NUL separation preserves whitespace; both old and new names of a rename
    # matter (moving a source out of the app still changes the shipped app).
    paths = (
        git(root, "ls-tree", "-r", "--name-only", "-z", head)
        if base == "0" * 40 else
        git(root, "diff", "--no-renames", "--name-only", "-z", base, head)
    )
    return platforms_for_paths(tuple(paths.rstrip("\0").split("\0")))


def scope_platforms(root: Path, base: str, head: str) -> list[str]:
    """Platforms whose release inputs changed in the push and that can publish."""
    platforms = changed_platforms(root, base, head, initial_push=os.environ.get("GITHUB_EVENT_NAME") == "push")
    return releasable_platforms(root, head, platforms)


def release(base: str, head: str) -> None:
    source = Path(os.environ.get("PLURX_MOBILE_SOURCE_REPO", "~/code/plurx")).expanduser().resolve()
    ansible = Path(os.environ.get("PLURX_ANSIBLE_DIR", "~/code/ansible/media")).expanduser().resolve()
    inventory = Path(os.environ.get("PLURX_ANSIBLE_INVENTORY", str(ansible / "inventory.yml"))).expanduser()
    state = Path(os.environ.get("PLURX_MOBILE_STATE_DIR", "~/.local/state/noirr/mobile-channels")).expanduser()
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Shares the same lock name across apps/repos on this signing controller.
    # Workflow concurrency queues this repo; the file lock also excludes other
    # automated publishers using this controller entry point.
    with (state / "publish.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise ValueError("another mobile channel release is running; retry this job") from exc
        if not (ansible / "mobile-channels.yml").is_file() or not inventory.is_file():
            raise ValueError("configure PLURX_ANSIBLE_DIR and PLURX_ANSIBLE_INVENTORY on the signing runner")
        require_first_parent_guard(ansible)
        subprocess.run(["git", "-C", str(source), "fetch", "origin",
                        "+refs/heads/main:refs/remotes/origin/main"], check=True)
        current = git(source, "rev-parse", "origin/main")
        # A queued event can publish newer main, but cannot publish an unrelated
        # repository or a history which no longer contains the triggering push.
        subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", head, current], check=True)
        platforms = changed_platforms(source, base, current, initial_push=os.environ.get("GITHUB_EVENT_NAME") == "push")
        if not platforms:
            print("No mobile release inputs changed.")
            return
        platforms = releasable_platforms(source, current, platforms)
        if not platforms:
            print("No changed mobile platform has a new build number yet.")
            return
        print(f"Publishing Cinema {current}: {', '.join(platforms)}", flush=True)
        pin = pinned_source(source, state, current)
        variables = {
            "mobile_release_channels_apps": ["cinema"],
            "mobile_release_channels_platforms": platforms,
            "mobile_release_channels_android_track": "internal",
            "mobile_release_channels_upload": True,
            "mobile_release_channels_receipts": str(state),
            "mobile_release_expected_commits": {"cinema": current},
            "mobile_release_repositories": {
                "cinema": {"source_path": str(pin), "metadata_kind": "plurx"},
            },
        }
        command = ["ansible-playbook", "-i", str(inventory), str(ansible / "mobile-channels.yml")]
        if extra := os.environ.get("PLURX_MOBILE_VARS_FILE"):
            command.extend(("-e", "@" + str(Path(extra).expanduser().resolve())))
        # Enforce app, track and real uploads after any local signing overrides.
        command.extend(("-e", json.dumps(variables)))
        subprocess.run(command, cwd=ansible.parent, check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("scope", "release"))
    parser.add_argument("--base", default=os.environ.get("MOBILE_BEFORE", ""))
    parser.add_argument("--head", default=os.environ.get("MOBILE_SHA", ""))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    try:
        if args.mode == "scope":
            platforms = scope_platforms(root, args.base, args.head)
            print(f"publish={'true' if platforms else 'false'}")
            print("platforms=" + ",".join(platforms))
        else:
            if os.environ.get("GITHUB_REF") != "refs/heads/main":
                raise ValueError("mobile channel publishing must run from refs/heads/main")
            if os.environ.get("GITHUB_EVENT_NAME") not in ("push", "workflow_dispatch"):
                raise ValueError("mobile channel publishing accepts main pushes or manual recovery only")
            if not re.fullmatch(r"[0-9a-f]{40}", args.head):
                raise ValueError("publishing requires the full triggering commit SHA")
            release(args.base, args.head)
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"mobile channels: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
