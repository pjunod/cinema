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
TRAIN_TESTS = re.compile(r"(?m)^Train-Tests: *(.*?) *$")
# `<binary id> <test path>`: binary ids look like `plurxd::bin/plurxd`; one space.
TEST_ID = re.compile(r"[A-Za-z0-9_-]+(?:::[A-Za-z0-9_./-]+)* [A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*")


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


def train_tests(event: dict, bot: str) -> list[str] | None:
    """The unit test ids a probe names, or None when it names none.

    Same authority as `selection`: only the bot's `train/` heads are believed.
    """
    pull = event.get("pull_request") if isinstance(event, dict) else None
    if not isinstance(pull, dict):
        return None
    head = (pull.get("head") or {}).get("ref") or ""
    author = (pull.get("user") or {}).get("login") or ""
    matches = TRAIN_TESTS.findall(pull.get("body") or "")
    if not (head.startswith("train/") and author == bot and matches):
        return None
    ids = [item.strip() for line in matches for item in line.split(",") if item.strip()]
    if not ids:
        raise SystemExit("merge-train: Train-Tests names no tests")
    bad = [item for item in ids if not TEST_ID.fullmatch(item)]
    if bad:
        raise SystemExit(f"merge-train: Train-Tests ids must read `<binary id> <test path>`: {', '.join(bad)}")
    return sorted(set(ids))


def scope(lines: list[str], jobs: list[str] | None, enabled: bool = True,
          tests: list[str] | None = None) -> list[str]:
    values = dict(line.split("=", 1) for line in lines if "=" in line)
    # Rust tests can read files compile scope ignores; selection must see
    # every ordinary ready PR and no-op on unchanged content keys.
    values.setdefault("rust_units", "true")
    values["train_tests"] = ""
    if tests and (jobs is None or "rust_units" not in jobs):
        raise SystemExit("merge-train: Train-Tests needs `rust_units` in Train-Jobs")
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
        values.update(docs_only="false", train_selected="true", run_lane="true",
                      train_tests=",".join(tests or []))
    return [f"{key}={value}" for key, value in values.items()]



def authenticated_manual_scope(lines, event, jobs, enabled, tests):
    """Explicit claims require real completed CI evidence, never a label waiver."""
    values = scope(lines, jobs, enabled, tests)
    pull = event.get('pull_request') or {}
    claims = [line for line in (pull.get('body') or '').splitlines() if line.startswith('Manual-Source-Gate:')]
    if not claims:
        return values
    from validation import python_unit_receipts as receipts
    receipts.require(len(claims) == 1 and jobs is None and not tests,
                     'Ambiguous manual source gate or merge-train probe combination')
    match = re.fullmatch(r'Manual-Source-Gate: ([1-9][0-9]*)', claims[0])
    receipts.require(match is not None, 'Malformed explicit manual source-gate claim')
    proof = authenticate_manual_source(event, int(match[1]))
    output = dict(line.split('=', 1) for line in values)
    for surface in SURFACES + ('preflight', 'worker_control'):
        output[surface] = 'false'
    output.update(run_lane='false', train_selected='false', train_tests='')
    print(f"Reused actual ci / Main promotion gate run {proof['run']} job {proof['job']}: {proof['url']}", file=sys.stderr)
    return [key + '=' + value for key, value in output.items()]


def authenticate_manual_source(event, run_id):
    from validation import python_unit_receipts as receipts
    from validation import promotion_unit_retention as retention
    from validation import source_gate_landing
    pull = event['pull_request']
    number, head = receipts.positive(event['number']), receipts.sha(os.environ['GITHUB_SHA'])
    receipts.require(pull['number'] == number and pull['head']['sha'] == head
                     and event['repository']['full_name'] == os.environ['GITHUB_REPOSITORY']
                     and pull['head']['repo']['full_name'] == os.environ['GITHUB_REPOSITORY']
                     and pull['base']['ref'] == 'main', 'Manual source-gate event identity differs')
    api = retention.runner_api(os.environ)
    main_tip = api.get('/branches/main')['commit']['id']
    return source_gate_landing.verify(api, number, head, main_tip, Path.cwd(), run_id)


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv != ["scope"]:
        raise SystemExit("usage: python3 -m validation.merge_train scope < ci_scope-output")
    path = os.environ.get("GITHUB_EVENT_PATH")
    event = json.loads(Path(path).read_text()) if path and Path(path).is_file() else {}
    bot = os.environ.get("MERGE_TRAIN_BOT") or "train-bot"
    enabled = lane_enabled(event, os.environ.get("MERGE_TRAIN", ""), bot)
    jobs = selection(event, bot)
    tests = train_tests(event, bot)
    if jobs is not None:
        print(f"merge-train probe: running only {', '.join(jobs) or '(nothing)'}", file=sys.stderr)
    if tests:
        print(f"merge-train probe: running only {len(tests)} named Rust unit test(s)", file=sys.stderr)
    print("\n".join(authenticated_manual_scope(sys.stdin.read().splitlines(), event, jobs, enabled, tests)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
