"""No status page may call a pull request open that this repository merged.

The status pages are prose, and prose about pull requests goes stale silently:
a milestone lands, its merge commit is on `main`, and the sentence saying it is
"OPEN, awaiting Paul's merge" keeps reading as fact for as long as nobody
happens to look. That is not hypothetical -- on 2026-09-02 three separate
merged pull requests were described as open or under review across STATUS.md
and docs/STATUS.html, and a reader planning the next milestone acted on it.

The check deliberately does NOT ask GitHub. A test that needs the network and
a credential cannot run in the gate that matters, and a status page's honesty
is not a fact about a remote service. It asks the repository's own history
instead: a merge commit for `#N` is proof that `#N` merged, and `main` is the
only place that proof needs to come from.

So the rule is one-directional and cheap. If the history contains
`Merge pull request #N`, then no status page may pair `#N` with a word that
claims it is still in flight. The reverse -- a pull request with no merge
commit -- is not an error here: it may be genuinely open, it may have been
squashed, and this test refuses to guess.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]

STATUS_PAGES = (
    "STATUS.md",
    "docs/STATUS.html",
    "docs/PLAYBACK-CONTROL-STATUS.md",
)

# Phrases that assert a pull request has not landed. Kept narrow on purpose,
# twice over. A milestone may honestly still be "building" while every pull
# request that built it so far is merged, so only claims about the *pull
# request* count -- and a bare lowercase "open" is ordinary English ("a single
# open", "open into main"), so it only counts when something makes it a status.
IN_FLIGHT = re.compile(
    r"\bOPEN\b"  # shouted, which on these pages is always a status
    r"|(?:is|still|remains|currently)\s+open\b"
    r"|\bopen\s*(?:,|--|—|\.|$)"
    r"|\bawaiting\s+(?:\w+\s+){0,3}merge\b"
    r"|\bunder\s+review\b"
    r"|\bnot\s+yet\s+merged\b"
    r"|\bunmerged\b"
)

PR_REFERENCE = re.compile(r"(?:pull/|PR\s+#|#)(\d{2,5})\b")

# How near a claim has to sit to the pull request it is about. A status line
# can name a dozen merged pull requests and then say something in-flight about
# a thirteenth; pairing every number on the line with every phrase on it would
# fail that honest sentence. Sixty characters is about a clause.
CLAIM_DISTANCE = 60


def merged_pull_requests() -> frozenset[int]:
    """Every pull request `main` carries a merge commit for."""
    subjects = subprocess.run(
        ["git", "log", "--format=%s", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return frozenset(
        int(number)
        for number in re.findall(r"^Merge pull request #(\d+)", subjects, re.MULTILINE)
    )


class StatusPullRequestClaimCase(unittest.TestCase):
    def test_no_status_page_calls_a_merged_pull_request_open(self) -> None:
        merged = merged_pull_requests()
        # A repository with no merge commits at all would pass this vacuously,
        # which would be the check quietly doing nothing.
        self.assertGreater(
            len(merged), 50, "history should carry many merge commits to compare against"
        )

        stale: list[str] = []
        for page in STATUS_PAGES:
            path = ROOT / page
            if not path.exists():
                continue
            for number, line in enumerate(
                path.read_text(encoding="utf-8").splitlines(), start=1
            ):
                claims = list(IN_FLIGHT.finditer(line))
                if not claims:
                    continue
                for reference in PR_REFERENCE.finditer(line):
                    pull = int(reference.group(1))
                    if pull not in merged:
                        continue
                    near = [
                        claim
                        for claim in claims
                        if abs(claim.start() - reference.start()) <= CLAIM_DISTANCE
                    ]
                    if near:
                        stale.append(
                            f"{page}:{number} calls merged #{pull} "
                            f"{near[0].group(0).strip()!r}"
                        )

        self.assertEqual(
            stale,
            [],
            "a status page describes a merged pull request as still in flight:\n  "
            + "\n  ".join(stale),
        )


if __name__ == "__main__":
    unittest.main()
