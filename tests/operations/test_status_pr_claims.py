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
instead: a commit that landed `#N` is proof that `#N` merged, and `main` is the
only place that proof needs to come from.

Two commit shapes count, because this repository has both. 490 subjects read
`Merge pull request #N` and 68 read `title (#N)` from the period when work was
squashed -- reading only the first shape would leave 68 merged pull requests
invisible to the check, which is the quiet way a guard becomes decoration.

So the rule is one-directional and cheap. If the history says `#N` landed, then
no status page may pair `#N` with a phrase claiming it is still in flight. The
reverse -- a pull request with no landing commit -- is not an error here: it may
be genuinely open, and this test refuses to guess.

Claims are matched against joined paragraphs rather than single lines. These
pages wrap prose at about 80 columns, and both real defects that prompted this
check were wrapped: `-- OPEN, awaiting Paul's` ended one line and `merge` began
the next. A line-by-line scan happened to catch those on the shouted `OPEN`
alone, which is luck rather than a check.
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
    "docs/playback-control/PLAYBACK-CONTROL-STATUS.md",
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
    # `open against \`main\`` is how these pages say it, and it was invisible
    # to every alternative above: the word is followed by "against", not by a
    # comma, a dash or an end of line, and nothing turns it into a shouted or
    # copular status. `STATUS.md` used the phrase twice, once honestly about a
    # branch and once about pull request #37, which had merged as `c661d387`
    # weeks earlier. The guard passed. A guard that cannot see the wording its
    # own pages use is decoration.
    r"|\bopen\s+against\b"
    # `[^.]` rather than `\w+` between the two words: the sentence this check
    # exists for was "awaiting Paul's merge", and an apostrophe is not `\w`.
    r"|\bawaiting\b[^.]{0,40}?\bmerge\b"
    r"|\bawaiting\s+review\b"
    r"|\bunder\s+review\b"
    r"|\bnot\s+(?:yet\s+)?merged\b"
    r"|\bunmerged\b"
    r"|\bstill\s+to\s+merge\b"
    r"|\bto\s+be\s+merged\b"
    r"|\bready\s+to\s+merge\b"
    r"|\bmerge\s+pending\b|\bpending\s+merge\b"
)

PR_REFERENCE = re.compile(r"(?:pull/|PR\s+#|#)(\d{2,5})\b")

# How near a claim has to sit to the pull request it is about. A status line
# can name a dozen merged pull requests and then say something in-flight about
# a thirteenth; pairing every number on the line with every phrase on it would
# fail that honest sentence. Sixty characters is about a clause.
CLAIM_DISTANCE = 60


LANDED = (
    re.compile(r"^Merge pull request #(\d+)", re.MULTILINE),
    re.compile(r"\(#(\d+)\)$", re.MULTILINE),
)


def history_is_shallow() -> bool:
    return (
        subprocess.run(
            ["git", "rev-parse", "--is-shallow-repository"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        == "true"
    )


def landed_pull_requests() -> frozenset[int]:
    """Every pull request this history carries a landing commit for."""
    subjects = subprocess.run(
        ["git", "log", "--format=%s", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return frozenset(
        int(number)
        for pattern in LANDED
        for number in pattern.findall(subjects)
    )


def paragraphs(text: str) -> list[tuple[int, str]]:
    """Consecutive non-blank lines joined, each tagged with its first line.

    A claim and the pull request it is about routinely sit on either side of a
    wrap, so the unit of matching has to be the paragraph the author wrote
    rather than the line the formatter produced.
    """
    joined: list[tuple[int, str]] = []
    start = 0
    buffer: list[str] = []
    for number, line in enumerate(text.splitlines(), start=1):
        if line.strip():
            if not buffer:
                start = number
            buffer.append(line.strip())
            continue
        if buffer:
            joined.append((start, " ".join(buffer)))
            buffer = []
    if buffer:
        joined.append((start, " ".join(buffer)))
    return joined


class StatusPullRequestClaimCase(unittest.TestCase):
    def test_no_status_page_calls_a_merged_pull_request_open(self) -> None:
        # A clone without history cannot answer the question. Skipping says so;
        # failing would blame the page for the checkout, and passing would be
        # the check quietly doing nothing.
        if history_is_shallow():
            self.skipTest("shallow clone: no landing commits to compare against")
        merged = landed_pull_requests()
        self.assertGreater(
            len(merged),
            50,
            "history should carry many landing commits to compare against",
        )

        stale: list[str] = []
        for page in STATUS_PAGES:
            path = ROOT / page
            if not path.exists():
                continue
            for number, line in paragraphs(path.read_text(encoding="utf-8")):
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


    def test_open_against_a_branch_is_an_in_flight_claim(self) -> None:
        """The wording the pages actually use, which the guard used to miss.

        `**PR [#37](…/pulls/37) — open against `main`.**` sat on `STATUS.md`
        for weeks after #37 landed as `c661d387`. None of the other
        alternatives match it: "open" is followed by "against", so the
        punctuation branch cannot fire, and nothing makes it copular or
        shouted. The regression is this phrase, not the page.
        """
        self.assertTrue(IN_FLIGHT.search("PR #37 — open against `main`."))
        self.assertTrue(IN_FLIGHT.search("open against main"))
        # This alternative is deliberately broader than the others: it will
        # also fire on ordinary English like "a valve held open against the
        # line". That costs nothing, because a match is only ever reported
        # when a *merged* `#N` sits within CLAIM_DISTANCE of it — the test
        # below pins that, and it is the only thing making the width safe.

    def test_a_branch_that_is_honestly_open_is_not_a_finding(self) -> None:
        """The guard stays one-directional after the addition.

        `STATUS.md` also says a *branch* is "open against `main`" with no
        pull request number beside it. The phrase now matches, so the only
        thing keeping that honest sentence out of the failure list is the
        rule that a claim needs a merged `#N` within `CLAIM_DISTANCE` of it.
        This pins that rule, because widening `IN_FLIGHT` without it would
        turn every truthful in-flight line into a failure.
        """
        line = "**Branch `fix/ci-runner-disk` — open against `main`.** Runners"
        self.assertTrue(IN_FLIGHT.search(line))
        self.assertEqual(list(PR_REFERENCE.finditer(line)), [])


if __name__ == "__main__":
    unittest.main()
