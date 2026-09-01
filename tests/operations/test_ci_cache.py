from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def duplicate_mapping_keys(path: Path) -> list[tuple[int, str]]:
    """Find repeated plain keys in the GitHub YAML subset used by this repo."""
    contexts: list[tuple[int, set[str]]] = []
    duplicate_keys: list[tuple[int, str]] = []
    block_scalar_indent: int | None = None

    for line_number, raw_line in enumerate(
        path.read_text(encoding="utf-8").splitlines(), start=1
    ):
        if not raw_line.strip() or raw_line.lstrip().startswith("#"):
            continue
        indent = len(raw_line) - len(raw_line.lstrip(" "))
        if block_scalar_indent is not None:
            if indent > block_scalar_indent:
                continue
            block_scalar_indent = None
        if raw_line.strip() == "---":
            contexts.clear()
            continue

        content = raw_line[indent:]
        sequence_item = content.startswith("- ")
        if sequence_item:
            content = content[2:]
            contexts[:] = [context for context in contexts if context[0] <= indent]
            mapping_indent = indent + 2
            contexts.append((mapping_indent, set()))
        else:
            mapping_indent = indent
            contexts[:] = [
                context for context in contexts if context[0] <= mapping_indent
            ]
            if not contexts or contexts[-1][0] != mapping_indent:
                contexts.append((mapping_indent, set()))

        match = re.match(r"([A-Za-z0-9_-]+):(?:\s|$)", content)
        if match is None:
            continue
        key = match.group(1)
        seen = contexts[-1][1]
        if key in seen:
            duplicate_keys.append((line_number, key))
        seen.add(key)
        value = content[match.end() :].strip()
        if value.startswith(("|", ">")):
            block_scalar_indent = indent

    return duplicate_keys


class CiCacheContractCase(unittest.TestCase):
    def test_github_yaml_has_no_duplicate_mapping_keys(self):
        paths = sorted((ROOT / ".github/workflows").glob("*.y*ml"))
        paths.extend(sorted((ROOT / ".github/actions").glob("*/action.y*ml")))
        failures = {
            str(path.relative_to(ROOT)): duplicates
            for path in paths
            if (duplicates := duplicate_mapping_keys(path))
        }
        self.assertEqual(failures, {})

    def test_cargo_cache_is_local_only_on_persistent_runners(self):
        action = (ROOT / ".github/actions/cargo-cache/action.yml").read_text(
            encoding="utf-8"
        )
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

        self.assertIn('"${RUNNER_ENVIRONMENT:-}" = github-hosted', action)
        self.assertIn('"$EXECUTION_MODE" != shadow', action)
        self.assertIn("$RUNNER_TOOL_CACHE/plurx-ci/cargo/", action)
        self.assertIn("toolchain=$(rustc -Vv", action)
        self.assertIn("CARGO_INCREMENTAL=0", action)
        self.assertIn("uses: Swatinem/rust-cache@v2", action)
        self.assertNotIn("uses: Swatinem/rust-cache@v2", workflow)
        self.assertIn("needs.scope.outputs.execution_mode", workflow)
        for lane in (
            "rust-gate",
            "cluster-auth",
            "cluster-wal",
            "cluster-daemon",
            "release-${{ matrix.target }}",
        ):
            self.assertIn(f"lane: {lane}", workflow)

    def test_cache_pruner_is_bounded_to_the_runner_tool_cache(self):
        script = ROOT / "scripts/ci-cache-prune"
        subprocess.run(["bash", "-n", str(script)], check=True)
        with tempfile.TemporaryDirectory() as raw_directory:
            tool_cache = Path(raw_directory)
            cache_root = (
                tool_cache / "plurx-ci/cargo/runner-01/rust-1.97.1"
            )
            target = cache_root / "rust-gate/target"
            target.mkdir(parents=True)
            (target / "proof").write_bytes(b"cache")
            summary = tool_cache / "summary.md"
            environment = os.environ.copy()
            environment.update(
                {
                    "GITHUB_STEP_SUMMARY": str(summary),
                    "RUNNER_TOOL_CACHE": str(tool_cache),
                }
            )

            subprocess.run(
                [str(script), str(cache_root), "1"],
                check=True,
                env=environment,
            )
            rejected = subprocess.run(
                [str(script), str(tool_cache / "unrelated"), "1"],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertTrue(target.is_dir())
            self.assertIn("prune decision: `within-budget`", summary.read_text())
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("refusing unsafe cache root", rejected.stderr)

    def test_buildkit_state_is_named_bounded_and_registry_aware(self):
        action = (ROOT / ".github/actions/buildx-cache/action.yml").read_text(
            encoding="utf-8"
        )
        config = (ROOT / ".github/buildkitd.toml").read_text(encoding="utf-8")
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

        self.assertIn("keep-state: true", action)
        self.assertIn('"$EXECUTION_MODE" != shadow', action)
        self.assertIn("plurx-$runner_name-$CACHE_LANE", action)
        self.assertIn("buildkitd-config:", action)
        self.assertIn('[registry."192.168.4.7:3000"]', config)
        self.assertIn("http = true", config)
        self.assertIn("--max-used-space 50GB --min-free-space 100GB", workflow)
        self.assertIn("'type=gha,mode=min' || ''", workflow)

    def test_execution_mode_is_explicit_and_fails_safe_to_legacy(self):
        script = ROOT / "scripts/ci-execution-mode"
        self.assertTrue(script.stat().st_mode & 0o111)
        help_result = subprocess.run(
            [str(script), "--help"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertIn("legacy|shadow|accelerated", help_result.stdout)
        for mode in ("legacy", "shadow", "accelerated"):
            resolved = subprocess.run(
                [str(script), "resolve", mode],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(resolved.stdout.strip(), mode)
        invalid = subprocess.run(
            [str(script), "resolve", "surprise"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(invalid.stdout.strip(), "legacy")
        self.assertIn("::warning::Unknown CI_EXECUTION_MODE", invalid.stderr)

        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        effort = (ROOT / ".github/workflows/effort-ci.yml").read_text(
            encoding="utf-8"
        )
        for text in (workflow, effort):
            self.assertIn("vars.CI_EXECUTION_MODE || 'legacy'", text)
            self.assertIn("scripts/ci-execution-mode resolve", text)


if __name__ == "__main__":
    unittest.main()
