"""`scripts/ci-require-disk` says the true thing before the linker lies.

A self-hosted runner out of disk does not report running out of disk. It
reports `ld terminated with signal 7 [Bus error]`, because the linker was
mid-write when the filesystem filled and a truncated write surfaces as SIGBUS.
That is indistinguishable at a glance from a toolchain fault, and the real
cause appears far later as a single `No space left on device`.

These tests pin the two things that make the preflight worth having: it names
the runner, and it deletes nothing.
"""

from __future__ import annotations

import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ci-require-disk"


def run(path: Path, required_gb: str, runner: str = "gha-test-01") -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(SCRIPT), str(path), required_gb],
        cwd=ROOT,
        env={"PATH": "/usr/bin:/bin", "RUNNER_NAME": runner},
        capture_output=True,
        text=True,
        timeout=60,
    )


class RequireDiskCase(unittest.TestCase):
    def test_enough_space_passes_quietly(self) -> None:
        """A preflight that is loud when nothing is wrong gets removed."""
        result = run(ROOT, "1")
        self.assertEqual(result.returncode, 0)
        self.assertIn("available", result.stdout)

    def test_too_little_space_fails_and_names_the_runner(self) -> None:
        """"A runner is full" is not actionable; "this one is" is.

        On a fleet of eighteen, the job log is the only place that mapping
        survives — the failure itself carries no hostname at all.
        """
        result = run(ROOT, "999999", runner="gha-rogg16-general-03")
        self.assertEqual(result.returncode, 1)
        self.assertIn("gha-rogg16-general-03", result.stderr)
        self.assertIn("out of disk", result.stderr)

    def test_it_explains_the_symptom_it_is_pre_empting(self) -> None:
        """The message has to name the lie it is replacing.

        Whoever reads this has probably just seen a bus error in another job
        and needs to connect the two.
        """
        result = run(ROOT, "999999")
        self.assertIn("signal 7", result.stderr)
        self.assertIn("looks like a toolchain fault and is not", result.stderr)

    def test_it_deletes_nothing(self) -> None:
        """Structural: the work root holds other jobs' checkouts.

        A cleanup that guesses which trees are finished is how a concurrent job
        loses its checkout mid-run, so this script must stay diagnostic. That
        is a property of the text, not of one execution.
        """
        body = SCRIPT.read_text(encoding="utf-8")
        for destructive in ("rm ", "rm -", "rmdir", "unlink", "truncate", "find -delete", "-exec rm"):
            for line in body.splitlines():
                stripped = line.strip()
                if stripped.startswith("#"):
                    continue
                self.assertNotIn(
                    destructive,
                    stripped,
                    f"the preflight must stay read-only; found {destructive!r}",
                )

    def test_a_missing_directory_is_an_error_not_a_pass(self) -> None:
        """Failing open would defeat the point.

        A preflight that treats "I could not check" as "fine" hands back the
        bus error it exists to prevent.
        """
        result = run(ROOT / "definitely-not-here", "1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no such directory", result.stderr)


if __name__ == "__main__":
    unittest.main()
