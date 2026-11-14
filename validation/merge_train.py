"""The fast lane's side of the merge train (docs/ci/MERGE-TRAIN.md).

`python3 -m validation.merge_train scope` reads the scope lines that
`validation.ci_scope` prints (`key=value`) on stdin and writes them back for
`$GITHUB_OUTPUT`, adding three:

* `preflight` and `worker_control`: whether those jobs run (`true` outside a probe);
* `train_selected`: `true` on a probe, where the promotion gate does not run.

A probe is a pull request the merge train opens to re-run only some jobs: its
head branch starts with `train/`, its author is the train's bot account
(`MERGE_TRAIN_BOT`, default `train-bot`), and its description has a
`Train-Jobs:` line naming fast-lane job ids. On a probe exactly the named
jobs run, each forced on whatever the diff says (a probe on `main` alone has an
empty diff, and a job must run to be judged); every other selectable job is off.
Anywhere else the scope passes through unchanged. A probe naming a job this
module cannot select fails the scope job, so the train sees its jobs skipped
and stops rather than misreading a skipped job as a pass.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import sys

# Fast-lane job id -> the scope outputs that switch it on.
SELECTABLE = {
    "preflight": ("preflight",),
    "worker_control_proof": ("worker_control",),
    "rust_compile": ("rust",),
    "rust_units": ("rust_units",),
    "web_compile": ("web_layout",),
    "apple_compile": ("apple", "desktop_macos"),
    "android_compile": ("android_jvm",),
}
SURFACES = ("rust_units", "rust", "web_layout", "apple", "desktop_macos", "android_jvm")
TRAIN_JOBS = re.compile(r"(?m)^Train-Jobs: *(.*?) *$")


def lane_enabled(event: dict, switch: str, bot: str) -> bool:
    """Forgejo expressions fold string case; the switch contract does not."""
    if switch != "on":
        return True
    pull = event.get("pull_request") or {}
    return ((pull.get("head") or {}).get("ref", "").startswith("train/")
            and (pull.get("user") or {}).get("login") == bot)


def selection(event: dict, bot: str) -> list[str] | None:
    """The jobs a probe names, or None when this run is not a probe."""
    pull = event.get("pull_request") if isinstance(event, dict) else None
    if not isinstance(pull, dict):
        return None
    head = (pull.get("head") or {}).get("ref") or ""
    author = (pull.get("user") or {}).get("login") or ""
    match = TRAIN_JOBS.search(pull.get("body") or "")
    if not (head.startswith("train/") and author == bot and match):
        return None
    return [job.strip() for job in match.group(1).split(",") if job.strip()]


def scope(lines: list[str], jobs: list[str] | None, enabled: bool = True) -> list[str]:
    values = dict(line.split("=", 1) for line in lines if "=" in line)
    # Rust tests can read files compile scope ignores; selection must see
    # every ordinary ready PR and no-op on unchanged content keys.
    values.setdefault("rust_units", "true")
    if not enabled:
        for output in SURFACES + ("preflight", "worker_control"):
            values[output] = "false"
        values.update(train_selected="false", run_lane="false")
    elif jobs is None:
        values.update(preflight="true", worker_control="true", train_selected="false", run_lane="true")
    else:
        unknown = sorted(set(jobs) - set(SELECTABLE))
        if unknown:
            raise SystemExit(f"merge-train: Train-Jobs names jobs this lane cannot select: {', '.join(unknown)}")
        wanted = {output for job in jobs for output in SELECTABLE[job]}
        for output in SURFACES + ("preflight", "worker_control"):
            values[output] = "true" if output in wanted else "false"
        values.update(docs_only="false", train_selected="true", run_lane="true")
    return [f"{key}={value}" for key, value in values.items()]


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv != ["scope"]:
        raise SystemExit("usage: python3 -m validation.merge_train scope < ci_scope-output")
    path = os.environ.get("GITHUB_EVENT_PATH")
    event = json.loads(Path(path).read_text()) if path and Path(path).is_file() else {}
    bot = os.environ.get("MERGE_TRAIN_BOT") or "train-bot"
    enabled = lane_enabled(event, os.environ.get("MERGE_TRAIN", ""), bot)
    jobs = selection(event, bot)
    if jobs is not None:
        print(f"merge-train probe: running only {', '.join(jobs) or '(nothing)'}", file=sys.stderr)
    print("\n".join(scope(sys.stdin.read().splitlines(), jobs, enabled)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
