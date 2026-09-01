from __future__ import annotations

import os
from pathlib import Path
import re
import runpy
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
        self.assertIn('"$PERSISTENT_ELIGIBLE" != true', action)
        self.assertIn('default: "false"', action)
        self.assertIn("$RUNNER_TOOL_CACHE/plurx-ci/cargo/", action)
        self.assertIn("toolchain=$(rustc -Vv", action)
        self.assertIn("CARGO_INCREMENTAL=0", action)
        self.assertIn("uses: Swatinem/rust-cache@v2", action)
        self.assertNotIn("uses: Swatinem/rust-cache@v2", workflow)
        self.assertIn("needs.scope.outputs.execution_mode", workflow)
        self.assertNotIn('$CARGO_TARGET_DIR/${{ matrix.target }}', workflow)
        self.assertEqual(
            workflow.count("uses: ./.github/actions/cargo-cache\n"),
            workflow.count("uses: ./.github/actions/cargo-cache-finalize\n"),
        )
        self.assertNotIn("persistent-eligible: true", workflow)
        effort = (ROOT / ".github/workflows/effort-ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertEqual(
            effort.count("uses: ./.github/actions/cargo-cache\n"),
            effort.count("uses: ./.github/actions/cargo-cache-finalize\n"),
        )
        for lane in (
            "rust-gate",
            "cluster-auth",
            "cluster-wal",
            "cluster-daemon",
        ):
            self.assertIn(f"lane: {lane}", workflow)

    def test_cache_pruner_is_bounded_to_the_runner_tool_cache(self):
        script = ROOT / "scripts/ci-cache-prune"
        subprocess.run(["bash", "-n", str(script)], check=True)
        with tempfile.TemporaryDirectory() as raw_directory:
            tool_cache = Path(raw_directory) / "tool"
            tool_cache.mkdir()
            fake_bin = tool_cache / "bin"
            fake_bin.mkdir()
            fake_df = fake_bin / "df"
            fake_df.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' "
                "'Filesystem 1024-blocks Used Available Capacity Mounted' "
                "'fixture 524288000 104857600 419430400 20% /fixture'\n",
                encoding="utf-8",
            )
            fake_df.chmod(0o755)
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
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
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
            self.assertIn("cache root does not exist", rejected.stderr)

            victim = tool_cache / "victim"
            victim.mkdir()
            (victim / "proof").write_text("outside cache", encoding="utf-8")
            escaped = str(cache_root / "../../../../victim")
            prefix_confusion = Path(f"{tool_cache}-other")
            confused_root = prefix_confusion / "plurx-ci/cargo/runner/rust-1.2.3"
            confused_root.mkdir(parents=True)
            symlink_root = cache_root.parent / "rust-9.9.9"
            symlink_root.symlink_to(victim, target_is_directory=True)
            for unsafe in (escaped, str(confused_root), str(symlink_root)):
                with self.subTest(unsafe=unsafe):
                    result = subprocess.run(
                        [str(script), unsafe, "1"],
                        env=environment,
                        capture_output=True,
                        text=True,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("refusing unsafe cache root", result.stderr)
                    self.assertTrue((victim / "proof").is_file())

    def test_cache_pruner_fails_when_pressure_cannot_be_restored(self):
        script = ROOT / "scripts/ci-cache-prune"
        with tempfile.TemporaryDirectory() as raw_directory:
            tool_cache = Path(raw_directory) / "tool"
            tool_cache.mkdir()
            fake_bin = tool_cache / "bin"
            fake_bin.mkdir()
            fake_df = fake_bin / "df"
            fake_df.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' "
                "'Filesystem 1024-blocks Used Available Capacity Mounted' "
                "'fixture 524288000 523239424 1048576 99% /fixture'\n",
                encoding="utf-8",
            )
            fake_df.chmod(0o755)
            runner_root = tool_cache / "plurx-ci/cargo/runner-01"
            cache_root = runner_root / "rust-1.97.1"
            target = cache_root / "rust-gate/target"
            target.mkdir(parents=True)
            (target / "proof").write_bytes(b"cache")
            stale = runner_root / "rust-1.96.0/stale/target"
            stale.mkdir(parents=True)
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                    "RUNNER_TOOL_CACHE": str(tool_cache),
                }
            )

            result = subprocess.run(
                [str(script), str(cache_root), "1"],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("below reserve", result.stderr)
            self.assertFalse((runner_root / "rust-1.96.0").exists())
            self.assertFalse(target.exists())

    def test_cache_pruner_uses_explicit_lru_markers(self):
        script = ROOT / "scripts/ci-cache-prune"
        with tempfile.TemporaryDirectory() as raw_directory:
            tool_cache = Path(raw_directory) / "tool"
            fake_bin = tool_cache / "bin"
            fake_bin.mkdir(parents=True)
            fake_df = fake_bin / "df"
            fake_df.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' "
                "'Filesystem 1024-blocks Used Available Capacity Mounted' "
                "'fixture 524288000 104857600 419430400 20% /fixture'\n",
                encoding="utf-8",
            )
            fake_df.chmod(0o755)
            fake_du = fake_bin / "du"
            fake_du.write_text(
                "#!/bin/sh\n"
                "if [ -d \"$STALE_TARGET\" ] && [ -d \"$HOT_TARGET\" ]; then\n"
                "  printf '2097152 %s\\n' \"$2\"\n"
                "else\n"
                "  printf '1 %s\\n' \"$2\"\n"
                "fi\n",
                encoding="utf-8",
            )
            fake_du.chmod(0o755)
            cache_root = tool_cache / "plurx-ci/cargo/runner-01/rust-1.97.1"
            stale_target = cache_root / "stale/target"
            hot_target = cache_root / "hot/target"
            stale_target.mkdir(parents=True)
            hot_target.mkdir(parents=True)
            stale_marker = stale_target.parent / ".last-used"
            hot_marker = hot_target.parent / ".last-used"
            stale_marker.touch()
            hot_marker.touch()
            os.utime(stale_marker, (1_000_000_000, 1_000_000_000))
            os.utime(hot_marker, (2_000_000_000, 2_000_000_000))
            environment = os.environ.copy()
            environment.update(
                {
                    "HOT_TARGET": str(hot_target),
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                    "RUNNER_TOOL_CACHE": str(tool_cache),
                    "STALE_TARGET": str(stale_target),
                }
            )

            subprocess.run(
                [str(script), str(cache_root), "1"],
                check=True,
                env=environment,
            )

            self.assertFalse(stale_target.exists())
            self.assertTrue(hot_target.is_dir())

    def test_buildkit_state_is_named_bounded_and_registry_aware(self):
        action = (ROOT / ".github/actions/buildx-cache/action.yml").read_text(
            encoding="utf-8"
        )
        config = (ROOT / ".github/buildkitd.toml").read_text(encoding="utf-8")
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

        self.assertIn("keep-state: true", action)
        self.assertIn('"$EXECUTION_MODE" != shadow', action)
        self.assertIn('"$PERSISTENT_ELIGIBLE" != true', action)
        self.assertIn('default: "false"', action)
        self.assertIn("builder_name=plurx-$runner_name", action)
        self.assertNotIn("plurx-$runner_name-$CACHE_LANE", action)
        self.assertIn("buildkitd-config:", action)
        self.assertIn('[registry."192.168.4.7:3000"]', config)
        self.assertIn("http = true", config)
        self.assertIn(
            'run: scripts/ci-buildkit-prune "$BUILDER_NAME" 50', workflow
        )
        self.assertIn("format('type=gha,mode=min,scope=package-", workflow)
        self.assertNotIn("persistent-eligible: true", workflow)

    def test_buildkit_pruner_enforces_hostwide_budget_and_reserve(self):
        script = ROOT / "scripts/ci-buildkit-prune"
        subprocess.run(["bash", "-n", str(script)], check=True)
        with tempfile.TemporaryDirectory() as raw_directory:
            fixture = Path(raw_directory)
            docker_root = fixture / "docker-root"
            fake_bin = fixture / "bin"
            docker_root.mkdir()
            fake_bin.mkdir()
            marker = fixture / "pruned"
            log = fixture / "docker.log"

            fake_docker = fake_bin / "docker"
            fake_docker.write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = info ]; then\n"
                "  printf '%s\\n' \"$DOCKER_ROOT_FIXTURE\"\n"
                "elif [ \"$1 $2\" = 'buildx du' ]; then\n"
                "  if [ -f \"$DOCKER_PRUNED\" ]; then size=$DU_AFTER_BYTES; "
                "else size=$DU_BEFORE_BYTES; fi\n"
                "  printf '{\"Size\":\"%s\"}\\n' \"$size\"\n"
                "elif [ \"$1 $2\" = 'buildx prune' ]; then\n"
                "  printf '%s\\n' \"$@\" > \"$DOCKER_LOG\"\n"
                "  touch \"$DOCKER_PRUNED\"\n"
                "else\n"
                "  exit 97\n"
                "fi\n",
                encoding="utf-8",
            )
            fake_docker.chmod(0o755)

            fake_df = fake_bin / "df"
            fake_df.write_text(
                "#!/bin/sh\n"
                "if [ -f \"$DOCKER_PRUNED\" ]; then "
                "available=$AFTER_AVAILABLE_KB; else available=52428800; fi\n"
                "printf '%s\\n' "
                "'Filesystem 1024-blocks Used Available Capacity Mounted'\n"
                "printf 'fixture 1073741824 1 %s 1%% /fixture\\n' \"$available\"\n",
                encoding="utf-8",
            )
            fake_df.chmod(0o755)

            summary = fixture / "summary.md"
            environment = os.environ.copy()
            environment.update(
                {
                    "AFTER_AVAILABLE_KB": "314572800",
                    "DOCKER_LOG": str(log),
                    "DOCKER_PRUNED": str(marker),
                    "DOCKER_ROOT_FIXTURE": str(docker_root),
                    "DU_AFTER_BYTES": str(40 * 1024**3),
                    "DU_BEFORE_BYTES": str(60 * 1024**3),
                    "GITHUB_STEP_SUMMARY": str(summary),
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                }
            )

            subprocess.run(
                [str(script), "plurx-runner-01", "50"],
                check=True,
                env=environment,
            )

            arguments = log.read_text(encoding="utf-8").splitlines()
            self.assertIn("--max-used-space", arguments)
            self.assertEqual(
                arguments[arguments.index("--max-used-space") + 1],
                str(50 * 1024**3),
            )
            self.assertIn("--min-free-space", arguments)
            self.assertEqual(
                arguments[arguments.index("--min-free-space") + 1],
                str(((1073741824 + 4) // 5) * 1024),
            )
            self.assertIn(
                "filesystem floor: 214748365 KiB", summary.read_text()
            )

            marker.unlink()
            environment.update(
                {
                    "AFTER_AVAILABLE_KB": "1048576",
                    "DU_AFTER_BYTES": str(60 * 1024**3),
                }
            )
            failed = subprocess.run(
                [str(script), "plurx-runner-01", "50"],
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("remains over", failed.stderr)

            unsafe = subprocess.run(
                [str(script), "../../foreign-builder", "50"],
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(unsafe.returncode, 0)
            self.assertIn("unsafe builder name", unsafe.stderr)

    def test_external_cargo_targets_reach_browser_harnesses(self):
        ui = (ROOT / "scripts/ui-baseline").read_text(encoding="utf-8")
        playback = (ROOT / "scripts/playback-lab").read_text(encoding="utf-8")
        action = (ROOT / ".github/actions/cargo-cache/action.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn('os.environ.get("CARGO_TARGET_DIR"', ui)
        self.assertIn("process.env.CARGO_TARGET_DIR", playback)
        self.assertIn('echo "CARGO_TARGET_DIR=$target_dir"', action)

        with tempfile.TemporaryDirectory() as raw_directory:
            target = Path(raw_directory) / "external-target"
            server = target / "debug" / "plurxd"
            server.parent.mkdir(parents=True)
            server.touch()
            environment = os.environ.copy()
            environment["CARGO_TARGET_DIR"] = str(target)
            node = subprocess.run(
                [
                    "node",
                    "-e",
                    "process.stdout.write(require('./scripts/playback-lab')"
                    ".defaultServerBin())",
                ],
                cwd=ROOT,
                env=environment,
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(Path(node.stdout), server)

            previous = os.environ.get("CARGO_TARGET_DIR")
            os.environ["CARGO_TARGET_DIR"] = str(target)
            try:
                contract = runpy.run_path(
                    str(ROOT / "scripts/ui-baseline"),
                    run_name="ui_baseline_cache_contract",
                )
                self.assertEqual(contract["default_server_bin"](), server)
            finally:
                if previous is None:
                    os.environ.pop("CARGO_TARGET_DIR", None)
                else:
                    os.environ["CARGO_TARGET_DIR"] = previous

    def test_scope_outputs_are_only_read_by_direct_dependents(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        matches = list(re.finditer(r"(?m)^  ([A-Za-z0-9_]+):\n", workflow))
        for index, match in enumerate(matches):
            end = matches[index + 1].start() if index + 1 < len(matches) else len(workflow)
            block = workflow[match.start() : end]
            if "needs.scope.outputs." not in block:
                continue
            with self.subTest(job=match.group(1)):
                direct_scope = re.search(
                    r"(?m)^    needs: (?:scope|\[[^\n\]]*\bscope\b[^\n\]]*\])$",
                    block,
                ) or re.search(
                    r"(?m)^    needs:\n(?:      - [^\n]+\n)*      - scope$",
                    block,
                )
                self.assertIsNotNone(direct_scope)

    def test_lru_and_post_job_enforcement_are_explicit(self):
        action = (ROOT / ".github/actions/cargo-cache/action.yml").read_text(
            encoding="utf-8"
        )
        finalizer = (
            ROOT / ".github/actions/cargo-cache-finalize/action.yml"
        ).read_text(encoding="utf-8")
        pruner = (ROOT / "scripts/ci-cache-prune").read_text(encoding="utf-8")

        self.assertIn(".last-used", action)
        self.assertIn('touch "$CACHE_LAST_USED"', finalizer)
        self.assertIn("always() && steps.cargo-cache.outputs.local == 'true'", (
            ROOT / ".github/workflows/ci.yml"
        ).read_text(encoding="utf-8"))
        self.assertIn('marker="$(dirname -- "$target_dir")/.last-used"', pruner)
        self.assertIn("cache remains over budget", pruner)

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
