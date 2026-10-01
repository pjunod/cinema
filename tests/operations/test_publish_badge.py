from __future__ import annotations

from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


class PublishBadgeCase(unittest.TestCase):
    def test_coverage_publication_refreshes_value_and_date_and_preserves_on_invalid_input(self):
        workflow = (ROOT / ".github/workflows/coverage.yml").read_text()
        step = workflow.split("      - name: Publish the unit coverage badge\n", 1)[1]
        command = textwrap.dedent(step.split("        run: |\n", 1)[1].split("      - name:", 1)[0])
        env = dict(os.environ)
        env.pop("GITHUB_TOKEN", None)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            remote = directory / "remote.git"
            source = directory / "source"
            subprocess.run(["git", "init", "--bare", "-q", remote], check=True)
            subprocess.run(["git", "init", "-q", source], check=True)
            subprocess.run(["git", "-C", source, "remote", "add", "origin", remote], check=True)
            (source / "scripts").symlink_to(ROOT / "scripts", target_is_directory=True)

            def git(*args):
                return subprocess.check_output(
                    ["git", f"--git-dir={remote}", *args], text=True
                ).strip()

            def publish(value):
                (source / "cov.json").write_text(json.dumps({
                    "data": [{"totals": {"lines": {"percent": value}}}]
                }))
                return subprocess.run(
                    ["bash", "-c", command], cwd=source, env=env,
                    capture_output=True, text=True,
                )

            # Execute the actual workflow shell and publisher against a bare
            # remote: both the value and visible date must survive publication.
            for percent, color in ((42.14, "yellow"), (91.25, "brightgreen"), (0, "red")):
                with self.subTest(percent=percent):
                    before = datetime.now(timezone.utc).date().isoformat()
                    result = publish(percent)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    after = datetime.now(timezone.utc).date().isoformat()
                    endpoint = json.loads(git("show", "badges:coverage.json"))
                    self.assertIn(endpoint["message"], {
                        f"{percent:.1f}% · {before} UTC",
                        f"{percent:.1f}% · {after} UTC",
                    })
                    self.assertEqual(endpoint["color"], color)
                    svg = git("show", "badges:coverage.svg")
                    self.assertIn(endpoint["message"], svg)
                    self.assertEqual(git("rev-parse", "badges"), git("rev-parse", "codex/badges"))

            last_good = git("rev-parse", "badges")
            for invalid in (None, "83.6", -1, 101):
                with self.subTest(invalid=invalid):
                    result = publish(invalid)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(git("rev-parse", "badges"), last_good)
            # A push failure must fail the workflow instead of silently leaving
            # the previous percentage on a green run.
            subprocess.run(
                ["git", "-C", source, "remote", "set-url", "origin", directory / "missing.git"],
                check=True,
            )
            self.assertNotEqual(publish(85).returncode, 0)
            self.assertEqual(git("rev-parse", "badges"), last_good)

    def test_publishes_renderable_svg_json_and_preview_alias(self):
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            remote = temporary_path / "remote.git"
            source = temporary_path / "source"
            subprocess.run(["git", "init", "--bare", "-q", remote], check=True)
            subprocess.run(["git", "init", "-q", source], check=True)
            subprocess.run(
                ["git", "-C", source, "remote", "add", "origin", remote],
                check=True,
            )

            subprocess.run(
                [
                    ROOT / "scripts/publish-badge",
                    "--branch",
                    "badges-ci",
                    "--alias",
                    "codex/badges-ci",
                    "--filename",
                    "ci",
                    "--label",
                    "ci & gate",
                    "--message",
                    "passing",
                    "--fill",
                    "#4c1",
                    "--json-color",
                    "brightgreen",
                    "--label-width",
                    "50",
                    "--message-width",
                    "48",
                ],
                cwd=source,
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )

            svg = subprocess.run(
                ["git", f"--git-dir={remote}", "show", "badges-ci:ci.svg"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout
            endpoint = subprocess.run(
                ["git", f"--git-dir={remote}", "show", "badges-ci:ci.json"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout
            canonical = subprocess.run(
                ["git", f"--git-dir={remote}", "rev-parse", "badges-ci"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout
            alias = subprocess.run(
                ["git", f"--git-dir={remote}", "rev-parse", "codex/badges-ci"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout

        self.assertIn('aria-label="ci &amp; gate: passing"', svg)
        self.assertEqual(
            json.loads(endpoint),
            {
                "schemaVersion": 1,
                "label": "ci & gate",
                "message": "passing",
                "color": "brightgreen",
            },
        )
        self.assertEqual(canonical, alias)


if __name__ == "__main__":
    unittest.main()
