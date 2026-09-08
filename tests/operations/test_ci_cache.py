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

    def test_cargo_cache_is_local_on_every_self_hosted_runner(self):
        action = (ROOT / ".github/actions/cargo-cache/action.yml").read_text(
            encoding="utf-8"
        )
        prepare = (ROOT / "scripts/ci-cargo-cache-prepare").read_text(
            encoding="utf-8"
        )
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

        # The bound cannot be conditional on a rollout flag. It was, and the
        # flag was never set, so every job took the unbounded hosted path into
        # a runner cache server that evicts nothing.
        self.assertIn('"${RUNNER_ENVIRONMENT:-}" = github-hosted', action)
        self.assertIn('[ -z "${RUNNER_TOOL_CACHE:-}" ]', action)
        self.assertNotIn('"$EXECUTION_MODE"', action)
        self.assertNotIn('"$PERSISTENT_ELIGIBLE"', action)
        self.assertNotIn("inputs.execution-mode", action)
        self.assertNotIn("inputs.persistent-eligible", action)
        self.assertIn("scripts/ci-runner-cache-audit", action)
        self.assertIn("scripts/ci-cargo-cache-prepare", action)
        self.assertIn("ensure_child_directory", prepare)
        self.assertIn("plurx-ci", prepare)
        self.assertIn("toolchain=$(rustc -Vv", action)
        self.assertIn("CARGO_INCREMENTAL=0", action)
        self.assertIn("uses: https://github.com/Swatinem/rust-cache@v2", action)
        self.assertNotIn("uses: https://github.com/Swatinem/rust-cache@v2", workflow)
        self.assertIn("needs.scope.outputs.execution_mode", workflow)
        self.assertNotIn('$CARGO_TARGET_DIR/${{ matrix.target }}', workflow)
        self.assertEqual(
            workflow.count("uses: ./.github/actions/cargo-cache\n"),
            workflow.count("uses: ./.github/actions/cargo-cache-finalize\n"),
        )
        self.assertNotIn("persistent-eligible", workflow)
        for caller in (
            ".github/workflows/ci.yml",
            ".github/workflows/effort-ci.yml",
            ".github/workflows/store-shards.yml",
            ".github/workflows/cluster-store-backstop.yml",
        ):
            text = (ROOT / caller).read_text(encoding="utf-8")
            self.assertNotIn("persistent-eligible", text)
            self.assertNotIn("          execution-mode:", text)
        effort = (ROOT / ".github/workflows/effort-ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertEqual(
            effort.count("uses: ./.github/actions/cargo-cache\n"),
            effort.count("uses: ./.github/actions/cargo-cache-finalize\n"),
        )
        for lane in (
            "rust-gate",
            "cluster-store-legacy",
            "cluster-topology",
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

    def test_cargo_cache_prepare_rejects_every_child_symlink(self):
        script = ROOT / "scripts/ci-cargo-cache-prepare"
        subprocess.run(["bash", "-n", str(script)], check=True)

        components = {
            "runner": "plurx-ci/cargo/runner-01",
            "toolchain": "plurx-ci/cargo/runner-01/rust-1.97.1",
            "cargo-home": (
                "plurx-ci/cargo/runner-01/rust-1.97.1/cargo-home"
            ),
            "lane": "plurx-ci/cargo/runner-01/rust-1.97.1/rust-gate",
            "target": (
                "plurx-ci/cargo/runner-01/rust-1.97.1/rust-gate/target"
            ),
            "marker": (
                "plurx-ci/cargo/runner-01/rust-1.97.1/"
                "rust-gate/.last-used"
            ),
        }
        for name, relative in components.items():
            with (
                self.subTest(component=name),
                tempfile.TemporaryDirectory() as raw,
            ):
                fixture = Path(raw)
                tool_cache = fixture / "tool"
                tool_cache.mkdir()
                victim_directory = fixture / "victim"
                victim_directory.mkdir()
                victim_file = fixture / "victim-marker"
                victim_file.write_text("outside cache", encoding="utf-8")
                candidate = tool_cache / relative
                candidate.parent.mkdir(parents=True)
                if name == "marker":
                    candidate.symlink_to(victim_file)
                else:
                    candidate.symlink_to(
                        victim_directory, target_is_directory=True
                    )

                result = subprocess.run(
                    [
                        str(script),
                        str(tool_cache),
                        "runner-01",
                        "1.97.1",
                        "rust-gate",
                    ],
                    capture_output=True,
                    text=True,
                )

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("refusing symbolic-link cache", result.stderr)
                self.assertEqual(
                    victim_file.read_text(encoding="utf-8"), "outside cache"
                )
                self.assertEqual(list(victim_directory.iterdir()), [])

    def test_cargo_cache_prepare_returns_canonical_bounded_paths(self):
        script = ROOT / "scripts/ci-cargo-cache-prepare"
        with tempfile.TemporaryDirectory() as raw:
            tool_cache = Path(raw) / "tool"
            tool_cache.mkdir()
            result = subprocess.run(
                [
                    str(script),
                    str(tool_cache),
                    "runner-01",
                    "1.97.1",
                    "rust-gate",
                ],
                check=True,
                capture_output=True,
                text=True,
            )

            cache_root, target_dir, cargo_home, marker = map(
                Path, result.stdout.splitlines()
            )
            expected_root = (
                tool_cache / "plurx-ci/cargo/runner-01/rust-1.97.1"
            ).resolve()
            self.assertEqual(cache_root, expected_root)
            self.assertEqual(target_dir, expected_root / "rust-gate/target")
            self.assertEqual(cargo_home, expected_root / "cargo-home")
            self.assertEqual(marker, expected_root / "rust-gate/.last-used")
            self.assertTrue(target_dir.is_dir())
            self.assertTrue(cargo_home.is_dir())
            self.assertTrue(marker.is_file())
            subprocess.run(
                [
                    str(ROOT / "scripts/ci-cargo-cache-refresh"),
                    str(cache_root),
                    str(marker),
                ],
                check=True,
            )

    def test_cargo_cache_refresh_rejects_post_job_symlink_swaps(self):
        prepare = ROOT / "scripts/ci-cargo-cache-prepare"
        refresh = ROOT / "scripts/ci-cargo-cache-refresh"
        subprocess.run(["bash", "-n", str(refresh)], check=True)

        for component in ("cargo-home", "lane", "target", "marker"):
            with (
                self.subTest(component=component),
                tempfile.TemporaryDirectory() as raw,
            ):
                fixture = Path(raw)
                tool_cache = fixture / "tool"
                tool_cache.mkdir()
                prepared = subprocess.run(
                    [
                        str(prepare),
                        str(tool_cache),
                        "runner-01",
                        "1.97.1",
                        "rust-gate",
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                cache_root, target, cargo_home, marker = map(
                    Path, prepared.stdout.splitlines()
                )
                lane = target.parent
                victim_directory = fixture / "victim"
                victim_directory.mkdir()
                victim_marker = fixture / "victim-marker"
                victim_marker.write_text("outside cache", encoding="utf-8")

                if component == "cargo-home":
                    cargo_home.rmdir()
                    cargo_home.symlink_to(
                        victim_directory, target_is_directory=True
                    )
                elif component == "lane":
                    marker.unlink()
                    target.rmdir()
                    lane.rmdir()
                    lane.symlink_to(victim_directory, target_is_directory=True)
                elif component == "target":
                    target.rmdir()
                    target.symlink_to(
                        victim_directory, target_is_directory=True
                    )
                else:
                    marker.unlink()
                    marker.symlink_to(victim_marker)

                result = subprocess.run(
                    [str(refresh), str(cache_root), str(marker)],
                    capture_output=True,
                    text=True,
                )

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("missing or symbolic", result.stderr)
                self.assertEqual(
                    victim_marker.read_text(encoding="utf-8"), "outside cache"
                )
                self.assertEqual(list(victim_directory.iterdir()), [])

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
        self.assertIn('"${RUNNER_ENVIRONMENT:-}" = github-hosted', action)
        self.assertNotIn('"$EXECUTION_MODE"', action)
        self.assertNotIn('"$PERSISTENT_ELIGIBLE"', action)
        self.assertNotIn("inputs.execution-mode", action)
        self.assertNotIn("inputs.persistent-eligible", action)
        self.assertIn("builder_name=plurx-$runner_name", action)
        self.assertNotIn("plurx-$runner_name-$CACHE_LANE", action)
        self.assertIn("buildkitd-config:", action)
        self.assertIn('[registry."192.168.4.7:3000"]', config)
        self.assertIn("http = true", config)
        self.assertIn(
            'run: scripts/ci-buildkit-prune "$BUILDER_NAME" 50', workflow
        )
        self.assertIn("scope=package-compile-{0}", workflow)
        self.assertIn("mode=max,scope=package-compile-{0}", workflow)
        self.assertIn("scope=package-runtime-{0}", workflow)
        self.assertIn("mode=min,scope=package-runtime-{0}", workflow)
        self.assertNotIn("persistent-eligible", workflow)

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

    def test_buildkit_usage_reads_the_units_buildx_actually_prints(self):
        """buildx 0.30.1 prints `"Size":"8.192kB"`, not a byte count.

        The parser wanted digits, so every record was rejected with `BuildKit
        returned an invalid disk-usage size` and the arm64 package lane failed
        after its build had succeeded. Both spellings are read now, and the
        totals still have to decide the budget correctly.
        """
        script = ROOT / "scripts/ci-buildkit-prune"
        with tempfile.TemporaryDirectory() as raw_directory:
            fixture = Path(raw_directory)
            docker_root = fixture / "docker-root"
            docker_root.mkdir()
            fake_bin = fixture / "bin"
            fake_bin.mkdir()
            marker = fixture / "pruned"
            log = fixture / "docker.log"

            fake_docker = fake_bin / "docker"
            fake_docker.write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = info ]; then\n"
                "  printf '%s\\n' \"$DOCKER_ROOT_FIXTURE\"\n"
                "elif [ \"$1 $2\" = 'buildx du' ]; then\n"
                "  if [ -f \"$DOCKER_PRUNED\" ]; then printf '%s\\n' "
                "\"$DU_AFTER\"; else printf '%s\\n' \"$DU_BEFORE\"; fi\n"
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
                "printf '%s\\n' "
                "'Filesystem 1024-blocks Used Available Capacity Mounted'\n"
                "printf 'fixture 1073741824 1 838860800 1%% /fixture\\n'\n",
                encoding="utf-8",
            )
            fake_df.chmod(0o755)

            summary = fixture / "summary.md"
            record = '{{"Description":"[build 2/4]","Size":"{0}","Type":"regular"}}'
            environment = os.environ.copy()
            environment.update(
                {
                    "DOCKER_LOG": str(log),
                    "DOCKER_PRUNED": str(marker),
                    "DOCKER_ROOT_FIXTURE": str(docker_root),
                    # 30 GB + 31 GB decimal = 61e9 bytes, over a 50 GiB budget.
                    "DU_BEFORE": "\n".join(
                        (record.format("30GB"), record.format("31GB"))
                    ),
                    # 8.192kB is verbatim what buildx 0.30.1 printed on nynuc.
                    "DU_AFTER": "\n".join(
                        (record.format("20GB"), record.format("8.192kB"))
                    ),
                    "GITHUB_STEP_SUMMARY": str(summary),
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                }
            )

            result = subprocess.run(
                [str(script), "plurx-runner-01", "50"],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            report = summary.read_text()
            # 61e9 bytes is 58174 MiB; 20000008192 is 19073 MiB.
            self.assertIn("before: 58174 MiB cache", report)
            self.assertIn("after: 19073 MiB cache", report)

            # And a builder still over budget in those units still fails.
            marker.unlink()
            environment["DU_AFTER"] = record.format("60GB")
            failed = subprocess.run(
                [str(script), "plurx-runner-01", "50"],
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("remains over", failed.stderr)

    def test_buildkit_budget_holds_when_the_docker_root_is_unreadable(self):
        """The runner can talk to the daemon and still not enter its directory.

        `/var/lib/docker` is root:root 0710 on a stock engine, so a runner in
        the `docker` group cannot `cd` into it. `gha-mbp-linux-arm-01` failed
        the arm64 package lane on exactly that, after its build had already
        succeeded. The budget does not need the filesystem — only the reserve
        does — so the budget is still enforced and the reserve is reported as
        unmeasured rather than guessed at or made fatal.
        """
        script = ROOT / "scripts/ci-buildkit-prune"
        with tempfile.TemporaryDirectory() as raw_directory:
            fixture = Path(raw_directory)
            fake_bin = fixture / "bin"
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

            summary = fixture / "summary.md"
            environment = os.environ.copy()
            environment.update(
                {
                    "DOCKER_LOG": str(log),
                    "DOCKER_PRUNED": str(marker),
                    # A path this process cannot enter, exactly as the runner
                    # cannot enter the real one.
                    "DOCKER_ROOT_FIXTURE": str(fixture / "unreadable/docker"),
                    "DU_AFTER_BYTES": str(40 * 1024**3),
                    "DU_BEFORE_BYTES": str(60 * 1024**3),
                    "GITHUB_STEP_SUMMARY": str(summary),
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                }
            )

            result = subprocess.run(
                [str(script), "plurx-runner-01", "50"],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            arguments = log.read_text(encoding="utf-8").splitlines()
            self.assertIn("--max-used-space", arguments)
            self.assertEqual(
                arguments[arguments.index("--max-used-space") + 1],
                str(50 * 1024**3),
            )
            # No reserve, because no reserve could be measured. Passing a
            # guessed one would be worse than passing none.
            self.assertNotIn("--min-free-space", arguments)
            report = summary.read_text()
            self.assertIn("not enforced", report)
            self.assertIn("not readable by this user", report)

            # The budget still discriminates: a builder that stays over it
            # fails, unreadable filesystem or not.
            marker.unlink()
            environment["DU_AFTER_BYTES"] = str(60 * 1024**3)
            failed = subprocess.run(
                [str(script), "plurx-runner-01", "50"],
                env=environment,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("remains over", failed.stderr)

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
        refresh = (ROOT / "scripts/ci-cargo-cache-refresh").read_text(
            encoding="utf-8"
        )

        self.assertIn("scripts/ci-cargo-cache-prepare", action)
        self.assertIn("scripts/ci-cargo-cache-refresh", finalizer)
        self.assertIn('touch -- "$last_used"', refresh)
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

    def test_cache_floor_is_satisfiable_on_the_smallest_runner_disk(self):
        """A reserve larger than the disk is a reserve that can never be met.

        The floor was a flat 100 GiB. Every Incus runner guest in this fleet is
        a 78-97 GB volume, so on those the pruner deleted every cache it was
        allowed to delete and then failed the job anyway. The floor is a share
        of the filesystem now, and this fixture is one of those guests.
        """
        script = ROOT / "scripts/ci-cache-prune"
        with tempfile.TemporaryDirectory() as raw_directory:
            tool_cache = Path(raw_directory) / "tool"
            tool_cache.mkdir()
            fake_bin = tool_cache / "bin"
            fake_bin.mkdir()
            fake_df = fake_bin / "df"
            # 78 GiB total, 40 GiB available: gha-m6-general-01's root volume.
            fake_df.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' "
                "'Filesystem 1024-blocks Used Available Capacity Mounted' "
                "'fixture 81788928 39845888 41943040 49% /fixture'\n",
                encoding="utf-8",
            )
            fake_df.chmod(0o755)
            cache_root = tool_cache / "plurx-ci/cargo/runner-01/rust-1.97.1"
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

            result = subprocess.run(
                [str(script), str(cache_root), "30"],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(target.is_dir())
            report = summary.read_text()
            self.assertIn("prune decision: `within-budget`", report)
            # 20 % of 78 GiB, and demonstrably less than the whole filesystem.
            self.assertIn("filesystem floor: 15 GiB", report)

    def test_runner_cache_server_is_reported_and_never_deleted(self):
        """The one cache a job can see and must not touch.

        `forgejo-runner` 13.1.0 serves `actions/cache` from a bolt.db index
        beside a tree of blobs and evicts neither, so the directory only grows.
        A job cannot safely delete from it — index rows and blobs have to go
        together, and that needs the runner stopped — so the contract is that
        the audit reports the number, warns on it, and leaves every byte alone.
        """
        script = ROOT / "scripts/ci-runner-cache-audit"
        subprocess.run(["bash", "-n", str(script)], check=True)
        with tempfile.TemporaryDirectory() as raw_directory:
            runner_root = Path(raw_directory) / "forgejo-runner"
            blobs = runner_root / "cache/cache/0a"
            blobs.mkdir(parents=True)
            (runner_root / "cache/bolt.db").write_bytes(b"index")
            for name in ("11", "12", "13"):
                (blobs / name).write_bytes(b"x" * 4096)
            (runner_root / "tool-cache").mkdir()
            summary = Path(raw_directory) / "summary.md"
            # A fixture cannot hold 41G, and the size is the whole point, so
            # `du` is the thing this substitutes — the same way the pruner
            # tests substitute `df`.
            fake_bin = Path(raw_directory) / "bin"
            fake_bin.mkdir()
            fake_du = fake_bin / "du"
            fake_du.write_text(
                "#!/bin/sh\n"
                "shift $(($# - 1))\n"
                "printf '%s\\t%s\\n' \"$FIXTURE_USED_KB\" \"$1\"\n",
                encoding="utf-8",
            )
            fake_du.chmod(0o755)
            environment = os.environ.copy()
            environment.update(
                {
                    "GITHUB_STEP_SUMMARY": str(summary),
                    "RUNNER_NAME": "gha-m6-general-01",
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                    "FIXTURE_USED_KB": str(41 * 1024 * 1024),
                }
            )

            loud = subprocess.run(
                [str(script), str(runner_root), "20"],
                env=environment,
                capture_output=True,
                text=True,
            )
            environment["FIXTURE_USED_KB"] = str(3 * 1024 * 1024)
            quiet = subprocess.run(
                [str(script), str(runner_root), "20"],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertEqual(quiet.returncode, 0, quiet.stderr)
            self.assertEqual(loud.returncode, 0, loud.stderr)
            report = summary.read_text()
            self.assertIn("### Runner cache server", report)
            self.assertIn("gha-m6-general-01", report)
            self.assertIn("in 3 entries", report)
            self.assertIn("eviction: none", report)
            self.assertIn("retained: 41 GiB", report)
            self.assertIn("retained: 3 GiB", report)
            # Discriminating in both directions: over the threshold it names
            # the runner, under it says nothing at all.
            self.assertIn("::warning::gha-m6-general-01's cache server holds 41G", loud.stdout)
            self.assertNotIn("::warning::", quiet.stdout)
            for name in ("11", "12", "13"):
                self.assertTrue((blobs / name).is_file())
            self.assertTrue((runner_root / "cache/bolt.db").is_file())

            # A runner whose layout this does not recognize is not an error.
            for absent in (runner_root / "tool-cache", Path(raw_directory) / "nope"):
                with self.subTest(absent=str(absent)):
                    skipped = subprocess.run(
                        [str(script), str(absent), "1"],
                        env=environment,
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(skipped.returncode, 0, skipped.stderr)

    @staticmethod
    def _composite_run_block(action_path, marker):
        """The shell of one composite step, dedented so it can be executed.

        These blocks are the only place the runner-local cache path exists, and
        until 2026-09-07 no job had ever run one — the gate above them was
        false on every job — so `RUNNER_NAME: unbound variable` sat in both of
        them undetected. Reading the file is not enough; the block has to run.
        """
        lines = (ROOT / action_path).read_text(encoding="utf-8").splitlines()
        start = next(
            index
            for index, line in enumerate(lines)
            if line.strip() == "run: |" and marker in "\n".join(lines[: index + 1])
        )
        indent = len(lines[start]) - len(lines[start].lstrip()) + 2
        body = []
        for line in lines[start + 1 :]:
            if line.strip() and len(line) - len(line.lstrip()) < indent:
                break
            body.append(line[indent:] if len(line) >= indent else line)
        return "\n".join(body)

    def _runner_fixture(self, directory):
        """A Forgejo runner instance: a runner root with a tool cache in it."""
        fixture = Path(directory)
        runner_root = fixture / "opt/forgejo-runner-03"
        tool_cache = runner_root / "tool-cache"
        tool_cache.mkdir(parents=True)
        fake_bin = fixture / "bin"
        fake_bin.mkdir()
        rustc = fake_bin / "rustc"
        rustc.write_text(
            "#!/bin/sh\nprintf 'rustc 1.97.1\\nrelease: 1.97.1\\n'\n",
            encoding="utf-8",
        )
        rustc.chmod(0o755)
        df = fake_bin / "df"
        df.write_text(
            "#!/bin/sh\n"
            "printf '%s\\n' "
            "'Filesystem 1024-blocks Used Available Capacity Mounted' "
            "'fixture 81788928 20971520 60817408 26%% /fixture'\n",
            encoding="utf-8",
        )
        df.chmod(0o755)
        environment = os.environ.copy()
        environment.pop("RUNNER_NAME", None)
        environment.update(
            {
                "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                "GITHUB_WORKSPACE": str(ROOT),
                "GITHUB_ENV": str(fixture / "env"),
                "GITHUB_OUTPUT": str(fixture / "output"),
                "GITHUB_STEP_SUMMARY": str(fixture / "summary"),
                "RUNNER_ENVIRONMENT": "self-hosted",
                "RUNNER_TOOL_CACHE": str(tool_cache),
                "RUNNER_WORKSPACE": str(fixture),
                "CACHE_LANE": "rust-gate",
                "CACHE_BUDGET_GB": "30",
                "DISK_GB": "25",
            }
        )
        return fixture, tool_cache, environment

    def test_the_cargo_cache_path_runs_without_a_RUNNER_NAME(self):
        block = self._composite_run_block(
            ".github/actions/cargo-cache/action.yml", "Select and bound"
        )
        with tempfile.TemporaryDirectory() as directory:
            fixture, tool_cache, environment = self._runner_fixture(directory)

            result = subprocess.run(
                ["bash", "-c", block],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("unbound variable", result.stderr)
            outputs = dict(
                line.split("=", 1)
                for line in (fixture / "output").read_text().splitlines()
                if "=" in line
            )
            self.assertEqual(outputs["local"], "true")
            # Unique per runner INSTANCE, not per host: nynuc runs four runners
            # and rogg16 five, and two sharing a Cargo root would prune each
            # other's target directories mid-build.
            self.assertIn("forgejo-runner-03", outputs["cache_root"])
            self.assertTrue(outputs["cache_root"].startswith(str(tool_cache.resolve())))
            self.assertTrue(Path(outputs["target_dir"]).is_dir())
            written = (fixture / "env").read_text()
            self.assertIn(f"CARGO_HOME={outputs['cache_root']}/cargo-home", written)
            self.assertIn("CARGO_INCREMENTAL=0", written)

    def test_the_buildkit_path_runs_without_a_RUNNER_NAME(self):
        block = self._composite_run_block(
            ".github/actions/buildx-cache/action.yml", "Select the BuildKit"
        )
        with tempfile.TemporaryDirectory() as directory:
            fixture, _, environment = self._runner_fixture(directory)
            environment["CACHE_LANE"] = "package-smoke-amd64"

            result = subprocess.run(
                ["bash", "-c", block],
                env=environment,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("unbound variable", result.stderr)
            outputs = dict(
                line.split("=", 1)
                for line in (fixture / "output").read_text().splitlines()
                if "=" in line
            )
            self.assertEqual(outputs["local"], "true")
            # Several runners on one host share one Docker daemon, so a builder
            # name that is only the hostname would have them pruning each other.
            self.assertTrue(outputs["builder_name"].startswith("plurx-"))
            self.assertIn("forgejo-runner-03", outputs["builder_name"])


if __name__ == "__main__":
    unittest.main()
