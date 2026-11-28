"""Whether a release commit already passed CI, read from the evidence its landing left.

The weekly release tag (docs/RELEASING.md "The weekly release tag") goes on
the release commit: the landing commit of the `scripts/release-cut` pull
request. `scripts/land` only lands a head whose `main fast lane / Main
promotion gate` is green, or records why not in a `Landed-Untested:` trailer.
So the question "is this release tested?" is answered by that landing, not by
running the suites again on the same code:

* the release commit is a two-parent landing whose message carries exactly one
  `Reviewed-on: .../pulls/<N>` line and no `Landed-Untested:` trailer;
* the forge reports pull request N merged as this very commit, from a head
  equal to the commit's second parent;
* that head carries a green promotion gate (`validation.landing.gate_state`,
  the same reading `scripts/land` makes).

Anything else -- a direct push, an untested landing, a landing admitted by a
manual source gate (which creates no gate status), a gate that is missing,
skipped or red -- is refused, and no tag is cut.

    python3 -m validation.release_evidence --sha <release commit>

prints `pr=`, `gated_head=` and `gate=` lines (for `$GITHUB_OUTPUT`) and exits
0, or names the refusal and exits 1; 2 is a usage or forge error. The forge is
`$GITHUB_SERVER_URL`/`$GITHUB_REPOSITORY` with `$GITHUB_TOKEN` inside Actions,
otherwise the `origin` remote and `$FORGEJO_TOKEN`.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

from validation.landing import (
    FULL_SHA_RE,
    GATE_CONTEXT,
    UNTESTED_TRAILER,
    Forge,
    ForgeError,
    _origin,
    gate_state,
)
from validation.runner import REPO_ROOT

REVIEWED_ON = re.compile(r"^Reviewed-on: \S+/pulls/(\d+)\s*$")


class EvidenceRefused(Exception):
    """The release commit has no green CI evidence; it must not be tagged."""


def _git(root: Path, *args: str) -> str:
    done = subprocess.run(("git", *args), cwd=root, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        raise EvidenceRefused(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


def verify(forge, root: Path, sha: str) -> dict[str, str]:
    """The green gate `sha` landed with: {pr, gated_head, gate}; EvidenceRefused otherwise."""

    if not FULL_SHA_RE.fullmatch(sha):
        raise EvidenceRefused(f"{sha!r} is not a full commit SHA")
    parents = _git(root, "rev-list", "--parents", "-n", "1", sha).split()[1:]
    if len(parents) != 2:
        raise EvidenceRefused(
            f"release commit {sha[:12]} has {len(parents)} parent(s); a release is the two-parent "
            "landing commit scripts/land makes"
        )
    head = parents[1]
    message = _git(root, "show", "-s", "--format=%B", sha)
    lines = [line.strip() for line in message.splitlines()]
    untested = [line for line in lines if line.startswith(UNTESTED_TRAILER + ":")]
    if untested:
        raise EvidenceRefused(f"release commit {sha[:12]} was landed untested ({untested[0]})")
    numbers = [int(found.group(1)) for found in map(REVIEWED_ON.match, lines) if found]
    if len(numbers) != 1:
        raise EvidenceRefused(
            f"release commit {sha[:12]} carries {len(numbers)} Reviewed-on pull request line(s), not one"
        )
    number = numbers[0]
    pull = forge.pull(number) or {}
    if not pull.get("merged") or pull.get("merge_commit_sha") != sha:
        raise EvidenceRefused(
            f"#{number} is not reported merged as {sha[:12]} "
            f"(merged={pull.get('merged')}, merge commit {pull.get('merge_commit_sha')})"
        )
    reported = str((pull.get("head") or {}).get("sha") or "")
    if reported != head:
        raise EvidenceRefused(
            f"#{number}'s head {reported[:12] or 'unknown'} is not the landing's second parent {head[:12]}"
        )
    state, url = gate_state(forge.statuses(head))
    if state != "success":
        raise EvidenceRefused(
            f"#{number} head {head[:12]}: '{GATE_CONTEXT}' is {state}, not success"
            + (f" ({url})" if url else "")
        )
    return {"pr": str(number), "gated_head": head, "gate": url}


def _forge(root: Path) -> Forge:
    server, repo = os.environ.get("GITHUB_SERVER_URL"), os.environ.get("GITHUB_REPOSITORY")
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("FORGEJO_TOKEN")
    if not (server and repo):
        origin = _origin(root)
        if origin is None:
            raise ForgeError(0, "no forge: set GITHUB_SERVER_URL and GITHUB_REPOSITORY, or an origin remote")
        server, repo = origin
    return Forge(server, repo, token)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--sha", required=True, help="the release commit (scripts/release-cut --pending's sha)")
    args = parser.parse_args(argv)
    root = REPO_ROOT
    try:
        found = verify(_forge(root), root, args.sha)
    except EvidenceRefused as refused:
        print(f"release-evidence: refused: {refused}. No tag.", file=sys.stderr)
        return 1
    except ForgeError as error:
        print(f"release-evidence: forge error {error}: {error.body[:300]}", file=sys.stderr)
        return 2
    for key, value in found.items():
        print(f"{key}={value}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
