from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class PublishBadgeCase(unittest.TestCase):
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
