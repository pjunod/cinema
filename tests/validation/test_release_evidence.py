"""The weekly tag reads the release commit's existing gate; it never reruns the suites.

`validation.release_evidence.verify` is the whole decision: these tests pin
that a release landed by `scripts/land` with a green promotion gate is
accepted, and that every other landing is refused, so no tag is cut.
"""

from __future__ import annotations

from pathlib import Path
import subprocess
import tempfile
import unittest

from validation import release_evidence
from validation.landing import GATE_CONTEXT

GATE = GATE_CONTEXT + " (pull_request)"


class Repo:
    def __init__(self, case: unittest.TestCase):
        directory = tempfile.TemporaryDirectory()
        case.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Evidence Test")
        self.git("config", "user.email", "evidence@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        (self.root / "CHANGELOG.md").write_text("base\n", encoding="utf-8")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "chore: base")

    def git(self, *args: str) -> str:
        return subprocess.run(("git", *args), cwd=self.root, check=True, text=True,
                              capture_output=True).stdout.strip()

    def land(self, message: str) -> tuple[str, str]:
        """(landing commit, PR head): a --no-ff merge as scripts/land makes."""

        self.git("checkout", "-q", "-b", "claude/release")
        (self.root / "CHANGELOG.md").write_text("## [1.0.0] — 2026-11-25\n", encoding="utf-8")
        self.git("commit", "-q", "-am", "release: v1.0.0")
        head = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", head, "-m", message)
        return self.git("rev-parse", "HEAD"), head


class FakeForge:
    def __init__(self, pull: dict, gate: str | None = "success"):
        self.state = pull
        self.gate = gate
        self.asked: list[str] = []

    def pull(self, number: int) -> dict:
        assert number == self.state["number"]
        return dict(self.state)

    def statuses(self, sha: str) -> list[dict]:
        self.asked.append(sha)
        rows = [{"id": 1, "context": "main fast lane / paperwork (pull_request)", "status": "success"}]
        if self.gate is not None:
            rows.append({"id": 2, "context": GATE, "status": self.gate,
                         "target_url": "/owner/repo/actions/runs/9/jobs/8"})
        return rows


SUBJECT = "Merge pull request 'release: v1.0.0' (#7) from claude/release into main"
REVIEWED = "Reviewed-on: https://forge.example/owner/repo/pulls/7"


class ReleaseEvidenceCase(unittest.TestCase):
    def setUp(self):
        self.repo = Repo(self)

    def landed(self, message: str = f"{SUBJECT}\n\n{REVIEWED}", gate: str | None = "success",
               merged: bool = True, head: str | None = None):
        sha, pr_head = self.repo.land(message)
        forge = FakeForge({"number": 7, "merged": merged, "merge_commit_sha": sha,
                           "head": {"sha": head or pr_head}}, gate=gate)
        return forge, sha, pr_head

    def refused(self, forge, sha: str) -> str:
        with self.assertRaises(release_evidence.EvidenceRefused) as caught:
            release_evidence.verify(forge, self.repo.root, sha)
        return str(caught.exception)

    def test_a_green_gated_landing_is_accepted_from_its_existing_evidence(self):
        forge, sha, head = self.landed()
        found = release_evidence.verify(forge, self.repo.root, sha)
        self.assertEqual(found, {"pr": "7", "gated_head": head, "gate": "/owner/repo/actions/runs/9/jobs/8"})
        self.assertEqual(forge.asked, [head])

    def test_a_missing_skipped_or_red_gate_is_refused(self):
        for gate in (None, "skipped", "failure", "pending"):
            with self.subTest(gate=gate):
                self.repo = Repo(self)
                forge, sha, _ = self.landed(gate=gate)
                self.assertIn("not success", self.refused(forge, sha))

    def test_an_untested_landing_is_refused_even_with_a_green_gate(self):
        forge, sha, _ = self.landed(f"{SUBJECT}\n\n{REVIEWED}\nLanded-Untested: owner ordered it")
        self.assertIn("landed untested", self.refused(forge, sha))

    def test_a_commit_scripts_land_did_not_make_is_refused(self):
        forge, sha, _ = self.landed()
        self.assertIn("parent", self.refused(forge, self.repo.git("rev-parse", "HEAD^2")))
        self.repo = Repo(self)
        forge, sha, _ = self.landed(f"{SUBJECT}\n")
        self.assertIn("Reviewed-on", self.refused(forge, sha))

    def test_the_forge_must_report_this_very_landing_from_this_head(self):
        forge, sha, _ = self.landed(merged=False)
        self.assertIn("not reported merged", self.refused(forge, sha))
        self.repo = Repo(self)
        forge, sha, _ = self.landed(head="0" * 40)
        self.assertIn("second parent", self.refused(forge, sha))

    def test_a_short_sha_is_refused(self):
        forge, sha, _ = self.landed()
        self.assertIn("full commit SHA", self.refused(forge, sha[:12]))


if __name__ == "__main__":
    unittest.main()
