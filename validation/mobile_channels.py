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


def platforms_for_paths(paths: tuple[str, ...]) -> list[str]:
    platforms = []
    if any(p in APPLE_RELEASE_FILES or p.startswith(APPLE_RELEASE_PATHS) for p in paths):
        platforms.extend(("ios", "tvos"))
    if any(p in ANDROID_RELEASE_FILES or p.startswith(ANDROID_RELEASE_PATHS) for p in paths):
        platforms.append("android")
    return platforms


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def changed_platforms(root: Path, base: str, head: str) -> list[str]:
    for revision in (base, head):
        if not re.fullmatch(r"[0-9a-f]{40}", revision):
            raise ValueError("push revisions must be full commit SHAs")
    if base == "0" * 40:
        return ["ios", "tvos", "android"]
    # NUL separation preserves whitespace; both old and new names of a rename
    # matter (moving a source out of the app still changes the shipped app).
    paths = git(root, "diff", "--no-renames", "--name-only", "-z", base, head)
    return platforms_for_paths(tuple(paths.rstrip("\0").split("\0")))


def release(base: str, head: str, *, all_platforms: bool = False) -> None:
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
        subprocess.run(["git", "-C", str(source), "fetch", "origin",
                        "+refs/heads/main:refs/remotes/origin/main"], check=True)
        current = git(source, "rev-parse", "origin/main")
        # A queued event can publish newer main, but cannot publish an unrelated
        # repository or a history which no longer contains the triggering push.
        subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", head, current], check=True)
        platforms = ["ios", "tvos", "android"] if all_platforms else changed_platforms(source, base, current)
        if not platforms:
            print("No mobile release inputs changed.")
            return
        print(f"Publishing Cinema {current}: {', '.join(platforms)}", flush=True)
        variables = {
            "mobile_release_channels_apps": ["cinema"],
            "mobile_release_channels_platforms": platforms,
            "mobile_release_channels_android_track": "internal",
            "mobile_release_channels_upload": True,
            "mobile_release_channels_receipts": str(state),
            "mobile_release_repositories": {
                "cinema": {"source_path": str(source), "metadata_kind": "plurx"},
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
    parser.add_argument("--all", action="store_true", dest="all_platforms")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    try:
        if args.mode == "scope":
            platforms = ["ios", "tvos", "android"] if args.all_platforms else changed_platforms(root, args.base, args.head)
            print(f"publish={'true' if platforms else 'false'}")
            print("platforms=" + ",".join(platforms))
        else:
            if os.environ.get("GITHUB_REF") != "refs/heads/main":
                raise ValueError("mobile channel publishing must run from refs/heads/main")
            if os.environ.get("GITHUB_EVENT_NAME") not in ("push", "workflow_dispatch"):
                raise ValueError("mobile channel publishing accepts main pushes or manual recovery only")
            if not re.fullmatch(r"[0-9a-f]{40}", args.head):
                raise ValueError("publishing requires the full triggering commit SHA")
            release(args.base, args.head, all_platforms=args.all_platforms)
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"mobile channels: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
